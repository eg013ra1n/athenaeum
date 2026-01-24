//! Siril CLI runner
//!
//! Executes Siril scripts via siril-cli and captures progress.
//! Also provides pipeline orchestration for complete export workflows.

use crate::export::models::{ExportProgress, ExportStage};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

/// Default Siril CLI executable name
#[cfg(target_os = "macos")]
pub const DEFAULT_SIRIL_CLI: &str = "/Applications/Siril.app/Contents/MacOS/siril-cli";

#[cfg(target_os = "windows")]
pub const DEFAULT_SIRIL_CLI: &str = "siril-cli.exe";

#[cfg(target_os = "linux")]
pub const DEFAULT_SIRIL_CLI: &str = "siril-cli";

/// Find Siril CLI executable
pub fn find_siril_cli() -> Option<String> {
    // Check common locations
    let paths = vec![
        DEFAULT_SIRIL_CLI.to_string(),
        #[cfg(target_os = "macos")]
        "/usr/local/bin/siril-cli".to_string(),
        #[cfg(target_os = "macos")]
        "/opt/homebrew/bin/siril-cli".to_string(),
        #[cfg(target_os = "linux")]
        "/usr/bin/siril-cli".to_string(),
        #[cfg(target_os = "linux")]
        "/usr/local/bin/siril-cli".to_string(),
    ];

    for path in paths {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    // Try to find in PATH
    if let Ok(output) = Command::new("which").arg("siril-cli").output() {
        if output.status.success() {
            if let Ok(path) = String::from_utf8(output.stdout) {
                let path = path.trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }
    }

    None
}

/// Run a Siril script and emit progress events
pub fn run_siril_script(
    siril_path: &str,
    script_path: &Path,
    app_handle: &AppHandle,
) -> Result<()> {
    println!("🚀 Running Siril script: {:?}", script_path);

    // Emit starting progress
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::SirilCalibrating,
            progress: 0.0,
            message: "Starting Siril...".to_string(),
            current_file: None,
        },
    );

    // Run siril-cli with the script
    let mut child = Command::new(siril_path)
        .arg("-s")
        .arg(script_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start Siril CLI at {}", siril_path))?;

    // Collect stderr in a separate thread
    let stderr = child.stderr.take();
    let stderr_handle = std::thread::spawn(move || {
        let mut stderr_output = Vec::new();
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            for line in reader.lines().flatten() {
                println!("  [SIRIL STDERR] {}", line);
                stderr_output.push(line);
            }
        }
        stderr_output
    });

    // Read stdout for progress
    let mut last_lines: Vec<String> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut current_stage = ExportStage::SirilCalibrating;
        let mut line_count = 0;

        for line in reader.lines() {
            if let Ok(line) = line {
                line_count += 1;
                println!("  [SIRIL] {}", line);

                // Keep last 20 lines for error reporting
                last_lines.push(line.clone());
                if last_lines.len() > 20 {
                    last_lines.remove(0);
                }

                // Parse Siril output to determine stage
                let (stage, message) = parse_siril_output(&line, &current_stage);
                current_stage = stage.clone();

                // Estimate progress (rough estimate based on typical workflow)
                let progress = estimate_progress(&current_stage, line_count);

                emit_progress(
                    app_handle,
                    ExportProgress {
                        stage,
                        progress,
                        message,
                        current_file: extract_filename(&line),
                    },
                );
            }
        }
    }

    println!("  [DEBUG] stdout loop finished, waiting for process to exit...");

    // Wait for completion with timeout (Siril on macOS can hang after "closing pipes")
    let timeout = Duration::from_secs(30);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("  [DEBUG] process exited with status: {:?}", status);
                break status;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    println!("  [DEBUG] process timeout after {:?}, killing...", timeout);
                    let _ = child.kill();
                    // Wait a bit for kill to take effect
                    std::thread::sleep(Duration::from_millis(500));
                    // Try one more time to get status
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        _ => {
                            // Consider it successful if stdout showed completion
                            if last_lines.iter().any(|l| l.contains("Script execution finished successfully")) {
                                println!("  [DEBUG] Script completed but process hung - treating as success");
                                // Return early with success
                                emit_progress(
                                    app_handle,
                                    ExportProgress {
                                        stage: ExportStage::Complete,
                                        progress: 100.0,
                                        message: "Siril processing complete".to_string(),
                                        current_file: None,
                                    },
                                );
                                return Ok(());
                            }
                            return Err(anyhow::anyhow!("Siril process timed out and could not be killed"));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(anyhow::anyhow!("Failed to wait for Siril: {}", e)),
        }
    };

    // If process exited successfully, don't wait for stderr thread
    // (Siril on macOS may not close stderr properly)
    if status.success() {
        println!("  [DEBUG] process succeeded, skipping stderr thread join");
        println!("✅ Siril script completed successfully");
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::Complete,
                progress: 100.0,
                message: "Siril processing complete".to_string(),
                current_file: None,
            },
        );
        return Ok(());
    }

    // Only wait for stderr on failure (need error details)
    println!("  [DEBUG] joining stderr thread for error details...");
    let stderr_output = match stderr_handle.join() {
        Ok(output) => {
            println!("  [DEBUG] stderr thread joined successfully");
            output
        }
        Err(_) => {
            println!("  [DEBUG] stderr thread join failed");
            Vec::new()
        }
    };

    // If we get here, the process failed - build error message
    let mut error_details = format!("Siril exited with code: {:?}\n", status.code());

    if !last_lines.is_empty() {
        error_details.push_str("\n--- Last stdout lines ---\n");
        for line in &last_lines {
            error_details.push_str(&format!("  {}\n", line));
        }
    }

    if !stderr_output.is_empty() {
        error_details.push_str("\n--- Stderr ---\n");
        for line in &stderr_output {
            error_details.push_str(&format!("  {}\n", line));
        }
    }

    println!("❌ Siril script failed:\n{}", error_details);

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::Failed,
            progress: 0.0,
            message: error_details.clone(),
            current_file: None,
        },
    );
    Err(anyhow::anyhow!("{}", error_details))
}

/// Parse Siril output to determine current stage
fn parse_siril_output(line: &str, current_stage: &ExportStage) -> (ExportStage, String) {
    let line_lower = line.to_lowercase();

    if line_lower.contains("convert") || line_lower.contains("converting") {
        (
            ExportStage::SirilCalibrating,
            "Converting files...".to_string(),
        )
    } else if line_lower.contains("stack") || line_lower.contains("stacking") {
        (ExportStage::SirilStacking, "Stacking frames...".to_string())
    } else if line_lower.contains("register") || line_lower.contains("registration") {
        (
            ExportStage::SirilRegistering,
            "Registering frames...".to_string(),
        )
    } else if line_lower.contains("calibrat") {
        (
            ExportStage::SirilCalibrating,
            "Calibrating frames...".to_string(),
        )
    } else if line_lower.contains("master") {
        (
            ExportStage::SirilCalibrating,
            "Creating master frame...".to_string(),
        )
    } else if line_lower.contains("error") || line_lower.contains("failed") {
        (ExportStage::Failed, line.to_string())
    } else {
        (current_stage.clone(), line.to_string())
    }
}

/// Estimate progress based on stage and line count
fn estimate_progress(stage: &ExportStage, line_count: usize) -> f64 {
    let base_progress = match stage {
        ExportStage::Collecting => 0.0,
        ExportStage::Organizing => 5.0,
        ExportStage::GeneratingScripts => 10.0,
        ExportStage::SirilCreatingMasters => 15.0,
        ExportStage::SirilCalibrating => 20.0,
        ExportStage::CollectingCalibratedFrames => 40.0,
        ExportStage::SirilRegistering => 50.0,
        ExportStage::SirilStacking => 80.0,
        ExportStage::Complete => 100.0,
        ExportStage::Failed => 0.0,
    };

    // Complete and Failed stages are fixed, don't add line increment
    if matches!(stage, ExportStage::Complete | ExportStage::Failed) {
        return base_progress;
    }

    // Add small increment based on lines processed
    let line_increment = (line_count as f64 * 0.1).min(10.0);

    (base_progress + line_increment).min(99.0)
}

/// Extract filename from Siril output line
fn extract_filename(line: &str) -> Option<String> {
    // Siril often outputs "Processing file: filename.fit" or similar
    if line.contains(".fit") || line.contains(".fits") || line.contains(".xisf") {
        // Try to extract the filename
        let parts: Vec<&str> = line.split_whitespace().collect();
        for part in parts {
            if part.ends_with(".fit")
                || part.ends_with(".fits")
                || part.ends_with(".xisf")
                || part.ends_with(".FIT")
                || part.ends_with(".FITS")
            {
                return Some(part.trim_matches(&['"', '\'', ':', ','][..]).to_string());
            }
        }
    }
    None
}

