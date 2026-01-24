//! Siril script generator
//!
//! Generates Siril .ssf scripts from export data and configuration.
//!
//! Phase 4: Updated to use new ExportGroup structure with subgroups
//! and MasterCreationPlan for proper dependency ordering.

use crate::export::folder_structures::SirilFolders;
use crate::export::models::{
    sanitize_folder_name, CameraType, DrizzleScale, ExportConfig, ExportDataV3,
    ImageWeightingMethod, RejectionAlgorithm,
};
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

// ============================================================================
// Helper Functions for Siril Command Generation
// ============================================================================

/// Build a register command with reference frame selection and optional drizzle
fn build_register_command(config: &ExportConfig, sequence_name: &str) -> String {
    let mut cmd = format!("register {}", sequence_name);

    // Reference frame mode
    match config.reference_frame_mode {
        crate::export::models::ReferenceFrameMode::SirilAuto => {
            cmd.push_str(" -2pass");
        }
        crate::export::models::ReferenceFrameMode::AtheneumScoring => {
            // For now, fall back to -2pass until quality scoring is implemented
            cmd.push_str(" -2pass");
        }
        crate::export::models::ReferenceFrameMode::Manual => {
            // Manual reference frame - would need frame index
            // For now, use -2pass as fallback
            cmd.push_str(" -2pass");
        }
    }

    // Drizzle
    if config.drizzle_enabled {
        cmd.push_str(" -drizzle");
        match config.drizzle_scale {
            DrizzleScale::None => {}
            DrizzleScale::X2 => cmd.push_str(" -scale=2.0"),
            DrizzleScale::X3 => cmd.push_str(" -scale=3.0"),
        }
    }

    cmd
}

// ============================================================================
// V3: Script Generation for Flat Siril Structure
// ============================================================================

/// Convert a list of frame indices to contiguous ranges for Siril select command
///
/// Siril's `select` command takes ranges (from, to), so we need to convert
/// a list like [1,2,3,4,5,51,52,53] into ranges [(1,5), (51,53)]
fn indices_to_ranges(indices: &[usize]) -> Vec<(usize, usize)> {
    if indices.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<usize> = indices.to_vec();
    sorted.sort();

    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &idx in &sorted[1..] {
        if idx == end + 1 {
            end = idx;
        } else {
            ranges.push((start, end));
            start = idx;
            end = idx;
        }
    }
    ranges.push((start, end));
    ranges
}

/// Generate Siril scripts for V3 export
///
/// Creates these monolithic scripts:
/// 1. 00_create_masters.ssf - All masters in dependency order
/// 2. 01_calibrate_lights.ssf - Calibrate all lights by branch
/// 3. 02_register_branches.ssf - Per-branch registration with branch-specific optics (NEW!)
/// 4. 02_register_and_stack.ssf - Legacy global registration (kept for backward compatibility)
///
/// Also generates per-step scripts in subdirectories (V5 mode):
/// - scripts/00_masters/01_master_bias_XX.ssf
/// - scripts/01_calibrate/01_calibrate_branch_XX.ssf
/// - scripts/02_register/01_register_mono.ssf
/// - scripts/03_stack/01_stack_ha_mono.ssf
///
/// MULTI-GROUP FIX: The pipeline now uses 02_register_branches.ssf for per-branch
/// registration with branch-specific focal length and pixel size, then the stacking
/// scripts are generated dynamically after frame collection.
pub fn generate_scripts_v3(config: &ExportConfig, data: &ExportDataV3) -> Result<Vec<PathBuf>> {
    println!("📝 Generating V3 Siril scripts (flat structure)");
    println!("  Output dir: {:?}", config.output_dir);
    println!("  Branches: {}, Masters: {}", data.branches.len(), data.master_plan.masters.len());

    let folders = SirilFolders::new(config.output_dir.clone());
    let mut scripts = Vec::new();

    // Generate per-step scripts (V5 mode) in addition to monolithic scripts
    let per_step_scripts = generate_per_step_scripts(config, &folders, data)?;
    scripts.extend(per_step_scripts);

    // Script 1: Create all masters (legacy monolithic - kept for backward compatibility)
    if !data.master_plan.masters.is_empty() && config.create_masters {
        let script = generate_master_script_v4(config, &folders, data)?;
        scripts.push(script);
    }

    // Script 2: Calibrate all lights (legacy monolithic)
    let calibrate_script = generate_calibrate_lights_script_v4(config, &folders, data)?;
    scripts.push(calibrate_script);

    // Script 3: Per-branch registration with branch-specific optics (NEW!)
    // This is the key fix for multi-group scenarios - each branch gets its own
    // focal length and pixel size for plate solving
    let register_branches_script = generate_register_branches_script(config, &folders, data)?;
    scripts.push(register_branches_script);

    // Script 4: Legacy global registration (kept for backward compatibility)
    // This can still be used when all branches share the same optical setup
    let register_and_stack_script = generate_register_and_stack_script_v4(config, &folders, data)?;
    scripts.push(register_and_stack_script);

    println!("  Generated {} scripts ({} per-step + 4 monolithic)",
        scripts.len(), scripts.len() - 4);
    Ok(scripts)
}

// ============================================================================
// V5: Per-Step Script Generation (Granular Control)
// ============================================================================

