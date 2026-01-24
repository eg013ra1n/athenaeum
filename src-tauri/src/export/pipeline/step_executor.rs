//! Step executor for pipeline execution
//!
//! Executes individual pipeline steps with progress tracking.
//! Each step is aligned with actual Siril script execution.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

use super::models::{
    ExportJob, ExportJobStep, StepStatus, StepType, PipelineProgress,
};
use super::state_machine::{
    update_step_status, update_step_output_files, update_completed_steps,
    update_job_status, get_job_with_steps,
};
use super::validation::{validate_source_files, validate_calibration_files};
use super::models::JobStatus;
use crate::export::siril::cli_runner::{
    find_siril_cli, run_siril_script, collect_calibrated_frames, generate_registration_script,
};
use crate::db::get_setting;

// ============================================================================
// Step Execution
// ============================================================================

/// Result of executing a step
pub struct StepResult {
    pub success: bool,
    pub output_files: Vec<String>,
    pub error_message: Option<String>,
    pub should_skip: bool,
}

impl StepResult {
    pub fn success(output_files: Vec<String>) -> Self {
        Self {
            success: true,
            output_files,
            error_message: None,
            should_skip: false,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            success: false,
            output_files: Vec::new(),
            error_message: Some(error),
            should_skip: false,
        }
    }

    pub fn skipped(reason: String) -> Self {
        Self {
            success: true,
            output_files: Vec::new(),
            error_message: Some(reason),
            should_skip: true,
        }
    }
}

/// Get Siril CLI path from settings or auto-detect
fn get_siril_path(conn: &Connection) -> Result<String> {
    // Try settings first
    if let Ok(Some(path)) = get_setting(conn, "siril_cli_path") {
        if Path::new(&path).exists() {
            return Ok(path);
        }
    }

    // Fall back to auto-detection
    find_siril_cli()
        .ok_or_else(|| anyhow::anyhow!(
            "Siril CLI not found. Please install Siril or set the path in Settings → Integration."
        ))
}

/// Execute a single step
pub fn execute_step(
    conn: &Connection,
    app_handle: &AppHandle,
    job: &ExportJob,
    step: &ExportJobStep,
) -> Result<StepResult> {
    // Emit progress at start
    emit_progress(app_handle, job, step, 0.0, &format!("Starting: {}", step.step_name));

    let result = match step.step_type {
        // Pre-processing steps
        StepType::ValidateSources => {
            execute_validate_sources(conn, job, step, app_handle)
        }
        StepType::OrganizeFiles => {
            execute_organize_files(conn, job, step, app_handle)
        }

        // Per-master steps (new granular)
        StepType::CreateMasterBias |
        StepType::CreateMasterDark |
        StepType::CreateMasterDarkFlat |
        StepType::CreateMasterFlat => {
            execute_create_single_master(conn, job, step, app_handle)
        }

        // Per-branch calibration (new granular)
        StepType::CalibrateBranch => {
            execute_calibrate_single_branch(conn, job, step, app_handle)
        }

        // Per-branch registration (multi-group fix)
        StepType::RegisterBranch => {
            execute_register_single_branch(conn, job, step, app_handle)
        }

        // Collection step - new version collects registered frames
        StepType::CollectRegistered => {
            execute_collect_registered(conn, job, step, app_handle)
        }

        // Global registration (multi-group fix)
        StepType::GlobalRegistration => {
            execute_global_registration(conn, job, step, app_handle)
        }

        // Prepare stacking directories (multi-group fix)
        StepType::PrepareStacking => {
            execute_prepare_stacking(conn, job, step, app_handle)
        }

        // Per-group stacking (new granular)
        StepType::StackGroup => {
            execute_stack_group(conn, job, step, app_handle)
        }

        // Legacy collection step (backward compatibility)
        StepType::CollectCalibrated => {
            execute_collect_calibrated(conn, job, step, app_handle)
        }

        // Legacy per-camera registration (backward compatibility)
        StepType::RegisterFrames => {
            execute_register_camera_type(conn, job, step, app_handle)
        }

        // Legacy monolithic steps (backward compatibility)
        StepType::CreateMasters => {
            execute_create_masters(conn, job, step, app_handle)
        }
        StepType::CalibrateLights => {
            execute_calibrate_lights(conn, job, step, app_handle)
        }
        StepType::GenerateRegistration => {
            execute_generate_registration(conn, job, step, app_handle)
        }
        StepType::RegisterAndStack => {
            execute_register_and_stack(conn, job, step, app_handle)
        }
    };

    // Emit progress at end
    if result.is_ok() {
        let r = result.as_ref().unwrap();
        if r.success && !r.should_skip {
            emit_progress(app_handle, job, step, 1.0, &format!("Completed: {}", step.step_name));
        }
    }

    result
}