/// Emit a progress event to the frontend
fn emit_progress(app_handle: &AppHandle, progress: ExportProgress) {
    let _ = app_handle.emit("export-progress", &progress);
}

// ============================================================================
// Export Pipeline Orchestration
// ============================================================================

/// Metadata for a single collected frame
#[derive(Debug, Clone)]
pub struct CollectedFrame {
    /// Filename in the collection directory
    pub filename: String,
    /// Filter name (e.g., "L", "Ha", "Red")
    pub filter: Option<String>,
    /// Exposure time in seconds (kept for future use)
    #[allow(dead_code)]
    pub exptime: Option<f64>,
}

/// Info about a collected frame group (OSC or Mono)
/// All frames regardless of dimensions go in the same group
/// seqapplyreg -framing=max handles dimension differences
#[derive(Debug, Clone)]
pub struct FrameGroup {
    pub dir: PathBuf,
    pub is_osc: bool,
    /// Frames in this group with their metadata
    pub frames: Vec<CollectedFrame>,
}

impl FrameGroup {
    pub fn count(&self) -> usize {
        self.frames.len()
    }
}

/// Result of collecting calibrated frames, separated by camera type only
/// (OSC vs Mono - NOT by dimensions)
#[derive(Debug, Clone)]
pub struct CollectedFrames {
    /// Mono frames group (may be None if no mono frames)
    pub mono: Option<FrameGroup>,
    /// OSC frames group (may be None if no OSC frames)
    pub osc: Option<FrameGroup>,
}

impl CollectedFrames {
    pub fn total(&self) -> usize {
        self.mono_count() + self.osc_count()
    }

    pub fn mono_count(&self) -> usize {
        self.mono.as_ref().map(|g| g.count()).unwrap_or(0)
    }

    pub fn osc_count(&self) -> usize {
        self.osc.as_ref().map(|g| g.count()).unwrap_or(0)
    }
}

/// FITS image metadata needed for stacking
#[derive(Debug, Clone)]
struct FitsMetadata {
    layers: usize,
    filter: Option<String>,
    exptime: Option<f64>,
}

/// A group of registered frames ready for final merge and stacking
///
/// After per-branch registration, frames are grouped by:
/// - Filter (L, Ha, OIII, R, G, B, etc.)
/// - Camera type (Mono vs OSC - cannot mix different layer counts)
/// - Optionally, exposure time (when tolerance mode is enabled)
#[derive(Debug, Clone)]
pub struct StackingGroup {
    /// Filter name (e.g., "L", "Ha", "None" for unfiltered)
    pub filter: String,
    /// Camera type: false = Mono (1 layer), true = OSC (3 layers)
    pub is_osc: bool,
    /// Representative exposure time (median of frames)
    pub exptime: Option<f64>,
    /// Display string for output filename (e.g., "60s", "300s", "")
    pub exptime_display: String,
    /// Output directory for this group (e.g., process/stack_L_mono/)
    pub output_dir: PathBuf,
    /// Source files (r_pp_lights_*.fit paths from branches)
    pub source_files: Vec<PathBuf>,
}

impl StackingGroup {
    pub fn frame_count(&self) -> usize {
        self.source_files.len()
    }

    /// Get the camera type suffix for naming
    pub fn camera_suffix(&self) -> &str {
        if self.is_osc { "osc" } else { "mono" }
    }

    /// Generate a unique key for this group
    pub fn group_key(&self) -> String {
        let filter_safe = self.filter.to_lowercase().replace(' ', "_");
        if self.exptime_display.is_empty() {
            format!("{}_{}", filter_safe, self.camera_suffix())
        } else {
            format!("{}_{}_{}", filter_safe, self.camera_suffix(), self.exptime_display)
        }
    }
}

/// Read FITS metadata (layer count, filter, exptime)
fn get_fits_metadata(path: &Path) -> Result<FitsMetadata> {
    use fitsio::FitsFile;

    let mut fptr = FitsFile::open(path)
        .with_context(|| format!("Failed to open FITS file: {:?}", path))?;

    let hdu = fptr.primary_hdu()
        .with_context(|| format!("Failed to get primary HDU: {:?}", path))?;

    // Read NAXIS to check if 3D (OSC vs Mono)
    let naxis: i64 = hdu.read_key(&mut fptr, "NAXIS")
        .with_context(|| format!("Failed to read NAXIS: {:?}", path))?;

    let layers = if naxis < 3 {
        1
    } else {
        hdu.read_key::<i64>(&mut fptr, "NAXIS3").unwrap_or(1) as usize
    };

    // Read filter (optional) - try multiple common keywords
    let filter: Option<String> = hdu.read_key(&mut fptr, "FILTER")
        .ok()
        .or_else(|| hdu.read_key(&mut fptr, "FILTER1").ok())
        .map(|s: String| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Read exposure time (optional)
    let exptime: Option<f64> = hdu.read_key(&mut fptr, "EXPTIME")
        .ok()
        .or_else(|| hdu.read_key(&mut fptr, "EXPOSURE").ok());

    Ok(FitsMetadata {
        layers,
        filter,
        exptime,
    })
}

/// A collected frame with its metadata
#[derive(Debug, Clone)]
struct RegisteredFrameInfo {
    path: PathBuf,
    filter: Option<String>,
    exptime: Option<f64>,
    is_osc: bool,
}

/// Collect registered frames from branch directories and group them for stacking
///
/// After `02_register_branches.ssf` runs, each branch directory contains
/// `r_pp_lights_*.fit` files (registered frames). This function:
///
/// 1. Walks all lights/branch_XX directories to find r_pp_lights_*.fit files
/// 2. Reads FITS headers for filter, camera type (OSC vs Mono), and exposure time
/// 3. Groups frames by (filter, camera_type) - and optionally by exposure time
/// 4. Creates output directories for each stacking group
/// 5. Returns StackingGroup structs for script generation
///
/// **IMPORTANT**: The function does NOT copy files yet - that happens when stacking
/// scripts are run. This allows Siril to work directly with the registered files.
pub fn collect_and_group_registered_frames(
    export_dir: &Path,
    exptime_tolerance_mode: &crate::export::models::ExptimeToleranceMode,
    exptime_tolerance_value: f64,
    app_handle: &AppHandle,
) -> Result<Vec<StackingGroup>> {
    use crate::export::models::ExptimeToleranceMode;
    use std::collections::HashMap;

    let lights_dir = export_dir.join("lights");
    let process_dir = export_dir.join("process");

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 0.0,
            message: "Collecting registered frames from branches...".to_string(),
            current_file: None,
        },
    );

    // Collect all registered frames from branch directories
    let mut all_frames: Vec<RegisteredFrameInfo> = Vec::new();
    let mut errors = Vec::new();

    // Walk through lights directory looking for r_pp_lights_*.fit files
    for entry in WalkDir::new(&lights_dir)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Only process files, not directories
        if !path.is_file() {
            continue;
        }

        // Check if this is a registered frame (r_pp_lights_*.fit)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("r_pp_lights_") && (name.ends_with(".fit") || name.ends_with(".fits")) {
                // Read FITS metadata
                match get_fits_metadata(path) {
                    Ok(metadata) => {
                        let is_osc = metadata.layers >= 3;
                        all_frames.push(RegisteredFrameInfo {
                            path: path.to_path_buf(),
                            filter: metadata.filter,
                            exptime: metadata.exptime,
                            is_osc,
                        });
                    }
                    Err(e) => {
                        errors.push(format!("Failed to read metadata for {:?}: {}", path, e));
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        println!("⚠️ Some frames failed to process:");
        for err in &errors {
            println!("  {}", err);
        }
    }

    println!("📊 Collected {} registered frames for grouping", all_frames.len());

    // Group key: (filter, is_osc, exptime_bucket)
    // exptime_bucket is None when tolerance mode is Disabled
    type GroupKey = (String, bool, Option<String>);
    let mut groups_map: HashMap<GroupKey, Vec<RegisteredFrameInfo>> = HashMap::new();

    for frame in all_frames {
        let filter_key = frame.filter.clone().unwrap_or_else(|| "Unfiltered".to_string());

        // Calculate exptime bucket based on tolerance mode
        let exptime_bucket = match exptime_tolerance_mode {
            ExptimeToleranceMode::Disabled => None,
            ExptimeToleranceMode::Absolute | ExptimeToleranceMode::Relative => {
                frame.exptime.map(|exp| format_exptime_bucket(exp))
            }
        };

        let key = (filter_key, frame.is_osc, exptime_bucket);
        groups_map.entry(key).or_default().push(frame);
    }

    // Create StackingGroup for each group
    let mut stacking_groups: Vec<StackingGroup> = Vec::new();

    for ((filter, is_osc, exptime_bucket), frames) in groups_map {
        if frames.is_empty() {
            continue;
        }

        // Calculate representative exposure time
        let mut exptimes: Vec<f64> = frames.iter()
            .filter_map(|f| f.exptime)
            .collect();
        exptimes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_exptime = if exptimes.is_empty() {
            None
        } else {
            let mid = exptimes.len() / 2;
            Some(exptimes[mid])
        };

        // Create display string
        let exptime_display = exptime_bucket.unwrap_or_default();

        // Create output directory name
        let filter_safe = filter.to_lowercase().replace(' ', "_");
        let camera_suffix = if is_osc { "osc" } else { "mono" };
        let dir_name = if exptime_display.is_empty() {
            format!("stack_{}_{}", filter_safe, camera_suffix)
        } else {
            format!("stack_{}_{}_{}", filter_safe, camera_suffix, exptime_display)
        };
        let output_dir = process_dir.join(&dir_name);

        // Collect source file paths
        let source_files: Vec<PathBuf> = frames.iter()
            .map(|f| f.path.clone())
            .collect();

        stacking_groups.push(StackingGroup {
            filter,
            is_osc,
            exptime: median_exptime,
            exptime_display,
            output_dir,
            source_files,
        });
    }

    // Sort groups by filter, then camera type, then exptime
    stacking_groups.sort_by(|a, b| {
        a.filter.cmp(&b.filter)
            .then(a.is_osc.cmp(&b.is_osc))
            .then(a.exptime_display.cmp(&b.exptime_display))
    });

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 100.0,
            message: format!("Grouped into {} stacking groups", stacking_groups.len()),
            current_file: None,
        },
    );

    println!("✅ Created {} stacking groups:", stacking_groups.len());
    for group in &stacking_groups {
        println!(
            "   {} {} ({} frames) → {:?}",
            group.filter,
            group.camera_suffix(),
            group.frame_count(),
            group.output_dir.file_name().unwrap_or_default()
        );
    }

    Ok(stacking_groups)
}

