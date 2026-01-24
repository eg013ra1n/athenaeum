//! File validation for export pipeline
//!
//! Validates source files and calibration links exist before processing,
//! and verifies output files after step completion.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;
use std::fs;

use super::models::{FileValidation, FileType, PipelinePlan, PlannedStep, StepData, StepType};
use crate::export::models::{ExportConfig, CameraType};
use crate::export::data_collector::collect_export_data_v3;
use crate::export::folder_structures::SirilFolders;

// ============================================================================
// Source File Validation
// ============================================================================

/// Validate all source files for a frame set exist on disk
pub fn validate_source_files(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<Vec<FileValidation>> {
    // Get all light frames for this frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fi.path
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         JOIN frames f ON sm.frame_id = f.id
         JOIN files fi ON f.file_id = fi.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    )?;

    let paths: Vec<String> = stmt.query_map([frame_set_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut validations = Vec::new();

    for path in paths {
        let file_path = Path::new(&path);
        let exists = file_path.exists();
        let size = if exists {
            fs::metadata(file_path).ok().map(|m| m.len() as i64)
        } else {
            None
        };

        validations.push(FileValidation {
            path,
            exists,
            size,
            error: if exists { None } else { Some("File not found".to_string()) },
            file_type: FileType::Source,
        });
    }

    Ok(validations)
}

/// Validate all calibration files linked to a frame set exist on disk
pub fn validate_calibration_files(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<Vec<FileValidation>> {
    // Get all calibration set IDs linked to this frame set's frames
    let mut stmt = conn.prepare(
        "SELECT DISTINCT cs.id, cs.imagetyp
         FROM calibration_set_to_frames cstf
         JOIN calibration_set cs ON cstf.calibration_set_id = cs.id
         WHERE cstf.source_type = 'frame'
         AND cstf.source_id IN (
             SELECT sm.frame_id
             FROM session_members sm
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             WHERE n.frames_set_id = ?1
         )"
    )?;

    let calibration_sets: Vec<(i64, String)> = stmt.query_map([frame_set_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?.filter_map(|r| r.ok()).collect();

    let mut validations = Vec::new();

    for (set_id, _imagetyp) in calibration_sets {
        // Get all files in this calibration set
        let mut file_stmt = conn.prepare(
            "SELECT fi.path
             FROM calibration_set_frames csf
             JOIN frames f ON csf.frame_id = f.id
             JOIN files fi ON f.file_id = fi.id
             WHERE csf.set_id = ?1"
        )?;

        let cal_paths: Vec<String> = file_stmt.query_map([set_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        for path in cal_paths {
            let file_path = Path::new(&path);
            let exists = file_path.exists();
            let size = if exists {
                fs::metadata(file_path).ok().map(|m| m.len() as i64)
            } else {
                None
            };

            validations.push(FileValidation {
                path,
                exists,
                size,
                error: if exists { None } else { Some("Calibration file not found".to_string()) },
                file_type: FileType::Calibration,
            });
        }
    }

    Ok(validations)
}

// ============================================================================
// Output Validation
// ============================================================================

/// Validate expected output files exist after a step completes
pub fn validate_output_files(expected_paths: &[String]) -> Vec<FileValidation> {
    expected_paths.iter().map(|path| {
        let file_path = Path::new(path);
        let exists = file_path.exists();
        let size = if exists {
            fs::metadata(file_path).ok().map(|m| m.len() as i64)
        } else {
            None
        };

        FileValidation {
            path: path.clone(),
            exists,
            size,
            error: if exists { None } else { Some("Output file not created".to_string()) },
            file_type: FileType::Output,
        }
    }).collect()
}

/// Check if a specific file exists
pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

// ============================================================================
// Pipeline Plan Building
// ============================================================================

/// Build a complete pipeline plan for a frame set
///
/// Creates a granular pipeline with per-step script execution:
///
/// Phase 1 - Preparation (unchanged):
/// - ValidateSources - Check all source files exist
/// - OrganizeFiles - Create folder structure and generate per-step scripts
///
/// Phase 2 - Masters (one step per master):
/// - CreateMasterBias - For each bias set
/// - CreateMasterDark - For each dark set (depends on bias)
/// - CreateMasterDarkFlat - For each darkflat set (depends on bias)
/// - CreateMasterFlat - For each flat set (depends on dark/darkflat)
///
/// Phase 3 - Calibration (one step per branch):
/// - CalibrateBranch - Calibrate lights for each branch (depends on relevant masters)
///
/// Phase 4 - Collection (unchanged):
/// - CollectCalibrated - Collect pp_lights to unified folders
///
/// Phase 5 - Registration (one step per camera type):
/// - RegisterFrames (mono) - If there are mono cameras
/// - RegisterFrames (osc) - If there are OSC cameras
///
/// Phase 6 - Stacking (one step per filter+camera):
/// - StackGroup - One per filter+camera combination
pub fn build_pipeline_plan(
    conn: &Connection,
    frame_set_id: i64,
    config: &ExportConfig,
) -> Result<PipelinePlan> {
    // Validate source files
    let source_validations = validate_source_files(conn, frame_set_id)?;
    let total_source_files = source_validations.len() as i32;
    let missing_source_files = source_validations.iter().filter(|v| !v.exists).count() as i32;

    // Validate calibration files
    let calibration_validations = validate_calibration_files(conn, frame_set_id)?;
    let total_calibration_files = calibration_validations.len() as i32;
    let missing_calibration_files = calibration_validations.iter().filter(|v| !v.exists).count() as i32;

    // Collect export data to understand the structure
    let export_data = collect_export_data_v3(conn, frame_set_id)?;

    let folders = SirilFolders::new(config.output_dir.clone());
    let scripts_dir = folders.root.join("scripts");

    let mut steps: Vec<PlannedStep> = Vec::new();
    let mut step_order = 1;

    // Track master set_id -> step_order mapping for dependency resolution
    let mut master_step_map: HashMap<i64, i32> = HashMap::new();

    // Count for display
    let total_masters = export_data.master_plan.masters.len();
    let total_light_frames: usize = export_data.branches.iter()
        .map(|b| b.light_frames.len())
        .sum();
    let _total_branches = export_data.branches.len();

    // ========== PHASE 1: Preparation ==========

    // Step 1: Validate sources
    steps.push(PlannedStep {
        step_type: StepType::ValidateSources,
        step_name: "Validate source files".to_string(),
        step_data: Some(StepData {
            frame_count: Some(total_source_files + total_calibration_files),
            ..Default::default()
        }),
        depends_on: vec![],
        can_resume_from: true,
    });
    let validate_step = step_order;
    step_order += 1;

    // Step 2: Organize files and generate scripts
    steps.push(PlannedStep {
        step_type: StepType::OrganizeFiles,
        step_name: "Organize files & generate scripts".to_string(),
        step_data: Some(StepData {
            frame_count: Some(total_light_frames as i32 + total_calibration_files),
            ..Default::default()
        }),
        depends_on: vec![validate_step],
        can_resume_from: true,
    });
    let organize_step = step_order;
    step_order += 1;

    // ========== PHASE 2: Per-Master Steps ==========

    if config.create_masters && total_masters > 0 {
        for (_idx, master) in export_data.master_plan.masters.iter().enumerate() {
            let master_type = match master.master_type.as_str() {
                "Bias" => StepType::CreateMasterBias,
                "Dark" => StepType::CreateMasterDark,
                "DarkFlat" => StepType::CreateMasterDarkFlat,
                "Flat" => StepType::CreateMasterFlat,
                _ => StepType::CreateMasterBias, // Fallback
            };

            // Resolve dependencies to step orders
            let mut depends_on = vec![organize_step];
            let mut depends_on_steps = Vec::new();
            for dep_set_id in &master.depends_on {
                if let Some(&dep_step) = master_step_map.get(dep_set_id) {
                    depends_on.push(dep_step);
                    depends_on_steps.push(dep_step);
                }
            }

            let type_lower = master.master_type.to_lowercase();
            // Use deterministic naming based on set_id only (not iteration index)
            let script_path = scripts_dir
                .join("00_masters")
                .join(format!("master_{}_{}.ssf", type_lower, master.set_id));

            steps.push(PlannedStep {
                step_type: master_type,
                step_name: format!(
                    "Create {} master (set {})",
                    master.master_type.to_lowercase(),
                    master.set_id
                ),
                step_data: Some(StepData {
                    set_id: Some(master.set_id),
                    master_type: Some(master.master_type.clone()),
                    frame_count: Some(master.source_frames.len() as i32),
                    siril_script_path: Some(script_path.to_string_lossy().to_string()),
                    output_path: Some(folders.masters.join(&master.output_name).to_string_lossy().to_string()),
                    depends_on_steps,
                    ..Default::default()
                }),
                depends_on,
                can_resume_from: true,
            });

            // Track this master's step order for branch dependencies
            master_step_map.insert(master.set_id, step_order);
            step_order += 1;
        }
    }

    // ========== PHASE 3: Per-Branch Calibration ==========

    let mut branch_step_orders: Vec<i32> = Vec::new();
    // Map branch_index -> calibrate step order (for registration dependencies)
    let mut branch_calibrate_step_map: HashMap<usize, i32> = HashMap::new();

    for (idx, branch) in export_data.branches.iter().enumerate() {
        // Resolve dependencies - all masters used by this branch
        let mut depends_on = vec![organize_step];
        let mut depends_on_steps = Vec::new();

        if branch.bias_id > 0 {
            if let Some(&step) = master_step_map.get(&branch.bias_id) {
                depends_on.push(step);
                depends_on_steps.push(step);
            }
        }
        if branch.dark_id > 0 {
            if let Some(&step) = master_step_map.get(&branch.dark_id) {
                depends_on.push(step);
                depends_on_steps.push(step);
            }
        }
        if branch.flat_id > 0 {
            if let Some(&step) = master_step_map.get(&branch.flat_id) {
                depends_on.push(step);
                depends_on_steps.push(step);
            }
        }
        if branch.darkflat_id > 0 {
            if let Some(&step) = master_step_map.get(&branch.darkflat_id) {
                depends_on.push(step);
                depends_on_steps.push(step);
            }
        }

        let _filter_safe = branch.filter.as_ref()
            .map(|f| crate::export::models::sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());

        // Use branch_id for deterministic naming (sanitize to be filesystem-safe)
        let branch_id_safe = crate::export::models::sanitize_folder_name(&branch.branch_id);
        let script_path = scripts_dir
            .join("01_calibrate")
            .join(format!("calibrate_{}.ssf", branch_id_safe));

        // Get the output folder path using SirilFolders to ensure consistency
        // with script generator (uses _nofilter when filter is None, not _unfiltered)
        let output_folder = folders.lights_branch_path(idx, branch.filter.as_deref());

        steps.push(PlannedStep {
            step_type: StepType::CalibrateBranch,
            step_name: format!(
                "Calibrate {} ({} frames)",
                branch.filter.as_deref().unwrap_or("unfiltered"),
                branch.light_frames.len()
            ),
            step_data: Some(StepData {
                branch_id: Some(branch.branch_id.clone()),
                branch_index: Some(idx as i32),
                filter: branch.filter.clone(),
                camera_type: Some(if branch.is_osc() { CameraType::Osc } else { CameraType::Mono }),
                frame_count: Some(branch.light_frames.len() as i32),
                siril_script_path: Some(script_path.to_string_lossy().to_string()),
                output_folder_path: Some(output_folder.to_string_lossy().to_string()),
                depends_on_steps,
                ..Default::default()
            }),
            depends_on,
            can_resume_from: true,
        });

        branch_step_orders.push(step_order);
        branch_calibrate_step_map.insert(idx, step_order);
        step_order += 1;
    }

    // ========== PHASE 4: Per-Branch Registration (MULTI-GROUP FIX) ==========
    // Each branch is registered with ITS OWN focal length and pixel size
    // This prevents "Cannot add an image with different properties" errors

    let mut branch_register_step_orders: Vec<i32> = Vec::new();

    for (idx, branch) in export_data.branches.iter().enumerate() {
        // Skip branches with < 2 frames (Siril requirement)
        if branch.light_frames.len() < 2 {
            continue;
        }

        // Get branch-specific optics from first light frame
        let focal_length = branch.light_frames.first()
            .and_then(|f| f.focallen)
            .filter(|&f| f > 0.0)
            .unwrap_or(500.0);  // Default fallback

        let pixel_size = branch.light_frames.first()
            .and_then(|f| f.xpixsz)
            .filter(|&p| p > 0.0)
            .unwrap_or(3.76);  // Default fallback

        let branch_id_safe = crate::export::models::sanitize_folder_name(&branch.branch_id);
        let script_path = scripts_dir
            .join("02_register")
            .join(format!("register_branch_{}.ssf", branch_id_safe));

        let output_folder = folders.lights_branch_path(idx, branch.filter.as_deref());

        // Depends on the calibrate step for this branch
        let calibrate_step = branch_calibrate_step_map.get(&idx).copied().unwrap_or(organize_step);

        steps.push(PlannedStep {
            step_type: StepType::RegisterBranch,
            step_name: format!(
                "Register {} (f={:.0}mm, px={:.2}µm, {} frames)",
                branch.filter.as_deref().unwrap_or("unfiltered"),
                focal_length,
                pixel_size,
                branch.light_frames.len()
            ),
            step_data: Some(StepData {
                branch_id: Some(branch.branch_id.clone()),
                branch_index: Some(idx as i32),
                filter: branch.filter.clone(),
                camera_type: Some(if branch.is_osc() { CameraType::Osc } else { CameraType::Mono }),
                frame_count: Some(branch.light_frames.len() as i32),
                siril_script_path: Some(script_path.to_string_lossy().to_string()),
                output_folder_path: Some(output_folder.to_string_lossy().to_string()),
                focal_length: Some(focal_length),
                pixel_size: Some(pixel_size),
                ..Default::default()
            }),
            depends_on: vec![calibrate_step],
            can_resume_from: true,
        });

        branch_register_step_orders.push(step_order);
        step_order += 1;
    }

    // ========== PHASE 5: Collect Registered Frames ==========

    steps.push(PlannedStep {
        step_type: StepType::CollectRegistered,
        step_name: "Collect registered frames".to_string(),
        step_data: None,
        depends_on: branch_register_step_orders.clone(),
        can_resume_from: true,
    });
    let collect_step = step_order;
    step_order += 1;

    // ========== PHASE 6: Global Registration (MULTI-GROUP FIX) ==========
    // After per-branch plate solving, align ALL frames together using homography -2pass
    // Mono and OSC must be processed separately (cannot mix different layer counts)

    let has_mono = export_data.branches.iter().any(|b| !b.is_osc());
    let has_osc = export_data.branches.iter().any(|b| b.is_osc());

    let mut global_registration_steps: Vec<i32> = Vec::new();

    if has_mono {
        let mono_frames: usize = export_data.branches.iter()
            .filter(|b| !b.is_osc() && b.light_frames.len() >= 2)
            .map(|b| b.light_frames.len())
            .sum();

        let script_path = scripts_dir
            .join("03_register_global")
            .join("global_register_mono.ssf");

        steps.push(PlannedStep {
            step_type: StepType::GlobalRegistration,
            step_name: format!("Global registration (Mono, {} frames)", mono_frames),
            step_data: Some(StepData {
                camera_type: Some(CameraType::Mono),
                frame_count: Some(mono_frames as i32),
                siril_script_path: Some(script_path.to_string_lossy().to_string()),
                ..Default::default()
            }),
            depends_on: vec![collect_step],
            can_resume_from: true,
        });
        global_registration_steps.push(step_order);
        step_order += 1;
    }

    if has_osc {
        let osc_frames: usize = export_data.branches.iter()
            .filter(|b| b.is_osc() && b.light_frames.len() >= 2)
            .map(|b| b.light_frames.len())
            .sum();

        let script_path = scripts_dir
            .join("03_register_global")
            .join("global_register_osc.ssf");

        steps.push(PlannedStep {
            step_type: StepType::GlobalRegistration,
            step_name: format!("Global registration (OSC, {} frames)", osc_frames),
            step_data: Some(StepData {
                camera_type: Some(CameraType::Osc),
                frame_count: Some(osc_frames as i32),
                siril_script_path: Some(script_path.to_string_lossy().to_string()),
                ..Default::default()
            }),
            depends_on: vec![collect_step],
            can_resume_from: true,
        });
        global_registration_steps.push(step_order);
        step_order += 1;
    }

    // ========== PHASE 7: Prepare Stacking ==========
    // Copy globally registered frames to stacking directories

    steps.push(PlannedStep {
        step_type: StepType::PrepareStacking,
        step_name: "Prepare stacking directories".to_string(),
        step_data: None,
        depends_on: global_registration_steps.clone(),
        can_resume_from: true,
    });
    let prepare_stacking_step = step_order;
    step_order += 1;

    // ========== PHASE 8: Per-Group Stacking ==========

    use crate::export::models::GlobalRegistrationPlan;
    let plan = GlobalRegistrationPlan::from_export_data(&export_data);
    let stacking_groups = plan.stacking_groups(
        &config.exptime_tolerance_mode,
        config.exptime_tolerance_value,
    );

    for (_idx, group) in stacking_groups.iter().enumerate() {
        let filter_safe = group.filter.as_ref()
            .map(|f| crate::export::models::sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());
        let camera_suffix = group.camera_type.display_name().to_lowercase();

        // Deterministic naming based on filter+camera (not index)
        // Note: Using 04_stack directory for the new pipeline structure
        let script_path = scripts_dir
            .join("04_stack")
            .join(format!("stack_{}_{}.ssf", filter_safe, camera_suffix));

        // Depend on the prepare stacking step (which depends on global registration)
        let stack_depends: Vec<i32> = vec![prepare_stacking_step];

        let filter_name = group.filter.as_deref().unwrap_or("Unfiltered");
        let exptime_str = if group.exptime_display.is_empty() {
            String::new()
        } else {
            format!(" {}", group.exptime_display)
        };

        steps.push(PlannedStep {
            step_type: StepType::StackGroup,
            step_name: format!(
                "Stack {} {} ({} frames){}",
                filter_name,
                camera_suffix,
                group.frame_indices.len(),
                exptime_str
            ),
            step_data: Some(StepData {
                filter: group.filter.clone(),
                camera_type: Some(group.camera_type.clone()),
                frame_count: Some(group.frame_indices.len() as i32),
                siril_script_path: Some(script_path.to_string_lossy().to_string()),
                exptime_display: if group.exptime_display.is_empty() { None } else { Some(group.exptime_display.clone()) },
                group_key: Some(format!("{}_{}", filter_safe, camera_suffix)),
                ..Default::default()
            }),
            depends_on: stack_depends,
            can_resume_from: true,
        });
        step_order += 1;
    }

    // Determine if there are blocking errors
    let has_blocking_errors = missing_source_files > 0;

    let mut warnings = Vec::new();
    if missing_calibration_files > 0 {
        warnings.push(format!("{} calibration files are missing", missing_calibration_files));
    }

    Ok(PipelinePlan {
        steps,
        source_validations,
        calibration_validations,
        total_source_files,
        total_calibration_files,
        missing_source_files,
        missing_calibration_files,
        has_blocking_errors,
        warnings,
    })
}

/// Build a legacy pipeline plan (7-step monolithic)
///
/// This is the original pipeline with monolithic scripts.
/// Used for backward compatibility with existing jobs.
pub fn build_legacy_pipeline_plan(
    conn: &Connection,
    frame_set_id: i64,
    _config: &ExportConfig,
) -> Result<PipelinePlan> {
    // Validate source files
    let source_validations = validate_source_files(conn, frame_set_id)?;
    let total_source_files = source_validations.len() as i32;
    let missing_source_files = source_validations.iter().filter(|v| !v.exists).count() as i32;

    // Validate calibration files
    let calibration_validations = validate_calibration_files(conn, frame_set_id)?;
    let total_calibration_files = calibration_validations.len() as i32;
    let missing_calibration_files = calibration_validations.iter().filter(|v| !v.exists).count() as i32;

    // Collect export data to understand the structure
    let export_data = collect_export_data_v3(conn, frame_set_id)?;

    let mut steps: Vec<PlannedStep> = Vec::new();

    // Count masters and lights for display
    let total_masters = export_data.master_plan.masters.len();
    let total_light_frames: usize = export_data.branches.iter()
        .map(|b| b.light_frames.len())
        .sum();
    let total_branches = export_data.branches.len();

    // Collect filter info for display
    let filters: Vec<String> = export_data.branches.iter()
        .filter_map(|b| b.filter.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let filter_summary = if filters.is_empty() {
        "Unfiltered".to_string()
    } else {
        filters.join(", ")
    };

    // Step 1: Validate sources
    steps.push(PlannedStep {
        step_type: StepType::ValidateSources,
        step_name: "Validate source files".to_string(),
        step_data: Some(StepData {
            frame_count: Some(total_source_files + total_calibration_files),
            ..Default::default()
        }),
        depends_on: vec![],
        can_resume_from: true,
    });

    // Step 2: Organize files and generate scripts
    steps.push(PlannedStep {
        step_type: StepType::OrganizeFiles,
        step_name: "Organize files & generate scripts".to_string(),
        step_data: Some(StepData {
            frame_count: Some(total_light_frames as i32 + total_calibration_files),
            ..Default::default()
        }),
        depends_on: vec![1],
        can_resume_from: true,
    });

    // Step 3: Create all masters (single step runs 00_create_masters.ssf)
    if total_masters > 0 {
        steps.push(PlannedStep {
            step_type: StepType::CreateMasters,
            step_name: format!("Create {} master calibrations", total_masters),
            step_data: Some(StepData {
                frame_count: Some(total_masters as i32),
                siril_script_path: Some("00_create_masters.ssf".to_string()),
                ..Default::default()
            }),
            depends_on: vec![2],
            can_resume_from: true,
        });
    }

    // Step 4: Calibrate all lights (single step runs 01_calibrate_lights.ssf)
    let create_masters_step = if total_masters > 0 { 3 } else { 2 };
    steps.push(PlannedStep {
        step_type: StepType::CalibrateLights,
        step_name: format!("Calibrate {} light frames ({} branches)", total_light_frames, total_branches),
        step_data: Some(StepData {
            frame_count: Some(total_light_frames as i32),
            siril_script_path: Some("01_calibrate_lights.ssf".to_string()),
            ..Default::default()
        }),
        depends_on: vec![create_masters_step],
        can_resume_from: true,
    });

    // Step 5: Collect calibrated frames
    let calibrate_step = if total_masters > 0 { 4 } else { 3 };
    steps.push(PlannedStep {
        step_type: StepType::CollectCalibrated,
        step_name: "Collect calibrated frames".to_string(),
        step_data: None,
        depends_on: vec![calibrate_step],
        can_resume_from: true,
    });

    // Step 6: Generate registration script
    let collect_step = calibrate_step + 1;
    steps.push(PlannedStep {
        step_type: StepType::GenerateRegistration,
        step_name: "Generate registration script".to_string(),
        step_data: Some(StepData {
            siril_script_path: Some("02_register_and_stack.ssf".to_string()),
            ..Default::default()
        }),
        depends_on: vec![collect_step],
        can_resume_from: true,
    });

    // Step 7: Register and stack (single step runs 02_register_and_stack.ssf)
    let gen_reg_step = collect_step + 1;
    steps.push(PlannedStep {
        step_type: StepType::RegisterAndStack,
        step_name: format!("Register & stack ({})", filter_summary),
        step_data: Some(StepData {
            siril_script_path: Some("02_register_and_stack.ssf".to_string()),
            ..Default::default()
        }),
        depends_on: vec![gen_reg_step],
        can_resume_from: true,
    });

    // Determine if there are blocking errors
    let has_blocking_errors = missing_source_files > 0;

    let mut warnings = Vec::new();
    if missing_calibration_files > 0 {
        warnings.push(format!("{} calibration files are missing", missing_calibration_files));
    }

    Ok(PipelinePlan {
        steps,
        source_validations,
        calibration_validations,
        total_source_files,
        total_calibration_files,
        missing_source_files,
        missing_calibration_files,
        has_blocking_errors,
        warnings,
    })
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Filter validations to only missing files
pub fn get_missing_files(validations: &[FileValidation]) -> Vec<&FileValidation> {
    validations.iter().filter(|v| !v.exists).collect()
}

/// Get file size if it exists
pub fn get_file_size(path: &str) -> Option<i64> {
    fs::metadata(path).ok().map(|m| m.len() as i64)
}