// ============================================================================
// Individual Step Implementations
// ============================================================================

fn execute_validate_sources(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    emit_progress(app_handle, job, step, 0.1, "Validating source files...");

    let source_validations = validate_source_files(conn, job.frame_set_id)?;
    let missing: Vec<_> = source_validations.iter().filter(|v| !v.exists).collect();

    emit_progress(app_handle, job, step, 0.5, "Validating calibration files...");

    let cal_validations = validate_calibration_files(conn, job.frame_set_id)?;
    let missing_cal: Vec<_> = cal_validations.iter().filter(|v| !v.exists).collect();

    emit_progress(app_handle, job, step, 0.9, &format!(
        "Validated {} source files, {} calibration files",
        source_validations.len(),
        cal_validations.len()
    ));

    if !missing.is_empty() {
        return Ok(StepResult::failure(format!(
            "{} source files are missing: {}",
            missing.len(),
            missing.iter().take(3).map(|v| v.path.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }

    if !missing_cal.is_empty() {
        // Calibration files missing is a warning, not a failure
        println!("Warning: {} calibration files are missing", missing_cal.len());
    }

    Ok(StepResult::success(vec![]))
}

fn execute_organize_files(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    emit_progress(app_handle, job, step, 0.1, "Collecting export data...");

    // Collect export data
    let export_data = crate::export::data_collector::collect_export_data_v3(conn, job.frame_set_id)?;

    emit_progress(app_handle, job, step, 0.3, "Organizing files into folder structure...");

    // Organize files
    let _organize_result = crate::export::organize_files_v4(
        &job.config,
        &export_data,
    )?;

    emit_progress(app_handle, job, step, 0.6, "Generating Siril scripts...");

    // Generate scripts - uses config.output_dir internally
    let scripts = crate::export::siril::generate_scripts_v3(
        &job.config,
        &export_data,
    )?;

    emit_progress(app_handle, job, step, 0.9, &format!(
        "Generated {} scripts, organized {} branches",
        scripts.len(),
        export_data.branches.len()
    ));

    // Collect output files (scripts and main folders)
    let output_dir = Path::new(&job.output_dir);
    let mut output_files: Vec<String> = scripts.iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    // Add the main folders
    if output_dir.join("biases").exists() {
        output_files.push(output_dir.join("biases").to_string_lossy().to_string());
    }
    if output_dir.join("darks").exists() {
        output_files.push(output_dir.join("darks").to_string_lossy().to_string());
    }
    if output_dir.join("flats").exists() {
        output_files.push(output_dir.join("flats").to_string_lossy().to_string());
    }
    if output_dir.join("lights").exists() {
        output_files.push(output_dir.join("lights").to_string_lossy().to_string());
    }
    if output_dir.join("masters").exists() {
        output_files.push(output_dir.join("masters").to_string_lossy().to_string());
    }

    Ok(StepResult::success(output_files))
}

fn execute_create_masters(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);
    let script_path = output_dir.join("00_create_masters.ssf");

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::skipped(
            "No masters script generated - calibration sets may be incomplete".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 0.1, "Getting Siril CLI path...");

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.15, &format!(
        "Running 00_create_masters.ssf with Siril at {}",
        siril_path
    ));

    // Run the script
    run_siril_script(&siril_path, &script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying master outputs...");

    // Verify outputs - scan for master files
    let masters_dir = output_dir.join("masters");
    let master_files: Vec<String> = WalkDir::new(&masters_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.ends_with(".fit") || name.ends_with(".fits")
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if master_files.is_empty() {
        // Check if there were any masters to create
        let export_data = crate::export::data_collector::collect_export_data_v3(conn, job.frame_set_id)?;
        if export_data.master_plan.masters.is_empty() {
            return Ok(StepResult::skipped("No masters were needed".to_string()));
        }
        return Ok(StepResult::failure("No master files were created by Siril".to_string()));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Created {} master calibration files",
        master_files.len()
    ));

    Ok(StepResult::success(master_files))
}

fn execute_calibrate_lights(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);
    let script_path = output_dir.join("01_calibrate_lights.ssf");

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(
            "Missing required script: 01_calibrate_lights.ssf".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 0.1, "Getting Siril CLI path...");

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.15, &format!(
        "Running 01_calibrate_lights.ssf with Siril at {}",
        siril_path
    ));

    // Run the script
    run_siril_script(&siril_path, &script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying calibrated outputs...");

    // Verify outputs - scan for pp_lights files in lights subdirectories
    let lights_dir = output_dir.join("lights");
    let pp_files: Vec<String> = WalkDir::new(&lights_dir)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if pp_files.is_empty() {
        return Ok(StepResult::failure(
            "No calibrated frames (pp_*.fit) were created by Siril".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Calibrated {} light frames",
        pp_files.len()
    ));

    Ok(StepResult::success(pp_files))
}

fn execute_collect_calibrated(
    _conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);

    emit_progress(app_handle, job, step, 0.1, "Scanning for calibrated frames...");

    // Use existing collect_calibrated_frames function
    let collected = collect_calibrated_frames(output_dir, app_handle)?;

    if collected.total() == 0 {
        return Ok(StepResult::failure(
            "No calibrated frames found. Check calibration script output.".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 0.9, &format!(
        "Collected {} frames ({} mono, {} OSC)",
        collected.total(),
        collected.mono_count(),
        collected.osc_count()
    ));

    // Build output files list
    let mut output_files = Vec::new();
    if let Some(ref mono) = collected.mono {
        output_files.push(mono.dir.to_string_lossy().to_string());
    }
    if let Some(ref osc) = collected.osc {
        output_files.push(osc.dir.to_string_lossy().to_string());
    }

    Ok(StepResult::success(output_files))
}

fn execute_generate_registration(
    _conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);

    emit_progress(app_handle, job, step, 0.1, "Re-scanning collected frames...");

    // Re-collect to get frame info (needed for filter grouping)
    let collected = collect_calibrated_frames(output_dir, app_handle)?;

    if collected.total() == 0 {
        return Ok(StepResult::failure(
            "No collected frames found for registration script generation.".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 0.3, "Generating registration script based on collected frames...");

    // Extract parameters from config
    let focal_length = 500.0;  // TODO: Get from config when added
    let pixel_size = 3.76;     // TODO: Get from config when added

    let rejection_low = job.config.rejection_low;
    let rejection_high = job.config.rejection_high;

    // Generate the registration script
    let script_path = generate_registration_script(
        &collected,
        output_dir,
        focal_length,
        pixel_size,
        rejection_low,
        rejection_high,
    )?;

    emit_progress(app_handle, job, step, 0.9, &format!(
        "Generated registration script: {}",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    if !script_path.exists() {
        return Ok(StepResult::failure(
            "Failed to generate registration script".to_string()
        ));
    }

    Ok(StepResult::success(vec![script_path.to_string_lossy().to_string()]))
}

fn execute_register_and_stack(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);
    let script_path = output_dir.join("02_register_and_stack.ssf");

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(
            "Missing registration script: 02_register_and_stack.ssf. Run GenerateRegistration step first.".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 0.1, "Getting Siril CLI path...");

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.15, &format!(
        "Running 02_register_and_stack.ssf with Siril at {}",
        siril_path
    ));

    // Run the script
    run_siril_script(&siril_path, &script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying stacked outputs...");

    // Verify outputs - scan for stacked files
    let masters_dir = output_dir.join("masters");
    let stacked_files: Vec<String> = WalkDir::new(&masters_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.ends_with("_stacked.fit") || name.ends_with("_stacked.fits")
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if stacked_files.is_empty() {
        // This might not be a failure - check for registered files at least
        let process_dir = output_dir.join("process");
        let reg_files: Vec<String> = WalkDir::new(&process_dir)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        if reg_files.is_empty() {
            return Ok(StepResult::failure(
                "No registered or stacked outputs found. Siril registration may have failed.".to_string()
            ));
        }

        emit_progress(app_handle, job, step, 1.0, &format!(
            "Registration complete ({} registered frames), but no stacked outputs",
            reg_files.len()
        ));
        return Ok(StepResult::success(reg_files));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Created {} stacked images",
        stacked_files.len()
    ));

    Ok(StepResult::success(stacked_files))
}

// ============================================================================
// Per-Step Executor Implementations (V5 Granular Pipeline)
// ============================================================================

/// Execute a single master creation step
fn execute_create_single_master(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for master creation step"))?;

    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;

    let script_path = Path::new(script_path_str);

    // Check if output already exists (skip logic)
    if let Some(ref output_path) = step_data.output_path {
        let output = Path::new(output_path);
        // Check for .fit or .fits extension
        if output.with_extension("fit").exists() || output.with_extension("fits").exists() {
            return Ok(StepResult::skipped(format!(
                "Master already exists: {}",
                output.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
    }

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(format!(
            "Missing master script: {}",
            script_path.display()
        )));
    }

    let master_type = step_data.master_type.as_deref().unwrap_or("Unknown");
    let set_id = step_data.set_id.unwrap_or(0);

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Creating {} master (set {})...",
        master_type.to_lowercase(),
        set_id
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying master output...");

    // Verify output
    let mut output_files = Vec::new();
    if let Some(ref output_path) = step_data.output_path {
        let output = Path::new(output_path);
        let fit_path = output.with_extension("fit");
        let fits_path = output.with_extension("fits");

        if fit_path.exists() {
            output_files.push(fit_path.to_string_lossy().to_string());
        } else if fits_path.exists() {
            output_files.push(fits_path.to_string_lossy().to_string());
        }
    }

    if output_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No master {} output created by Siril",
            master_type.to_lowercase()
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Created {} master",
        master_type.to_lowercase()
    ));

    Ok(StepResult::success(output_files))
}

/// Execute a single branch calibration step
fn execute_calibrate_single_branch(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for calibration step"))?;

    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;

    let script_path = Path::new(script_path_str);
    let output_dir = Path::new(&job.output_dir);

    // Get branch info
    let _branch_id = step_data.branch_id.as_deref().unwrap_or("unknown");
    let branch_index = step_data.branch_index.unwrap_or(0) as usize;
    let filter = step_data.filter.as_deref().unwrap_or("unfiltered");

    // Use output_folder_path from step_data if available (ensures consistency with script generator)
    // Fall back to computed path for backward compatibility with existing jobs
    let lights_dir = if let Some(ref path) = step_data.output_folder_path {
        std::path::PathBuf::from(path)
    } else {
        // Legacy fallback - compute path (may not match script generator for "nofilter" case)
        output_dir.join("lights").join(format!("branch_{:02}_{}", branch_index + 1, filter))
    };

    // Simple check - look for any pp_ files
    if lights_dir.exists() {
        let pp_files: Vec<_> = WalkDir::new(&lights_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .collect();

        if !pp_files.is_empty() {
            return Ok(StepResult::skipped(format!(
                "Branch {} already calibrated ({} files)",
                filter,
                pp_files.len()
            )));
        }
    }

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(format!(
            "Missing calibration script: {}",
            script_path.display()
        )));
    }

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Calibrating {} frames ({})...",
        step_data.frame_count.unwrap_or(0),
        filter
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying calibrated outputs...");

    // Verify outputs
    let pp_files: Vec<String> = WalkDir::new(&lights_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if pp_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No calibrated frames created for branch {}",
            filter
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Calibrated {} frames",
        pp_files.len()
    ));

    Ok(StepResult::success(pp_files))
}

/// Execute registration for a specific camera type
fn execute_register_camera_type(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for registration step"))?;

    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;

    let script_path = Path::new(script_path_str);
    let output_dir = Path::new(&job.output_dir);

    let camera_type = step_data.camera_type.as_ref()
        .map(|ct| ct.display_name().to_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    // Check if output already exists (skip logic)
    let process_dir = output_dir.join("process").join(format!("all_lights_{}", camera_type));
    if process_dir.exists() {
        let reg_files: Vec<_> = WalkDir::new(&process_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .collect();

        if !reg_files.is_empty() {
            return Ok(StepResult::skipped(format!(
                "Registration already complete for {} ({} frames)",
                camera_type,
                reg_files.len()
            )));
        }
    }

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(format!(
            "Missing registration script: {}",
            script_path.display()
        )));
    }

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Registering {} frames...",
        camera_type
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying registered outputs...");

    // Verify outputs
    let reg_files: Vec<String> = WalkDir::new(&process_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("r_pp_") && (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if reg_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No registered frames created for {} camera type",
            camera_type
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Registered {} {} frames",
        reg_files.len(),
        camera_type
    ));

    Ok(StepResult::success(reg_files))
}

/// Execute stacking for a specific filter+camera group
fn execute_stack_group(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for stacking step"))?;

    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;

    let script_path = Path::new(script_path_str);
    let output_dir = Path::new(&job.output_dir);

    let filter = step_data.filter.as_deref().unwrap_or("unfiltered");
    let camera_type = step_data.camera_type.as_ref()
        .map(|ct| ct.display_name().to_lowercase())
        .unwrap_or_else(|| "unknown".to_string());
    let _group_key = step_data.group_key.as_deref().unwrap_or("unknown");

    // Check if output already exists (skip logic)
    let masters_dir = output_dir.join("masters");
    let expected_output_pattern = format!("{}_{}_stacked", filter.to_lowercase(), camera_type);

    if masters_dir.exists() {
        let stacked_files: Vec<_> = WalkDir::new(&masters_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.contains(&expected_output_pattern) &&
                    (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .collect();

        if !stacked_files.is_empty() {
            return Ok(StepResult::skipped(format!(
                "Stack already exists for {} {}",
                filter,
                camera_type
            )));
        }
    }

    // Check if script exists
    if !script_path.exists() {
        return Ok(StepResult::failure(format!(
            "Missing stacking script: {}",
            script_path.display()
        )));
    }

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Stacking {} {} ({} frames)...",
        filter,
        camera_type,
        step_data.frame_count.unwrap_or(0)
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying stacked output...");

    // Verify output
    let stacked_files: Vec<String> = WalkDir::new(&masters_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.contains(&expected_output_pattern) &&
                (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if stacked_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No stacked output created for {} {}",
            filter,
            camera_type
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Stacked {} {}",
        filter,
        camera_type
    ));

    Ok(StepResult::success(stacked_files))
}

// ============================================================================
// Multi-Group Pipeline Step Executors
// ============================================================================

/// Execute per-branch registration with branch-specific optics
fn execute_register_single_branch(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for registration step"))?;

    let branch_id = step_data.branch_id.as_deref().unwrap_or("unknown");
    let filter = step_data.filter.as_deref().unwrap_or("unfiltered");
    let focal_length = step_data.focal_length.unwrap_or(500.0);
    let pixel_size = step_data.pixel_size.unwrap_or(3.76);

    // Get branch directory from step_data
    let branch_dir = step_data.output_folder_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing output_folder_path in step_data"))?;
    let branch_dir_path = Path::new(branch_dir);

    // Check if registration already done (r_pp_lights_*.fit files exist)
    if branch_dir_path.exists() {
        let reg_files: Vec<_> = WalkDir::new(branch_dir_path)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_pp_lights_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .collect();

        if !reg_files.is_empty() {
            return Ok(StepResult::skipped(format!(
                "Branch {} already registered ({} frames)",
                filter,
                reg_files.len()
            )));
        }
    }

    // Get or generate the script path
    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;
    let script_path = Path::new(script_path_str);

    // Generate the per-branch registration script if it doesn't exist
    if !script_path.exists() {
        // Create the script directory
        if let Some(parent) = script_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Generate the script
        let mut script = String::new();
        script.push_str(&format!(
            r#"############################################
# Per-Branch Registration Script
# Generated by Athenaeum Pipeline Executor
#
# Branch: {}
# Filter: {}
# Focal Length: {:.1}mm
# Pixel Size: {:.2}µm
############################################

requires 1.3.0

cd {}
convert pp_lights -out=. -fitseq

seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}

register pp_lights -2pass

seqapplyreg pp_lights -framing=max

close
"#,
            branch_id,
            filter,
            focal_length,
            pixel_size,
            branch_dir_path.to_string_lossy(),
            focal_length,
            pixel_size
        ));

        std::fs::write(script_path, &script)?;
    }

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Registering {} (f={:.0}mm, px={:.2}µm)...",
        filter,
        focal_length,
        pixel_size
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying registered outputs...");

    // Verify outputs
    let reg_files: Vec<String> = WalkDir::new(branch_dir_path)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("r_pp_lights_") && (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if reg_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No registered frames created for branch {}",
            filter
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Registered {} frames for {}",
        reg_files.len(),
        filter
    ));

    Ok(StepResult::success(reg_files))
}

/// Execute collection of registered frames
fn execute_collect_registered(
    _conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);
    let process_dir = output_dir.join("process");
    let lights_dir = output_dir.join("lights");

    emit_progress(app_handle, job, step, 0.1, "Collecting registered frames...");

    // Create output directories
    let mono_dir = process_dir.join("all_mono");
    let osc_dir = process_dir.join("all_osc");
    std::fs::create_dir_all(&mono_dir)?;
    std::fs::create_dir_all(&osc_dir)?;

    let mut mono_count = 0usize;
    let mut osc_count = 0usize;
    let mut collected_files = Vec::new();

    // Walk through lights directory looking for r_pp_lights_*.fit files
    for entry in WalkDir::new(&lights_dir)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("r_pp_lights_") && (name.ends_with(".fit") || name.ends_with(".fits")) {
                // Read FITS metadata to determine if OSC or Mono
                let is_osc = {
                    use fitsio::FitsFile;
                    if let Ok(mut fptr) = FitsFile::open(path) {
                        if let Ok(hdu) = fptr.primary_hdu() {
                            let naxis: i64 = hdu.read_key(&mut fptr, "NAXIS").unwrap_or(2);
                            if naxis >= 3 {
                                let naxis3: i64 = hdu.read_key(&mut fptr, "NAXIS3").unwrap_or(1);
                                naxis3 >= 3
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                let (dest_dir, counter) = if is_osc {
                    osc_count += 1;
                    (&osc_dir, osc_count)
                } else {
                    mono_count += 1;
                    (&mono_dir, mono_count)
                };

                let new_name = format!("all_r_{:05}.fit", counter);
                let dest_path = dest_dir.join(&new_name);

                std::fs::copy(path, &dest_path)?;
                collected_files.push(dest_path.to_string_lossy().to_string());
            }
        }
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Collected {} frames ({} mono, {} OSC)",
        mono_count + osc_count,
        mono_count,
        osc_count
    ));

    Ok(StepResult::success(collected_files))
}

/// Execute global registration for mono or OSC frames
fn execute_global_registration(
    conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let step_data = step.step_data.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing step_data for global registration step"))?;

    let camera_type = step_data.camera_type.as_ref()
        .map(|ct| ct.display_name().to_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    let output_dir = Path::new(&job.output_dir);
    let process_dir = output_dir.join("process");
    let working_dir = process_dir.join(format!("all_{}", camera_type));

    // Check if global registration already done (r_all_r_*.fit files exist)
    if working_dir.exists() {
        let reg_files: Vec<_> = WalkDir::new(&working_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_all_r_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .collect();

        if !reg_files.is_empty() {
            return Ok(StepResult::skipped(format!(
                "Global registration already complete for {} ({} frames)",
                camera_type,
                reg_files.len()
            )));
        }
    }

    // Get or generate the script path
    let script_path_str = step_data.siril_script_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing siril_script_path in step_data"))?;
    let script_path = Path::new(script_path_str);

    // Generate the global registration script if it doesn't exist
    if !script_path.exists() {
        if let Some(parent) = script_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut script = String::new();
        script.push_str(&format!(
            r#"############################################
# Global Registration Script ({})
# Generated by Athenaeum Pipeline Executor
#
# Aligns ALL {} frames using homography -2pass
############################################

requires 1.3.0

cd {}

convert all_r -out=. -fitseq

register all_r -transf=homography -2pass

seqapplyreg all_r -framing=max

close
"#,
            camera_type.to_uppercase(),
            camera_type.to_uppercase(),
            working_dir.to_string_lossy()
        ));

        std::fs::write(script_path, &script)?;
    }

    emit_progress(app_handle, job, step, 0.1, &format!(
        "Running global registration for {} frames...",
        camera_type
    ));

    // Get Siril path
    let siril_path = get_siril_path(conn)?;

    emit_progress(app_handle, job, step, 0.2, &format!(
        "Running {} with Siril",
        script_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Run the script
    run_siril_script(&siril_path, script_path, app_handle)?;

    emit_progress(app_handle, job, step, 0.9, "Verifying globally registered outputs...");

    // Verify outputs
    let reg_files: Vec<String> = WalkDir::new(&working_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.starts_with("r_all_r_") && (name.ends_with(".fit") || name.ends_with(".fits"))
        })
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    if reg_files.is_empty() {
        return Ok(StepResult::failure(format!(
            "No globally registered frames created for {} camera type",
            camera_type
        )));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Globally registered {} {} frames",
        reg_files.len(),
        camera_type
    ));

    Ok(StepResult::success(reg_files))
}

/// Execute preparation of stacking directories
fn execute_prepare_stacking(
    _conn: &Connection,
    job: &ExportJob,
    step: &ExportJobStep,
    app_handle: &AppHandle,
) -> Result<StepResult> {
    let output_dir = Path::new(&job.output_dir);
    let process_dir = output_dir.join("process");

    emit_progress(app_handle, job, step, 0.1, "Preparing stacking directories...");

    // The stacking directories are created by the StackGroup step
    // This step just verifies that globally registered frames exist

    let mono_dir = process_dir.join("all_mono");
    let osc_dir = process_dir.join("all_osc");

    let mut total_registered = 0;
    let mut output_files = Vec::new();

    // Check for globally registered frames in mono dir
    if mono_dir.exists() {
        let reg_files: Vec<_> = WalkDir::new(&mono_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_all_r_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        total_registered += reg_files.len();
        output_files.extend(reg_files);
    }

    // Check for globally registered frames in osc dir
    if osc_dir.exists() {
        let reg_files: Vec<_> = WalkDir::new(&osc_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("r_all_r_") && (name.ends_with(".fit") || name.ends_with(".fits"))
            })
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        total_registered += reg_files.len();
        output_files.extend(reg_files);
    }

    if total_registered == 0 {
        return Ok(StepResult::failure(
            "No globally registered frames found. Global registration may have failed.".to_string()
        ));
    }

    emit_progress(app_handle, job, step, 1.0, &format!(
        "Ready to stack {} globally registered frames",
        total_registered
    ));

    Ok(StepResult::success(output_files))
}

// ============================================================================
// Progress Emission
// ============================================================================

fn emit_progress(
    app_handle: &AppHandle,
    job: &ExportJob,
    step: &ExportJobStep,
    step_progress: f64,
    message: &str,
) {
    let overall_progress = if job.total_steps > 0 {
        ((step.step_order - 1) as f64 + step_progress) / job.total_steps as f64
    } else {
        0.0
    };

    let progress = PipelineProgress {
        job_id: job.id,
        step_id: Some(step.id),
        step_order: step.step_order,
        total_steps: job.total_steps,
        step_type: step.step_type.clone(),
        step_progress,
        overall_progress,
        message: message.to_string(),
        current_file: None,
        is_resumable: step.step_type.can_resume_from(),
        can_retry: true,
    };

    let _ = app_handle.emit("pipeline-progress", progress);
}

// ============================================================================
// Job Execution Loop
// ============================================================================

/// Run all pending steps in a job
pub fn run_job(
    conn: &Connection,
    app_handle: &AppHandle,
    job_id: i64,
) -> Result<()> {
    println!("🚀 Starting export job {}", job_id);

    // Mark job as running
    update_job_status(conn, job_id, JobStatus::Running, None)?;

    loop {
        // Get current job state
        let job = get_job_with_steps(conn, job_id)?;

        // Check if job was cancelled/paused
        if job.status != JobStatus::Running {
            break;
        }

        // Get next pending step
        let next_step = job.steps.iter()
            .find(|s| s.status == StepStatus::Pending);

        let step = match next_step {
            Some(s) => s.clone(),
            None => {
                // All steps complete
                update_job_status(conn, job_id, JobStatus::Completed, None)?;
                break;
            }
        };

        // Mark step as running
        update_step_status(conn, step.id, StepStatus::Running, None)?;

        // Log step start
        println!("▶️ Starting step {}/{}: {}", step.step_order, job.total_steps, step.step_name);

        // Execute the step
        let result = execute_step(conn, app_handle, &job, &step);

        match result {
            Ok(step_result) => {
                if step_result.should_skip {
                    println!("⏭️ Skipped: {} - {:?}", step.step_name, step_result.error_message);
                    update_step_status(conn, step.id, StepStatus::Skipped, step_result.error_message.as_deref())?;
                } else if step_result.success {
                    println!("✅ Completed: {} ({} output files)", step.step_name, step_result.output_files.len());
                    update_step_output_files(conn, step.id, &step_result.output_files)?;
                    update_step_status(conn, step.id, StepStatus::Completed, None)?;
                } else {
                    println!("❌ Failed: {} - {:?}", step.step_name, step_result.error_message);
                    update_step_status(conn, step.id, StepStatus::Failed, step_result.error_message.as_deref())?;
                    update_job_status(conn, job_id, JobStatus::Failed, step_result.error_message.as_deref())?;
                    break;
                }
            }
            Err(e) => {
                let error_msg = e.to_string();
                println!("❌ Error in step {}: {}", step.step_name, error_msg);
                update_step_status(conn, step.id, StepStatus::Failed, Some(&error_msg))?;
                update_job_status(conn, job_id, JobStatus::Failed, Some(&error_msg))?;
                break;
            }
        }

        // Update completed steps count
        update_completed_steps(conn, job_id)?;
    }

    // Log final status
    if let Ok(final_job) = get_job_with_steps(conn, job_id) {
        match final_job.status {
            JobStatus::Completed => println!("🎉 Job {} completed successfully!", job_id),
            JobStatus::Failed => println!("💥 Job {} failed", job_id),
            JobStatus::Paused => println!("⏸️ Job {} paused", job_id),
            JobStatus::Cancelled => println!("🚫 Job {} cancelled", job_id),
            _ => println!("📋 Job {} ended with status: {:?}", job_id, final_job.status),
        }
    }

    Ok(())
}
