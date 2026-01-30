//! Stacking-related Tauri commands
//!
//! Commands for managing and executing stacking jobs using the pure Rust stacking pipeline.

use crate::commands::AppState;
use crate::stacking::{
    self,
    executor::{ExecutorConfig, FfiExecutor, FfiStepResult, StepStats},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, State};

// ============================================================================
// Types
// ============================================================================

/// Status of a stacking job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackingJobStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for StackingJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackingJobStatus::Pending => write!(f, "pending"),
            StackingJobStatus::Running => write!(f, "running"),
            StackingJobStatus::Paused => write!(f, "paused"),
            StackingJobStatus::Completed => write!(f, "completed"),
            StackingJobStatus::Failed => write!(f, "failed"),
            StackingJobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for StackingJobStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(StackingJobStatus::Pending),
            "running" => Ok(StackingJobStatus::Running),
            "paused" => Ok(StackingJobStatus::Paused),
            "completed" => Ok(StackingJobStatus::Completed),
            "failed" => Ok(StackingJobStatus::Failed),
            "cancelled" => Ok(StackingJobStatus::Cancelled),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

/// Status of a stacking step
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackingStepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for StackingStepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackingStepStatus::Pending => write!(f, "pending"),
            StackingStepStatus::Running => write!(f, "running"),
            StackingStepStatus::Completed => write!(f, "completed"),
            StackingStepStatus::Failed => write!(f, "failed"),
            StackingStepStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl std::str::FromStr for StackingStepStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(StackingStepStatus::Pending),
            "running" => Ok(StackingStepStatus::Running),
            "completed" => Ok(StackingStepStatus::Completed),
            "failed" => Ok(StackingStepStatus::Failed),
            "skipped" => Ok(StackingStepStatus::Skipped),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

/// Type of stacking step
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackingStepType {
    CreateMasterBias,
    CreateMasterDark,
    CreateMasterFlat,
    CalibrateLights,
    MeasureQuality,
    PlatesolveFrames,
    RegisterFrames,
    StackFrames,
}

impl std::fmt::Display for StackingStepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackingStepType::CreateMasterBias => write!(f, "create_master_bias"),
            StackingStepType::CreateMasterDark => write!(f, "create_master_dark"),
            StackingStepType::CreateMasterFlat => write!(f, "create_master_flat"),
            StackingStepType::CalibrateLights => write!(f, "calibrate_lights"),
            StackingStepType::MeasureQuality => write!(f, "measure_quality"),
            StackingStepType::PlatesolveFrames => write!(f, "platesolve_frames"),
            StackingStepType::RegisterFrames => write!(f, "register_frames"),
            StackingStepType::StackFrames => write!(f, "stack_frames"),
        }
    }
}

impl std::str::FromStr for StackingStepType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "create_master_bias" => Ok(StackingStepType::CreateMasterBias),
            "create_master_dark" => Ok(StackingStepType::CreateMasterDark),
            "create_master_flat" => Ok(StackingStepType::CreateMasterFlat),
            "calibrate_lights" => Ok(StackingStepType::CalibrateLights),
            "measure_quality" => Ok(StackingStepType::MeasureQuality),
            "platesolve_frames" => Ok(StackingStepType::PlatesolveFrames),
            "register_frames" => Ok(StackingStepType::RegisterFrames),
            "stack_frames" => Ok(StackingStepType::StackFrames),
            _ => Err(format!("Unknown step type: {}", s)),
        }
    }
}

/// A single step in a stacking job
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingStep {
    pub id: i64,
    pub job_id: i64,
    pub step_order: i32,
    pub step_type: String,
    pub step_name: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
    pub output_files: Vec<String>,
    pub stats: Option<StepStats>,
}

/// A stacking job with all its steps
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingJob {
    pub id: i64,
    pub frame_set_id: i64,
    pub output_dir: String,
    pub config: ExecutorConfig,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
    pub steps: Vec<StackingStep>,
}

/// Progress event for stacking operations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingProgress {
    pub job_id: i64,
    pub step_order: i32,
    pub total_steps: i32,
    pub step_type: String,
    pub step_progress: f32,
    pub overall_progress: f32,
    pub message: String,
    pub current_file: Option<String>,
    pub frames_processed: Option<u32>,
    pub frames_total: Option<u32>,
}

// ============================================================================
// Commands
// ============================================================================

/// Check if stacking is available (always true with pure Rust implementation)
#[tauri::command]
pub async fn check_stacking_available() -> Result<bool, String> {
    Ok(stacking::is_ffi_available())
}

