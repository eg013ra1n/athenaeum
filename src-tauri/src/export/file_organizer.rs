//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for processing.
//!
//! V2: Organizes calibration files per set ID so each master can be created
//! from its own set of files without mixing with other sets.
//!
//! V3: Nested folder hierarchy based on calibration chain:
//!     stack/camera {name}/bias {id}/darks {id}/flats {id}/fdarks {id}/lights/

use crate::export::models::{
    sanitize_folder_name, CalibrationBranch, CalibrationSetInfo, ExportConfig, ExportData,
    ExportDataV3, ExportResult, ExportTarget,
};
use crate::export::folder_structures::{SirilFolders, WBPPFolders};
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

// ============================================================================
// V3: Nested Folder Hierarchy
// ============================================================================

/// V3 folder structure with nested calibration hierarchy
/// Root structure:
///   - stack/      : Source files in nested hierarchy
///   - calibration/: Siril work folder for intermediate files
///   - masters/    : Generated masters + final stacked result
pub struct ExportFoldersV3 {
    pub root: PathBuf,
    pub stack: PathBuf,
    pub calibration: PathBuf,
    pub masters: PathBuf,
}

impl ExportFoldersV3 {
    /// Create V3 folder structure from a root path
    pub fn new(root: PathBuf) -> Self {
        Self {
            stack: root.join("stack"),
            calibration: root.join("calibration"),
            masters: root.join("masters"),
            root,
        }
    }

    /// Create base directories
    pub fn create_all(&self) -> Result<()> {
        fs::create_dir_all(&self.stack).context("Failed to create stack folder")?;
        fs::create_dir_all(&self.calibration).context("Failed to create calibration folder")?;
        fs::create_dir_all(&self.masters).context("Failed to create masters folder")?;
        Ok(())
    }

    /// Get branch-specific calibration work folder
    pub fn calibration_branch_folder(&self, branch_id: &str) -> PathBuf {
        self.calibration.join(branch_id)
    }

    /// Build nested path for bias folder
    pub fn bias_path(&self, camera_folder_name: &str, bias_id: i64) -> PathBuf {
        self.stack
            .join(format!("camera {}", camera_folder_name))
            .join(format!("bias {}", bias_id))
    }

    /// Build nested path for darks folder
    pub fn darks_path(&self, camera_folder_name: &str, bias_id: i64, dark_id: i64) -> PathBuf {
        self.bias_path(camera_folder_name, bias_id)
            .join(format!("darks {}", dark_id))
    }

    /// Build nested path for flats folder
    pub fn flats_path(
        &self,
        camera_folder_name: &str,
        bias_id: i64,
        dark_id: i64,
        flat_id: i64,
    ) -> PathBuf {
        self.darks_path(camera_folder_name, bias_id, dark_id)
            .join(format!("flats {}", flat_id))
    }

    /// Build nested path for fdarks (dark flats) folder
    pub fn fdarks_path(
        &self,
        camera_folder_name: &str,
        bias_id: i64,
        dark_id: i64,
        flat_id: i64,
        fdark_id: i64,
    ) -> PathBuf {
        self.flats_path(camera_folder_name, bias_id, dark_id, flat_id)
            .join(format!("fdarks {}", fdark_id))
    }

    /// Build nested path for lights folder
    pub fn lights_path(&self, branch: &CalibrationBranch) -> PathBuf {
        let base = self.flats_path(
            &branch.camera_folder_name,
            branch.bias_id,
            branch.dark_id,
            branch.flat_id,
        );
        if branch.darkflat_id > 0 {
            base.join(format!("fdarks {}", branch.darkflat_id))
                .join("lights")
        } else {
            base.join("lights")
        }
    }
}