/// Format exposure time for bucket naming
fn format_exptime_bucket(exptime: f64) -> String {
    if exptime < 1.0 {
        format!("{:.0}ms", exptime * 1000.0)
    } else if (exptime - exptime.round()).abs() < 0.01 {
        format!("{:.0}s", exptime)
    } else {
        format!("{:.1}s", exptime)
    }
}

/// Information about a branch's registered frames for grouping
#[derive(Debug, Clone)]
pub struct BranchRegisteredFrames {
    pub branch_id: String,
    pub branch_index: usize,
    pub filter: Option<String>,
    pub is_osc: bool,
    pub exptime: Option<f64>,
    pub frame_count: usize,
    /// Expected path to the branch directory (lights/branch_XX)
    pub branch_dir: PathBuf,
}

/// Collect registered frames using BRANCH METADATA (not FITS scanning)
///
/// This is the CORRECT approach - it uses the existing CalibrationBranch metadata
/// which already knows camera type (from bayerpat), filter, and exptime.
///
/// Groups frames by:
/// - Camera type (mono/osc) - MUST be separate
/// - Filter
/// - Exposure time (within user's tolerance from settings)
///
/// Returns StackingGroup structs with the expected file paths based on branch structure.
pub fn collect_registered_frames_from_branches(
    branches: &[crate::export::models::CalibrationBranch],
    export_dir: &Path,
    exptime_tolerance_mode: &crate::export::models::ExptimeToleranceMode,
    exptime_tolerance_value: f64,
    folders: &crate::export::folder_structures::SirilFolders,
    app_handle: &AppHandle,
) -> Result<Vec<StackingGroup>> {
    use crate::export::models::{CameraType, ExptimeToleranceMode};
    use std::collections::HashMap;

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 0.0,
            message: "Collecting registered frames using branch metadata...".to_string(),
            current_file: None,
        },
    );

    // Collect branch info using metadata (NOT scanning files)
    let mut branch_infos: Vec<BranchRegisteredFrames> = Vec::new();

    for (idx, branch) in branches.iter().enumerate() {
        // Skip branches with < 2 frames (can't be registered)
        if branch.light_frames.len() < 2 {
            println!("  Skipping branch {} ({} frames < 2)", branch.branch_id, branch.light_frames.len());
            continue;
        }

        // Get camera type from first frame's bayerpat
        let is_osc = branch.light_frames.first()
            .and_then(|f| f.bayerpat.as_deref())
            .map(|bp| !bp.trim().is_empty())
            .unwrap_or(false);

        // Get exptime from first frame (all frames in branch should have same exptime)
        let exptime = branch.light_frames.first()
            .and_then(|f| f.exptime);

        // Get branch directory path
        let branch_dir = folders.lights_path(branch, idx);

        branch_infos.push(BranchRegisteredFrames {
            branch_id: branch.branch_id.clone(),
            branch_index: idx,
            filter: branch.filter.clone(),
            is_osc,
            exptime,
            frame_count: branch.light_frames.len(),
            branch_dir,
        });

        println!(
            "  Branch {}: {} {} frames, filter={:?}, exptime={:?}",
            branch.branch_id,
            if is_osc { "OSC" } else { "Mono" },
            branch.light_frames.len(),
            branch.filter,
            exptime
        );
    }

    println!("📊 Collected {} branches for grouping", branch_infos.len());

    // Group key: (filter, is_osc, exptime_bucket)
    type GroupKey = (String, bool, Option<String>);
    let mut groups_map: HashMap<GroupKey, Vec<BranchRegisteredFrames>> = HashMap::new();

    for info in branch_infos {
        let filter_key = info.filter.clone().unwrap_or_else(|| "Unfiltered".to_string());

        // Calculate exptime bucket based on tolerance mode
        let exptime_bucket = match exptime_tolerance_mode {
            ExptimeToleranceMode::Disabled => None,
            ExptimeToleranceMode::Absolute => {
                // Group by rounded exptime within tolerance
                info.exptime.map(|exp| {
                    let bucket = (exp / exptime_tolerance_value).round() * exptime_tolerance_value;
                    format_exptime_bucket(bucket)
                })
            }
            ExptimeToleranceMode::Relative => {
                // Just use the raw exptime for display (actual grouping TBD)
                info.exptime.map(|exp| format_exptime_bucket(exp))
            }
        };

        let key = (filter_key, info.is_osc, exptime_bucket);
        groups_map.entry(key).or_default().push(info);
    }

    // Create StackingGroup for each group
    let process_dir = export_dir.join("process");
    let mut stacking_groups: Vec<StackingGroup> = Vec::new();

    for ((filter, is_osc, exptime_bucket), branch_infos) in groups_map {
        if branch_infos.is_empty() {
            continue;
        }

        // Calculate total frame count
        let total_frames: usize = branch_infos.iter().map(|b| b.frame_count).sum();

        // Calculate representative exposure time
        let median_exptime = branch_infos.first().and_then(|b| b.exptime);

        // Create display string
        let exptime_display = exptime_bucket.unwrap_or_default();

        // Create output directory name
        let filter_safe = filter.to_lowercase().replace(' ', "_");
        let camera_suffix = if is_osc { "osc" } else { "mono" };
        let dir_name = if exptime_display.is_empty() {
            format!("stack_{}_{}", filter_safe, camera_suffix)
        } else {
            format!("stack_{}_{}_{}", filter_safe, camera_suffix, exptime_display)
        };
        let output_dir = process_dir.join(&dir_name);

        // Build expected source file paths from branch directories
        // After registration, each branch has r_pp_lights_NNNNN.fit files
        let mut source_files: Vec<PathBuf> = Vec::new();
        for info in &branch_infos {
            for i in 1..=info.frame_count {
                let filename = format!("r_pp_lights_{:05}.fit", i);
                source_files.push(info.branch_dir.join(&filename));
            }
        }

        stacking_groups.push(StackingGroup {
            filter,
            is_osc,
            exptime: median_exptime,
            exptime_display,
            output_dir,
            source_files,
        });
    }

    // Sort groups by filter, then camera type, then exptime
    stacking_groups.sort_by(|a, b| {
        a.filter.cmp(&b.filter)
            .then(a.is_osc.cmp(&b.is_osc))
            .then(a.exptime_display.cmp(&b.exptime_display))
    });

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 100.0,
            message: format!("Grouped into {} stacking groups", stacking_groups.len()),
            current_file: None,
        },
    );

    println!("✅ Created {} stacking groups from branch metadata:", stacking_groups.len());
    for group in &stacking_groups {
        println!(
            "   {} {} ({} frames) → {:?}",
            group.filter,
            group.camera_suffix(),
            group.frame_count(),
            group.output_dir.file_name().unwrap_or_default()
        );
    }

    Ok(stacking_groups)
}

