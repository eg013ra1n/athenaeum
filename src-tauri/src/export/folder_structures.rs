//! Folder structure implementations for different export targets
//!
//! This module provides folder structure strategies for:
//! - Siril: Flat structure with type-based organization
//! - PixInsight WBPP: Camera-grouped structure with flats containing lights

use std::path::{Path, PathBuf};
use anyhow::Result;

use super::models::{CalibrationBranch, ExportDataV3, ExportTarget, sanitize_folder_name};

// ============================================================================
// Siril Folder Structure
// ============================================================================

/// Folder paths for Siril export structure
/// Flat organization: biases/, darks/, flats/, lights/, masters/, process/
#[derive(Debug, Clone)]
pub struct SirilFolders {
    /// Root export directory
    pub root: PathBuf,
    /// Bias frames directory: biases/
    pub biases: PathBuf,
    /// Dark frames directory: darks/
    pub darks: PathBuf,
    /// Flat frames directory: flats/
    pub flats: PathBuf,
    /// Light frames directory: lights/
    pub lights: PathBuf,
    /// Master calibration output: masters/
    pub masters: PathBuf,
    /// Siril working directory: process/
    pub process: PathBuf,
}

impl SirilFolders {
    /// Create new Siril folder structure from root path
    pub fn new(root: PathBuf) -> Self {
        Self {
            biases: root.join("biases"),
            darks: root.join("darks"),
            flats: root.join("flats"),
            lights: root.join("lights"),
            masters: root.join("masters"),
            process: root.join("process"),
            root,
        }
    }

    /// Get path for a specific bias set: biases/set_{id}/
    pub fn bias_set_path(&self, set_id: i64) -> PathBuf {
        self.biases.join(format!("set_{}", set_id))
    }

    /// Get path for a specific dark set: darks/set_{id}/
    pub fn dark_set_path(&self, set_id: i64) -> PathBuf {
        self.darks.join(format!("set_{}", set_id))
    }

    /// Get path for a specific flat set: flats/set_{id}_{filter}/
    pub fn flat_set_path(&self, set_id: i64, filter: Option<&str>) -> PathBuf {
        let filter_suffix = filter
            .map(|f| format!("_{}", sanitize_folder_name(f)))
            .unwrap_or_default();
        self.flats.join(format!("set_{}{}", set_id, filter_suffix))
    }

    /// Get path for a specific light branch: lights/branch_{idx}_{filter}/
    pub fn lights_branch_path(&self, branch_idx: usize, filter: Option<&str>) -> PathBuf {
        let filter_suffix = filter
            .map(|f| format!("_{}", sanitize_folder_name(f)))
            .unwrap_or_else(|| "_nofilter".to_string());
        self.lights.join(format!("branch_{:02}{}", branch_idx + 1, filter_suffix))
    }

    /// Get path for lights using CalibrationBranch
    pub fn lights_path(&self, branch: &CalibrationBranch, branch_idx: usize) -> PathBuf {
        self.lights_branch_path(branch_idx, branch.filter.as_deref())
    }

    /// Create all directories
    pub fn create_all(&self) -> Result<()> {
        std::fs::create_dir_all(&self.biases)?;
        std::fs::create_dir_all(&self.darks)?;
        std::fs::create_dir_all(&self.flats)?;
        std::fs::create_dir_all(&self.lights)?;
        std::fs::create_dir_all(&self.masters)?;
        std::fs::create_dir_all(&self.process)?;
        Ok(())
    }
}

// ============================================================================
// PixInsight WBPP Folder Structure
// ============================================================================

/// Folder paths for PixInsight WBPP export structure
/// Grouped organization: {camera}/darks/, {camera}/flats_{id}/lights/
#[derive(Debug, Clone)]
pub struct WBPPFolders {
    /// Root export directory
    pub root: PathBuf,
}

