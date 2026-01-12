//! Siril CLI runner
//!
//! Executes Siril scripts via siril-cli and captures progress.

use crate::export::models::{ExportProgress, ExportStage};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

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
        ExportStage::SirilCalibrating => 20.0,
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
