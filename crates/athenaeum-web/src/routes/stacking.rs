// Stacking-pipeline routes (M1 Plan 5a Task 9) — mirror of
// `crates/athenaeum-tauri/src/commands/stacking.rs`. Thin wrappers only:
// extraction + handler call + error mapping. Business logic lives in
// `athenaeum_core::api::stacking`.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
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
    StackingPaths, StackingPresets, StackingRunDetail, StackingRunSummary, StackingSetConfig,
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

/// POST /api/get_stacking_presets
///
/// The three built-in stacking presets (Default / Fast preview / Maximum
/// quality) — pure, no DB, mirrors `get_stacking_defaults`'s "no live
/// catalog needed" contract.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_stacking_presets(
    Json(_): Json<serde_json::Value>,
) -> Result<Json<StackingPresets>, (StatusCode, String)> {
    Ok(Json(api::get_stacking_presets()))
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

// ── Master-light preview (M4d Task 3, ruling R-M4d-5) ────────────────────

/// The preview's arguments, shared by the two entry points below: the
/// browser-friendly `GET` with query parameters and the `POST` mirror of the
/// Tauri command. `kind` decodes as [`api::MasterLightKind`]'s camelCase wire
/// form (`master` / `drizzle` / `weightMap`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterLightPreviewArgs {
    pub run_id: i64,
    pub group_key: String,
    pub kind: api::MasterLightKind,
    #[serde(default)]
    pub max_px: Option<u32>,
}