/// Prepare a stacking group directory by copying/linking registered frames
///
/// This function:
/// 1. Creates the group output directory
/// 2. Copies r_pp_lights_*.fit files from branches with sequential naming
///
/// Returns the number of files copied.
pub fn prepare_stacking_group_directory(
    group: &StackingGroup,
    app_handle: &AppHandle,
) -> Result<usize> {
    // Create output directory
    std::fs::create_dir_all(&group.output_dir)
        .with_context(|| format!("Failed to create directory: {:?}", group.output_dir))?;

    let mut copied = 0;

    for (idx, source_path) in group.source_files.iter().enumerate() {
        // Sequential naming: r_pp_lights_00001.fit, r_pp_lights_00002.fit, etc.
        let new_name = format!("r_pp_lights_{:05}.fit", idx + 1);
        let dest_path = group.output_dir.join(&new_name);

        // Copy file
        std::fs::copy(source_path, &dest_path)
            .with_context(|| format!("Failed to copy {:?} to {:?}", source_path, dest_path))?;

        copied += 1;

        // Emit progress every 10 frames
        if copied % 10 == 0 {
            emit_progress(
                app_handle,
                ExportProgress {
                    stage: ExportStage::CollectingCalibratedFrames,
                    progress: (copied as f64 / group.source_files.len() as f64) * 100.0,
                    message: format!("Copying frames for {} {}...", group.filter, group.camera_suffix()),
                    current_file: Some(new_name),
                },
            );
        }
    }

    Ok(copied)
}

/// Prepare all frames for global registration by camera type
///
/// Collects r_pp_lights_*.fit files from all branches and copies them
/// to process/all_mono/ or process/all_osc/ with sequential naming.
///
/// Returns the paths to the mono and osc directories (if they have files).
pub fn prepare_global_registration_directories(
    groups: &[StackingGroup],
    export_dir: &Path,
    app_handle: &AppHandle,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let process_dir = export_dir.join("process");
    let mono_dir = process_dir.join("all_mono");
    let osc_dir = process_dir.join("all_osc");

    let mut mono_files: Vec<PathBuf> = Vec::new();
    let mut osc_files: Vec<PathBuf> = Vec::new();

    // Collect files by camera type from all groups
    for group in groups {
        if group.is_osc {
            osc_files.extend(group.source_files.clone());
        } else {
            mono_files.extend(group.source_files.clone());
        }
    }

    let mut result_mono = None;
    let mut result_osc = None;

    // Copy mono files
    if !mono_files.is_empty() {
        std::fs::create_dir_all(&mono_dir)?;
        println!("📁 Copying {} mono frames to {:?}", mono_files.len(), mono_dir);

        for (idx, source) in mono_files.iter().enumerate() {
            let dest = mono_dir.join(format!("all_r_{:05}.fit", idx + 1));
            std::fs::copy(source, &dest)
                .with_context(|| format!("Failed to copy {:?} to {:?}", source, dest))?;

            if (idx + 1) % 20 == 0 {
                emit_progress(
                    app_handle,
                    ExportProgress {
                        stage: ExportStage::CollectingCalibratedFrames,
                        progress: (idx as f64 / mono_files.len() as f64) * 50.0,
                        message: format!("Copying mono frame {}/{}...", idx + 1, mono_files.len()),
                        current_file: Some(source.file_name().unwrap_or_default().to_string_lossy().to_string()),
                    },
                );
            }
        }
        result_mono = Some(mono_dir);
    }

    // Copy OSC files
    if !osc_files.is_empty() {
        std::fs::create_dir_all(&osc_dir)?;
        println!("📁 Copying {} OSC frames to {:?}", osc_files.len(), osc_dir);

        for (idx, source) in osc_files.iter().enumerate() {
            let dest = osc_dir.join(format!("all_r_{:05}.fit", idx + 1));
            std::fs::copy(source, &dest)
                .with_context(|| format!("Failed to copy {:?} to {:?}", source, dest))?;

            if (idx + 1) % 20 == 0 {
                emit_progress(
                    app_handle,
                    ExportProgress {
                        stage: ExportStage::CollectingCalibratedFrames,
                        progress: 50.0 + (idx as f64 / osc_files.len() as f64) * 50.0,
                        message: format!("Copying OSC frame {}/{}...", idx + 1, osc_files.len()),
                        current_file: Some(source.file_name().unwrap_or_default().to_string_lossy().to_string()),
                    },
                );
            }
        }
        result_osc = Some(osc_dir);
    }

    Ok((result_mono, result_osc))
}

/// Generate global registration script for mono or OSC frames
///
/// This script performs:
/// 1. Convert all registered frames from branches to a single sequence
/// 2. Register with HOMOGRAPHY and -2pass (aligns across different telescopes)
/// 3. Apply registration with max framing
///
/// **CRITICAL**: Uses `-transf=homography -2pass` to align frames from different
/// optical setups (telescopes, cameras) after per-branch plate solving.
pub fn generate_global_registration_script(
    camera_type: &str,  // "mono" or "osc"
    working_dir: &Path,
    export_dir: &Path,
) -> Result<PathBuf> {
    let scripts_dir = export_dir.join("scripts").join("03_register_global");
    std::fs::create_dir_all(&scripts_dir)?;

    let script_name = format!("03_register_global_{}.ssf", camera_type);
    let script_path = scripts_dir.join(&script_name);

    let mut script = String::new();

    script.push_str(&format!(
        r#"############################################
# Global Registration Script ({})
# Generated by Athenaeum
#
# This script aligns ALL {} frames from different branches
# using homography transformation with -2pass refinement.
#
# After per-branch plate solving, this step:
# - Combines frames from different telescopes/cameras
# - Handles different image dimensions via -framing=max
# - Uses -2pass for sub-pixel alignment
############################################

requires 1.3.0

cd {}

# Convert all registered frames to a single sequence
convert all_r -out=. -fitseq

# Global registration with homography + 2pass
# -2pass: Uses plate solving for sub-pixel alignment
# -transf=homography: Handles projective distortion between frames
register all_r -transf=homography -2pass

# Apply registration with maximum framing
# This pads smaller frames to match the largest
seqapplyreg all_r -framing=max

close
"#,
        camera_type.to_uppercase(),
        camera_type.to_uppercase(),
        working_dir.to_string_lossy()
    ));

    std::fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write global registration script to {:?}", script_path))?;

    println!("  Created global registration script: {:?}", script_path);
    Ok(script_path)
}

