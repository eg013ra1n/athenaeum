//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for processing.
//!
//! V2: Organizes calibration files per set ID so each master can be created
//! from its own set of files without mixing with other sets.

use crate::export::models::{CalibrationSetInfo, ExportConfig, ExportData, ExportResult};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Folder structure for organized export
pub struct ExportFolders {
    pub root: PathBuf,
    pub lights: PathBuf,
    #[allow(dead_code)]
    pub calibration: PathBuf,
    pub darks: PathBuf,
    pub flats: PathBuf,
    pub bias: PathBuf,
    pub dark_flats: PathBuf,
    pub masters: PathBuf,
    pub process: PathBuf,
    pub result: PathBuf,
}

impl ExportFolders {
    /// Create folder structure from a root path
    pub fn new(root: PathBuf) -> Self {
        let calibration = root.join("Calibration");
        Self {
            lights: root.join("Lights"),
            darks: calibration.join("Darks"),
            flats: calibration.join("Flats"),
            bias: calibration.join("Bias"),
            dark_flats: calibration.join("DarkFlats"),
            masters: root.join("masters"),
            process: root.join("process"),
            result: root.join("result"),
            calibration,
            root,
        }
    }

    /// Create all directories
    pub fn create_all(&self) -> Result<()> {
        fs::create_dir_all(&self.lights).context("Failed to create Lights folder")?;
        fs::create_dir_all(&self.darks).context("Failed to create Darks folder")?;
        fs::create_dir_all(&self.flats).context("Failed to create Flats folder")?;
        fs::create_dir_all(&self.bias).context("Failed to create Bias folder")?;
        fs::create_dir_all(&self.dark_flats).context("Failed to create DarkFlats folder")?;
        fs::create_dir_all(&self.masters).context("Failed to create masters folder")?;
        fs::create_dir_all(&self.process).context("Failed to create process folder")?;
        fs::create_dir_all(&self.result).context("Failed to create result folder")?;
        Ok(())
    }

    /// Get folder for a specific filter (creates subfolder under Lights)
    pub fn lights_for_filter(&self, filter: Option<&str>) -> PathBuf {
        match filter {
            Some(f) => self.lights.join(f),
            None => self.lights.clone(),
        }
    }

    /// Get folder for flats of a specific filter
    pub fn flats_for_filter(&self, filter: Option<&str>) -> PathBuf {
        match filter {
            Some(f) => self.flats.join(f),
            None => self.flats.clone(),
        }
    }

    /// Get folder for a specific calibration set by ID
    /// V2: Organizes calibration frames per set for proper Siril processing
    pub fn calibration_set_folder(&self, imagetyp: &str, set_id: i64) -> PathBuf {
        let base = match imagetyp {
            "Bias" => &self.bias,
            "Dark" => &self.darks,
            "DarkFlat" => &self.dark_flats,
            "Flat" => &self.flats,
            _ => &self.calibration,
        };
        base.join(format!("set_{}", set_id))
    }
}

