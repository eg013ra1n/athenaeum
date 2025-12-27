//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for processing.

use crate::export::models::{ExportConfig, ExportData, ExportResult};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Folder structure for organized export
pub struct ExportFolders {
    pub root: PathBuf,
    pub lights: PathBuf,
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
}

/// Organize files into export folder structure
pub fn organize_files(config: &ExportConfig, data: &ExportData) -> Result<ExportResult> {
    let folders = ExportFolders::new(config.output_dir.clone());
    folders.create_all()?;

    let mut files_organized = 0;
    let mut warnings = Vec::new();

    // Organize light frames by filter
    for filter_group in &data.filters {
        let filter_folder = folders.lights_for_filter(filter_group.filter.as_deref());
        fs::create_dir_all(&filter_folder)?;

        for frame in &filter_group.light_frames {
            let dest = filter_folder.join(&frame.filename);
            copy_or_link(&frame.file_path, &dest, config.use_symlinks)?;
            files_organized += 1;
        }

        // Organize flats for this filter
        let flat_folder = folders.flats_for_filter(filter_group.filter.as_deref());
        for flat_set in &filter_group.flat_sets {
            fs::create_dir_all(&flat_folder)?;
            for frame in &flat_set.frames {
                let dest = flat_folder.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                    warnings.push(format!("Failed to copy flat {}: {}", frame.filename, e));
                } else {
                    files_organized += 1;
                }
            }

            // Organize sub-calibrations (dark flats, darks, bias for flats)
            for sub_cal in &flat_set.sub_calibrations {
                let sub_folder = match sub_cal.imagetyp.as_str() {
                    "DARKFLAT" => &folders.dark_flats,
                    "DARK" => &folders.darks,
                    "BIAS" => &folders.bias,
                    _ => continue,
                };
                for frame in &sub_cal.frames {
                    let dest = sub_folder.join(&frame.filename);
                    if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                        warnings.push(format!("Failed to copy {}: {}", frame.filename, e));
                    } else {
                        files_organized += 1;
                    }
                }
            }
        }

        // Organize darks
        for dark_set in &filter_group.dark_sets {
            for frame in &dark_set.frames {
                let dest = folders.darks.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                    warnings.push(format!("Failed to copy dark {}: {}", frame.filename, e));
                } else {
                    files_organized += 1;
                }
            }

            // Bias for darks
            for sub_cal in &dark_set.sub_calibrations {
                if sub_cal.imagetyp == "BIAS" {
                    for frame in &sub_cal.frames {
                        let dest = folders.bias.join(&frame.filename);
                        if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                            warnings.push(format!("Failed to copy bias {}: {}", frame.filename, e));
                        } else {
                            files_organized += 1;
                        }
                    }
                }
            }
        }

        // Organize standalone bias
        for bias_set in &filter_group.bias_sets {
            for frame in &bias_set.frames {
                let dest = folders.bias.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                    warnings.push(format!("Failed to copy bias {}: {}", frame.filename, e));
                } else {
                    files_organized += 1;
                }
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