/// Copy globally registered frames to stacking group directories
///
/// After global registration, r_all_r_*.fit files are in all_mono/ or all_osc/.
/// This function copies them to the appropriate stacking directories based on
/// their filter and exptime (which we track via branch metadata).
///
/// Uses the branch metadata to know which globally registered frame index
/// maps to which filter/exptime group.
pub fn copy_globally_registered_to_stacking_dirs(
    branches: &[crate::export::models::CalibrationBranch],
    stacking_groups: &[StackingGroup],
    export_dir: &Path,
    app_handle: &AppHandle,
) -> Result<usize> {
    let process_dir = export_dir.join("process");

    // Build a mapping: frame_index (in all_mono or all_osc) -> (filter, exptime, is_osc)
    // This uses branch metadata to track which frames have which properties
    let mut mono_index = 0usize;
    let mut osc_index = 0usize;

    // Track which stacking group each frame belongs to
    // Key: (filter, is_osc, exptime_bucket_or_empty)
    // Value: (dest_dir, local_index)
    let mut frame_assignments: Vec<(PathBuf, String, bool, Option<f64>)> = Vec::new();

    for branch in branches {
        if branch.light_frames.len() < 2 {
            continue;
        }

        let is_osc = branch.light_frames.first()
            .and_then(|f| f.bayerpat.as_deref())
            .map(|bp| !bp.trim().is_empty())
            .unwrap_or(false);

        for frame in &branch.light_frames {
            let filter = branch.filter.clone().unwrap_or_else(|| "Unfiltered".to_string());
            let exptime = frame.exptime;

            frame_assignments.push((
                if is_osc { process_dir.join("all_osc") } else { process_dir.join("all_mono") },
                filter,
                is_osc,
                exptime,
            ));

            if is_osc {
                osc_index += 1;
            } else {
                mono_index += 1;
            }
        }
    }

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 0.0,
            message: "Copying globally registered frames to stacking directories...".to_string(),
            current_file: None,
        },
    );

    let mut total_copied = 0;
    let mut mono_copy_idx = 0usize;
    let mut osc_copy_idx = 0usize;

    // Create stacking directories and copy files
    for group in stacking_groups {
        std::fs::create_dir_all(&group.output_dir)?;

        let source_dir = if group.is_osc {
            process_dir.join("all_osc")
        } else {
            process_dir.join("all_mono")
        };

        let mut group_file_idx = 1usize;

        // Find frames that belong to this group
        let mut local_idx = if group.is_osc { 0 } else { 0 };
        for (src_base_dir, filter, is_osc, exptime) in &frame_assignments {
            if *is_osc != group.is_osc {
                continue;
            }

            local_idx += 1;

            // Check if this frame matches the group's filter
            if *filter != group.filter {
                continue;
            }

            // Check exptime tolerance if enabled
            if !group.exptime_display.is_empty() {
                // TODO: Implement exptime tolerance matching
                // For now, just check if exptime matches the bucket
                if let Some(exp) = exptime {
                    let bucket = format_exptime_bucket(*exp);
                    if bucket != group.exptime_display {
                        continue;
                    }
                }
            }

            // Source file: r_all_r_NNNNN.fit
            let src_file = source_dir.join(format!("r_all_r_{:05}.fit", local_idx));

            // Destination file: r_all_r_NNNNN.fit in stacking dir
            let dest_file = group.output_dir.join(format!("r_all_r_{:05}.fit", group_file_idx));

            if src_file.exists() {
                std::fs::copy(&src_file, &dest_file)
                    .with_context(|| format!("Failed to copy {:?} to {:?}", src_file, dest_file))?;
                group_file_idx += 1;
                total_copied += 1;
            } else {
                println!("  ⚠️ Source file not found: {:?}", src_file);
            }
        }

        println!(
            "  Copied {} frames to {:?}",
            group_file_idx - 1,
            group.output_dir.file_name().unwrap_or_default()
        );
    }

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 100.0,
            message: format!("Copied {} globally registered frames to stacking directories", total_copied),
            current_file: None,
        },
    );

    Ok(total_copied)
}

/// Generate a stacking script for a specific group
///
/// This script ONLY stacks - no registration needed because:
/// 1. Per-branch registration already plate-solved frames
/// 2. Global homography registration already aligned all frames
///
/// Frames have already been copied to the stacking directory as r_all_r_*.fit
pub fn generate_group_stacking_script(
    group: &StackingGroup,
    export_dir: &Path,
    rejection_low: f64,
    rejection_high: f64,
) -> Result<PathBuf> {
    use std::fs;

    let masters_dir = export_dir.join("masters");
    fs::create_dir_all(&masters_dir)?;

    let scripts_dir = export_dir.join("scripts").join("04_stack");
    fs::create_dir_all(&scripts_dir)?;

    let mut script = String::new();

    let filter_safe = group.filter.to_lowercase().replace(' ', "_");
    let camera_suffix = group.camera_suffix();

    // Script name
    let script_name = if group.exptime_display.is_empty() {
        format!("04_stack_{}_{}.ssf", filter_safe, camera_suffix)
    } else {
        format!("04_stack_{}_{}_{}.ssf", filter_safe, camera_suffix, group.exptime_display)
    };

    // Output filename
    let output_name = if group.exptime_display.is_empty() {
        format!("{}_{}_stacked", filter_safe, camera_suffix)
    } else {
        format!("{}_{}_{}_stacked", filter_safe, camera_suffix, group.exptime_display)
    };
    let output_path = masters_dir.join(&output_name);

    script.push_str(&format!(
        r#"############################################
# Siril Stacking Script
# Generated by Athenaeum
#
# Filter: {}
# Camera Type: {}
# Frame Count: {}
# Exposure: {}
#
# NOTE: Frames are ALREADY globally registered via homography -2pass.
# This script only needs to stack.
############################################

requires 1.3.0

"#,
        group.filter,
        camera_suffix.to_uppercase(),
        group.frame_count(),
        if group.exptime_display.is_empty() { "Mixed" } else { &group.exptime_display }
    ));

    // Skip groups with < 2 frames
    if group.frame_count() < 2 {
        script.push_str("# SKIPPED: Need >= 2 frames for stacking\nclose\n");
        let script_path = scripts_dir.join(&script_name);
        fs::write(&script_path, &script)?;
        return Ok(script_path);
    }

    // Step 1: Change to group directory
    script.push_str(&format!("cd {}\n\n", group.output_dir.to_string_lossy()));

    // Step 2: Convert globally registered frames to sequence
    // Files are named r_all_r_*.fit after global registration
    script.push_str("# Convert globally registered frames to sequence\n");
    script.push_str("convert r_all_r -out=. -fitseq\n\n");

    // Step 3: Stack (no registration needed - already done)
    script.push_str("# Stack frames\n");
    let mut stack_cmd = "stack r_all_r".to_string();
    stack_cmd.push_str(&format!(" rej sigma {:.1} {:.1}", rejection_low, rejection_high));
    stack_cmd.push_str(" -norm=addscale -output_norm -weight=wfwhm");

    if group.is_osc {
        stack_cmd.push_str(" -rgb_equal");
    }

    stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
    script.push_str(&format!("{}\n\n", stack_cmd));

    script.push_str("close\n");

    let script_path = scripts_dir.join(&script_name);
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write stacking script to {:?}", script_path))?;

    println!("  Created stacking script: {:?}", script_path);
    Ok(script_path)
}

/// Generate all stacking scripts for the collected groups
///
/// Returns a vector of script paths in the order they should be executed.
pub fn generate_all_stacking_scripts(
    groups: &[StackingGroup],
    export_dir: &Path,
    rejection_low: f64,
    rejection_high: f64,
) -> Result<Vec<PathBuf>> {
    let mut scripts = Vec::new();

    for group in groups {
        if group.frame_count() >= 2 {
            let script = generate_group_stacking_script(
                group,
                export_dir,
                rejection_low,
                rejection_high,
            )?;
            scripts.push(script);
        } else {
            println!("  ⚠️ Skipping {} {} ({} frames - need >= 2)",
                group.filter, group.camera_suffix(), group.frame_count());
        }
    }

    Ok(scripts)
}

