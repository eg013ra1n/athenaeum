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
    sanitize_display_folder_name, sanitize_folder_name, CalibrationSetInfo, CalibrationSubgroup,
    ExportData, ExportProgressEvent, WbppExportConfig,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::events::{ProgressEmitter, emit_event};

/// Result of organizing files for export
#[derive(Debug, Clone)]
pub struct OrganizeResult {
    pub files_organized: i32,
    pub warnings: Vec<String>,
}

/// Count total files that will be organized (for progress tracking).
/// Mirrors the logic of `organize_subgroup` to count without copying.
fn count_total_files(data: &ExportData) -> usize {
    let mut total = 0usize;
    let mut counted_set_ids: HashSet<i64> = HashSet::new();

    for group in &data.groups {
        for subgroup in &group.subgroups {
            // Count calibration frames (same dedup logic as organize_subgroup)
            let bias = subgroup
                .bias
                .as_ref()
                .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
            let dark = subgroup.dark.as_ref();
            let flat = subgroup.flat.as_ref();
            let dark_flat = flat.and_then(|f| f.dark_flat.as_deref());
            let flat_dark = flat.and_then(|f| f.dark.as_deref());
            let flat_bias = flat.and_then(|f| f.bias.as_deref());

            // Bias frames
            if let Some(b) = bias {
                if counted_set_ids.insert(b.set_id) {
                    total += b.frames.len();
                }
            }
            if let Some(fb) = flat_bias {
                if counted_set_ids.insert(fb.set_id) {
                    total += fb.frames.len();
                }
            }
            // Dark frames
            if let Some(d) = dark {
                if counted_set_ids.insert(d.set_id) {
                    total += d.frames.len();
                }
                if let Some(ref db) = d.bias {
                    if counted_set_ids.insert(db.set_id) {
                        if bias.is_none() && flat_bias.is_none() {
                            total += db.frames.len();
                        }
                    }
                }
            }
            if let Some(fd) = flat_dark {
                if counted_set_ids.insert(fd.set_id) {
                    total += fd.frames.len();
                }
            }
            if let Some(df) = dark_flat {
                if counted_set_ids.insert(df.set_id) {
                    total += df.frames.len();
                }
            }
            // Flat frames
            if let Some(f) = flat {
                if counted_set_ids.insert(f.set_id) {
                    total += f.frames.len();
                }
            }
            // Light frames
            total += subgroup.frames.len();
        }
    }
    total
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
    emitter: Option<&dyn ProgressEmitter>,
    frame_set_id: i64,
    cancel_flag: &std::sync::atomic::AtomicBool,
) -> Result<OrganizeResult> {
    let span = tracing::info_span!("export", frame_set_id);
    let _g = span.enter();

    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_set_ids: HashSet<i64> = HashSet::new();

    // Create parent directory named after the frame set
    let object_dir = output_dir.join(sanitize_display_folder_name(&data.frame_set_name));
    let output_dir = object_dir.as_path();

    let total_files = count_total_files(data);
    let mut last_emit = Instant::now();

    // Helper closure to emit progress (throttled to every 100ms)
    let mut emit_progress = |current: usize, filename: Option<&str>| {
        let now = Instant::now();
        if now.duration_since(last_emit).as_millis() >= 100 || current == total_files {
            if let Some(e) = emitter {
                let percent = if total_files > 0 {
                    (current as f64 / total_files as f64) * 100.0
                } else {
                    0.0
                };
                emit_event(
                    e,
                    "export-progress",
                    &ExportProgressEvent {
                        frame_set_id,
                        current,
                        total: total_files,
                        percent,
                        current_file: filename.map(|s| s.to_string()),
                        phase: "copying".to_string(),
                    },
                );
            }
            last_emit = now;
        }
    };

    for group in &data.groups {
        for subgroup in &group.subgroups {
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let result = organize_subgroup(
                output_dir,
                group.filter.as_deref(),
                subgroup,
                use_symlinks,
                &mut organized_set_ids,
                cancel_flag,
                &mut |count, filename| {
                    files_organized += count;
                    emit_progress(files_organized as usize, filename);
                },
            )?;
            warnings.extend(result.warnings);
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
}

/// Organize a single subgroup into the nested hierarchy.
/// The `on_file` callback is called with (1, filename) after each successful file copy.
fn organize_subgroup(
    output_dir: &Path,
    _group_filter: Option<&str>,
    subgroup: &CalibrationSubgroup,
    use_symlinks: bool,
    organized_set_ids: &mut HashSet<i64>,
    cancel_flag: &std::sync::atomic::AtomicBool,
    on_file: &mut dyn FnMut(i32, Option<&str>),
) -> Result<OrganizeResult> {
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
    let bias: Option<&CalibrationSetInfo> = subgroup
        .bias
        .as_ref()
        .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
    let dark: Option<&CalibrationSetInfo> = subgroup.dark.as_ref();
    let flat: Option<&CalibrationSetInfo> = subgroup.flat.as_ref();
    let dark_flat: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.dark_flat.as_deref());
    let flat_dark: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.dark.as_deref());
    let flat_bias: Option<&CalibrationSetInfo> =
        flat.and_then(|f| f.bias.as_deref());

    let mut current_dir = camera_dir.clone();

    // Level 1: BIAS (outermost calibration)
    if let Some(bias_info) = bias {
        let bias_folder = format!("BIAS_{}", bias_info.set_id);
        current_dir = current_dir.join(&bias_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(bias_info.set_id) {
            warnings.extend(place_frames(&bias_info.frames, &current_dir, use_symlinks, cancel_flag, on_file));
        }

        if let Some(fb) = flat_bias {
            if fb.set_id != bias_info.set_id && organized_set_ids.insert(fb.set_id) {
                warnings.extend(place_frames(&fb.frames, &current_dir, use_symlinks, cancel_flag, on_file));
            }
        }
    } else if let Some(fb) = flat_bias {
        let bias_folder = format!("BIAS_{}", fb.set_id);
        current_dir = current_dir.join(&bias_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(fb.set_id) {
            warnings.extend(place_frames(&fb.frames, &current_dir, use_symlinks, cancel_flag, on_file));
        }
    }

    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(OrganizeResult { files_organized: 0, warnings });
    }

    // Level 2: DARKS
    if dark.is_some() || dark_flat.is_some() || flat_dark.is_some() {
        let darks_set_id = dark
            .map(|d| d.set_id)
            .or_else(|| flat_dark.map(|d| d.set_id))
            .or_else(|| dark_flat.map(|df| df.set_id))
            .unwrap_or(0);
        let darks_folder = format!("DARKS_{}", darks_set_id);
        current_dir = current_dir.join(&darks_folder);
        fs::create_dir_all(&current_dir)?;

        if let Some(dark_info) = dark {
            if organized_set_ids.insert(dark_info.set_id) {
                warnings.extend(place_frames(&dark_info.frames, &current_dir, use_symlinks, cancel_flag, on_file));
            }
            if let Some(ref dark_bias) = dark_info.bias {
                if organized_set_ids.insert(dark_bias.set_id) {
                    if bias.is_none() && flat_bias.is_none() {
                        warnings.extend(place_frames(&dark_bias.frames, &current_dir, use_symlinks, cancel_flag, on_file));
                    }
                }
            }
        }

        if let Some(fd) = flat_dark {
            if organized_set_ids.insert(fd.set_id) {
                warnings.extend(place_frames(&fd.frames, &current_dir, use_symlinks, cancel_flag, on_file));
            }
        }

        if let Some(df_info) = dark_flat {
            if organized_set_ids.insert(df_info.set_id) {
                warnings.extend(place_frames(&df_info.frames, &current_dir, use_symlinks, cancel_flag, on_file));
            }
        }
    }

    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(OrganizeResult { files_organized: 0, warnings });
    }

    // Level 3: FLAT
    if let Some(flat_info) = flat {
        let flat_folder = format!("FLAT_{}", flat_info.set_id);
        current_dir = current_dir.join(&flat_folder);
        fs::create_dir_all(&current_dir)?;

        if organized_set_ids.insert(flat_info.set_id) {
            warnings.extend(place_frames(&flat_info.frames, &current_dir, use_symlinks, cancel_flag, on_file));
        }
    }

    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(OrganizeResult { files_organized: 0, warnings });
    }

    // Innermost: lights/
    let lights_dir = current_dir.join("lights");
    fs::create_dir_all(&lights_dir)?;

    for frame in &subgroup.frames {
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let dest = lights_dir.join(&frame.filename);
        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
            Ok(_) => on_file(1, Some(&frame.filename)),
            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
        }
    }

    Ok(OrganizeResult {
        files_organized: 0, // tracked via on_file callback
        warnings,
    })
}

/// Place frames from a calibration set into a directory, calling on_file per frame.
/// Returns warnings only (count is tracked via on_file).
fn place_frames(
    frames: &[crate::export::models::ExportFrame],
    dir: &Path,
    use_symlinks: bool,
    cancel_flag: &std::sync::atomic::AtomicBool,
    on_file: &mut dyn FnMut(i32, Option<&str>),
) -> Vec<String> {
    let mut warnings = Vec::new();

    for frame in frames {
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let dest = dir.join(&frame.filename);
        match copy_or_link(&frame.file_path, &dest, use_symlinks) {
            Ok(_) => on_file(1, Some(&frame.filename)),
            Err(e) => warnings.push(format!("Failed to copy {}: {}", frame.filename, e)),
        }
    }

    warnings
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
