//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for processing.
//!
//! V4: Dual target support (Siril + WBPP) with clean folder structures.

use crate::export::models::{
    sanitize_folder_name, ExportConfig, ExportDataV3, ExportResult, ExportTarget,
    GlobalRegistrationPlan,
};
use crate::export::folder_structures::{SirilFolders, WBPPFolders};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

// ============================================================================
// V4 Export: Dual Target Support (Siril + WBPP)
// ============================================================================

/// Organize files based on export target (V4 dispatcher)
pub fn organize_files_v4(config: &ExportConfig, data: &ExportDataV3) -> Result<ExportResult> {
    match config.target {
        ExportTarget::Siril => organize_for_siril(config, data),
        ExportTarget::PixInsightWBPP => organize_for_wbpp(config, data),
    }
}

/// Organize files for Siril export
/// Uses flat structure: biases/, darks/, flats/, lights/, masters/, process/
pub fn organize_for_siril(config: &ExportConfig, data: &ExportDataV3) -> Result<ExportResult> {
    let folders = SirilFolders::new(config.output_dir.clone());
    folders.create_all()?;

    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_sets: HashSet<i64> = HashSet::new();

    // Organize calibration frames from master plan
    for master in &data.master_plan.masters {
        if organized_sets.contains(&master.set_id) {
            continue;
        }

        let set_path = match master.master_type.as_str() {
            "Bias" => folders.bias_set_path(master.set_id),
            "Dark" | "DarkFlat" => folders.dark_set_path(master.set_id),
            "Flat" => {
                // Get filter from source frames
                let filter = master.source_frames.first().and_then(|f| f.filter.as_deref());
                folders.flat_set_path(master.set_id, filter)
            }
            _ => continue,
        };

        fs::create_dir_all(&set_path)?;

        for frame in &master.source_frames {
            let dest = set_path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy {}: {}", frame.filename, e));
            } else {
                files_organized += 1;
            }
        }

        organized_sets.insert(master.set_id);
    }

    // Organize light frames by branch
    for (idx, branch) in data.branches.iter().enumerate() {
        let lights_path = folders.lights_path(branch, idx);
        fs::create_dir_all(&lights_path)?;

        for frame in &branch.light_frames {
            let dest = lights_path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy light {}: {}", frame.filename, e));
            } else {
                files_organized += 1;
            }
        }
    }

    Ok(ExportResult {
        success: true,
        output_dir: config.output_dir.to_string_lossy().to_string(),
        files_organized,
        scripts_generated: Vec::new(),
        warnings,
        error: None,
    })
}

/// Organize files for PixInsight WBPP export
/// Uses grouped structure: {camera}/darks/, {camera}/flats_{id}/lights/
pub fn organize_for_wbpp(config: &ExportConfig, data: &ExportDataV3) -> Result<ExportResult> {
    let folders = WBPPFolders::new(config.output_dir.clone());

    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_darks: HashSet<(String, i64)> = HashSet::new(); // (camera, set_id)

    // Get unique cameras
    let cameras: HashSet<String> = data.branches.iter().map(|b| b.camera_folder_name.clone()).collect();

    // Create base folders for each camera
    for camera in &cameras {
        folders.create_camera_dirs(camera)?;
    }

    // Organize dark-type frames (bias, dark, darkflat) into {camera}/darks/
    for master in &data.master_plan.masters {
        let imagetyp = master.master_type.as_str();
        if !matches!(imagetyp, "Bias" | "Dark" | "DarkFlat") {
            continue;
        }

        // Get camera from source frames
        let camera = master.source_frames.first()
            .and_then(|f| f.instrume.as_ref())
            .map(|s| sanitize_folder_name(s))
            .unwrap_or_else(|| "unknown".to_string());

        let key = (camera.clone(), master.set_id);
        if organized_darks.contains(&key) {
            continue;
        }

        let darks_path = folders.darks_path(&camera);
        fs::create_dir_all(&darks_path)?;

        for frame in &master.source_frames {
            let dest = darks_path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy {}: {}", frame.filename, e));
            } else {
                files_organized += 1;
            }
        }

        organized_darks.insert(key);
    }

    // Organize flats and lights grouped by flat set
    let mut organized_flats: HashSet<(String, i64)> = HashSet::new();

    for branch in &data.branches {
        let camera = &branch.camera_folder_name;
        let flat_id = branch.flat_id;

        // Create flat set folder structure
        folders.create_flat_set_dirs(camera, flat_id)?;

        // Copy flat frames (if not already done for this camera+flat combination)
        let flat_key = (camera.clone(), flat_id);
        if !organized_flats.contains(&flat_key) {
            if let Some(ref flat_info) = branch.flat_info {
                let flats_path = folders.flats_path(camera, flat_id);
                for frame in &flat_info.frames {
                    let dest = flats_path.join(&frame.filename);
                    if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                        warnings.push(format!("Failed to copy flat {}: {}", frame.filename, e));
                    } else {
                        files_organized += 1;
                    }
                }
            }
            organized_flats.insert(flat_key);
        }

        // Copy light frames to {camera}/flats_{id}/lights/
        let lights_path = folders.lights_path(camera, flat_id);
        for frame in &branch.light_frames {
            let dest = lights_path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy light {}: {}", frame.filename, e));
            } else {
                files_organized += 1;
            }
        }
    }

    Ok(ExportResult {
        success: true,
        output_dir: config.output_dir.to_string_lossy().to_string(),
        files_organized,
        scripts_generated: Vec::new(),
        warnings,
        error: None,
    })
}