/// Collect calibrated frames from branch directories to separate directories
/// based on camera type (OSC vs Mono) only - NOT by dimensions.
///
/// ALL mono frames go to all_lights_mono/ (regardless of camera dimensions)
/// ALL OSC frames go to all_lights_osc/ (regardless of camera dimensions)
///
/// seqapplyreg -framing=max handles different camera dimensions by padding
/// smaller frames to match the largest.
///
/// IMPORTANT: Files are renamed with a global sequential counter to prevent
/// naming collisions when multiple branches have files with the same name
/// (e.g., pp_lights_00001.fit in both branch_01_L and branch_02_Ha).
///
/// Directory structure:
/// - process/all_lights_mono/ - ALL mono frames (1 channel)
/// - process/all_lights_osc/  - ALL OSC frames (3 channels)
pub fn collect_calibrated_frames(
    export_dir: &Path,
    app_handle: &AppHandle,
) -> Result<CollectedFrames> {
    let lights_dir = export_dir.join("lights");
    let process_dir = export_dir.join("process");

    // Create directories
    let mono_dir = process_dir.join("all_lights_mono");
    let osc_dir = process_dir.join("all_lights_osc");
    std::fs::create_dir_all(&mono_dir)
        .with_context(|| format!("Failed to create directory: {:?}", mono_dir))?;
    std::fs::create_dir_all(&osc_dir)
        .with_context(|| format!("Failed to create directory: {:?}", osc_dir))?;

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 0.0,
            message: "Collecting calibrated frames...".to_string(),
            current_file: None,
        },
    );

    let mut mono_frames: Vec<CollectedFrame> = Vec::new();
    let mut osc_frames: Vec<CollectedFrame> = Vec::new();
    let mut errors = Vec::new();
    let mut total_copied = 0;

    // Global sequential counters to prevent naming collisions
    // Each branch may have pp_lights_00001.fit, pp_lights_00002.fit, etc.
    // Without global counters, files from different branches would overwrite each other
    let mut mono_counter: usize = 1;
    let mut osc_counter: usize = 1;

    // Walk through lights directory looking for branch directories
    for entry in WalkDir::new(&lights_dir)
        .min_depth(1)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Only process files, not directories
        if !path.is_file() {
            continue;
        }

        // Check if this is a calibrated frame (pp_lights_*.fit)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("pp_lights_") && (name.ends_with(".fit") || name.ends_with(".fits")) {
                // Read FITS metadata (layer count, filter, exptime)
                let metadata = match get_fits_metadata(path) {
                    Ok(m) => m,
                    Err(e) => {
                        errors.push(format!("Failed to read metadata for {}: {}", name, e));
                        continue;
                    }
                };

                let is_osc = metadata.layers >= 3;

                // Generate new sequential filename to prevent collisions
                // Use the appropriate counter based on camera type
                let (dest_dir, new_name) = if is_osc {
                    let new_name = format!("pp_lights_{:05}.fit", osc_counter);
                    osc_counter += 1;
                    (&osc_dir, new_name)
                } else {
                    let new_name = format!("pp_lights_{:05}.fit", mono_counter);
                    mono_counter += 1;
                    (&mono_dir, new_name)
                };

                let dest = dest_dir.join(&new_name);

                // Copy file with new sequential name
                match std::fs::copy(path, &dest) {
                    Ok(_) => {
                        // Log the rename for debugging (only if name changed)
                        if name != new_name {
                            println!("   {} → {} ({})",
                                name,
                                new_name,
                                if is_osc { "OSC" } else { "mono" }
                            );
                        }

                        let frame = CollectedFrame {
                            filename: new_name.clone(),
                            filter: metadata.filter,
                            exptime: metadata.exptime,
                        };

                        if is_osc {
                            osc_frames.push(frame);
                        } else {
                            mono_frames.push(frame);
                        }

                        total_copied += 1;

                        // Emit progress every 10 frames
                        if total_copied % 10 == 0 {
                            emit_progress(
                                app_handle,
                                ExportProgress {
                                    stage: ExportStage::CollectingCalibratedFrames,
                                    progress: 50.0,
                                    message: format!("Copied {} frames ({} mono, {} OSC)...",
                                        total_copied, mono_frames.len(), osc_frames.len()),
                                    current_file: Some(new_name.clone()),
                                },
                            );
                        }
                    }
                    Err(e) => {
                        errors.push(format!("Failed to copy {} → {}: {}", name, new_name, e));
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        println!("⚠️ Some frames failed to process:");
        for err in &errors {
            println!("  {}", err);
        }
    }

    // Sort frames by filename for consistent sequence ordering
    mono_frames.sort_by(|a, b| a.filename.cmp(&b.filename));
    osc_frames.sort_by(|a, b| a.filename.cmp(&b.filename));

    // Build result
    let mono = if !mono_frames.is_empty() {
        Some(FrameGroup {
            dir: mono_dir,
            is_osc: false,
            frames: mono_frames,
        })
    } else {
        None
    };

    let osc = if !osc_frames.is_empty() {
        Some(FrameGroup {
            dir: osc_dir,
            is_osc: true,
            frames: osc_frames,
        })
    } else {
        None
    };

    let result = CollectedFrames { mono, osc };

    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 100.0,
            message: format!("Collected {} frames ({} mono, {} OSC)",
                total_copied, result.mono_count(), result.osc_count()),
            current_file: None,
        },
    );

    println!("✅ Collected {} calibrated frames:", total_copied);
    println!("   Mono: {} frames", result.mono_count());
    println!("   OSC: {} frames", result.osc_count());

    Ok(result)
}

/// Generate registration and stacking script for OSC and Mono pipelines
///
/// This is called AFTER frame collection. Each pipeline (OSC/Mono) processes
/// ALL frames together regardless of camera dimensions:
/// 1. convert → create sequence from all collected frames
/// 2. seqplatesolve → embed WCS coordinates for astrometric alignment
/// 3. register -2pass → compute registration transforms
/// 4. seqapplyreg -framing=max → PADS smaller frames to match largest dimensions
/// 5. convert r_pp_lights → create registered sequence
/// 6. stack per filter → create one stacked output per filter
pub fn generate_registration_script(
    collected: &CollectedFrames,
    export_dir: &Path,
    focal_length: f64,
    pixel_size: f64,
    rejection_low: f64,
    rejection_high: f64,
) -> Result<PathBuf> {
    use std::fs;
    use std::collections::HashMap;

    let masters_dir = export_dir.join("masters");
    fs::create_dir_all(&masters_dir)?;

    let mut script = String::new();

    // Header
    script.push_str(&format!(
        r#"############################################
# Siril Registration and Stacking Script
# Generated by Athenaeum
#
# Mono frames: {}
# OSC frames: {}
# Total: {} frames
#
# IMPORTANT: All frames (regardless of camera dimensions)
# are registered together. seqapplyreg -framing=max
# pads smaller frames to match the largest.
############################################

requires 1.3.0

"#,
        collected.mono_count(),
        collected.osc_count(),
        collected.total()
    ));

    // Helper function to generate pipeline for a frame group
    fn generate_pipeline(
        script: &mut String,
        group: &FrameGroup,
        masters_dir: &Path,
        focal_length: f64,
        pixel_size: f64,
        rejection_low: f64,
        rejection_high: f64,
    ) {
        let camera_type = if group.is_osc { "OSC" } else { "MONO" };
        let camera_suffix = if group.is_osc { "osc" } else { "mono" };

        script.push_str(&format!(
            "\n############################################\n"
        ));
        script.push_str(&format!(
            "# {} PIPELINE ({} frames)\n",
            camera_type, group.count()
        ));
        script.push_str(&format!(
            "############################################\n\n"
        ));

        // Step 1: Change to directory and convert
        script.push_str(&format!(
            "# --- Step 1: Create {} Sequence ---\n",
            camera_type
        ));
        script.push_str(&format!("cd {}\n", group.dir.to_string_lossy()));
        script.push_str("convert pp_lights -out=. -fitseq\n\n");

        // Step 2: Plate solve for WCS
        script.push_str(&format!(
            "# --- Step 2: Plate Solve {} ---\n",
            camera_type
        ));
        script.push_str(&format!(
            "seqplatesolve pp_lights -focal={:.1} -pixelsize={:.2}\n\n",
            focal_length, pixel_size
        ));

        // Step 3: Register
        script.push_str(&format!(
            "# --- Step 3: Register {} ---\n",
            camera_type
        ));
        script.push_str("register pp_lights -2pass\n\n");

        // Step 4: Apply registration with max framing
        script.push_str(&format!(
            "# --- Step 4: Apply Registration {} ---\n",
            camera_type
        ));
        script.push_str("# -framing=max PADS smaller frames to match largest dimensions\n");
        script.push_str("seqapplyreg pp_lights -framing=max\n\n");

        // Step 5: Convert registered files to sequence
        script.push_str(&format!(
            "# --- Step 5: Create Registered Sequence {} ---\n",
            camera_type
        ));
        script.push_str("convert r_pp_lights -out=. -fitseq\n\n");

        // Step 6: Stack per filter
        script.push_str(&format!(
            "# --- Step 6: Stack {} per filter ---\n",
            camera_type
        ));

        // Group frames by filter
        let mut filter_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, frame) in group.frames.iter().enumerate() {
            let filter_key = frame.filter.clone().unwrap_or_else(|| "Unfiltered".to_string());
            filter_groups.entry(filter_key).or_default().push(idx + 1); // Siril is 1-indexed
        }

        let total_frames = group.count();

        if filter_groups.len() == 1 {
            // Single filter - stack all frames
            let filter_name = filter_groups.keys().next().unwrap();
            let safe_filter = filter_name.to_lowercase().replace(' ', "_");
            let output_name = format!("{}_{}_stacked", safe_filter, camera_suffix);
            let output_path = masters_dir.join(&output_name);

            script.push_str(&format!(
                "# Stack all {} frames (single filter: {})\n",
                camera_type, filter_name
            ));

            let mut stack_cmd = format!("stack r_pp_lights");
            stack_cmd.push_str(&format!(" rej sigma {:.1} {:.1}", rejection_low, rejection_high));
            stack_cmd.push_str(" -norm=addscale -output_norm -weight=wfwhm");
            if group.is_osc {
                stack_cmd.push_str(" -rgb_equal");
            }
            stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
            script.push_str(&format!("{}\n\n", stack_cmd));
        } else {
            // Multiple filters - stack each filter separately
            for (filter_name, indices) in &filter_groups {
                if indices.len() < 2 {
                    script.push_str(&format!(
                        "# {} ({} frame) - SKIPPED (need >= 2 frames)\n\n",
                        filter_name, indices.len()
                    ));
                    continue;
                }

                let safe_filter = filter_name.to_lowercase().replace(' ', "_");
                let output_name = format!("{}_{}_stacked", safe_filter, camera_suffix);
                let output_path = masters_dir.join(&output_name);

                script.push_str(&format!(
                    "# Stack {} {} ({} frames)\n",
                    camera_type, filter_name, indices.len()
                ));

                // Unselect all, then select frames for this filter
                script.push_str(&format!("unselect r_pp_lights 1 {}\n", total_frames));

                let ranges = indices_to_ranges(indices);
                for (start, end) in &ranges {
                    script.push_str(&format!("select r_pp_lights {} {}\n", start, end));
                }

                let mut stack_cmd = format!("stack r_pp_lights");
                stack_cmd.push_str(&format!(" rej sigma {:.1} {:.1}", rejection_low, rejection_high));
                stack_cmd.push_str(" -filter-included");
                stack_cmd.push_str(" -norm=addscale -output_norm -weight=wfwhm");
                if group.is_osc {
                    stack_cmd.push_str(" -rgb_equal");
                }
                stack_cmd.push_str(&format!(" -out={}", output_path.to_string_lossy()));
                script.push_str(&format!("{}\n\n", stack_cmd));
            }
        }
    }

    // Process Mono pipeline
    if let Some(ref mono) = collected.mono {
        generate_pipeline(
            &mut script,
            mono,
            &masters_dir,
            focal_length,
            pixel_size,
            rejection_low,
            rejection_high,
        );
    }

    // Process OSC pipeline
    if let Some(ref osc) = collected.osc {
        generate_pipeline(
            &mut script,
            osc,
            &masters_dir,
            focal_length,
            pixel_size,
            rejection_low,
            rejection_high,
        );
    }

    script.push_str("close\n");

    // Write the script
    let script_path = export_dir.join("02_register_and_stack.ssf");
    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write registration script to {:?}", script_path))?;

    println!("✅ Generated registration script: {:?}", script_path);
    Ok(script_path)
}

