//! Resume and recovery logic for export pipeline
//!
//! Handles detecting completed work in output directories
//! and resuming failed or paused jobs.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::fs;

use super::models::{DetectedWork, StepStatus, StepType};
use super::state_machine::{get_job_with_steps, update_step_status, update_job_status};
use super::models::JobStatus;

// ============================================================================
// Detect Completed Work
// ============================================================================

/// Scan an output directory to detect already-completed work
pub fn detect_completed_work(output_dir: &Path) -> Result<DetectedWork> {
    let mut detected = DetectedWork::default();

    // Scan masters directory
    let masters_dir = output_dir.join("masters");
    if masters_dir.exists() {
        if let Ok(entries) = fs::read_dir(&masters_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    let path_str = path.to_string_lossy().to_string();

                    if filename.starts_with("master_bias") {
                        detected.master_bias_files.push(path_str);
                    } else if filename.starts_with("master_dark") && !filename.contains("darkflat") {
                        detected.master_dark_files.push(path_str);
                    } else if filename.starts_with("master_darkflat") {
                        detected.master_darkflat_files.push(path_str);
                    } else if filename.starts_with("master_flat") {
                        detected.master_flat_files.push(path_str);
                    }
                }
            }
        }
    }

    // Scan process directory for calibrated and registered files
    let process_dir = output_dir.join("process");
    if process_dir.exists() {
        scan_process_directory(&process_dir, &mut detected)?;
    }

    // Also check subdirectories like all_lights_mono, all_lights_osc
    for subdir in &["all_lights_mono", "all_lights_osc"] {
        let sub_process_dir = process_dir.join(subdir);
        if sub_process_dir.exists() {
            scan_process_directory(&sub_process_dir, &mut detected)?;
        }
    }

    // Scan for stacked outputs
    scan_for_stacked_files(output_dir, &mut detected)?;

    Ok(detected)
}

fn scan_process_directory(dir: &Path, detected: &mut DetectedWork) -> Result<()> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                let path_str = path.to_string_lossy().to_string();

                // Check for calibrated files (pp_*)
                if filename.starts_with("pp_") && !filename.starts_with("r_pp_") {
                    detected.calibrated_light_files.push(path_str);
                }
                // Check for registered files (r_pp_*)
                else if filename.starts_with("r_pp_") {
                    detected.registered_light_files.push(path_str);
                }
            }
        }
    }
    Ok(())
}

