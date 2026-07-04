// Compute-queue inspection/control commands — thin wrappers only.
// Business logic lives in `athenaeum_core::api::compute` (mirrors the
// `commands/analysis.rs` conversion pattern).

use tauri::State;

use athenaeum_core::api::compute as api;
use athenaeum_core::services::compute_queue::ComputeQueueEntry;

use super::AppState;

/// Snapshot of every queued/running compute job.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_compute_queue(state: State<'_, AppState>) -> Result<Vec<ComputeQueueEntry>, String> {
    Ok(api::get_compute_queue(&state.ctx))
}

/// Cancel a queued or running compute job.
///
/// NOTE param naming: matches the snake_case Rust-param convention used by
/// `cancel_analysis` (`frame_set_id: i64`) rather than a JS-camelCase name —
/// Tauri v2 maps the frontend's camelCase JS arg onto this Rust identifier
/// automatically, so `job_id` here receives the frontend's `jobId`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_compute_job(state: State<'_, AppState>, job_id: i64) -> Result<(), String> {
    api::cancel_compute_job(&state.ctx, job_id).map_err(|e| e.to_string())
}

/// Persist and apply the global compute-queue concurrency ceiling.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_compute_max_concurrent(state: State<'_, AppState>, n: usize) -> Result<(), String> {
    api::set_compute_max_concurrent(&state.ctx, n).map_err(|e| e.to_string())
}