/// Convert a list of indices to (start, end) ranges
fn indices_to_ranges(indices: &[usize]) -> Vec<(usize, usize)> {
    if indices.is_empty() {
        return Vec::new();
    }

    let mut sorted = indices.to_vec();
    sorted.sort();

    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &idx in sorted.iter().skip(1) {
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

/// Run the complete Siril export pipeline (LEGACY - uses single registration script)
///
/// This orchestrates all export steps:
/// 1. Create masters (if 00_create_masters.ssf exists)
/// 2. Calibrate light frames (01_calibrate_lights.ssf)
/// 3. Collect calibrated frames to dimension-based directories
/// 4. Generate registration script based on actual dimension groups
/// 5. Register and stack (dynamically generated 02_register_and_stack.ssf)
///
/// **NOTE**: For multi-group scenarios (different telescopes/cameras), use
/// `run_export_pipeline_v2` which uses per-branch registration.
pub fn run_export_pipeline(
    export_dir: &Path,
    app_handle: &AppHandle,
) -> Result<()> {
    println!("🚀 Starting Siril export pipeline for: {:?}", export_dir);

    // Find Siril CLI
    let siril_cli = find_siril_cli()
        .ok_or_else(|| anyhow::anyhow!(
            "Siril CLI not found. Please install Siril or set the path in settings."
        ))?;
    println!("  Using Siril CLI: {}", siril_cli);

    // Determine which scripts exist
    let masters_script = export_dir.join("00_create_masters.ssf");
    let calibrate_script = export_dir.join("01_calibrate_lights.ssf");

    let has_masters = masters_script.exists();
    let has_calibrate = calibrate_script.exists();

    if !has_calibrate {
        return Err(anyhow::anyhow!(
            "Missing required script: 01_calibrate_lights.ssf"
        ));
    }

    // Calculate total steps: masters(opt) + calibrate + collect + generate script + register/stack
    let total_steps = if has_masters { 5 } else { 4 };
    let mut current_step = 1;

    // Step 1: Create masters (optional)
    if has_masters {
        println!("\n📍 Step {}/{}: Creating calibration masters...", current_step, total_steps);
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilCreatingMasters,
                progress: (current_step as f64 / total_steps as f64) * 100.0 * 0.1,
                message: format!("Step {}/{}: Creating calibration masters...", current_step, total_steps),
                current_file: None,
            },
        );

        run_siril_script(&siril_cli, &masters_script, app_handle)?;
        current_step += 1;
    }

    // Step 2: Calibrate lights
    println!("\n📍 Step {}/{}: Calibrating light frames...", current_step, total_steps);
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::SirilCalibrating,
            progress: (current_step as f64 / total_steps as f64) * 100.0 * 0.3,
            message: format!("Step {}/{}: Calibrating light frames...", current_step, total_steps),
            current_file: None,
        },
    );

    run_siril_script(&siril_cli, &calibrate_script, app_handle)?;
    current_step += 1;

    // Step 3: Collect calibrated frames
    println!("\n📍 Step {}/{}: Collecting calibrated frames...", current_step, total_steps);
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: (current_step as f64 / total_steps as f64) * 100.0 * 0.5,
            message: format!("Step {}/{}: Collecting calibrated frames...", current_step, total_steps),
            current_file: None,
        },
    );

    let collected = collect_calibrated_frames(export_dir, app_handle)?;
    if collected.total() == 0 {
        return Err(anyhow::anyhow!(
            "No calibrated frames found. Check calibration script output."
        ));
    }
    println!("   Collected: {} mono, {} OSC frames",
        collected.mono_count(), collected.osc_count());
    current_step += 1;

    // Step 4: Generate dimension-aware registration script
    println!("\n📍 Step {}/{}: Generating registration script...", current_step, total_steps);
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::GeneratingScripts,
            progress: (current_step as f64 / total_steps as f64) * 100.0 * 0.55,
            message: format!("Step {}/{}: Generating registration script...",
                current_step, total_steps),
            current_file: None,
        },
    );

    // Default parameters - these can be made configurable in the future
    let focal_length = 500.0;  // mm - common default
    let pixel_size = 3.76;     // um - common for ASI/QHY cameras
    let rejection_low = 2.5;
    let rejection_high = 2.5;

    let register_script = generate_registration_script(
        &collected,
        export_dir,
        focal_length,
        pixel_size,
        rejection_low,
        rejection_high,
    )?;
    current_step += 1;

    // Step 5: Register and stack
    println!("\n📍 Step {}/{}: Registering and stacking...", current_step, total_steps);
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::SirilRegistering,
            progress: (current_step as f64 / total_steps as f64) * 100.0 * 0.7,
            message: format!("Step {}/{}: Registering and stacking...", current_step, total_steps),
            current_file: None,
        },
    );

    run_siril_script(&siril_cli, &register_script, app_handle)?;

    // Complete!
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::Complete,
            progress: 100.0,
            message: "Export pipeline complete!".to_string(),
            current_file: None,
        },
    );

    println!("\n✅ Export pipeline complete!");
    println!("   Check masters/ directory for stacked results");

    Ok(())
}

