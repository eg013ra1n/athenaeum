//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for PixInsight WBPP.
//!
//! The folder hierarchy encodes the calibration pipeline — parent folder's
//! frames calibrate child folder's frames. WBPP reads this via "Grouping
//! Keywords with Pre".
//!
//! Full hierarchy (when all calibrations exist):
//! ```text
//! camera_{instrume}/
//!   BIAS_{bias_set_id}/
//!     bias frames...
//!     DARKS_{dark_set_id}/
//!       dark frames + darkflat frames...
//!       FLAT_{flat_set_id}/
//!         flat frames...
//!         lights/
//!           light frames...
//! ```
//!
//! Missing calibration levels are simply omitted (collapsed).

use crate::export::models::{
    sanitize_folder_name, CalibrationSetInfo, CalibrationSubgroup, ExportData, WbppExportConfig,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Result of organizing files for export
#[derive(Debug, Clone)]
pub struct OrganizeResult {
    pub files_organized: i32,
    pub warnings: Vec<String>,
}

/// Organize files for PixInsight WBPP export
///
/// Creates a nested folder structure where parent calibrates child,
/// matching WBPP's "Grouping Keywords with Pre" feature.
pub fn organize_files_wbpp(
    output_dir: &Path,
    data: &ExportData,
    use_symlinks: bool,
    _config: &WbppExportConfig,
) -> Result<OrganizeResult> {
    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_set_ids: HashSet<i64> = HashSet::new();

    for group in &data.groups {
        for subgroup in &group.subgroups {
            let result = organize_subgroup(
                output_dir,
                group.filter.as_deref(),
                subgroup,
                use_symlinks,
                &mut organized_set_ids,
            )?;
            files_organized += result.files_organized;
            warnings.extend(result.warnings);
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
}

/// Organize a single subgroup into the nested hierarchy
fn organize_subgroup(
    output_dir: &Path,
    _group_filter: Option<&str>,
    subgroup: &CalibrationSubgroup,
    use_symlinks: bool,
    organized_set_ids: &mut HashSet<i64>,
) -> Result<OrganizeResult> {
    let mut files_organized = 0;
    let mut warnings = Vec::new();

    // Get camera name from first light frame
    let camera_name = subgroup
        .frames
        .first()
        .and_then(|f| f.instrume.as_ref())
        .map(|s| sanitize_folder_name(s))
        .unwrap_or_else(|| "unknown".to_string());
    let camera_dir = output_dir.join(format!("camera_{}", camera_name));

    // Resolve the effective calibration sets for this subgroup:
    // bias: subgroup.bias, or fallback to subgroup.dark.bias
    // dark: subgroup.dark
    // dark_flat: subgroup.flat.dark_flat (if flat exists)
    // flat: subgroup.flat
    let bias: Option<&CalibrationSetInfo> = subgroup
        .bias
        .as_ref()
        .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
    let dark: Option<&CalibrationSetInfo> = subgroup.dark.as_ref();
    let flat: Option<&CalibrationSetInfo> = subgroup.flat.as_ref();
    let dark_flat: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.dark_flat.as_deref());
    // Also check for flat's own dark (regular dark for flat calibration)
    let flat_dark: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.dark.as_deref());
    // And flat's own bias
    let flat_bias: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.bias.as_deref());

    // Build the nested path from outermost to innermost, tracking the current directory.
    // Each calibration level that exists adds a folder segment and places its frames.
    let mut current_dir = camera_dir.clone();

    // Level 1: BIAS (outermost calibration)
    if let Some(bias_info) = bias {
        let bias_folder = format!("BIAS_{}", bias_info.set_id);
        current_dir = current_dir.join(&bias_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(bias_info.set_id) {
            let r = place_frames(&bias_info.frames, &current_dir, use_symlinks);
            files_organized += r.0;
            warnings.extend(r.1);
        }

        // Also place flat's own bias here if it's a different set
        if let Some(fb) = flat_bias {
            if fb.set_id != bias_info.set_id && organized_set_ids.insert(fb.set_id) {
                let r = place_frames(&fb.frames, &current_dir, use_symlinks);
                files_organized += r.0;
                warnings.extend(r.1);
            }
        }
    } else if let Some(fb) = flat_bias {
        // No light bias, but flat has its own bias — use that as the outermost level
        let bias_folder = format!("BIAS_{}", fb.set_id);
        current_dir = current_dir.join(&bias_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(fb.set_id) {
            let r = place_frames(&fb.frames, &current_dir, use_symlinks);
            files_organized += r.0;
            warnings.extend(r.1);
        }
    }

    // Level 2: DARKS (contains dark frames + darkflat frames)
    if dark.is_some() || dark_flat.is_some() || flat_dark.is_some() {
        // Use the dark set id, or darkflat set id, or flat_dark set id for the folder name
        let darks_set_id = dark
            .map(|d| d.set_id)
            .or_else(|| flat_dark.map(|d| d.set_id))
            .or_else(|| dark_flat.map(|df| df.set_id))
            .unwrap_or(0);
        let darks_folder = format!("DARKS_{}", darks_set_id);
        current_dir = current_dir.join(&darks_folder);
        fs::create_dir_all(&current_dir)?;

        // Place dark frames
        if let Some(dark_info) = dark {
            if organized_set_ids.insert(dark_info.set_id) {
                let r = place_frames(&dark_info.frames, &current_dir, use_symlinks);
                files_organized += r.0;
                warnings.extend(r.1);
            }
            // Dark's own bias (if different from the one already placed at BIAS level)
            if let Some(ref dark_bias) = dark_info.bias {
                if organized_set_ids.insert(dark_bias.set_id) {
                    // If we didn't create a BIAS folder, place bias frames here in darks
                    if bias.is_none() && flat_bias.is_none() {
                        let r = place_frames(&dark_bias.frames, &current_dir, use_symlinks);
                        files_organized += r.0;
                        warnings.extend(r.1);
                    }
                }
            }
        }

        // Place flat's own dark frames (if different from light's dark)
        if let Some(fd) = flat_dark {
            if organized_set_ids.insert(fd.set_id) {
                let r = place_frames(&fd.frames, &current_dir, use_symlinks);
                files_organized += r.0;
                warnings.extend(r.1);
            }
        }

        // Place darkflat frames alongside darks (WBPP matches by IMAGETYP/EXPTIME)
        if let Some(df_info) = dark_flat {
            if organized_set_ids.insert(df_info.set_id) {
                let r = place_frames(&df_info.frames, &current_dir, use_symlinks);
                files_organized += r.0;
                warnings.extend(r.1);
            }
        }
    }

    // Level 3: FLAT (contains flat frames, with lights as child)
    if let Some(flat_info) = flat {
        let flat_folder = format!("FLAT_{}", flat_info.set_id);
        current_dir = current_dir.join(&flat_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(flat_info.set_id) {
            let r = place_frames(&flat_info.frames, &current_dir, use_symlinks);
            files_organized += r.0;
            warnings.extend(r.1);
        }
    }

    // Innermost: lights/
    let lights_dir = current_dir.join("lights");
    fs::create_dir_all(&lights_dir)?;

    for frame in &subgroup.frames {
        let dest = lights_dir.join(&frame.filename);
        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
            Ok(_) => files_organized += 1,
            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
}

/// Place frames from a calibration set into a directory
/// Returns (files_organized, warnings)
fn place_frames(
    frames: &[crate::export::models::ExportFrame],
    dir: &Path,
    use_symlinks: bool,
) -> (i32, Vec<String>) {
    let mut count = 0;
    let mut warnings = Vec::new();

    for frame in frames {
        let dest = dir.join(&frame.filename);
        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
            Ok(_) => count += 1,
            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
        }
    }

    (count, warnings)
}

/// Copy file or create symlink
fn copy_or_link(source: &str, dest: &PathBuf, use_symlinks: bool) -> Result<()> {
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