fn scan_for_stacked_files(output_dir: &Path, detected: &mut DetectedWork) -> Result<()> {
    // Check in masters directory
    let masters_dir = output_dir.join("masters");
    if masters_dir.exists() {
        if let Ok(entries) = fs::read_dir(&masters_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    // Stacked files typically end with _stacked or similar
                    if filename.contains("_stacked") || filename.contains("_stack") {
                        detected.stacked_files.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // Check in output directory root
    if let Ok(entries) = fs::read_dir(output_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename.contains("_stacked") || filename.contains("_stack") {
                    detected.stacked_files.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Resume Logic
// ============================================================================

/// Check if a job can be resumed
pub fn can_resume_job(conn: &Connection, job_id: i64) -> Result<bool> {
    let job = get_job_with_steps(conn, job_id)?;

    // Can resume if:
    // 1. Job is failed or paused
    // 2. At least one step was completed
    // 3. There are pending steps remaining
    let has_completed = job.steps.iter().any(|s| s.status == StepStatus::Completed);
    let has_pending = job.steps.iter().any(|s| s.status == StepStatus::Pending);
    let is_resumable = matches!(job.status, JobStatus::Failed | JobStatus::Paused);

    Ok(is_resumable && has_completed && has_pending)
}

/// Get the step order to resume from (first non-completed step)
pub fn get_resume_point(conn: &Connection, job_id: i64) -> Result<Option<i32>> {
    let job = get_job_with_steps(conn, job_id)?;

    // Find the first step that is not completed or skipped
    let resume_step = job.steps.iter()
        .find(|s| s.status != StepStatus::Completed && s.status != StepStatus::Skipped);

    Ok(resume_step.map(|s| s.step_order))
}

/// Prepare a job for resuming from a specific step
pub fn prepare_resume(
    conn: &Connection,
    job_id: i64,
    from_step_order: Option<i32>,
) -> Result<()> {
    let job = get_job_with_steps(conn, job_id)?;

    // If a specific step is provided, reset from that step onwards
    // If not, reset from the first failed/pending step
    let reset_from = from_step_order.unwrap_or_else(|| {
        job.steps.iter()
            .find(|s| s.status == StepStatus::Failed || s.status == StepStatus::Pending)
            .map(|s| s.step_order)
            .unwrap_or(1)
    });

    // Reset all steps from reset_from onwards to pending
    for step in job.steps.iter().filter(|s| s.step_order >= reset_from) {
        if step.status != StepStatus::Completed && step.status != StepStatus::Skipped {
            update_step_status(conn, step.id, StepStatus::Pending, None)?;
        }
    }

    // Set job status back to pending
    update_job_status(conn, job_id, JobStatus::Pending, None)?;

    Ok(())
}

/// Smart resume that checks filesystem for already-completed work
pub fn smart_resume(
    conn: &Connection,
    job_id: i64,
) -> Result<Vec<String>> {
    let job = get_job_with_steps(conn, job_id)?;
    let output_dir = Path::new(&job.output_dir);

    // Detect what work has been done
    let detected = detect_completed_work(output_dir)?;

    let mut skipped_steps: Vec<String> = Vec::new();

    // Mark steps as completed if their outputs exist
    for step in &job.steps {
        if step.status == StepStatus::Pending || step.status == StepStatus::Failed {
            let should_skip = match step.step_type {
                StepType::ValidateSources | StepType::OrganizeFiles => {
                    // Always re-run validation and organization
                    false
                }
                // Per-master steps - check if specific master exists
                StepType::CreateMasterBias => {
                    // Check if this specific master bias exists
                    if let Some(ref data) = step.step_data {
                        if let Some(set_id) = data.set_id {
                            detected.master_bias_files.iter()
                                .any(|f| f.contains(&format!("master_bias_{}", set_id)))
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                StepType::CreateMasterDark => {
                    if let Some(ref data) = step.step_data {
                        if let Some(set_id) = data.set_id {
                            detected.master_dark_files.iter()
                                .any(|f| f.contains(&format!("master_dark_{}", set_id)))
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                StepType::CreateMasterDarkFlat => {
                    if let Some(ref data) = step.step_data {
                        if let Some(set_id) = data.set_id {
                            detected.master_darkflat_files.iter()
                                .any(|f| f.contains(&format!("master_darkflat_{}", set_id)))
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                StepType::CreateMasterFlat => {
                    if let Some(ref data) = step.step_data {
                        if let Some(set_id) = data.set_id {
                            detected.master_flat_files.iter()
                                .any(|f| f.contains(&format!("master_flat_{}", set_id)))
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                // Legacy monolithic master step
                StepType::CreateMasters => {
                    // Check if any master files exist (not checking individual sets in new simplified model)
                    !detected.master_bias_files.is_empty()
                        || !detected.master_dark_files.is_empty()
                        || !detected.master_darkflat_files.is_empty()
                        || !detected.master_flat_files.is_empty()
                }
                // Per-branch calibration - check if pp_ files exist for this branch
                StepType::CalibrateBranch => {
                    // For simplicity, just check if any calibrated files exist
                    // More precise logic would check the specific branch folder
                    !detected.calibrated_light_files.is_empty()
                }
                StepType::CalibrateLights => {
                    // Check if calibrated files exist
                    !detected.calibrated_light_files.is_empty()
                }
                StepType::CollectCalibrated => {
                    // Already have calibrated files collected
                    !detected.calibrated_light_files.is_empty()
                }
                // Per-branch registration (multi-group fix)
                StepType::RegisterBranch => {
                    // Check if r_pp_lights_*.fit files exist in branch folder
                    !detected.registered_light_files.is_empty()
                }
                // Collect registered frames
                StepType::CollectRegistered => {
                    // Check if all_r_*.fit files exist in process/all_mono or all_osc
                    !detected.registered_light_files.is_empty()
                }
                // Global registration (multi-group fix)
                StepType::GlobalRegistration => {
                    // Check if r_all_r_*.fit files exist
                    !detected.registered_light_files.is_empty()
                }
                // Prepare stacking directories
                StepType::PrepareStacking => {
                    // Check if globally registered files exist
                    !detected.registered_light_files.is_empty()
                }
                // Per-camera registration (legacy)
                StepType::RegisterFrames => {
                    // Check if registered files exist
                    !detected.registered_light_files.is_empty()
                }
                // Per-group stacking
                StepType::StackGroup => {
                    // Check if stacked files exist for this group
                    !detected.stacked_files.is_empty()
                }
                StepType::GenerateRegistration => {
                    // Check if registration script exists
                    let script_path = output_dir.join("02_register_and_stack.ssf");
                    script_path.exists()
                }
                StepType::RegisterAndStack => {
                    // Check if registered or stacked files exist
                    !detected.registered_light_files.is_empty() || !detected.stacked_files.is_empty()
                }
            };

            if should_skip {
                update_step_status(conn, step.id, StepStatus::Skipped,
                    Some("Output already exists from previous run"))?;
                skipped_steps.push(step.step_name.clone());
            }
        }
    }

    Ok(skipped_steps)
}

// ============================================================================
// Retry Logic
// ============================================================================

/// Retry a failed step
pub fn retry_step(
    conn: &Connection,
    step_id: i64,
) -> Result<()> {
    // Reset step to pending
    update_step_status(conn, step_id, StepStatus::Pending, None)?;

    // Increment retry count (done in state_machine)
    super::state_machine::increment_step_retry(conn, step_id)?;

    Ok(())
}

/// Check if a step can be retried (max 3 retries)
pub fn can_retry_step(conn: &Connection, step_id: i64) -> Result<bool> {
    let step = super::state_machine::get_step(conn, step_id)?;

    // Can retry if:
    // 1. Step failed
    // 2. Retry count < 3
    Ok(step.status == StepStatus::Failed && step.retry_count < 3)
}
