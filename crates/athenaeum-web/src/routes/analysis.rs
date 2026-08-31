// Analysis route handlers — business logic single-sourced in
// `athenaeum_core::api::analysis` (Task 12 conversion — see
// `.superpowers/sdd/p1-task-12-report.md`).
//
// Thin wrappers only: extraction + handler call + error mapping.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::analysis::config::AnalysisConfig;
use athenaeum_core::api::analysis as api;
use athenaeum_core::flat_analysis::FlatContourOpts;
use athenaeum_core::models::{FrameAnalysis, StarMetricsResponse};

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::analysis::{AnalyzeFrameSetResult, FlatContourPlotResponse};

// ── Request / Response structs ───────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeFrameSetArgs {
    pub frame_set_id: i64,
    pub force: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frame_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdArgs {
    pub frame_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAnalysisArgs {
    pub frame_set_id: i64,
}

/// Request body for `set_analysis_config`. The frontend calls
/// `api.invoke('set_analysis_config', { config })` per the Tauri named-arg
/// convention used by every `api.invoke` call, so the HTTP body is
/// `{ "config": { ... } }`, not a bare `AnalysisConfig`. See
/// `.superpowers/sdd/task-10-report.md` (Web wrapper rider).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAnalysisConfigArgs {
    pub config: AnalysisConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatContourPlotArgs {
    pub file_id: i64,
    pub opts: FlatContourOpts,
}

// ── Config routes ────────────────────────────────────────────────────────────

/// POST /api/get_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_analysis_config(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AnalysisConfig>, (StatusCode, String)> {
    api::get_analysis_config(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_analysis_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetAnalysisConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_analysis_config(&state.ctx, args.config).map(Json).map_err(api_err)
}

/// POST /api/reset_analysis_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_analysis_config(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<AnalysisConfig>, (StatusCode, String)> {
    api::reset_analysis_config(&state.ctx).map(Json).map_err(api_err)
}

// ── Query routes ─────────────────────────────────────────────────────────────

/// POST /api/get_analysis_for_frame_set
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_analysis_for_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<Vec<FrameAnalysis>>, (StatusCode, String)> {
    api::get_analysis_for_frame_set(&state.ctx, args.frame_set_id).map(Json).map_err(api_err)
}

// ── Frame set analysis (with SSE progress) ───────────────────────────────────

/// POST /api/analyze_frame_set
///
/// Analyzes all LIGHT frames in a frame set. Emits `analysis-progress` /
/// `analysis-complete` SSE events via `SseProgressEmitter`.
///
/// `api::analyze_frame_set` is a plain sync fn taking `&dyn ProgressEmitter`
/// (see its doc comment) — it can't be moved into `spawn_blocking` itself,
/// so this wrapper owns the blocking boundary: the emitter is constructed
/// *inside* the closure, then the shared handler is called synchronously
/// from there. Mirrors `routes/registration.rs::register_frame_set`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn analyze_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<AnalyzeFrameSetArgs>,
) -> Result<Json<AnalyzeFrameSetResult>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let event_tx = state.event_tx.clone();

    let result = tokio::task::spawn_blocking(move || {
        let emitter = SseProgressEmitter::new(event_tx);
        api::analyze_frame_set(&ctx, args.frame_set_id, args.force, api::ANALYZE_THREAD_CAP, &emitter)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Analysis task panicked: {}", e)))?
    .map_err(api_err)?;

    Ok(Json(result))
}

// ── Cancel analysis ──────────────────────────────────────────────────────────

/// POST /api/cancel_analysis
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_analysis(
    State(state): State<WebAppState>,
    Json(args): Json<CancelAnalysisArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_analysis(&state.ctx, args.frame_set_id).map(Json).map_err(api_err)
}

// ── Star metrics ─────────────────────────────────────────────────────────────

/// POST /api/get_frame_star_metrics
///
/// Get star metrics for a frame. Returns from DB if fresh, otherwise analyzes on-demand.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_frame_star_metrics(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<StarMetricsResponse>, (StatusCode, String)> {
    api::get_frame_star_metrics(&state.ctx, args.frame_id).await.map(Json).map_err(api_err)
}

// ── Flat contour plot (PixInsight FlatContourPlot port) ──────────────────────

/// POST /api/compute_flat_contour_plot
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn compute_flat_contour_plot(
    State(state): State<WebAppState>,
    Json(args): Json<FlatContourPlotArgs>,
) -> Result<Json<FlatContourPlotResponse>, (StatusCode, String)> {
    api::compute_flat_contour_plot(&state.ctx, args.file_id, args.opts).await.map(Json).map_err(api_err)
}

#[cfg(test)]
mod analysis_config_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// analysis config. Mirrors `settings::logging_config_tests::test_state`.
    fn test_state(db: Database) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: std::sync::Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
            collab_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `AnalysisConfig`. Exercises the handler via the wrapped
    /// `SetAnalysisConfigArgs` struct directly.
    #[tokio::test]
    async fn set_analysis_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let mut cfg = AnalysisConfig::default();
        cfg.max_stars = 750;

        let _ = set_analysis_config(State(state.clone()), Json(SetAnalysisConfigArgs { config: cfg }))
            .await
            .expect("valid config must be accepted");

        let resp = get_analysis_config(State(state), Json(serde_json::json!({})))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.max_stars, 750);
    }

    /// Pins the fix: the handler now requires the `{ "config": ... }`
    /// wrapper (matching `SetAnalysisConfigArgs`). Deserializing a bare
    /// `AnalysisConfig` JSON body must fail hard (serde error), not
    /// silently succeed with the wrong shape.
    #[test]
    fn bare_analysis_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::to_value(AnalysisConfig::default()).unwrap();

        let result: Result<SetAnalysisConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare AnalysisConfig body must NOT deserialize into SetAnalysisConfigArgs — \
             this is what closes the silent-mismatch hole (axum returns 422/400 for this shape)"
        );
    }
}
