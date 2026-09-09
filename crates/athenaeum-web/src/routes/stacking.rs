// Stacking-pipeline routes (M1 Plan 5a Task 9) — mirror of
// `crates/athenaeum-tauri/src/commands/stacking.rs`. Thin wrappers only:
// extraction + handler call + error mapping. Business logic lives in
// `athenaeum_core::api::stacking`.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::stacking as api;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::paths::{CleanupWhat, WorkUsage};
use athenaeum_core::stacking::plan::{StackingPlan, Stage};
use athenaeum_core::stacking::run::StartedStacking;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::routes::scan_roots::allowed_roots_policy;
use crate::WebAppState;

pub use athenaeum_core::api::stacking::{
    StackingPaths, StackingRunDetail, StackingRunSummary, StackingSetConfig,
};

// ── Request structs ───────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStackingPlanArgs {
    pub set_id: i64,
    #[serde(default)]
    pub config: Option<StackingConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartStackingArgs {
    pub set_id: i64,
    #[serde(default)]
    pub config: Option<StackingConfig>,
    #[serde(default)]
    pub rerun_from: Option<Stage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelStackingArgs {
    pub run_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStackingRunsArgs {
    pub set_id: i64,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStackingRunArgs {
    pub run_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStackingConfigArgs {
    pub set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStackingConfigArgs {
    pub set_id: i64,
    pub config: StackingConfig,
    // Fix round 1, item 5: REQUIRED, not `#[serde(default)]` — the Tauri
    // side already rejects a call omitting `excludedFrameIds` (a required
    // positional argument there); an omitting web caller must not be able
    // to silently wipe a set's manual exclusions by defaulting to `[]`.
    pub excluded_frame_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStackingDefaultsArgs {
    pub config: StackingConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStackingPathsArgs {
    #[serde(default)]
    pub working: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStackingWorkUsageArgs {
    pub set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupStackingWorkArgs {
    pub set_id: i64,
    pub what: CleanupWhat,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// POST /api/get_stacking_plan
///
/// What a stacking run would do for `setId` right now: groups, gate
/// blockers, stale-stage report, folder/space state. Pure DB reads plus
/// cheap filesystem probes, run under `spawn_blocking` (same reasoning as
/// `preview_master_build`) so the work stays off the async executor.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_plan(
    State(state): State<WebAppState>,
    Json(args): Json<GetStackingPlanArgs>,
) -> Result<Json<StackingPlan>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::get_stacking_plan(&ctx, &policy, args.set_id, args.config)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Plan task panicked: {}", e),
        )
    })?
    .map_err(api_err)?;

    Ok(Json(result))
}

/// POST /api/start_stacking
///
/// Start a stacking run. Returns as soon as the run thread is spawned;
/// `stacking-progress` / `stacking-complete` SSE events are emitted via
/// `SseProgressEmitter` from that thread. Fix round 1, item 1: passes
/// `state.allowed_paths`'s policy — the SAME one `get_stacking_plan`/
/// `set_stacking_paths` use — since a config-supplied
/// `paths.workingDir`/`paths.outputDir` override is a caller-controlled
/// path, not necessarily an already-stored settings one. Final fix wave,
/// item 2: `api::start_stacking` runs the full `build_plan` (per-frame
/// hashing, master-flat reads on cardless flats) plus one `insert_group` per
/// group before it ever spawns the run thread — under `spawn_blocking` (same
/// reasoning as `get_stacking_plan`) so that work stays off the async
/// executor.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_stacking(
    State(state): State<WebAppState>,
    Json(args): Json<StartStackingArgs>,
) -> Result<Json<StartedStacking>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let ctx = state.ctx.clone();
    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let result = tokio::task::spawn_blocking(move || {
        api::start_stacking(
            ctx,
            emitter,
            &policy,
            env!("CARGO_PKG_VERSION").to_string(),
            args.set_id,
            args.config,
            args.rerun_from,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Start task panicked: {}", e),
        )
    })?
    .map_err(api_err)?;

    Ok(Json(result))
}

/// POST /api/cancel_stacking
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_stacking(
    State(state): State<WebAppState>,
    Json(args): Json<CancelStackingArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_stacking(&state.ctx, args.run_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_stacking_runs
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_runs(
    State(state): State<WebAppState>,
    Json(args): Json<GetStackingRunsArgs>,
) -> Result<Json<Vec<StackingRunSummary>>, (StatusCode, String)> {
    api::get_stacking_runs(&state.ctx, args.set_id, args.limit)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_stacking_run
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_run(
    State(state): State<WebAppState>,
    Json(args): Json<GetStackingRunArgs>,
) -> Result<Json<StackingRunDetail>, (StatusCode, String)> {
    api::get_stacking_run(&state.ctx, args.run_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_stacking_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_config(
    State(state): State<WebAppState>,
    Json(args): Json<GetStackingConfigArgs>,
) -> Result<Json<StackingSetConfig>, (StatusCode, String)> {
    api::get_stacking_config(&state.ctx, args.set_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/set_stacking_config
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_stacking_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetStackingConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_stacking_config(
        &state.ctx,
        args.set_id,
        args.config,
        args.excluded_frame_ids,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/get_stacking_defaults
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_defaults(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<StackingConfig>, (StatusCode, String)> {
    api::get_stacking_defaults(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/set_stacking_defaults
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_stacking_defaults(
    State(state): State<WebAppState>,
    Json(args): Json<SetStackingDefaultsArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_stacking_defaults(&state.ctx, args.config)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/reset_stacking_defaults
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_stacking_defaults(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<StackingConfig>, (StatusCode, String)> {
    api::reset_stacking_defaults(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_stacking_paths
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_paths(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<StackingPaths>, (StatusCode, String)> {
    api::get_stacking_paths(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/set_stacking_paths
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_stacking_paths(
    State(state): State<WebAppState>,
    Json(args): Json<SetStackingPathsArgs>,
) -> Result<Json<StackingPaths>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::set_stacking_paths(&state.ctx, &policy, args.working, args.output)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_stacking_work_usage
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_work_usage(
    State(state): State<WebAppState>,
    Json(args): Json<GetStackingWorkUsageArgs>,
) -> Result<Json<WorkUsage>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result =
        tokio::task::spawn_blocking(move || api::get_stacking_work_usage(&ctx, args.set_id))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Work-usage task panicked: {}", e),
                )
            })?
            .map_err(api_err)?;

    Ok(Json(result))
}

/// POST /api/cleanup_stacking_work
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cleanup_stacking_work(
    State(state): State<WebAppState>,
    Json(args): Json<CleanupStackingWorkArgs>,
) -> Result<Json<u64>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        api::cleanup_stacking_work(&ctx, args.set_id, args.what)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cleanup task panicked: {}", e),
        )
    })?
    .map_err(api_err)?;

    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::tests::test_state;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    /// `POST /api/get_stacking_defaults` with `{}` is settings-only (no DB
    /// row read past `SettingsManager`'s own precedence) — 200, and the
    /// decoded body's `version` is the current `StackingConfig` version (1).
    #[tokio::test]
    async fn get_stacking_defaults_no_db_needed() {
        let state = test_state(None);
        let router = crate::routes::build_router(state, None);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/get_stacking_defaults")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["version"], 1);
    }

    /// `POST /api/get_stacking_plan` proves the route is registered past the
    /// auth layer: with no DB behind `test_state()`, the handler's own DB
    /// lookup fails and surfaces as 500 rather than 404 (route not found) or
    /// 401 (auth rejected).
    #[tokio::test]
    async fn get_stacking_plan_route_registered() {
        let state = test_state(None);
        let router = crate::routes::build_router(state, None);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/get_stacking_plan")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"setId":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