impl WBPPFolders {
    /// Create new WBPP folder structure from root path
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Get path for camera root: camera {camera_name}/
    /// Note: WBPP requires space separator, not underscore
    pub fn camera_path(&self, camera_name: &str) -> PathBuf {
        self.root.join(format!("camera {}", sanitize_folder_name(camera_name)))
    }

    /// Get path for darks folder (contains all dark-type frames): {camera}/darks/
    /// This folder will contain bias, darks, and darkflats - WBPP auto-detects via IMAGETYP
    pub fn darks_path(&self, camera_name: &str) -> PathBuf {
        self.camera_path(camera_name).join("darks")
    }

    /// Get path for a flat set folder: {camera}/flats_{id}/
    pub fn flats_path(&self, camera_name: &str, flat_set_id: i64) -> PathBuf {
        self.camera_path(camera_name).join(format!("flats_{}", flat_set_id))
    }

    /// Get path for lights within a flat set: {camera}/flats_{id}/lights/
    pub fn lights_path(&self, camera_name: &str, flat_set_id: i64) -> PathBuf {
        self.flats_path(camera_name, flat_set_id).join("lights")
    }

    /// Get path for lights using CalibrationBranch
    pub fn lights_path_for_branch(&self, branch: &CalibrationBranch) -> PathBuf {
        self.lights_path(&branch.camera_folder_name, branch.flat_id)
    }

    /// Create base directories for a camera
    pub fn create_camera_dirs(&self, camera_name: &str) -> Result<()> {
        std::fs::create_dir_all(self.darks_path(camera_name))?;
        Ok(())
    }

    /// Create flat set directory with lights subfolder
    pub fn create_flat_set_dirs(&self, camera_name: &str, flat_set_id: i64) -> Result<()> {
        std::fs::create_dir_all(self.flats_path(camera_name, flat_set_id))?;
        std::fs::create_dir_all(self.lights_path(camera_name, flat_set_id))?;
        Ok(())
    }
}

// ============================================================================
// Folder Structure Factory
// ============================================================================

/// Enum to hold either folder structure type
#[derive(Debug, Clone)]
pub enum ExportFolders {
    Siril(SirilFolders),
    WBPP(WBPPFolders),
}

impl ExportFolders {
    /// Create appropriate folder structure based on export target
    pub fn new(root: PathBuf, target: &ExportTarget) -> Self {
        match target {
            ExportTarget::Siril => ExportFolders::Siril(SirilFolders::new(root)),
            ExportTarget::PixInsightWBPP => ExportFolders::WBPP(WBPPFolders::new(root)),
        }
    }

    /// Get root path
    pub fn root(&self) -> &Path {
        match self {
            ExportFolders::Siril(f) => &f.root,
            ExportFolders::WBPP(f) => &f.root,
        }
    }

    /// Get masters path (only for Siril, returns root for WBPP)
    pub fn masters_path(&self) -> PathBuf {
        match self {
            ExportFolders::Siril(f) => f.masters.clone(),
            ExportFolders::WBPP(f) => f.root.clone(),
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get unique camera names from export data (sanitized for folder names)
pub fn get_unique_cameras(data: &ExportDataV3) -> Vec<String> {
    let mut cameras: Vec<String> = data.branches
        .iter()
        .map(|b| b.camera_folder_name.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    cameras.sort();
    cameras
}

/// Get unique flat set IDs per camera
pub fn get_flat_sets_by_camera(data: &ExportDataV3) -> std::collections::HashMap<String, Vec<i64>> {
    let mut result: std::collections::HashMap<String, std::collections::HashSet<i64>> =
        std::collections::HashMap::new();

    for branch in &data.branches {
        if branch.flat_id > 0 {
            result
                .entry(branch.camera_folder_name.clone())
                .or_default()
                .insert(branch.flat_id);
        }
    }

    result
        .into_iter()
        .map(|(camera, ids)| {
            let mut ids: Vec<i64> = ids.into_iter().collect();
            ids.sort();
            (camera, ids)
        })
        .collect()
}