/// Copy or create symlink based on config
fn copy_or_link(source: &str, dest: &Path, use_symlinks: bool) -> Result<()> {
    // Skip if destination already exists
    if dest.exists() {
        return Ok(());
    }

    if use_symlinks {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source, dest)
                .with_context(|| format!("Failed to symlink {} -> {:?}", source, dest))?;
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(source, dest)
                .with_context(|| format!("Failed to symlink {} -> {:?}", source, dest))?;
        }
    } else {
        fs::copy(source, dest)
            .with_context(|| format!("Failed to copy {} -> {:?}", source, dest))?;
    }

    Ok(())
}

// ============================================================================
// Post-Registration File Organization
// ============================================================================

/// Result of organizing registered files
#[derive(Debug, Clone)]
pub struct RegisteredFilesResult {
    pub files_organized: usize,
    pub filters_created: Vec<String>,
    pub warnings: Vec<String>,
}

/// Organize globally registered files into filter-specific folders
///
/// After script 02 (global registration) runs, this function:
/// 1. Reads the registered files from `process/r_all_lights_*.fit`
/// 2. Uses the GlobalRegistrationPlan to map frame numbers to filters
/// 3. Creates filter-specific folders in `process/registered/{filter}/`
/// 4. Symlinks (or copies) registered files into appropriate filter folders
///
/// The registered files follow Siril's naming convention: r_all_lights_XXXXX.fit
/// where XXXXX is the 5-digit frame number (1-based).
pub fn organize_registered_files(
    output_dir: &Path,
    plan: &GlobalRegistrationPlan,
    use_symlinks: bool,
) -> Result<RegisteredFilesResult> {
    let process_dir = output_dir.join("process");
    let registered_base = process_dir.join("registered");

    let mut files_organized = 0;
    let mut filters_created = Vec::new();
    let mut warnings = Vec::new();

    // Create the registered base folder
    fs::create_dir_all(&registered_base)
        .context("Failed to create registered folder")?;

    // Create filter-specific folders
    for filter in &plan.filters {
        let filter_safe = filter
            .as_ref()
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());

        let filter_dir = registered_base.join(&filter_safe);
        fs::create_dir_all(&filter_dir)?;
        filters_created.push(filter_safe);
    }

    // Organize registered files by filter
    for merge_info in &plan.merge_order {
        let filter_safe = merge_info.filter
            .as_ref()
            .map(|f| sanitize_folder_name(f))
            .unwrap_or_else(|| "unfiltered".to_string());

        let filter_dir = registered_base.join(&filter_safe);

        // Process each frame in this branch's range
        for frame_num in merge_info.start_frame..=merge_info.end_frame {
            // Siril names registered files as r_{sequence}_{frame:05}.fit
            let registered_filename = format!("r_all_lights_{:05}.fit", frame_num);
            let source_path = process_dir.join(&registered_filename);

            // Destination uses "registered_" prefix for convert command
            let dest_filename = format!("registered_{:05}.fit", frame_num);
            let dest_path = filter_dir.join(&dest_filename);

            if source_path.exists() {
                let source_str = source_path.to_string_lossy();
                if let Err(e) = copy_or_link(&source_str, &dest_path, use_symlinks) {
                    warnings.push(format!(
                        "Failed to organize {}: {}",
                        registered_filename, e
                    ));
                } else {
                    files_organized += 1;
                }
            } else {
                warnings.push(format!(
                    "Expected registered file not found: {}",
                    registered_filename
                ));
            }
        }
    }

    // Deduplicate filter list
    filters_created.sort();
    filters_created.dedup();

    Ok(RegisteredFilesResult {
        files_organized,
        filters_created,
        warnings,
    })
}

/// Check if registered files exist in the process folder
///
/// Returns the count of r_all_lights_*.fit files found
pub fn count_registered_files(output_dir: &Path) -> usize {
    let process_dir = output_dir.join("process");

    match fs::read_dir(&process_dir) {
        Ok(entries) => {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    name_str.starts_with("r_all_lights_") && name_str.ends_with(".fit")
                })
                .count()
        }
        Err(_) => 0,
    }
}