/// Render (or serve from the on-disk cache) one written master light,
/// off the async executor — the render is CPU-bound, exactly like
/// `routes::images::get_frame_preview`'s.
///
/// Two things that route does and this one deliberately does not:
/// - **No VNG gate.** A master light is an integrated float image with no
///   CFA pattern, so `rustafits_processor::needs_vng_gate` is false for it
///   at every resolution a preview asks for.
/// - **No `image_semaphore` permit.** That semaphore bounds the blink
///   viewer's *prefetch*, which can have dozens of full-resolution renders
///   in flight; here the fan-out is a run's group count (a handful, once per
///   panel mount, at a quarter resolution) and the CPU is already bounded by
///   `ctx.image_pool`, which `process_fits_to_jpeg` runs on. Queueing
///   thumbnails behind the blink viewer's permit would make the Results
///   panel wait on an unrelated feature.
async fn render_master_light_preview(
    state: WebAppState,
    args: MasterLightPreviewArgs,
) -> Result<Response, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let max_px = args.max_px.unwrap_or(api::DEFAULT_MASTER_PREVIEW_MAX_PX);
    let bytes = tokio::task::spawn_blocking(move || {
        api::get_master_light_preview(&ctx, args.run_id, &args.group_key, args.kind, max_px)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Master-preview task panicked: {}", e),
        )
    })?
    .map_err(api_err)?;

    Ok(([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

/// `GET /api/stacking/master-preview?runId=&groupKey=&kind=&maxPx=` — raw
/// JPEG bytes with `Content-Type: image/jpeg`; 404 for an unknown run, an
/// output this run never wrote, or a master file gone from disk.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_master_light_preview_query(
    State(state): State<WebAppState>,
    Query(args): Query<MasterLightPreviewArgs>,
) -> Result<Response, (StatusCode, String)> {
    render_master_light_preview(state, args).await
}

/// `POST /api/get_master_light_preview` — the one-for-one mirror of the
/// Tauri command of the same name, and what the frontend's `api.invoke`
/// calls on the web target (`httpApi.invoke` already turns an `image/*`
/// response into bytes). Same body, same status codes, as the `GET` above.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_master_light_preview(
    State(state): State<WebAppState>,
    Json(args): Json<MasterLightPreviewArgs>,
) -> Result<Response, (StatusCode, String)> {
    render_master_light_preview(state, args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::tests::test_state;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    /// Plan 5b Task 1, Step 2: `POST /api/get_stacking_presets` with `{}` is
    /// pure (no DB at all) — 200, and `fastPreview.output.cleanup` is the
    /// `DeleteIntermediates` transform `stacking::config::preset` applies.
    #[tokio::test]
    async fn get_stacking_presets_route() {
        let state = test_state(None);
        let router = crate::routes::build_router(state, None);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/get_stacking_presets")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["fastPreview"]["output"]["cleanup"],
            "deleteIntermediates"
        );
    }

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

    // ── M4d Task 3: the master-light preview ────────────────────────────

    /// A `WebAppState` backed by a real (file-based, temp) catalog — the
    /// preview route resolves a `master_lights` row before it renders
    /// anything, so the shared `test_state(None)` (deliberately DB-less)
    /// cannot exercise it. Mirrors `routes::analysis::tests::test_state`.
    fn db_test_state(db: athenaeum_core::db::Database) -> WebAppState {
        use athenaeum_core::cache::MemoryImageCache;
        use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
        use athenaeum_core::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, RwLock};

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
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<crate::events::SseEvent>(16);
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

    /// Seeds a frame set, a finished run and one `master_lights` row whose
    /// `path` is a small real FITS written into `dir`. Returns the run id
    /// and the group key.
    fn seed_master_light(
        conn: &rusqlite::Connection,
        dir: &std::path::Path,
        working_dir: &std::path::Path,
    ) -> (i64, String) {
        use athenaeum_core::db::stacking::{insert_run, NewMasterLight, NewRun};

        conn.execute(
            "INSERT INTO frames_set (name) VALUES ('LDN 1272')",
            rusqlite::params![],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        let run_id = insert_run(
            conn,
            &NewRun {
                frames_set_id: set_id,
                config_json: "{}",
                config_hash: "hash",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working_dir.to_str().unwrap(),
                output_dir: dir.to_str().unwrap(),
            },
        )
        .unwrap();

        // A 32x24 gradient is enough for the renderer to produce a real
        // JPEG; the route's contract is the bytes' format, not their content.
        let (width, height) = (32usize, 24usize);
        let data: Vec<f32> = (0..width * height).map(|i| i as f32).collect();
        let master = dir.join("master_light.fits");
        athenaeum_core::fits_writer::write_fits_f32(&master, width, height, 1, &data, &[]).unwrap();

        let group_key = "mono__NoFilter__bin1__60s".to_string();
        athenaeum_core::db::stacking::insert_master_light(
            conn,
            &NewMasterLight {
                frames_set_id: set_id,
                run_id,
                group_key: &group_key,
                kind: "master",
                path: master.to_str().unwrap(),
                format: "fits",
                width: width as i64,
                height: height as i64,
                channels: 1,
                frames: 4,
                total_exposure_s: Some(240.0),
            },
        )
        .unwrap();

        (run_id, group_key)
    }

    /// Brief Step 2: `GET /api/stacking/master-preview` on a seeded catalog
    /// returns `200 image/jpeg` with JPEG magic; an unknown run is 404, not
    /// a 500 and not an empty 200.
    #[tokio::test]
    async fn master_preview_route_renders_jpeg_and_404s_an_unknown_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let working = tempfile::TempDir::new().unwrap();
        let db = athenaeum_core::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let (run_id, group_key) = seed_master_light(&db.conn(), tmp.path(), working.path());
        let state = db_test_state(db);
        let router = crate::routes::build_router(state, None);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/stacking/master-preview?runId={run_id}&groupKey={group_key}\
                         &kind=master&maxPx=256"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/jpeg")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            body.starts_with(&[0xFF, 0xD8, 0xFF]),
            "expected JPEG magic, got {:?}",
            &body[..body.len().min(8)]
        );

        // The cache file R-M4d-5 names must exist after the first render.
        let cache = working
            .path()
            .join("LDN_1272")
            .join("previews")
            .join(format!("run-{run_id}"))
            .join(format!("{group_key}_master_256.jpg"));
        assert!(cache.exists(), "preview cache not written: {cache:?}");

        let missing = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/stacking/master-preview?runId={}&groupKey={group_key}\
                         &kind=master&maxPx=256",
                        run_id + 999
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    /// The same handler is also reachable as `POST
    /// /api/get_master_light_preview` — the one-for-one mirror of the Tauri
    /// command, which is what the frontend's `api.invoke` uses on both
    /// targets (`httpApi.invoke` already decodes an `image/*` response into
    /// bytes). Registered past the auth layer like every other route here.
    #[tokio::test]
    async fn master_preview_post_mirror_renders_the_same_jpeg() {
        let tmp = tempfile::TempDir::new().unwrap();
        let working = tempfile::TempDir::new().unwrap();
        let db = athenaeum_core::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let (run_id, group_key) = seed_master_light(&db.conn(), tmp.path(), working.path());
        let state = db_test_state(db);
        let router = crate::routes::build_router(state, None);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/get_master_light_preview")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"runId":{run_id},"groupKey":"{group_key}","kind":"master","maxPx":256}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(body.starts_with(&[0xFF, 0xD8, 0xFF]), "expected JPEG magic");
    }
}