/// Get default stacking configuration
#[tauri::command]
pub async fn get_default_stacking_config() -> Result<ExecutorConfig, String> {
    Ok(ExecutorConfig::default())
}

/// Create a new stacking job with planned steps
#[tauri::command]
pub async fn create_stacking_job(
    state: State<'_, AppState>,
    frame_set_id: i64,
    output_dir: String,
    config: ExecutorConfig,
) -> Result<StackingJob, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Serialize config to JSON
    let config_json = serde_json::to_string(&config).map_err(|e| e.to_string())?;

    // Create the job
    conn.execute(
        "INSERT INTO stacking_jobs (frame_set_id, output_dir, config, status, created_at)
         VALUES (?1, ?2, ?3, 'pending', datetime('now'))",
        params![frame_set_id, output_dir, config_json],
    )
    .map_err(|e| e.to_string())?;

    let job_id = conn.last_insert_rowid();

    // Plan steps based on configuration
    let steps = plan_stacking_steps(&conn, frame_set_id, &config)?;

    // Insert steps
    for (order, (step_type, step_name)) in steps.iter().enumerate() {
        conn.execute(
            "INSERT INTO stacking_steps (job_id, step_order, step_type, step_name, status)
             VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![job_id, order as i32, step_type, step_name],
        )
        .map_err(|e| e.to_string())?;
    }

    // Return the created job
    get_stacking_job_internal(&conn, job_id)
}

/// Get a stacking job with all its steps
#[tauri::command]
pub async fn get_stacking_job(
    state: State<'_, AppState>,
    job_id: i64,
) -> Result<StackingJob, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    get_stacking_job_internal(&conn, job_id)
}

/// Get all stacking jobs, optionally filtered by frame set
#[tauri::command]
pub async fn get_stacking_jobs(
    state: State<'_, AppState>,
    frame_set_id: Option<i64>,
) -> Result<Vec<StackingJob>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let mut jobs = Vec::new();

    let query = if frame_set_id.is_some() {
        "SELECT id FROM stacking_jobs WHERE frame_set_id = ?1 ORDER BY created_at DESC"
    } else {
        "SELECT id FROM stacking_jobs ORDER BY created_at DESC"
    };

    let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;

    let job_ids: Vec<i64> = if let Some(fs_id) = frame_set_id {
        stmt.query_map([fs_id], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };

    for job_id in job_ids {
        if let Ok(job) = get_stacking_job_internal(&conn, job_id) {
            jobs.push(job);
        }
    }

    Ok(jobs)
}

/// Global cancellation flags for running jobs
static CANCEL_FLAGS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<i64, Arc<AtomicBool>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Start executing a stacking job
#[tauri::command]
pub async fn start_stacking_job(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    job_id: i64,
) -> Result<(), String> {
    // First check the job status and get the database path
    let (db_path, job) = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        let job = get_stacking_job_internal(&conn, job_id)?;

        if job.status != "pending" {
            return Err(format!("Job is not in pending status: {}", job.status));
        }

        (db.path().to_string_lossy().to_string(), job)
    };

    // Create cancellation flag
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut flags = CANCEL_FLAGS.lock().unwrap();
        flags.insert(job_id, Arc::clone(&cancel_flag));
    }

    // Spawn the job execution in a background thread
    std::thread::spawn(move || {
        // Open a new database connection for this thread
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to open database for stacking job: {}", e);
                return;
            }
        };

        // Run the job
        if let Err(e) = execute_stacking_job(&conn, &app_handle, job_id, &job, &cancel_flag) {
            eprintln!("Stacking job {} failed: {}", job_id, e);
            // Update job status to failed
            let _ = conn.execute(
                "UPDATE stacking_jobs SET status = 'failed', error_message = ?1, completed_at = datetime('now') WHERE id = ?2",
                params![e.to_string(), job_id],
            );
        }

        // Clean up cancellation flag
        let mut flags = CANCEL_FLAGS.lock().unwrap();
        flags.remove(&job_id);
    });

    Ok(())
}