/// Run the complete Siril export pipeline V2 (MULTI-GROUP FIX)
///
/// This version handles multi-group scenarios (different telescopes, cameras, focal lengths)
/// correctly by using per-branch registration with branch-specific optical parameters.
///
/// Steps:
/// 1. Create masters (if 00_create_masters.ssf exists)
/// 2. Calibrate light frames (01_calibrate_lights.ssf)
/// 3. Per-branch registration (02_register_branches.ssf) - per-branch plate solving
/// 4. Collect r_pp_lights using branch metadata (NOT FITS scanning)
/// 5. Copy to all_mono/ and all_osc/ directories
/// 6. Run global homography registration -2pass (mono and OSC separately)
/// 7. Copy globally registered frames to stacking directories
/// 8. Generate and run per-group stacking scripts
///
/// **CRITICAL**: Uses per-branch plate solving with branch-specific focal length
/// and pixel size, preventing "Cannot add an image with different properties" errors.
pub fn run_export_pipeline_v2(
    export_dir: &Path,
    config: &crate::export::models::ExportConfig,
    app_handle: &AppHandle,
) -> Result<()> {
    println!("🚀 Starting Siril export pipeline V2 (multi-group) for: {:?}", export_dir);

    // Find Siril CLI
    let siril_cli = find_siril_cli()
        .ok_or_else(|| anyhow::anyhow!(
            "Siril CLI not found. Please install Siril or set the path in settings."
        ))?;
    println!("  Using Siril CLI: {}", siril_cli);

    // Determine which scripts exist
    let masters_script = export_dir.join("00_create_masters.ssf");
    let calibrate_script = export_dir.join("01_calibrate_lights.ssf");
    let register_branches_script = export_dir.join("02_register_branches.ssf");

    let has_masters = masters_script.exists();
    let has_calibrate = calibrate_script.exists();
    let has_register_branches = register_branches_script.exists();

    if !has_calibrate {
        return Err(anyhow::anyhow!(
            "Missing required script: 01_calibrate_lights.ssf"
        ));
    }

    if !has_register_branches {
        return Err(anyhow::anyhow!(
            "Missing required script: 02_register_branches.ssf. \
             Please regenerate export scripts or use the legacy pipeline."
        ));
    }

    // Step 1: Create masters (optional)
    if has_masters {
        println!("\n📍 Step 1: Creating calibration masters...");
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilCreatingMasters,
                progress: 5.0,
                message: "Creating calibration masters...".to_string(),
                current_file: None,
            },
        );

        run_siril_script(&siril_cli, &masters_script, app_handle)?;
    }

    // Step 2: Calibrate lights
    println!("\n📍 Step 2: Calibrating light frames...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::SirilCalibrating,
            progress: 10.0,
            message: "Calibrating light frames...".to_string(),
            current_file: None,
        },
    );

    run_siril_script(&siril_cli, &calibrate_script, app_handle)?;

    // Step 3: Per-branch registration with branch-specific optics
    println!("\n📍 Step 3: Per-branch registration (plate solving with branch optics)...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::SirilRegistering,
            progress: 20.0,
            message: "Per-branch registration with branch-specific optics...".to_string(),
            current_file: None,
        },
    );

    run_siril_script(&siril_cli, &register_branches_script, app_handle)?;

    // Step 4: Load branch metadata for grouping (NOT scanning FITS files!)
    // We need to reload the export data to get branch info
    // For now, we'll use the FITS scanning approach but it should use cached branch data
    // TODO: Pass branches from caller or cache them
    println!("\n📍 Step 4: Collecting registered frames using branch metadata...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 30.0,
            message: "Collecting registered frames...".to_string(),
            current_file: None,
        },
    );

    // For now, use the FITS scanning approach (will be replaced with branch metadata)
    let stacking_groups = collect_and_group_registered_frames(
        export_dir,
        &config.exptime_tolerance_mode,
        config.exptime_tolerance_value,
        app_handle,
    )?;

    if stacking_groups.is_empty() {
        return Err(anyhow::anyhow!(
            "No registered frames found. Check per-branch registration script output."
        ));
    }

    println!("   Found {} stacking groups", stacking_groups.len());

    // Step 5: Prepare global registration directories (all_mono/, all_osc/)
    println!("\n📍 Step 5: Preparing global registration directories...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 40.0,
            message: "Copying frames for global registration...".to_string(),
            current_file: None,
        },
    );

    let (mono_dir, osc_dir) = prepare_global_registration_directories(
        &stacking_groups,
        export_dir,
        app_handle,
    )?;

    // Step 6: Run global homography registration (mono and OSC separately)
    println!("\n📍 Step 6: Global homography registration...");

    let mut global_scripts = Vec::new();

    if let Some(ref mono_path) = mono_dir {
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilRegistering,
                progress: 50.0,
                message: "Generating global registration script for Mono frames...".to_string(),
                current_file: None,
            },
        );

        let script = generate_global_registration_script("mono", mono_path, export_dir)?;
        global_scripts.push(("Mono", script));
    }

    if let Some(ref osc_path) = osc_dir {
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilRegistering,
                progress: 55.0,
                message: "Generating global registration script for OSC frames...".to_string(),
                current_file: None,
            },
        );

        let script = generate_global_registration_script("osc", osc_path, export_dir)?;
        global_scripts.push(("OSC", script));
    }

    // Run global registration scripts
    for (camera_type, script) in &global_scripts {
        println!("\n   Running global registration for {} frames...", camera_type);
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilRegistering,
                progress: 60.0,
                message: format!("Global homography registration ({})...", camera_type),
                current_file: Some(script.file_name().unwrap_or_default().to_string_lossy().to_string()),
            },
        );

        run_siril_script(&siril_cli, script, app_handle)?;
    }

    // Step 7: Copy globally registered frames to stacking directories
    println!("\n📍 Step 7: Copying globally registered frames to stacking directories...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::CollectingCalibratedFrames,
            progress: 70.0,
            message: "Copying globally registered frames...".to_string(),
            current_file: None,
        },
    );

    // Create stacking directories and copy r_all_r_*.fit files
    for group in &stacking_groups {
        std::fs::create_dir_all(&group.output_dir)?;
    }

    // Copy from all_mono/ or all_osc/ based on group's camera type
    // Filter by the group's filter name (read from FITS header or use known index mapping)
    // For simplicity now, we copy all files and let Siril handle it
    // TODO: Implement proper filtering using branch metadata
    let process_dir = export_dir.join("process");
    for group in &stacking_groups {
        let source_dir = if group.is_osc {
            process_dir.join("all_osc")
        } else {
            process_dir.join("all_mono")
        };

        // Find r_all_r_*.fit files and copy to group directory
        if source_dir.exists() {
            let mut idx = 1;
            for entry in std::fs::read_dir(&source_dir)? {
                let entry = entry?;
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("r_all_r_") && name.ends_with(".fit") {
                        // Read FITS to check filter
                        if let Ok(meta) = get_fits_metadata(&path) {
                            let file_filter = meta.filter.unwrap_or_else(|| "Unfiltered".to_string());
                            if file_filter == group.filter {
                                let dest = group.output_dir.join(format!("r_all_r_{:05}.fit", idx));
                                std::fs::copy(&path, &dest)?;
                                idx += 1;
                            }
                        }
                    }
                }
            }
            println!(
                "   Copied {} frames to {:?}",
                idx - 1,
                group.output_dir.file_name().unwrap_or_default()
            );
        }
    }

    // Step 8: Generate and run stacking scripts
    println!("\n📍 Step 8: Generating stacking scripts...");
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::GeneratingScripts,
            progress: 75.0,
            message: "Generating per-group stacking scripts...".to_string(),
            current_file: None,
        },
    );

    let stacking_scripts = generate_all_stacking_scripts(
        &stacking_groups,
        export_dir,
        config.rejection_low,
        config.rejection_high,
    )?;

    println!("   Generated {} stacking scripts", stacking_scripts.len());

    // Run stacking scripts
    let total_stacking_steps = stacking_scripts.len();
    for (idx, script) in stacking_scripts.iter().enumerate() {
        let script_name = script.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        println!("\n📍 Stacking {}/{}: {}", idx + 1, total_stacking_steps, script_name);
        emit_progress(
            app_handle,
            ExportProgress {
                stage: ExportStage::SirilStacking,
                progress: 75.0 + (idx as f64 / total_stacking_steps as f64) * 20.0,
                message: format!("Stacking {}/{}: {}", idx + 1, total_stacking_steps, script_name),
                current_file: Some(script_name.to_string()),
            },
        );

        run_siril_script(&siril_cli, script, app_handle)?;
    }

    // Complete!
    emit_progress(
        app_handle,
        ExportProgress {
            stage: ExportStage::Complete,
            progress: 100.0,
            message: "Export pipeline complete!".to_string(),
            current_file: None,
        },
    );

    println!("\n✅ Export pipeline V2 complete!");
    println!("   Check masters/ directory for stacked results");
    println!("   Created {} stacked outputs", stacking_scripts.len());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_siril_output() {
        let (stage, _) = parse_siril_output("Converting files...", &ExportStage::Collecting);
        assert_eq!(stage, ExportStage::SirilCalibrating);

        let (stage, _) = parse_siril_output("Registering images...", &ExportStage::SirilCalibrating);
        assert_eq!(stage, ExportStage::SirilRegistering);

        let (stage, _) = parse_siril_output("Stacking 50 images", &ExportStage::SirilRegistering);
        assert_eq!(stage, ExportStage::SirilStacking);
    }

    #[test]
    fn test_extract_filename() {
        assert_eq!(
            extract_filename("Processing file: light_001.fit"),
            Some("light_001.fit".to_string())
        );
        assert_eq!(
            extract_filename("Loading M42_Ha.fits"),
            Some("M42_Ha.fits".to_string())
        );
        assert_eq!(extract_filename("No file here"), None);
    }

    #[test]
    fn test_progress_estimation() {
        assert!(estimate_progress(&ExportStage::SirilCalibrating, 0) >= 20.0);
        assert!(estimate_progress(&ExportStage::SirilRegistering, 0) >= 50.0);
        assert!(estimate_progress(&ExportStage::SirilStacking, 0) >= 80.0);
        assert!(estimate_progress(&ExportStage::Complete, 0) >= 100.0);
    }
}
