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

/// Collect calibrated frames from branch directories to separate directories
/// based on camera type (OSC vs Mono) only - NOT by dimensions.
///
/// ALL mono frames go to all_lights_mono/ (regardless of camera dimensions)
/// ALL OSC frames go to all_lights_osc/ (regardless of camera dimensions)
///
/// seqapplyreg -framing=max handles different camera dimensions by padding
/// smaller frames to match the largest.
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
                let dest_dir = if is_osc { &osc_dir } else { &mono_dir };
                let dest = dest_dir.join(name);

                // Copy file
                match std::fs::copy(path, &dest) {
                    Ok(_) => {
                        let frame = CollectedFrame {
                            filename: name.to_string(),
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
                                    current_file: Some(name.to_string()),
                                },
                            );
                        }
                    }
                    Err(e) => {
                        errors.push(format!("Failed to copy {}: {}", name, e));
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

/// Run the complete Siril export pipeline
///
/// This orchestrates all export steps:
/// 1. Create masters (if 00_create_masters.ssf exists)
/// 2. Calibrate light frames (01_calibrate_lights.ssf)
/// 3. Collect calibrated frames to dimension-based directories
/// 4. Generate registration script based on actual dimension groups
/// 5. Register and stack (dynamically generated 02_register_and_stack.ssf)
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