/// Organize files into export folder structure
/// V2: Uses ExportGroups and MasterCreationPlan for proper per-set organization
pub fn organize_files(config: &ExportConfig, data: &ExportData) -> Result<ExportResult> {
    let folders = ExportFolders::new(config.output_dir.clone());
    folders.create_all()?;

    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_sets: HashSet<i64> = HashSet::new();

    // Organize light frames by group (filter + camera type) and subgroup
    for group in &data.groups {
        let filter_folder = folders.lights_for_filter(group.filter.as_deref());
        fs::create_dir_all(&filter_folder)?;

        // If only one subgroup, put lights directly in filter folder
        // If multiple subgroups, organize by subgroup key for separate calibration
        let has_multiple_subgroups = group.subgroups.len() > 1;

        for (subgroup_idx, subgroup) in group.subgroups.iter().enumerate() {
            let light_dest_folder = if has_multiple_subgroups {
                // Multiple subgroups: organize by subgroup for separate calibration
                filter_folder.join(format!("subgroup_{}", subgroup_idx + 1))
            } else {
                // Single subgroup: put directly in filter folder
                filter_folder.clone()
            };
            fs::create_dir_all(&light_dest_folder)?;

            for frame in &subgroup.frames {
                let dest = light_dest_folder.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                    warnings.push(format!("Failed to copy light {}: {}", frame.filename, e));
                } else {
                    files_organized += 1;
                }
            }

            // Organize calibration sets per set ID (not mixed together)
            if let Some(ref flat) = subgroup.flat {
                files_organized +=
                    organize_calibration_set(&folders, flat, config.use_symlinks, &mut warnings, &mut organized_sets)?;
            }
            if let Some(ref dark) = subgroup.dark {
                files_organized +=
                    organize_calibration_set(&folders, dark, config.use_symlinks, &mut warnings, &mut organized_sets)?;
            }
            if let Some(ref bias) = subgroup.bias {
                files_organized +=
                    organize_calibration_set(&folders, bias, config.use_symlinks, &mut warnings, &mut organized_sets)?;
            }
        }
    }

    // Also organize calibration sets from master_plan that might not be in subgroups
    for master in &data.master_plan.masters {
        if organized_sets.contains(&master.set_id) {
            continue;
        }

        let set_folder = folders.calibration_set_folder(&master.master_type, master.set_id);
        fs::create_dir_all(&set_folder)?;

        for frame in &master.source_frames {
            let dest = set_folder.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy {} {}: {}", master.master_type, frame.filename, e));
            } else {
                files_organized += 1;
            }
        }
        organized_sets.insert(master.set_id);
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

/// Recursively organize a calibration set and its sub-calibrations
fn organize_calibration_set(
    folders: &ExportFolders,
    cal_set: &CalibrationSetInfo,
    use_symlinks: bool,
    warnings: &mut Vec<String>,
    organized_sets: &mut HashSet<i64>,
) -> Result<i32> {
    // Skip if already organized
    if organized_sets.contains(&cal_set.set_id) {
        return Ok(0);
    }

    let mut count = 0;
    let set_folder = folders.calibration_set_folder(&cal_set.imagetyp, cal_set.set_id);
    fs::create_dir_all(&set_folder)?;

    for frame in &cal_set.frames {
        let dest = set_folder.join(&frame.filename);
        if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
            warnings.push(format!("Failed to copy {} {}: {}", cal_set.imagetyp, frame.filename, e));
        } else {
            count += 1;
        }
    }

    organized_sets.insert(cal_set.set_id);

    // Recursively organize sub-calibrations
    if let Some(ref dark_flat) = cal_set.dark_flat {
        count += organize_calibration_set(folders, dark_flat, use_symlinks, warnings, organized_sets)?;
    }
    if let Some(ref dark) = cal_set.dark {
        count += organize_calibration_set(folders, dark, use_symlinks, warnings, organized_sets)?;
    }
    if let Some(ref bias) = cal_set.bias {
        count += organize_calibration_set(folders, bias, use_symlinks, warnings, organized_sets)?;
    }

    Ok(count)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_folders_structure() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export_test"));

        assert_eq!(folders.lights, PathBuf::from("/tmp/export_test/Lights"));
        assert_eq!(
            folders.darks,
            PathBuf::from("/tmp/export_test/Calibration/Darks")
        );
        assert_eq!(
            folders.flats,
            PathBuf::from("/tmp/export_test/Calibration/Flats")
        );
        assert_eq!(folders.masters, PathBuf::from("/tmp/export_test/masters"));
    }

    #[test]
    fn test_filter_folders() {
        let folders = ExportFolders::new(PathBuf::from("/tmp/export_test"));

        assert_eq!(
            folders.lights_for_filter(Some("Ha")),
            PathBuf::from("/tmp/export_test/Lights/Ha")
        );
        assert_eq!(
            folders.lights_for_filter(None),
            PathBuf::from("/tmp/export_test/Lights")
        );
        assert_eq!(
            folders.flats_for_filter(Some("OIII")),
            PathBuf::from("/tmp/export_test/Calibration/Flats/OIII")
        );
    }
}
