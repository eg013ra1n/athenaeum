// Analysis commands — frame-set star detection/PSF analysis and flat-contour
// plotting.
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in `athenaeum_core::api::analysis` (Task 12 conversion — see
// `.superpowers/sdd/p1-task-12-report.md`).

use tauri::State;

use athenaeum_core::analysis::config::AnalysisConfig;
use athenaeum_core::api::analysis as api;
use athenaeum_core::flat_analysis::FlatContourOpts;
use athenaeum_core::models::{FrameAnalysis, StarMetricsResponse};

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::analysis::{AnalyzeFrameSetResult, FlatContourPlotResponse};

// ========== Config Commands ==========

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_analysis_config(state: State<'_, AppState>) -> Result<AnalysisConfig, String> {
    api::get_analysis_config(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_analysis_config(state: State<'_, AppState>, config: AnalysisConfig) -> Result<(), String> {
    api::set_analysis_config(&state.ctx, config).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_analysis_config(state: State<'_, AppState>) -> Result<AnalysisConfig, String> {
    api::reset_analysis_config(&state.ctx).map_err(|e| e.to_string())
}

// ========== Analysis Commands ==========

/// Analyze all LIGHT frames in a frame set.
/// Emits `analysis-progress` / `analysis-complete` events via `TauriProgressEmitter`.
///
/// `api::analyze_frame_set` is a plain sync fn taking `&dyn ProgressEmitter`
/// (see its doc comment) — it can't be moved into `spawn_blocking` itself,
/// so this wrapper owns the blocking boundary: the emitter is constructed
/// *inside* the closure, then the shared handler is called synchronously
/// from there. Mirrors `commands/registration.rs::register_frame_set`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn analyze_frame_set(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    frame_set_id: i64,
    force: Option<bool>,
) -> Result<AnalyzeFrameSetResult, String> {
    let ctx = state.ctx.clone();

    tokio::task::spawn_blocking(move || {
        let emitter = TauriProgressEmitter(app_handle);
        api::analyze_frame_set(&ctx, frame_set_id, force, api::ANALYZE_THREAD_CAP, &emitter)
    })
    .await
    .map_err(|e| format!("Analysis task panicked: {}", e))?
    .map_err(|e| e.to_string())
}

/// Cancel an active analysis.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_analysis(frame_set_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    api::cancel_analysis(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

/// Get all stored analysis results for a frame set.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_analysis_for_frame_set(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<Vec<FrameAnalysis>, String> {
    api::get_analysis_for_frame_set(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_frame_star_metrics(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<StarMetricsResponse, String> {
    api::get_frame_star_metrics(&state.ctx, frame_id).await.map_err(|e| e.to_string())
}

// ========== Flat Contour Plot ==========
//
// PixInsight FlatContourPlot v1.3.1 port — see
// `athenaeum_core::api::analysis::compute_flat_contour_plot` for the
// business logic.

#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn compute_flat_contour_plot(
    state: State<'_, AppState>,
    file_id: i64,
    opts: FlatContourOpts,
) -> Result<FlatContourPlotResponse, String> {
    api::compute_flat_contour_plot(&state.ctx, file_id, opts).await.map_err(|e| e.to_string())
}