/// Generate all per-step scripts for V5 granular pipeline
///
/// Creates scripts in subdirectories:
/// - scripts/00_masters/   - One script per master
/// - scripts/01_calibrate/ - One script per branch
/// - scripts/02_register/  - One script per camera type
/// - scripts/03_stack/     - One script per filter+camera
pub fn generate_per_step_scripts(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<Vec<PathBuf>> {
    let mut scripts = Vec::new();

    // Create scripts subdirectory
    let scripts_dir = folders.root.join("scripts");
    fs::create_dir_all(&scripts_dir)?;

    // Create phase subdirectories
    let masters_dir = scripts_dir.join("00_masters");
    let calibrate_dir = scripts_dir.join("01_calibrate");
    let register_dir = scripts_dir.join("02_register");
    let stack_dir = scripts_dir.join("03_stack");

    fs::create_dir_all(&masters_dir)?;
    fs::create_dir_all(&calibrate_dir)?;
    fs::create_dir_all(&register_dir)?;
    fs::create_dir_all(&stack_dir)?;

    // Generate per-master scripts
    if config.create_masters {
        for (idx, master) in data.master_plan.masters.iter().enumerate() {
            let script = generate_single_master_script(config, folders, master, idx, &masters_dir)?;
            scripts.push(script);
        }
    }

    // Generate per-branch calibration scripts
    for (idx, branch) in data.branches.iter().enumerate() {
        let script = generate_branch_calibration_script(config, folders, branch, idx, &calibrate_dir)?;
        scripts.push(script);
    }

    // Generate registration scripts by camera type
    let has_mono = data.branches.iter().any(|b| !b.is_osc());
    let has_osc = data.branches.iter().any(|b| b.is_osc());

    if has_mono {
        let script = generate_registration_script(
            config, folders, data, CameraType::Mono, &register_dir
        )?;
        scripts.push(script);
    }

    if has_osc {
        let script = generate_registration_script(
            config, folders, data, CameraType::Osc, &register_dir
        )?;
        scripts.push(script);
    }

    // Generate per-group stacking scripts
    use crate::export::models::GlobalRegistrationPlan;
    let plan = GlobalRegistrationPlan::from_export_data(data);
    let stacking_groups = plan.stacking_groups(
        &config.exptime_tolerance_mode,
        config.exptime_tolerance_value,
    );

    for (idx, group) in stacking_groups.iter().enumerate() {
        let script = generate_stacking_script(config, folders, group, idx, &stack_dir)?;
        scripts.push(script);
    }

    println!("  Generated {} per-step scripts", scripts.len());
    Ok(scripts)
}

/// Generate a single master creation script
pub fn generate_single_master_script(
    config: &ExportConfig,
    folders: &SirilFolders,
    master: &crate::export::models::MasterInfo,
    _index: usize,
    output_dir: &std::path::Path,
) -> Result<PathBuf> {
    let mut script = String::new();

    let type_lower = master.master_type.to_lowercase();
    // Deterministic naming based on set_id only (not iteration index)
    let script_name = format!("master_{}_{}.ssf", type_lower, master.set_id);

    script.push_str(&format!(
        r#"############################################
# Siril Master Creation Script (Per-Step)
# Generated by Athenaeum
#
# Master Type: {}
# Set ID: {}
# Source Frames: {}
############################################

requires 1.2.0

"#,
        master.master_type,
        master.set_id,
        master.source_frames.len()
    ));

    // Work folder path based on master type
    let work_folder = match master.master_type.as_str() {
        "Bias" => folders.bias_set_path(master.set_id),
        "Dark" | "DarkFlat" => folders.dark_set_path(master.set_id),
        "Flat" => {
            let filter = master.source_frames.first().and_then(|f| f.filter.as_deref());
            folders.flat_set_path(master.set_id, filter)
        }
        _ => folders.process.clone(),
    };

    let output_path = folders.masters.join(&master.output_name);

    script.push_str(&format!(
        "cd {}\nconvert {}\n",
        work_folder.to_string_lossy(),
        type_lower
    ));

    // Apply calibrations based on type (same logic as monolithic script)
    match master.master_type.as_str() {
        "Bias" => {
            script.push_str(&format!(
                "stack {} rej {} {} -nonorm -out={}\n",
                type_lower,
                config.rejection_low,
                config.rejection_high,
                output_path.to_string_lossy()
            ));
        }
        "Dark" | "DarkFlat" => {
            if let Some(bias_id) = master.apply_bias {
                let bias_master = folders.masters.join(format!("master_bias_{}", bias_id));
                script.push_str(&format!(
                    "calibrate {} -bias={}\n",
                    type_lower,
                    bias_master.to_string_lossy()
                ));
                script.push_str(&format!(
                    "stack pp_{} rej {} {} -nonorm -out={}\n",
                    type_lower,
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            } else {
                script.push_str(&format!(
                    "stack {} rej {} {} -nonorm -out={}\n",
                    type_lower,
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
        }
        "Flat" => {
            let mut cal_args = String::new();

            if let Some(darkflat_id) = master.apply_darkflat {
                let darkflat_master = folders.masters.join(format!("master_darkflat_{}", darkflat_id));
                cal_args.push_str(&format!(" -dark={}", darkflat_master.to_string_lossy()));
            } else if let Some(dark_id) = master.apply_dark {
                let dark_master = folders.masters.join(format!("master_dark_{}", dark_id));
                cal_args.push_str(&format!(" -dark={}", dark_master.to_string_lossy()));
            } else if let Some(bias_id) = master.apply_bias {
                let bias_master = folders.masters.join(format!("master_bias_{}", bias_id));
                cal_args.push_str(&format!(" -dark={}", bias_master.to_string_lossy()));
            }

            if !cal_args.is_empty() {
                script.push_str(&format!("calibrate flat{}\n", cal_args));
                script.push_str(&format!(
                    "stack pp_flat rej {} {} -norm=mul -out={}\n",
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            } else {
                script.push_str(&format!(
                    "stack flat rej {} {} -norm=mul -out={}\n",
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
        }
        _ => {}
    }

    script.push_str("close\n");

    let script_path = output_dir.join(&script_name);
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write master script to {:?}", script_path))?;

    Ok(script_path)
}

/// Generate a branch calibration script
pub fn generate_branch_calibration_script(
    config: &ExportConfig,
    folders: &SirilFolders,
    branch: &crate::export::models::CalibrationBranch,
    index: usize,
    output_dir: &std::path::Path,
) -> Result<PathBuf> {
    let mut script = String::new();

    let _filter_safe = branch.filter.as_ref()
        .map(|f| sanitize_folder_name(f))
        .unwrap_or_else(|| "unfiltered".to_string());

    // Deterministic naming based on branch_id (not iteration index)
    let branch_id_safe = sanitize_folder_name(&branch.branch_id);
    let script_name = format!("calibrate_{}.ssf", branch_id_safe);

    script.push_str(&format!(
        r#"############################################
# Siril Branch Calibration Script (Per-Step)
# Generated by Athenaeum
#
# Branch: {}
# Camera: {}
# Filter: {}
# Light Frames: {}
############################################

requires 1.2.0

"#,
        branch.branch_id,
        branch.camera_name,
        branch.filter.as_deref().unwrap_or("None"),
        branch.light_frames.len()
    ));

    // Skip branches with < 2 lights
    if branch.light_frames.len() < 2 {
        script.push_str("# SKIPPED: Siril requires at least 2 frames\n");
        script.push_str("# This script is a placeholder\nclose\n");

        let script_path = output_dir.join(&script_name);
        fs::write(&script_path, &script)?;
        return Ok(script_path);
    }

    // Path to lights
    let lights_path = folders.lights_path(branch, index);

    script.push_str(&format!("cd {}\n", lights_path.to_string_lossy()));
    script.push_str("convert lights\n");

    // Build calibrate command
    let mut cal_args = Vec::new();
    let is_osc = branch.is_osc();

    if branch.dark_id > 0 {
        let dark_master = folders.masters.join(format!("master_dark_{}", branch.dark_id));
        cal_args.push(format!("-dark={}", dark_master.to_string_lossy()));
        cal_args.push("-cc=dark".to_string());
        if is_osc {
            cal_args.push("-cfa".to_string());
        }
    }

    if branch.flat_id > 0 {
        let flat_master = folders.masters.join(format!("master_flat_{}", branch.flat_id));
        cal_args.push(format!("-flat={}", flat_master.to_string_lossy()));
    }

    if is_osc && !config.drizzle_enabled {
        cal_args.push("-debayer".to_string());
    }

    if cal_args.is_empty() {
        script.push_str("calibrate lights\n");
    } else {
        script.push_str(&format!("calibrate lights {}\n", cal_args.join(" ")));
    }

    script.push_str("close\n");

    let script_path = output_dir.join(&script_name);
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write calibration script to {:?}", script_path))?;

    Ok(script_path)
}

/// Generate a registration script for a specific camera type
pub fn generate_registration_script(
    _config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
    camera_type: CameraType,
    output_dir: &std::path::Path,
) -> Result<PathBuf> {
    let mut script = String::new();

    let camera_suffix = camera_type.display_name().to_lowercase();
    let script_name = format!("01_register_{}.ssf", camera_suffix);

    // Count frames for this camera type
    let frame_count: usize = data.branches.iter()
        .filter(|b| if camera_type == CameraType::Osc { b.is_osc() } else { !b.is_osc() })
        .map(|b| b.light_frames.len())
        .sum();

    // Get focal length and pixel size
    let (focal_length, pixel_size) = get_optics_params(data);

    script.push_str(&format!(
        r#"############################################
# Siril Registration Script (Per-Step)
# Generated by Athenaeum
#
# Camera Type: {}
# Frame Count: {}
############################################

requires 1.3.0

"#,
        camera_suffix.to_uppercase(),
        frame_count
    ));

    let lights_dir = folders.process.join(format!("all_lights_{}", camera_suffix));

    script.push_str(&format!("cd {}\n", lights_dir.to_string_lossy()));
    script.push_str("convert pp_lights -out=. -fitseq\n\n");

    script.push_str(&format!(
        "seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}\n\n",
        focal_length, pixel_size
    ));

    script.push_str("register pp_lights -2pass\n\n");
    script.push_str("convert r_pp_lights -out=. -fitseq\n");

    script.push_str("close\n");

    let script_path = output_dir.join(&script_name);
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write registration script to {:?}", script_path))?;

    Ok(script_path)
}

/// Generate a stacking script for a specific filter+camera group
pub fn generate_stacking_script(
    config: &ExportConfig,
    folders: &SirilFolders,
    group: &crate::export::models::ExptimeStackGroup,
    _index: usize,
    output_dir: &std::path::Path,
) -> Result<PathBuf> {
    let mut script = String::new();

    let filter_safe = group.filter.as_ref()
        .map(|f| sanitize_folder_name(f))
        .unwrap_or_else(|| "unfiltered".to_string());
    let camera_suffix = group.camera_type.display_name().to_lowercase();

    // Deterministic naming based on filter+camera (not iteration index)
    let script_name = format!("stack_{}_{}.ssf", filter_safe, camera_suffix);

    let filter_name = group.filter.as_deref().unwrap_or("Unfiltered");

    script.push_str(&format!(
        r#"############################################
# Siril Stacking Script (Per-Step)
# Generated by Athenaeum
#
# Filter: {}
# Camera Type: {}
# Frame Count: {}
# Exptime: {}
############################################

requires 1.3.0

"#,
        filter_name,
        camera_suffix.to_uppercase(),
        group.frame_indices.len(),
        if group.exptime_display.is_empty() { "Mixed" } else { &group.exptime_display }
    ));

    // Skip groups with < 2 frames
    if group.frame_indices.len() < 2 {
        script.push_str("# SKIPPED: Need >= 2 frames for stacking\nclose\n");
        let script_path = output_dir.join(&script_name);
        fs::write(&script_path, &script)?;
        return Ok(script_path);
    }

    let lights_dir = folders.process.join(format!("all_lights_{}", camera_suffix));
    let registered_sequence_name = "r_pp_lights";

    script.push_str(&format!("cd {}\n\n", lights_dir.to_string_lossy()));

    // Calculate total frames for unselect
    // Note: This assumes all frames for this camera type are in the sequence
    let total_frames = group.frame_indices.iter().max().unwrap_or(&0) + 1;

    script.push_str(&format!("unselect {} 1 {}\n", registered_sequence_name, total_frames));

    let ranges = indices_to_ranges(&group.frame_indices);
    for (start, end) in &ranges {
        script.push_str(&format!("select {} {} {}\n", registered_sequence_name, start, end));
    }

    // Output filename
    let output_name = if group.exptime_display.is_empty() {
        format!("{}_{}_stacked", filter_safe, camera_suffix)
    } else {
        format!("{}_{}_{}_stacked", filter_safe, camera_suffix, group.exptime_display)
    };
    let output_path = folders.masters.join(&output_name);

    // Build stack command
    let mut stack_cmd = format!("stack {}", registered_sequence_name);

    let rej_cmd = match config.rejection_algorithm {
        RejectionAlgorithm::Percentile => {
            format!(" rej percentile {} {}", config.rejection_low, config.rejection_high)
        }
        RejectionAlgorithm::Sigma => {
            format!(" rej sigma {} {}", config.rejection_low, config.rejection_high)
        }
        RejectionAlgorithm::LinearFit => {
            format!(" rej linear {} {}", config.rejection_low, config.rejection_high)
        }
        RejectionAlgorithm::Gesd => {
            format!(" rej gesd {} {}", config.rejection_low, config.rejection_high)
        }
        RejectionAlgorithm::Mad => {
            format!(" rej mad {} {}", config.rejection_low, config.rejection_high)
        }
    };
    stack_cmd.push_str(&rej_cmd);
    stack_cmd.push_str(" -filter-included");
    stack_cmd.push_str(" -norm=addscale -output_norm");

    match config.image_weighting {
        ImageWeightingMethod::None => {}
        ImageWeightingMethod::Wfwhm => stack_cmd.push_str(" -weight=wfwhm"),
        ImageWeightingMethod::Stars => stack_cmd.push_str(" -weight=nbstars"),
        ImageWeightingMethod::Noise => stack_cmd.push_str(" -weight=noise"),
        ImageWeightingMethod::ExposureTime => stack_cmd.push_str(" -weight=itime"),
    }

    if group.camera_type == CameraType::Osc {
        stack_cmd.push_str(" -rgb_equal");
    }

    stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
    script.push_str(&format!("{}\n", stack_cmd));

    script.push_str("close\n");

    let script_path = output_dir.join(&script_name);
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write stacking script to {:?}", script_path))?;

    Ok(script_path)
}

/// Get information about generated per-step scripts for pipeline building
pub struct PerStepScriptInfo {
    /// Path to the script
    pub script_path: PathBuf,
    /// Master set ID (for master scripts)
    pub set_id: Option<i64>,
    /// Master type (Bias, Dark, DarkFlat, Flat)
    pub master_type: Option<String>,
    /// Branch index (for calibration scripts)
    pub branch_index: Option<usize>,
    /// Branch ID
    pub branch_id: Option<String>,
    /// Camera type (for registration/stacking scripts)
    pub camera_type: Option<CameraType>,
    /// Filter (for stacking scripts)
    pub filter: Option<String>,
    /// Frame count
    pub frame_count: usize,
    /// Step dependencies (set IDs that must complete first)
    pub depends_on_sets: Vec<i64>,
}

/// Get script info for all per-step scripts without generating them
pub fn get_per_step_script_info(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Vec<PerStepScriptInfo> {
    let mut info = Vec::new();

    let scripts_dir = folders.root.join("scripts");
    let masters_dir = scripts_dir.join("00_masters");
    let calibrate_dir = scripts_dir.join("01_calibrate");
    let register_dir = scripts_dir.join("02_register");
    let stack_dir = scripts_dir.join("03_stack");

    // Master script info
    if config.create_masters {
        for (_idx, master) in data.master_plan.masters.iter().enumerate() {
            let type_lower = master.master_type.to_lowercase();
            // Deterministic naming based on set_id (not iteration index)
            let script_name = format!("master_{}_{}.ssf", type_lower, master.set_id);

            info.push(PerStepScriptInfo {
                script_path: masters_dir.join(&script_name),
                set_id: Some(master.set_id),
                master_type: Some(master.master_type.clone()),
                branch_index: None,
                branch_id: None,
                camera_type: None,
                filter: None,
                frame_count: master.source_frames.len(),
                depends_on_sets: master.depends_on.clone(),
            });
        }
    }

    // Branch calibration script info
    for (idx, branch) in data.branches.iter().enumerate() {
        // Deterministic naming based on branch_id (not iteration index)
        let branch_id_safe = sanitize_folder_name(&branch.branch_id);
        let script_name = format!("calibrate_{}.ssf", branch_id_safe);

        // Collect dependencies - all masters used by this branch
        let mut depends = Vec::new();
        if branch.bias_id > 0 { depends.push(branch.bias_id); }
        if branch.dark_id > 0 { depends.push(branch.dark_id); }
        if branch.flat_id > 0 { depends.push(branch.flat_id); }
        if branch.darkflat_id > 0 { depends.push(branch.darkflat_id); }

        info.push(PerStepScriptInfo {
            script_path: calibrate_dir.join(&script_name),
            set_id: None,
            master_type: None,
            branch_index: Some(idx),
            branch_id: Some(branch.branch_id.clone()),
            camera_type: Some(if branch.is_osc() { CameraType::Osc } else { CameraType::Mono }),
            filter: branch.filter.clone(),
            frame_count: branch.light_frames.len(),
            depends_on_sets: depends,
        });
    }

    // Registration script info
    let has_mono = data.branches.iter().any(|b| !b.is_osc());
    let has_osc = data.branches.iter().any(|b| b.is_osc());

    if has_mono {
        let frame_count: usize = data.branches.iter()
            .filter(|b| !b.is_osc())
            .map(|b| b.light_frames.len())
            .sum();

        info.push(PerStepScriptInfo {
            script_path: register_dir.join("01_register_mono.ssf"),
            set_id: None,
            master_type: None,
            branch_index: None,
            branch_id: None,
            camera_type: Some(CameraType::Mono),
            filter: None,
            frame_count,
            depends_on_sets: Vec::new(), // Depends on all calibration branches (tracked at step level)
        });
    }

    if has_osc {
        let frame_count: usize = data.branches.iter()
            .filter(|b| b.is_osc())
            .map(|b| b.light_frames.len())
            .sum();

        info.push(PerStepScriptInfo {
            script_path: register_dir.join("01_register_osc.ssf"),
            set_id: None,
            master_type: None,
            branch_index: None,
            branch_id: None,
            camera_type: Some(CameraType::Osc),
            filter: None,
            frame_count,
            depends_on_sets: Vec::new(),
        });
    }

    // Stacking script info
    use crate::export::models::GlobalRegistrationPlan;
    let plan = GlobalRegistrationPlan::from_export_data(data);
    let stacking_groups = plan.stacking_groups(
        &config.exptime_tolerance_mode,
        config.exptime_tolerance_value,
    );

    for (_idx, group) in stacking_groups.iter().enumerate() {
        let filter_safe = group.filter.as_ref()
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());
        let camera_suffix = group.camera_type.display_name().to_lowercase();

        // Deterministic naming based on filter+camera (not iteration index)
        let script_name = format!("stack_{}_{}.ssf", filter_safe, camera_suffix);

        info.push(PerStepScriptInfo {
            script_path: stack_dir.join(&script_name),
            set_id: None,
            master_type: None,
            branch_index: None,
            branch_id: None,
            camera_type: Some(group.camera_type.clone()),
            filter: group.filter.clone(),
            frame_count: group.frame_indices.len(),
            depends_on_sets: Vec::new(), // Depends on registration (tracked at step level)
        });
    }

    info
}

// ============================================================================
// V4: Script Generation for Flat Siril Folder Structure
// ============================================================================

/// Generate 00_create_masters.ssf using flat SirilFolders structure
fn generate_master_script_v4(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    let mut script = String::new();

    script.push_str(&format!(
        r#"############################################
# Siril Master Creation Script (V4 - Flat Structure)
# Generated by Athenaeum
# Total masters: {}
# Cameras: {}
############################################

requires 1.2.0

"#,
        data.master_plan.masters.len(),
        data.cameras.join(", ")
    ));

    // Process masters in dependency order
    for (idx, master) in data.master_plan.masters.iter().enumerate() {
        script.push_str(&format!(
            "# ========== Master {} of {}: {} (Set {}) ==========\n",
            idx + 1,
            data.master_plan.masters.len(),
            master.master_type,
            master.set_id
        ));

        // Work folder path based on master type (flat structure)
        let work_folder = match master.master_type.as_str() {
            "Bias" => folders.bias_set_path(master.set_id),
            "Dark" | "DarkFlat" => folders.dark_set_path(master.set_id),
            "Flat" => {
                // Get filter from first source frame
                let filter = master.source_frames.first().and_then(|f| f.filter.as_deref());
                folders.flat_set_path(master.set_id, filter)
            }
            _ => folders.process.clone(),
        };

        let output_path = folders.masters.join(&master.output_name);
        let type_lower = master.master_type.to_lowercase();

        // Work directly in the set folder
        script.push_str(&format!(
            "cd {}\nconvert {}\n",
            work_folder.to_string_lossy(),
            type_lower
        ));

        // Apply calibrations based on type
        match master.master_type.as_str() {
            "Bias" => {
                script.push_str(&format!(
                    "stack {} rej {} {} -nonorm -out={}\n\n",
                    type_lower,
                    config.rejection_low,
                    config.rejection_high,
                    output_path.to_string_lossy()
                ));
            }
            "Dark" | "DarkFlat" => {
                if let Some(bias_id) = master.apply_bias {
                    let bias_master = folders.masters.join(format!("master_bias_{}", bias_id));
                    script.push_str(&format!(
                        "calibrate {} -bias={}\n",
                        type_lower,
                        bias_master.to_string_lossy()
                    ));
                    script.push_str(&format!(
                        "stack pp_{} rej {} {} -nonorm -out={}\n\n",
                        type_lower,
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                } else {
                    script.push_str(&format!(
                        "stack {} rej {} {} -nonorm -out={}\n\n",
                        type_lower,
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                }
            }
            "Flat" => {
                // Flat calibration priority:
                // 1. DarkFlat (same exposure as flat) - best
                // 2. Dark with matching exposure (±30%) - good
                // 3. Bias - fallback (minimal subtraction, better than wrong-exposure dark)
                // 4. No calibration - if nothing available
                let mut cal_args = String::new();

                if let Some(darkflat_id) = master.apply_darkflat {
                    // Best: Use DarkFlat (same exposure as flat)
                    let darkflat_master = folders.masters.join(format!("master_darkflat_{}", darkflat_id));
                    cal_args.push_str(&format!(" -dark={}", darkflat_master.to_string_lossy()));
                } else if let Some(dark_id) = master.apply_dark {
                    // Good: Use Dark with matching exposure (apply_dark is only set if exposure matches)
                    let dark_master = folders.masters.join(format!("master_dark_{}", dark_id));
                    cal_args.push_str(&format!(" -dark={}", dark_master.to_string_lossy()));
                } else if let Some(bias_id) = master.apply_bias {
                    // Fallback: Use Bias (minimal subtraction - better than over-subtracting with wrong-exposure dark)
                    // This prevents the bright edges issue caused by using long-exposure darks for short-exposure flats
                    let bias_master = folders.masters.join(format!("master_bias_{}", bias_id));
                    cal_args.push_str(&format!(" -dark={}", bias_master.to_string_lossy()));
                }
                // If nothing available, skip calibration entirely

                if !cal_args.is_empty() {
                    script.push_str(&format!("calibrate flat{}\n", cal_args));
                    script.push_str(&format!(
                        "stack pp_flat rej {} {} -norm=mul -out={}\n\n",
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                } else {
                    script.push_str(&format!(
                        "stack flat rej {} {} -norm=mul -out={}\n\n",
                        config.rejection_low,
                        config.rejection_high,
                        output_path.to_string_lossy()
                    ));
                }
            }
            _ => {}
        }
    }

    script.push_str("close\n");

    let script_path = folders.root.join("00_create_masters.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write master script to {:?}", script_path))?;

    println!("  Created: {:?}", script_path);
    Ok(script_path)
}

/// Generate 01_calibrate_lights.ssf using flat SirilFolders structure
fn generate_calibrate_lights_script_v4(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    let mut script = String::new();

    script.push_str(&format!(
        r#"############################################
# Siril Light Calibration Script (V4 - Flat Structure)
# Generated by Athenaeum
# Total branches: {}
# Total lights: {}
############################################

requires 1.2.0

"#,
        data.branches.len(),
        data.total_light_frames
    ));

    // Process each branch
    for (idx, branch) in data.branches.iter().enumerate() {
        script.push_str(&format!(
            "# ========== Branch {} of {}: {} ==========\n",
            idx + 1,
            data.branches.len(),
            branch.branch_id
        ));
        script.push_str(&format!("# Camera: {}\n", branch.camera_name));
        script.push_str(&format!(
            "# Filter: {}\n",
            branch.filter.as_deref().unwrap_or("None")
        ));
        script.push_str(&format!("# Lights: {}\n", branch.light_frames.len()));

        // Skip branches with less than 2 lights
        if branch.light_frames.len() < 2 {
            script.push_str("# SKIPPED: Siril requires at least 2 frames\n\n");
            continue;
        }

        script.push_str("\n");

        // Path to lights in flat structure: lights/branch_XX_filter/
        let lights_path = folders.lights_path(branch, idx);

        script.push_str(&format!("cd {}\n", lights_path.to_string_lossy()));
        script.push_str("convert lights\n");

        // Build calibrate command
        let mut cal_args = Vec::new();
        let is_osc = branch.is_osc();

        if branch.dark_id > 0 {
            let dark_master = folders.masters.join(format!("master_dark_{}", branch.dark_id));
            cal_args.push(format!("-dark={}", dark_master.to_string_lossy()));
            cal_args.push("-cc=dark".to_string());
            // Add -cfa for OSC to ensure cosmetic correction respects Bayer pattern
            if is_osc {
                cal_args.push("-cfa".to_string());
            }
        }

        if branch.flat_id > 0 {
            let flat_master = folders.masters.join(format!("master_flat_{}", branch.flat_id));
            cal_args.push(format!("-flat={}", flat_master.to_string_lossy()));
        }

        // OSC debayer - add to calibrate command unless drizzle is enabled
        // With drizzle, we preserve the CFA pattern and debayer happens after drizzle stacking
        if is_osc && !config.drizzle_enabled {
            cal_args.push("-debayer".to_string());
        }

        // Always run calibrate to ensure 32-bit float output (pp_lights_.seq)
        // This is required even without calibration files for consistent bit depth in merge
        // See: https://siril.readthedocs.io/en/latest/Commands.html
        if cal_args.is_empty() {
            script.push_str("calibrate lights\n");
        } else {
            script.push_str(&format!("calibrate lights {}\n", cal_args.join(" ")));
        }

        script.push_str("\n");
    }

    script.push_str("close\n");

    let script_path = folders.root.join("01_calibrate_lights.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write calibration script to {:?}", script_path))?;

    println!("  Created: {:?}", script_path);
    Ok(script_path)
}

/// Generate 02_register_branches.ssf - Per-branch registration with branch-specific optics
///
/// This script handles the CRITICAL FIX for multi-group scenarios:
/// - Each branch is plate-solved and registered with ITS OWN focal length and pixel size
/// - This prevents the "Cannot add an image with different properties" error
/// - After this script runs, each branch has r_pp_lights_*.fit files ready for merge
///
/// Workflow per branch:
/// 1. cd to branch directory
/// 2. convert pp_lights to create sequence
/// 3. seqplatesolve with BRANCH-SPECIFIC focal/pixelsize
/// 4. register -2pass (auto-selects best reference frame within branch)
/// 5. seqapplyreg -framing=max (creates r_pp_lights_*.fit files)
pub fn generate_register_branches_script(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    let mut script = String::new();

    // Count branches with >= 2 frames (Siril requirement)
    let valid_branches: Vec<_> = data.branches.iter()
        .enumerate()
        .filter(|(_, b)| b.light_frames.len() >= 2)
        .collect();

    let total_frames: usize = valid_branches.iter()
        .map(|(_, b)| b.light_frames.len())
        .sum();

    script.push_str(&format!(
        r#"############################################
# Siril Per-Branch Registration Script
# Generated by Athenaeum
#
# MULTI-GROUP FIX: Each branch is registered with its own
# focal length and pixel size, preventing "different properties" errors.
#
# Branches: {} (with >= 2 frames)
# Total frames: {}
############################################

requires 1.3.0

"#,
        valid_branches.len(),
        total_frames
    ));

    // Process each branch with its own optical parameters
    for (idx, branch) in &valid_branches {
        let (focal_length, pixel_size) = get_branch_optics(branch);
        let lights_path = folders.lights_path(branch, *idx);

        script.push_str(&format!(
            "# ========== Branch {} of {}: {} ==========\n",
            idx + 1,
            valid_branches.len(),
            branch.branch_id
        ));
        script.push_str(&format!("# Camera: {}\n", branch.camera_name));
        script.push_str(&format!(
            "# Filter: {}\n",
            branch.filter.as_deref().unwrap_or("None")
        ));
        script.push_str(&format!("# Frames: {}\n", branch.light_frames.len()));
        script.push_str(&format!("# Focal: {:.1}mm, Pixel: {:.2}µm\n", focal_length, pixel_size));
        script.push_str("\n");

        // Step 1: cd to branch directory
        script.push_str(&format!("cd {}\n", lights_path.to_string_lossy()));

        // Step 2: Convert to sequence
        script.push_str("convert pp_lights -out=. -fitseq\n\n");

        // Step 3: Plate solve with BRANCH-SPECIFIC optics
        script.push_str(&format!(
            "seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}\n\n",
            focal_length, pixel_size
        ));

        // Step 4: Register with -2pass
        script.push_str("# -2pass auto-selects best reference frame within this branch\n");
        let register_cmd = build_register_command(config, "pp_lights");
        script.push_str(&format!("{}\n\n", register_cmd));

        // Step 5: Apply registration with max framing
        script.push_str("# Apply registration - creates r_pp_lights_*.fit files\n");
        script.push_str("seqapplyreg pp_lights -framing=max\n\n");
    }

    script.push_str("close\n");

    let script_path = folders.root.join("02_register_branches.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write per-branch registration script to {:?}", script_path))?;

    println!("  Created: {:?}", script_path);
    Ok(script_path)
}

/// Generate 02_register_and_stack.ssf - Astrometric registration and per-group stacking
///
/// Uses the MOSAIC approach which handles different camera dimensions:
/// 1. All calibrated frames are collected to a unified directory (by wrapper script)
/// 2. Convert creates a unified sequence from all frames
/// 3. Plate solving embeds WCS coordinates for astrometric alignment
/// 4. seqapplyreg -framing=max pads smaller frames to match largest
/// 5. Stacks per (filter + camera_type + exptime) group
fn generate_register_and_stack_script_v4(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    // Use the new astrometric/mosaic approach for all cases
    // This handles different camera dimensions via plate solving + max framing
    generate_astrometric_script_v4(config, folders, data)
}

/// Generate astrometric registration script using dual-pipeline approach
///
/// This approach handles different camera types by processing OSC and Mono
/// frames in SEPARATE pipelines (Siril cannot mix different layer counts):
///
/// For each camera type (if present):
/// 1. Change to camera-specific directory (all_lights_mono or all_lights_osc)
/// 2. Convert to sequence
/// 3. Plate solve for WCS coordinates
/// 4. Apply astrometric registration with -framing=max (pads smaller frames)
/// 5. Stack per (filter + exptime) group
fn generate_astrometric_script_v4(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    use crate::export::models::GlobalRegistrationPlan;

    let mut script = String::new();

    // Build the registration plan for frame indexing
    let plan = GlobalRegistrationPlan::from_export_data(data);

    // Count frames by camera type
    let mono_branches: Vec<_> = data.branches.iter().filter(|b| !b.is_osc()).collect();
    let osc_branches: Vec<_> = data.branches.iter().filter(|b| b.is_osc()).collect();

    let mono_frames: usize = mono_branches.iter().map(|b| b.light_frames.len()).sum();
    let osc_frames: usize = osc_branches.iter().map(|b| b.light_frames.len()).sum();
    let total_frames = mono_frames + osc_frames;

    let has_mono = mono_frames > 0;
    let has_osc = osc_frames > 0;

    let camera_mode = match (has_osc, has_mono) {
        (true, true) => "DUAL PIPELINE (OSC + Mono)",
        (true, false) => "OSC only",
        (false, true) => "Mono only",
        (false, false) => "Unknown",
    };

    // Get unique camera names for documentation
    let camera_names: Vec<String> = data.branches.iter()
        .map(|b| b.camera_name.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Get focal length and pixel size for plate solving
    let (focal_length, pixel_size) = get_optics_params(data);

    script.push_str(&format!(
        r#"############################################
# Siril Registration and Stacking Script
# Generated by Athenaeum
#
# DUAL PIPELINE MODE
# OSC and Mono frames processed in separate pipelines
# (Siril cannot mix different layer counts in one sequence)
#
# Camera mode: {}
# Cameras: {}
# Mono frames: {}
# OSC frames: {}
# Total: {} frames
#
# NOTE: Frame collection is handled automatically by Athenaeum
# Frames are separated into all_lights_mono/ and all_lights_osc/
############################################

requires 1.3.0

"#,
        camera_mode,
        camera_names.join(", "),
        mono_frames,
        osc_frames,
        total_frames,
    ));

    // Get stacking groups (grouped by filter + camera_type + exptime)
    let all_stacking_groups = plan.stacking_groups(
        &config.exptime_tolerance_mode,
        config.exptime_tolerance_value,
    );

    // Separate groups by camera type
    let mono_groups: Vec<_> = all_stacking_groups.iter()
        .filter(|g| g.camera_type == CameraType::Mono)
        .collect();
    let osc_groups: Vec<_> = all_stacking_groups.iter()
        .filter(|g| g.camera_type == CameraType::Osc)
        .collect();

    // ==================== MONO PIPELINE ====================
    if has_mono && !mono_groups.is_empty() {
        let mono_dir = folders.process.join("all_lights_mono");

        script.push_str("############################################\n");
        script.push_str("# MONO CAMERA PIPELINE\n");
        script.push_str(&format!("# {} frames from {} branches\n", mono_frames, mono_branches.len()));
        script.push_str("############################################\n\n");

        // Step 1: Change to mono directory and convert
        script.push_str("# --- Step 1: Create Mono Sequence ---\n");
        script.push_str(&format!("cd {}\n", mono_dir.to_string_lossy()));
        script.push_str("convert pp_lights -out=. -fitseq\n\n");

        // Step 2: Plate solve
        script.push_str("# --- Step 2: Plate Solve for WCS ---\n");
        script.push_str(&format!(
            "seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}\n\n",
            focal_length, pixel_size
        ));

        // Step 3: Register (creates registration transforms using WCS data)
        script.push_str("# --- Step 3: Register Frames ---\n");
        script.push_str("# -2pass auto-selects best reference frame\n");
        script.push_str("# Registration uses WCS data from plate solving\n");
        script.push_str("register pp_lights -2pass\n\n");

        // Step 3b: Convert registered files to sequence
        script.push_str("# --- Step 3b: Create Registered Sequence ---\n");
        script.push_str("# register creates r_pp_lights_*.fit files but no sequence file\n");
        script.push_str("convert r_pp_lights -out=. -fitseq\n\n");

        // Step 4: Stack per group
        script.push_str("# --- Step 4: Stack Per Group ---\n");
        generate_stack_commands_for_groups(
            &mut script,
            config,
            folders,
            &mono_groups,
            mono_frames,
            "r_pp_lights",  // Sequence created by convert above
        );
    }

    // ==================== OSC PIPELINE ====================
    if has_osc && !osc_groups.is_empty() {
        let osc_dir = folders.process.join("all_lights_osc");

        script.push_str("\n############################################\n");
        script.push_str("# OSC CAMERA PIPELINE\n");
        script.push_str(&format!("# {} frames from {} branches\n", osc_frames, osc_branches.len()));
        script.push_str("############################################\n\n");

        // Step 1: Change to OSC directory and convert
        script.push_str("# --- Step 1: Create OSC Sequence ---\n");
        script.push_str(&format!("cd {}\n", osc_dir.to_string_lossy()));
        script.push_str("convert pp_lights -out=. -fitseq\n\n");

        // Step 2: Plate solve
        script.push_str("# --- Step 2: Plate Solve for WCS ---\n");
        script.push_str(&format!(
            "seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}\n\n",
            focal_length, pixel_size
        ));

        // Step 3: Register (creates registration transforms using WCS data)
        script.push_str("# --- Step 3: Register Frames ---\n");
        script.push_str("# -2pass auto-selects best reference frame\n");
        script.push_str("# Registration uses WCS data from plate solving\n");
        script.push_str("register pp_lights -2pass\n\n");

        // Step 3b: Convert registered files to sequence
        script.push_str("# --- Step 3b: Create Registered Sequence ---\n");
        script.push_str("# register creates r_pp_lights_*.fit files but no sequence file\n");
        script.push_str("convert r_pp_lights -out=. -fitseq\n\n");

        // Step 4: Stack per group
        script.push_str("# --- Step 4: Stack Per Group ---\n");
        generate_stack_commands_for_groups(
            &mut script,
            config,
            folders,
            &osc_groups,
            osc_frames,
            "r_pp_lights",  // Sequence created by convert above
        );
    }

    script.push_str("close\n");

    // Write the main Siril script
    let script_path = folders.root.join("02_register_and_stack.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write registration/stacking script to {:?}", script_path))?;

    println!("  Created: {:?}", script_path);
    Ok(script_path)
}

/// Generate stack commands for a set of stacking groups
///
/// This generates stack commands for each filter/exptime group within a camera type's pipeline.
/// All frames are registered together (for global alignment), but stacking is done per group
/// using frame selection to pick the right frames for each filter/exptime combination.
///
/// Frame indices come from the calibration branch order, and since filenames are preserved
/// during collection (pp_lights_XXXXX.fit), Siril assigns sequence indices in the same order.
fn generate_stack_commands_for_groups(
    script: &mut String,
    config: &ExportConfig,
    folders: &SirilFolders,
    groups: &[&crate::export::models::ExptimeStackGroup],
    total_frames_in_pipeline: usize,
    registered_sequence_name: &str,
) {
    // Get unique filters for info comment
    let unique_filters: std::collections::HashSet<_> = groups.iter()
        .map(|g| g.filter.as_deref().unwrap_or("Unfiltered"))
        .collect();

    script.push_str(&format!(
        "# Stacking {} groups across {} filter(s): {}\n",
        groups.len(),
        unique_filters.len(),
        unique_filters.iter().cloned().collect::<Vec<_>>().join(", ")
    ));
    script.push_str(&format!("# Total frames in sequence: {}\n\n", total_frames_in_pipeline));

    // Stack each group separately using frame selection
    for group in groups {
        let filter_name = group.filter.as_deref().unwrap_or("Unfiltered");
        let filter_safe = group.filter
            .as_ref()
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());
        let camera_type_str = group.camera_type.display_name().to_lowercase();

        // Skip groups with < 2 frames
        if group.frame_indices.len() < 2 {
            let display_label = format!(
                "{} {} ({} frame)",
                filter_name,
                if group.exptime_display.is_empty() { "" } else { &group.exptime_display },
                group.frame_indices.len()
            );
            script.push_str(&format!(
                "# --- {} - SKIPPED (need >= 2 frames) ---\n\n",
                display_label
            ));
            continue;
        }

        // Build display label with filter and exptime
        let display_label = format!(
            "{} {} ({} frames)",
            filter_name,
            if group.exptime_display.is_empty() { camera_type_str.clone() } else { format!("{} {}", camera_type_str, group.exptime_display) },
            group.frame_indices.len()
        );

        script.push_str(&format!("# --- {} ---\n", display_label));

        // Unselect all frames first
        script.push_str(&format!("unselect {} 1 {}\n", registered_sequence_name, total_frames_in_pipeline));

        // Select only the frames for this group
        let ranges = indices_to_ranges(&group.frame_indices);
        for (start, end) in &ranges {
            script.push_str(&format!("select {} {} {}\n", registered_sequence_name, start, end));
        }

        // Output filename includes filter and exptime
        let output_name = if group.exptime_display.is_empty() {
            format!("{}_{}_stacked", filter_safe, camera_type_str)
        } else {
            format!("{}_{}_{}_stacked", filter_safe, camera_type_str, group.exptime_display)
        };
        let output_path = folders.masters.join(&output_name);

        // Build stack command
        let mut stack_cmd = format!("stack {}", registered_sequence_name);

        // Rejection algorithm
        let rej_cmd = match config.rejection_algorithm {
            RejectionAlgorithm::Percentile => {
                format!(" rej percentile {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Sigma => {
                format!(" rej sigma {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::LinearFit => {
                format!(" rej linear {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Gesd => {
                format!(" rej gesd {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Mad => {
                format!(" rej mad {} {}", config.rejection_low, config.rejection_high)
            }
        };
        stack_cmd.push_str(&rej_cmd);
        stack_cmd.push_str(" -filter-included");  // Only stack selected frames
        stack_cmd.push_str(" -norm=addscale -output_norm");

        // Image weighting
        match config.image_weighting {
            ImageWeightingMethod::None => {}
            ImageWeightingMethod::Wfwhm => stack_cmd.push_str(" -weight=wfwhm"),
            ImageWeightingMethod::Stars => stack_cmd.push_str(" -weight=nbstars"),
            ImageWeightingMethod::Noise => stack_cmd.push_str(" -weight=noise"),
            ImageWeightingMethod::ExposureTime => stack_cmd.push_str(" -weight=itime"),
        }

        // RGB compositing for OSC cameras
        if group.camera_type == CameraType::Osc {
            stack_cmd.push_str(" -rgb_equal");
        }

        stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
        script.push_str(&format!("{}\n\n", stack_cmd));
    }
}

/// Get focal length and pixel size from export data (global, uses first available)
/// Falls back to reasonable defaults if not available
///
/// Note: For multi-telescope setups, use `get_branch_optics()` instead to get
/// per-branch optical parameters.
fn get_optics_params(data: &ExportDataV3) -> (f64, f64) {
    // Try to get focal length from first branch with valid data
    let mut focal = 0.0;
    let mut pixel_size = 0.0;
    for branch in &data.branches {
        if let Some(frame) = branch.light_frames.first() {
            if focal <= 0.0 {
                if let Some(f) = frame.focallen {
                    if f > 0.0 {
                        focal = f;
                    }
                }
            }
            if pixel_size <= 0.0 {
                if let Some(p) = frame.xpixsz {
                    if p > 0.0 {
                        pixel_size = p;
                    }
                }
            }
            if focal > 0.0 && pixel_size > 0.0 {
                break;
            }
        }
    }

    // Use defaults if not found
    // Defaults: 500mm focal, 3.76um pixel (common for ASI/QHY cameras)
    if focal <= 0.0 {
        focal = 500.0;
    }

    // Pixel size defaults - common values:
    // ASI2600/QHY268: 3.76um
    // ASI533/QHY533: 3.76um
    // ASI294/QHY294: 4.63um
    // ASI1600/QHY163: 3.8um
    if pixel_size <= 0.0 {
        pixel_size = 3.76;
    }

    (focal, pixel_size)
}

/// Get focal length and pixel size for a specific calibration branch
///
/// Each branch may have different optical parameters (different telescope, camera).
/// Returns (focal_length_mm, pixel_size_um) with fallback to defaults.
fn get_branch_optics(branch: &crate::export::models::CalibrationBranch) -> (f64, f64) {
    // Get focal length from branch's first light frame
    let focal = branch.light_frames.first()
        .and_then(|f| f.focallen)
        .filter(|&f| f > 0.0)
        .unwrap_or(500.0);  // Default fallback

    // Get pixel size from branch's first light frame
    let pixel_size = branch.light_frames.first()
        .and_then(|f| f.xpixsz)
        .filter(|&p| p > 0.0)
        .unwrap_or(3.76);  // Default fallback

    (focal, pixel_size)
}

/// Generate script for mixed OSC + Mono camera types (DEPRECATED)
/// Kept for backwards compatibility but now uses astrometric approach
#[allow(dead_code)]
fn generate_mixed_camera_script_v4(
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
) -> Result<PathBuf> {
    let mut script = String::new();

    // Separate branches by camera type
    // Note: Branches with < 2 frames are already handled by the calibration script
    // which creates pp_lights_.seq via seqcpy even for uncalibrated branches
    let osc_branch_indices: Vec<usize> = data.branches.iter()
        .enumerate()
        .filter(|(_, b)| b.is_osc())
        .map(|(i, _)| i)
        .collect();
    let mono_branch_indices: Vec<usize> = data.branches.iter()
        .enumerate()
        .filter(|(_, b)| !b.is_osc())
        .map(|(i, _)| i)
        .collect();

    let osc_frames: usize = osc_branch_indices.iter()
        .map(|&i| data.branches[i].light_frames.len())
        .sum();
    let mono_frames: usize = mono_branch_indices.iter()
        .map(|&i| data.branches[i].light_frames.len())
        .sum();

    script.push_str(&format!(
        r#"############################################
# Siril Registration and Stacking Script
# Generated by Athenaeum
#
# MIXED CAMERA MODE: OSC + Mono cameras detected
# Processing each camera type in separate pipelines
#
# OSC branches: {} ({} frames)
# Mono branches: {} ({} frames)
############################################

requires 1.2.0

cd {}

"#,
        osc_branch_indices.len(), osc_frames,
        mono_branch_indices.len(), mono_frames,
        folders.process.to_string_lossy()
    ));

    // ========== OSC Pipeline ==========
    if !osc_branch_indices.is_empty() {
        script.push_str("############################################\n");
        script.push_str("# OSC CAMERA PIPELINE\n");
        script.push_str("############################################\n\n");

        generate_pipeline_for_camera_type(
            &mut script,
            config,
            folders,
            data,
            &osc_branch_indices,
            "osc",
            true, // is_osc
        )?;
    }

    // ========== Mono Pipeline ==========
    if !mono_branch_indices.is_empty() {
        script.push_str("\n############################################\n");
        script.push_str("# MONO CAMERA PIPELINE\n");
        script.push_str("############################################\n\n");

        generate_pipeline_for_camera_type(
            &mut script,
            config,
            folders,
            data,
            &mono_branch_indices,
            "mono",
            false, // is_osc
        )?;
    }

    script.push_str("close\n");

    let script_path = folders.root.join("02_register_and_stack.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write registration/stacking script to {:?}", script_path))?;

    println!("  Created: {:?}", script_path);
    Ok(script_path)
}

/// Generate merge/register/stack pipeline for a specific camera type
fn generate_pipeline_for_camera_type(
    script: &mut String,
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
    branch_indices: &[usize],
    camera_suffix: &str,
    is_osc: bool,
) -> Result<()> {
    let seq_name = format!("all_lights_{}", camera_suffix);
    let reg_seq_name = format!("r_all_lights_{}_", camera_suffix);

    // Always start by changing to process folder (important after previous pipeline may have changed CWD)
    script.push_str(&format!("cd {}\n\n", folders.process.to_string_lossy()));

    // Calculate total frames for this camera type
    let total_frames: usize = branch_indices.iter()
        .map(|&i| data.branches[i].light_frames.len())
        .sum();

    // Step 1: Merge (if multiple branches)
    if branch_indices.len() == 1 {
        let branch = &data.branches[branch_indices[0]];
        let lights_path = folders.lights_path(branch, branch_indices[0]);

        // Always use pp_lights_ (calibrate always runs, even without calibration files)
        let local_seq_name = "pp_lights_".to_string();
        let local_reg_seq_name = "r_pp_lights_".to_string();

        script.push_str(&format!("# Single {} branch - no merge needed\n", camera_suffix.to_uppercase()));
        script.push_str(&format!("cd {}\n\n", lights_path.to_string_lossy()));

        // Register
        script.push_str(&format!("# Register {} frames\n", camera_suffix.to_uppercase()));
        let register_cmd = build_register_command(config, &local_seq_name);
        script.push_str(&format!("{}\n", register_cmd));
        script.push_str(&format!("seqapplyreg {} -framing=max\n\n", local_seq_name));

        // Stack
        generate_stacking_for_branches(
            script,
            config,
            folders,
            data,
            branch_indices,
            &local_reg_seq_name,
            total_frames,
            camera_suffix,
            is_osc,
        )?;
    } else {
        // Merge multiple branches
        script.push_str(&format!("# Merge {} branches\n", camera_suffix.to_uppercase()));

        let mut all_sequences: Vec<String> = Vec::new();
        let mut frame_offset = 1;
        for &idx in branch_indices {
            let branch = &data.branches[idx];
            let lights_path = folders.lights_path(branch, idx);

            // Always use pp_lights_ (calibrate always runs, even without calibration files)
            let seq_path = format!("\"{}/pp_lights_\"", lights_path.to_string_lossy());
            all_sequences.push(seq_path);

            let frame_count = branch.light_frames.len();
            script.push_str(&format!(
                "#   {} ({} frames, frames {}-{})\n",
                branch.branch_id,
                frame_count,
                frame_offset,
                frame_offset + frame_count - 1
            ));
            frame_offset += frame_count;
        }

        script.push_str(&format!(
            "\nmerge {} {}\n\n",
            all_sequences.join(" "),
            seq_name
        ));

        // Register
        script.push_str(&format!("# Register {} frames\n", camera_suffix.to_uppercase()));
        let register_cmd = build_register_command(config, &seq_name);
        script.push_str(&format!("{}\n", register_cmd));
        script.push_str(&format!("seqapplyreg {} -framing=max\n\n", seq_name));

        // Stack
        generate_stacking_for_branches(
            script,
            config,
            folders,
            data,
            branch_indices,
            &reg_seq_name,
            total_frames,
            camera_suffix,
            is_osc,
        )?;
    }

    Ok(())
}

/// Generate stacking commands for branches of a specific camera type
fn generate_stacking_for_branches(
    script: &mut String,
    config: &ExportConfig,
    folders: &SirilFolders,
    data: &ExportDataV3,
    branch_indices: &[usize],
    registered_sequence_name: &str,
    total_frames: usize,
    camera_suffix: &str,
    is_osc: bool,
) -> Result<()> {
    script.push_str(&format!("# Stack {} frames by filter\n", camera_suffix.to_uppercase()));

    // Group frames by filter for this camera type
    let mut filter_groups: std::collections::HashMap<Option<String>, Vec<usize>> = std::collections::HashMap::new();
    let mut current_frame = 1;

    for &idx in branch_indices {
        let branch = &data.branches[idx];
        let filter = branch.filter.clone();
        let frame_count = branch.light_frames.len();

        for i in 0..frame_count {
            filter_groups.entry(filter.clone()).or_default().push(current_frame + i);
        }
        current_frame += frame_count;
    }

    // Stack each filter group
    for (filter, frame_indices) in &filter_groups {
        let filter_name = filter.as_deref().unwrap_or("Unfiltered");
        let filter_safe = filter
            .as_ref()
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());

        if frame_indices.len() < 2 {
            script.push_str(&format!(
                "# --- {} ({} frame) - SKIPPED (need >= 2 frames) ---\n\n",
                filter_name, frame_indices.len()
            ));
            continue;
        }

        script.push_str(&format!("# --- {} ({} frames) ---\n", filter_name, frame_indices.len()));

        // Unselect all, then select specific frames
        script.push_str(&format!("unselect {} 1 {}\n", registered_sequence_name, total_frames));
        let ranges = indices_to_ranges(frame_indices);
        for (start, end) in &ranges {
            script.push_str(&format!("select {} {} {}\n", registered_sequence_name, start, end));
        }

        // Output filename with camera suffix
        let output_name = format!("{}_{}_stacked", filter_safe, camera_suffix);
        let output_path = folders.masters.join(&output_name);

        // Build stack command
        let mut stack_cmd = format!("stack {}", registered_sequence_name);

        // Rejection algorithm
        let rej_cmd = match config.rejection_algorithm {
            RejectionAlgorithm::Percentile => {
                format!(" rej percentile {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Sigma => {
                format!(" rej sigma {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::LinearFit => {
                format!(" rej linear {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Gesd => {
                format!(" rej gesd {} {}", config.rejection_low, config.rejection_high)
            }
            RejectionAlgorithm::Mad => {
                format!(" rej mad {} {}", config.rejection_low, config.rejection_high)
            }
        };
        stack_cmd.push_str(&rej_cmd);
        stack_cmd.push_str(" -filter-included");
        stack_cmd.push_str(" -norm=addscale -output_norm");

        // Image weighting
        match config.image_weighting {
            ImageWeightingMethod::None => {}
            ImageWeightingMethod::Wfwhm => stack_cmd.push_str(" -weight=wfwhm"),
            ImageWeightingMethod::Stars => stack_cmd.push_str(" -weight=nbstars"),
            ImageWeightingMethod::Noise => stack_cmd.push_str(" -weight=noise"),
            ImageWeightingMethod::ExposureTime => stack_cmd.push_str(" -weight=itime"),
        }

        // RGB compositing for OSC
        if is_osc {
            stack_cmd.push_str(" -rgb_equal");
        }

        stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
        script.push_str(&format!("{}\n\n", stack_cmd));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indices_to_ranges() {
        assert_eq!(indices_to_ranges(&[]), vec![]);
        assert_eq!(indices_to_ranges(&[1, 2, 3, 4, 5]), vec![(1, 5)]);
        assert_eq!(indices_to_ranges(&[1, 2, 3, 51, 52, 53]), vec![(1, 3), (51, 53)]);
        assert_eq!(indices_to_ranges(&[1, 3, 5, 7]), vec![(1, 1), (3, 3), (5, 5), (7, 7)]);
        assert_eq!(indices_to_ranges(&[10]), vec![(10, 10)]);
    }
}