/// Organize files into V3 nested hierarchy structure
pub fn organize_files_v3(config: &ExportConfig, data: &ExportDataV3) -> Result<ExportResult> {
    let folders = ExportFoldersV3::new(config.output_dir.clone());
    folders.create_all()?;

    let mut files_organized = 0;
    let mut warnings = Vec::new();
    let mut organized_calibration_sets: HashSet<i64> = HashSet::new();

    // Organize each branch
    for branch in &data.branches {
        // Build and create the nested path for lights
        let lights_path = folders.lights_path(branch);
        fs::create_dir_all(&lights_path)?;

        // Copy light frames
        for frame in &branch.light_frames {
            let dest = lights_path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, config.use_symlinks) {
                warnings.push(format!("Failed to copy light {}: {}", frame.filename, e));
            } else {
                files_organized += 1;
            }
        }

        // Organize calibration frames at appropriate hierarchy levels
        files_organized += organize_branch_calibrations_v3(
            &folders,
            branch,
            config.use_symlinks,
            &mut warnings,
            &mut organized_calibration_sets,
        )?;
    }

    // Organize any remaining calibrations from master plan that weren't covered by branches
    // This handles sub-calibrations (e.g., darks used for flat calibration)
    files_organized += organize_master_plan_calibrations(
        &folders,
        data,
        config.use_symlinks,
        &mut warnings,
        &mut organized_calibration_sets,
    )?;

    // Create calibration branch folders (Siril's convert -out doesn't create directories)
    for branch in &data.branches {
        let branch_folder = folders.calibration_branch_folder(&branch.branch_id);
        if let Err(e) = fs::create_dir_all(&branch_folder) {
            warnings.push(format!(
                "Failed to create calibration folder {}: {}",
                branch_folder.display(),
                e
            ));
        }
    }

    // Create per-master work folders WITH symlinks to source files
    // This allows Siril to work in these folders (no spaces in path)
    // and keeps source stack/ folder clean
    for master in &data.master_plan.masters {
        let master_work_folder = folders.calibration.join(format!("master_{}", master.set_id));
        if let Err(e) = fs::create_dir_all(&master_work_folder) {
            warnings.push(format!(
                "Failed to create master work folder {}: {}",
                master_work_folder.display(),
                e
            ));
            continue;
        }

        // Create symlinks to source files from master.source_frames
        for frame in &master.source_frames {
            let dest = master_work_folder.join(&frame.filename);
            if dest.exists() {
                continue; // Skip if already exists
            }

            // Create symlink to source file
            #[cfg(unix)]
            {
                if let Err(e) = std::os::unix::fs::symlink(&frame.file_path, &dest) {
                    warnings.push(format!(
                        "Failed to symlink {} to {}: {}",
                        frame.filename,
                        dest.display(),
                        e
                    ));
                } else {
                    files_organized += 1;
                }
            }
            #[cfg(windows)]
            {
                if let Err(e) = std::os::windows::fs::symlink_file(&frame.file_path, &dest) {
                    warnings.push(format!(
                        "Failed to symlink {} to {}: {}",
                        frame.filename,
                        dest.display(),
                        e
                    ));
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

/// Organize calibration frames for a branch at appropriate hierarchy levels
fn organize_branch_calibrations_v3(
    folders: &ExportFoldersV3,
    branch: &CalibrationBranch,
    use_symlinks: bool,
    warnings: &mut Vec<String>,
    organized_sets: &mut HashSet<i64>,
) -> Result<i32> {
    let mut count = 0;

    // 1. Bias frames: camera {name}/bias {id}/
    if let Some(ref bias) = branch.bias_info {
        if !organized_sets.contains(&bias.set_id) && bias.set_id > 0 {
            let bias_path = folders.bias_path(&branch.camera_folder_name, bias.set_id);
            fs::create_dir_all(&bias_path)?;

            for frame in &bias.frames {
                let dest = bias_path.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
                    warnings.push(format!("Failed to copy bias {}: {}", frame.filename, e));
                } else {
                    count += 1;
                }
            }
            organized_sets.insert(bias.set_id);
        }
    }

    // 2. Dark frames: camera {name}/bias {id}/darks {id}/
    if let Some(ref dark) = branch.dark_info {
        if !organized_sets.contains(&dark.set_id) && dark.set_id > 0 {
            let dark_path =
                folders.darks_path(&branch.camera_folder_name, branch.bias_id, dark.set_id);
            fs::create_dir_all(&dark_path)?;

            for frame in &dark.frames {
                let dest = dark_path.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
                    warnings.push(format!("Failed to copy dark {}: {}", frame.filename, e));
                } else {
                    count += 1;
                }
            }
            organized_sets.insert(dark.set_id);
        }
    }

    // 3. Flat frames: camera {name}/bias {id}/darks {id}/flats {id}/
    if let Some(ref flat) = branch.flat_info {
        if !organized_sets.contains(&flat.set_id) && flat.set_id > 0 {
            let flat_path = folders.flats_path(
                &branch.camera_folder_name,
                branch.bias_id,
                branch.dark_id,
                flat.set_id,
            );
            fs::create_dir_all(&flat_path)?;

            for frame in &flat.frames {
                let dest = flat_path.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
                    warnings.push(format!("Failed to copy flat {}: {}", frame.filename, e));
                } else {
                    count += 1;
                }
            }
            organized_sets.insert(flat.set_id);
        }
    }

    // 4. DarkFlat frames: .../flats {id}/fdarks {id}/
    if let Some(ref darkflat) = branch.darkflat_info {
        if !organized_sets.contains(&darkflat.set_id) && darkflat.set_id > 0 {
            let fdark_path = folders.fdarks_path(
                &branch.camera_folder_name,
                branch.bias_id,
                branch.dark_id,
                branch.flat_id,
                darkflat.set_id,
            );
            fs::create_dir_all(&fdark_path)?;

            for frame in &darkflat.frames {
                let dest = fdark_path.join(&frame.filename);
                if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
                    warnings.push(format!("Failed to copy darkflat {}: {}", frame.filename, e));
                } else {
                    count += 1;
                }
            }
            organized_sets.insert(darkflat.set_id);
        }
    }

    Ok(count)
}