/// Cancel a running stacking job
#[tauri::command]
pub async fn cancel_stacking_job(
    state: State<'_, AppState>,
    job_id: i64,
) -> Result<(), String> {
    // Set the cancellation flag
    {
        let flags = CANCEL_FLAGS.lock().unwrap();
        if let Some(flag) = flags.get(&job_id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    // Update the job status
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    conn.execute(
        "UPDATE stacking_jobs SET status = 'cancelled', completed_at = datetime('now') WHERE id = ?1",
        params![job_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Delete a stacking job
#[tauri::command]
pub async fn delete_stacking_job(
    state: State<'_, AppState>,
    job_id: i64,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Delete the job (steps are deleted via CASCADE)
    conn.execute("DELETE FROM stacking_jobs WHERE id = ?1", params![job_id])
        .map_err(|e| e.to_string())?;

    Ok(())
}

// ============================================================================
// Internal Functions
// ============================================================================

/// Get a stacking job with all its steps (internal function)
fn get_stacking_job_internal(
    conn: &rusqlite::Connection,
    job_id: i64,
) -> Result<StackingJob, String> {
    // Get the job
    let job: (i64, i64, String, String, String, String, Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT id, frame_set_id, output_dir, config, status, created_at, started_at, completed_at, error_message
             FROM stacking_jobs WHERE id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .map_err(|e| format!("Job not found: {}", e))?;

    // Parse config
    let config: ExecutorConfig = serde_json::from_str(&job.3).map_err(|e| e.to_string())?;

    // Get steps
    let mut stmt = conn
        .prepare(
            "SELECT id, job_id, step_order, step_type, step_name, status, started_at, completed_at, error_message, output_files, stats
             FROM stacking_steps WHERE job_id = ?1 ORDER BY step_order",
        )
        .map_err(|e| e.to_string())?;

    let steps: Vec<StackingStep> = stmt
        .query_map(params![job_id], |row| {
            let output_files_json: Option<String> = row.get(9)?;
            let output_files: Vec<String> = output_files_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();

            let stats_json: Option<String> = row.get(10)?;
            let stats: Option<StepStats> = stats_json.and_then(|j| serde_json::from_str(&j).ok());

            Ok(StackingStep {
                id: row.get(0)?,
                job_id: row.get(1)?,
                step_order: row.get(2)?,
                step_type: row.get(3)?,
                step_name: row.get(4)?,
                status: row.get(5)?,
                started_at: row.get(6)?,
                completed_at: row.get(7)?,
                error_message: row.get(8)?,
                output_files,
                stats,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(StackingJob {
        id: job.0,
        frame_set_id: job.1,
        output_dir: job.2,
        config,
        status: job.4,
        created_at: job.5,
        started_at: job.6,
        completed_at: job.7,
        error_message: job.8,
        steps,
    })
}

/// Plan stacking steps based on frame set and configuration
fn plan_stacking_steps(
    conn: &rusqlite::Connection,
    frame_set_id: i64,
    _config: &ExecutorConfig,
) -> Result<Vec<(String, String)>, String> {
    let mut steps = Vec::new();

    // Check what calibration is available for this frame set
    let has_bias = check_calibration_available(conn, frame_set_id, "Bias")?;
    let has_dark = check_calibration_available(conn, frame_set_id, "Dark")?;
    let has_flat = check_calibration_available(conn, frame_set_id, "Flat")?;

    // Step 1: Create master bias if bias frames are available
    if has_bias {
        steps.push((
            "create_master_bias".to_string(),
            "Create Master Bias".to_string(),
        ));
    }

    // Step 2: Create master dark if dark frames are available
    if has_dark {
        steps.push((
            "create_master_dark".to_string(),
            "Create Master Dark".to_string(),
        ));
    }

    // Step 3: Create master flat if flat frames are available
    if has_flat {
        steps.push((
            "create_master_flat".to_string(),
            "Create Master Flat".to_string(),
        ));
    }

    // Step 4: Calibrate lights (always)
    steps.push((
        "calibrate_lights".to_string(),
        "Calibrate Light Frames".to_string(),
    ));

    // Step 5: Measure quality
    steps.push((
        "measure_quality".to_string(),
        "Measure Frame Quality".to_string(),
    ));

    // Step 6: Register frames
    steps.push((
        "register_frames".to_string(),
        "Register Frames".to_string(),
    ));

    // Step 7: Stack frames
    steps.push(("stack_frames".to_string(), "Stack Frames".to_string()));

    Ok(steps)
}

/// Check if calibration of a given type is available for a frame set
fn check_calibration_available(
    conn: &rusqlite::Connection,
    frame_set_id: i64,
    calibration_type: &str,
) -> Result<bool, String> {
    // Check if there are calibration links for this frame set
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT cstf.calibration_set_id)
             FROM calibration_set_to_frames cstf
             JOIN session_members sm ON cstf.source_id = sm.frame_id AND cstf.source_type = 'frame'
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             JOIN calibration_set cs ON cstf.calibration_set_id = cs.id
             WHERE n.frames_set_id = ?1 AND cs.imagetyp = ?2",
            params![frame_set_id, calibration_type],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(count > 0)
}

/// Execute a stacking job
fn execute_stacking_job(
    conn: &rusqlite::Connection,
    app_handle: &tauri::AppHandle,
    job_id: i64,
    job: &StackingJob,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Update job status to running
    conn.execute(
        "UPDATE stacking_jobs SET status = 'running', started_at = datetime('now') WHERE id = ?1",
        params![job_id],
    )
    .map_err(|e| e.to_string())?;

    // Create executor
    let executor = FfiExecutor::new(job.config.clone());

    // Collect frame data
    let (light_paths, bias_paths, dark_paths, flat_paths) =
        collect_frame_paths(conn, job.frame_set_id)?;

    let output_dir = PathBuf::from(&job.output_dir);
    std::fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;

    let total_steps = job.steps.len() as i32;
    let mut master_bias_path: Option<PathBuf> = None;
    let mut master_dark_path: Option<PathBuf> = None;
    let mut master_flat_path: Option<PathBuf> = None;
    let mut calibrated_paths: Vec<PathBuf> = Vec::new();
    let mut registered_paths: Vec<PathBuf> = Vec::new();

    for step in &job.steps {
        // Check for cancellation
        if cancel_flag.load(Ordering::SeqCst) {
            return Err("Job cancelled".to_string());
        }

        let step_order = step.step_order;

        // Update step status to running
        conn.execute(
            "UPDATE stacking_steps SET status = 'running', started_at = datetime('now') WHERE id = ?1",
            params![step.id],
        )
        .map_err(|e| e.to_string())?;

        // Emit progress
        let _ = app_handle.emit(
            "stacking-progress",
            StackingProgress {
                job_id,
                step_order,
                total_steps,
                step_type: step.step_type.clone(),
                step_progress: 0.0,
                overall_progress: (step_order as f32 / total_steps as f32) * 100.0,
                message: format!("Starting {}...", step.step_name),
                current_file: None,
                frames_processed: None,
                frames_total: None,
            },
        );

        // Execute the step
        let result = match step.step_type.as_str() {
            "create_master_bias" => {
                let output_path = output_dir.join("master_bias.fit");
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let result =
                    executor.create_master_bias(&bias_paths, &output_path, Some(progress_callback));
                if result.as_ref().map(|r| r.success).unwrap_or(false) {
                    master_bias_path = Some(output_path);
                }
                result
            }
            "create_master_dark" => {
                let output_path = output_dir.join("master_dark.fit");
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let result =
                    executor.create_master_dark(&dark_paths, &output_path, Some(progress_callback));
                if result.as_ref().map(|r| r.success).unwrap_or(false) {
                    master_dark_path = Some(output_path);
                }
                result
            }
            "create_master_flat" => {
                let output_path = output_dir.join("master_flat.fit");
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let result = executor.create_master_flat(
                    &flat_paths,
                    &output_path,
                    master_bias_path.as_deref(),
                    master_dark_path.as_deref(),
                    Some(progress_callback),
                );
                if result.as_ref().map(|r| r.success).unwrap_or(false) {
                    master_flat_path = Some(output_path);
                }
                result
            }
            "calibrate_lights" => {
                let calibrated_dir = output_dir.join("calibrated");
                std::fs::create_dir_all(&calibrated_dir).map_err(|e| e.to_string())?;
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let result = executor.calibrate_lights(
                    &light_paths,
                    &calibrated_dir,
                    master_bias_path.as_deref(),
                    master_dark_path.as_deref(),
                    master_flat_path.as_deref(),
                    Some(progress_callback),
                );
                if let Ok(ref r) = result {
                    calibrated_paths = r
                        .output_files
                        .iter()
                        .map(|p| PathBuf::from(p))
                        .collect();
                }
                result
            }
            "measure_quality" => {
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let (result, _metrics) = executor
                    .measure_quality(&calibrated_paths, Some(progress_callback))
                    .map_err(|e| e.to_string())?;
                Ok(result)
            }
            "register_frames" => {
                let registered_dir = output_dir.join("registered");
                std::fs::create_dir_all(&registered_dir).map_err(|e| e.to_string())?;
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                let result = executor.register_frames(
                    &calibrated_paths,
                    0, // Use first frame as reference
                    &registered_dir,
                    Some(progress_callback),
                );
                if let Ok(ref r) = result {
                    registered_paths = r
                        .output_files
                        .iter()
                        .map(|p| PathBuf::from(p))
                        .collect();
                }
                result
            }
            "stack_frames" => {
                let progress_callback = create_progress_callback(
                    app_handle.clone(),
                    job_id,
                    step_order,
                    total_steps,
                    step.step_type.clone(),
                );
                executor.stack_frames(&registered_paths, None, &output_dir, Some(progress_callback))
            }
            _ => Ok(FfiStepResult::skipped(format!(
                "Unknown step type: {}",
                step.step_type
            ))),
        };

        // Handle result
        match result {
            Ok(step_result) => {
                let status = if step_result.skipped {
                    "skipped"
                } else if step_result.success {
                    "completed"
                } else {
                    "failed"
                };

                let output_files_json = serde_json::to_string(&step_result.output_files).ok();
                let stats_json = serde_json::to_string(&step_result.stats).ok();

                conn.execute(
                    "UPDATE stacking_steps SET status = ?1, completed_at = datetime('now'), output_files = ?2, stats = ?3, error_message = ?4 WHERE id = ?5",
                    params![
                        status,
                        output_files_json,
                        stats_json,
                        step_result.error_message,
                        step.id
                    ],
                )
                .map_err(|e| e.to_string())?;

                if !step_result.success && !step_result.skipped {
                    return Err(step_result
                        .error_message
                        .unwrap_or_else(|| "Step failed".to_string()));
                }
            }
            Err(e) => {
                conn.execute(
                    "UPDATE stacking_steps SET status = 'failed', completed_at = datetime('now'), error_message = ?1 WHERE id = ?2",
                    params![e.to_string(), step.id],
                )
                .map_err(|e| e.to_string())?;

                return Err(e.to_string());
            }
        }

        // Emit completion progress
        let _ = app_handle.emit(
            "stacking-progress",
            StackingProgress {
                job_id,
                step_order,
                total_steps,
                step_type: step.step_type.clone(),
                step_progress: 100.0,
                overall_progress: ((step_order + 1) as f32 / total_steps as f32) * 100.0,
                message: format!("{} completed", step.step_name),
                current_file: None,
                frames_processed: None,
                frames_total: None,
            },
        );
    }

    // Update job status to completed
    conn.execute(
        "UPDATE stacking_jobs SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
        params![job_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Collect frame paths for a frame set
fn collect_frame_paths(
    conn: &rusqlite::Connection,
    frame_set_id: i64,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>), String> {
    // Get light frame paths
    let mut light_stmt = conn
        .prepare(
            "SELECT DISTINCT fi.path
             FROM session_members sm
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             JOIN frames f ON sm.frame_id = f.id
             JOIN files fi ON f.file_id = fi.id
             WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'",
        )
        .map_err(|e| e.to_string())?;

    let light_paths: Vec<PathBuf> = light_stmt
        .query_map([frame_set_id], |row| {
            let path: String = row.get(0)?;
            Ok(PathBuf::from(path))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Get calibration frame paths via calibration links
    let bias_paths = get_calibration_frame_paths(conn, frame_set_id, "Bias")?;
    let dark_paths = get_calibration_frame_paths(conn, frame_set_id, "Dark")?;
    let flat_paths = get_calibration_frame_paths(conn, frame_set_id, "Flat")?;

    Ok((light_paths, bias_paths, dark_paths, flat_paths))
}

/// Get calibration frame paths for a frame set
fn get_calibration_frame_paths(
    conn: &rusqlite::Connection,
    frame_set_id: i64,
    calibration_type: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT fi.path
             FROM calibration_set_to_frames cstf
             JOIN session_members sm ON cstf.source_id = sm.frame_id AND cstf.source_type = 'frame'
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             JOIN calibration_set cs ON cstf.calibration_set_id = cs.id
             JOIN calibration_set_frames csf ON cs.id = csf.set_id
             JOIN frames f ON csf.frame_id = f.id
             JOIN files fi ON f.file_id = fi.id
             WHERE n.frames_set_id = ?1 AND cs.imagetyp = ?2",
        )
        .map_err(|e| e.to_string())?;

    let paths: Vec<PathBuf> = stmt
        .query_map(params![frame_set_id, calibration_type], |row| {
            let path: String = row.get(0)?;
            Ok(PathBuf::from(path))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(paths)
}

/// Create a progress callback that emits events
fn create_progress_callback(
    app_handle: tauri::AppHandle,
    job_id: i64,
    step_order: i32,
    total_steps: i32,
    step_type: String,
) -> Box<dyn Fn(f32, &str) + Send + Sync> {
    Box::new(move |progress, message| {
        let _ = app_handle.emit(
            "stacking-progress",
            StackingProgress {
                job_id,
                step_order,
                total_steps,
                step_type: step_type.clone(),
                step_progress: progress * 100.0,
                overall_progress: ((step_order as f32 + progress) / total_steps as f32) * 100.0,
                message: message.to_string(),
                current_file: None,
                frames_processed: None,
                frames_total: None,
            },
        );
    })
}
