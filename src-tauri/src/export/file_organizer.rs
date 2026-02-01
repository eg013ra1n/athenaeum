//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for PixInsight WBPP.

use crate::export::models::{sanitize_folder_name, ExportData};
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
/// Creates folder structure:
/// ```
/// output/
/// └── {camera}/
///     ├── darks/
///     │   └── (bias, dark, darkflat files)
///     └── flats_{filter}/
///         ├── (flat files)
///         └── lights/
///             └── (light frames)
/// ```
pub fn organize_files_wbpp(
    output_dir: &Path,
    data: &ExportData,
    use_symlinks: bool,
) -> Result<OrganizeResult> {
    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_calibrations: HashSet<i64> = HashSet::new();

    // Process each export group
    for group in &data.groups {
        for subgroup in &group.subgroups {
            // Get camera name from first frame
            let camera_name = subgroup
                .frames
                .first()
                .and_then(|f| f.instrume.as_ref())
                .map(|s| sanitize_folder_name(s))
                .unwrap_or_else(|| "unknown".to_string());
            let camera_folder = format!("camera_{}", camera_name);

            let camera_dir = output_dir.join(&camera_folder);
            let darks_dir = camera_dir.join("darks");

            // Create camera directories
            fs::create_dir_all(&darks_dir)?;

            // Process Flat calibration
            if let Some(ref flat) = subgroup.flat {
                if !organized_calibrations.contains(&flat.set_id) {
                    let filter = flat
                        .frames
                        .first()
                        .and_then(|f| f.filter.as_ref())
                        .map(|s| sanitize_folder_name(s))
                        .unwrap_or_else(|| "no_filter".to_string());

                    let flat_dir = camera_dir.join(format!("flats_{}", filter));
                    fs::create_dir_all(&flat_dir)?;

                    // Copy flat frames
                    for frame in &flat.frames {
                        let dest = flat_dir.join(&frame.filename);
                        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                            Ok(_) => files_organized += 1,
                            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                        }
                    }

                    // Process flat's sub-calibrations (DarkFlat, Dark, Bias) -> go to darks folder
                    if let Some(ref dark_flat) = flat.dark_flat {
                        if !organized_calibrations.contains(&dark_flat.set_id) {
                            for frame in &dark_flat.frames {
                                let dest = darks_dir.join(&frame.filename);
                                match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                                    Ok(_) => files_organized += 1,
                                    Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                                }
                            }
                            organized_calibrations.insert(dark_flat.set_id);
                        }
                    }

                    if let Some(ref dark) = flat.dark {
                        if !organized_calibrations.contains(&dark.set_id) {
                            for frame in &dark.frames {
                                let dest = darks_dir.join(&frame.filename);
                                match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                                    Ok(_) => files_organized += 1,
                                    Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                                }
                            }
                            organized_calibrations.insert(dark.set_id);

                            // Dark's bias
                            if let Some(ref bias) = dark.bias {
                                if !organized_calibrations.contains(&bias.set_id) {
                                    for frame in &bias.frames {
                                        let dest = darks_dir.join(&frame.filename);
                                        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                                            Ok(_) => files_organized += 1,
                                            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                                        }
                                    }
                                    organized_calibrations.insert(bias.set_id);
                                }
                            }
                        }
                    }

                    if let Some(ref bias) = flat.bias {
                        if !organized_calibrations.contains(&bias.set_id) {
                            for frame in &bias.frames {
                                let dest = darks_dir.join(&frame.filename);
                                match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                                    Ok(_) => files_organized += 1,
                                    Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                                }
                            }
                            organized_calibrations.insert(bias.set_id);
                        }
                    }

                    // Copy lights into flats_{filter}/lights/
                    let lights_dir = flat_dir.join("lights");
                    fs::create_dir_all(&lights_dir)?;

                    for frame in &subgroup.frames {
                        let dest = lights_dir.join(&frame.filename);
                        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                            Ok(_) => files_organized += 1,
                            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                        }
                    }

                    organized_calibrations.insert(flat.set_id);
                }
            }

            // Process Dark calibration (direct for lights)
            if let Some(ref dark) = subgroup.dark {
                if !organized_calibrations.contains(&dark.set_id) {
                    for frame in &dark.frames {
                        let dest = darks_dir.join(&frame.filename);
                        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                            Ok(_) => files_organized += 1,
                            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                        }
                    }

                    // Dark's bias
                    if let Some(ref bias) = dark.bias {
                        if !organized_calibrations.contains(&bias.set_id) {
                            for frame in &bias.frames {
                                let dest = darks_dir.join(&frame.filename);
                                match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                                    Ok(_) => files_organized += 1,
                                    Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                                }
                            }
                            organized_calibrations.insert(bias.set_id);
                        }
                    }

                    organized_calibrations.insert(dark.set_id);
                }
            }

            // Process Bias calibration (direct for lights)
            if let Some(ref bias) = subgroup.bias {
                if !organized_calibrations.contains(&bias.set_id) {
                    for frame in &bias.frames {
                        let dest = darks_dir.join(&frame.filename);
                        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                            Ok(_) => files_organized += 1,
                            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                        }
                    }
                    organized_calibrations.insert(bias.set_id);
                }
            }

            // If no flat was found, still need to copy lights somewhere
            if subgroup.flat.is_none() {
                let filter = group
                    .filter
                    .as_ref()
                    .map(|s| sanitize_folder_name(s))
                    .unwrap_or_else(|| "no_filter".to_string());

                let lights_dir = camera_dir.join(format!("lights_{}", filter));
                fs::create_dir_all(&lights_dir)?;

                for frame in &subgroup.frames {
                    let dest = lights_dir.join(&frame.filename);
                    match copy_or_link(&frame.file_path, &dest, use_symlinks) {
                        Ok(_) => files_organized += 1,
                        Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
                    }
                }
            }
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
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