/// Organize calibrations from master plan that weren't organized by branches
/// This handles sub-calibrations like darks used for flat calibration
fn organize_master_plan_calibrations(
    folders: &ExportFoldersV3,
    data: &ExportDataV3,
    use_symlinks: bool,
    warnings: &mut Vec<String>,
    organized_sets: &mut HashSet<i64>,
) -> Result<i32> {
    let mut count = 0;

    // Get the first branch to determine camera info
    // All branches should be from the same frame set, so camera should be consistent
    let default_camera = data
        .branches
        .first()
        .map(|b| b.camera_folder_name.clone())
        .unwrap_or_else(|| "unknown".to_string());

    for master in &data.master_plan.masters {
        // Skip if already organized
        if organized_sets.contains(&master.set_id) {
            continue;
        }

        // Skip if no source frames
        if master.source_frames.is_empty() {
            continue;
        }

        // Determine camera from source frames
        let camera_name = master
            .source_frames
            .first()
            .and_then(|f| f.instrume.as_ref())
            .map(|i| sanitize_folder_name(i))
            .unwrap_or_else(|| default_camera.clone());

        // Organize based on master type
        let path = match master.master_type.as_str() {
            "Bias" => {
                folders.bias_path(&camera_name, master.set_id)
            }
            "Dark" | "DarkFlat" => {
                // Get bias_id from master's apply_bias or use 0
                let bias_id = master.apply_bias.unwrap_or(0);
                folders.darks_path(&camera_name, bias_id, master.set_id)
            }
            "Flat" => {
                // Flats should already be organized by branches, but handle edge case
                // Use apply_dark to determine parent level
                let bias_id = master.apply_bias.unwrap_or(0);
                let dark_id = master.apply_dark.unwrap_or(0);
                folders.flats_path(&camera_name, bias_id, dark_id, master.set_id)
            }
            _ => continue,
        };

        // Create directory and copy files
        fs::create_dir_all(&path)?;

        for frame in &master.source_frames {
            let dest = path.join(&frame.filename);
            if let Err(e) = copy_or_link(&frame.file_path, &dest, use_symlinks) {
                warnings.push(format!(
                    "Failed to copy {} {}: {}",
                    master.master_type, frame.filename, e
                ));
            } else {
                count += 1;
            }
        }

        organized_sets.insert(master.set_id);
    }

    Ok(count)
}

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
