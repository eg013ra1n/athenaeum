use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::CatalogOpenError;
use athenaeum_core::plate_solve::storage;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

fn get_dso_catalog(state: &WebAppState) -> Option<Arc<DsoCatalog>> {
    {
        let guard = state.ctx.dso_catalog.read().unwrap();
        if let Some(ref cat) = *guard {
            return Some(cat.clone());
        }
    }
    match DsoCatalog::load() {
        Ok(cat) => {
            let arc = Arc::new(cat);
            let mut guard = state.ctx.dso_catalog.write().unwrap();
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load DSO catalog");
            None
        }
    }
}

/// Resolve and open the additive density-tier catalog stack under
/// `<app-data>/catalogs/smac_gaia` (base → deepest), or a legacy single
/// `stars.smac` there as a 1-layer fallback. Opens fresh each call (mmap is
/// cheap); the batch path opens once and shares across workers.
fn resolve_layer_caches(
    state: &WebAppState,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".into(),
    ))?;
    let db_path = db.path();
    let parent = std::path::Path::new(&db_path).parent().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Cannot determine app data dir".into(),
    ))?;
    athenaeum_core::plate_solve::open_catalog_layers(parent).map_err(|e| match e {
        CatalogOpenError::NotInstalled { root } => (
            StatusCode::PRECONDITION_FAILED,
            format!(
                "solvemyastro star catalog not found under {}. \
                 Please download the Gaia DR3 prebuilt catalog.",
                root.display()
            ),
        ),
        CatalogOpenError::Open { dir, error } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to open star cache at {}: {error}", dir.display()),
        ),
    })
}

/// Open the installed quad-index packs under `<app-data>/catalogs/smac_index`
/// (empty when none are installed; a pack that does not open is skipped with a
/// warning). Opened once per batch, like the catalog layers.
fn resolve_index_packs(state: &WebAppState) -> Vec<Arc<solvemyastro::QuadIndex>> {
    let Some(db) = state.ctx.db.get() else {
        return Vec::new();
    };
    let db_path = db.path();
    match std::path::Path::new(&db_path).parent() {
        Some(parent) => athenaeum_core::plate_solve::open_index_packs(parent),
        None => Vec::new(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdArgs {
    pub frame_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchArgs {
    pub frame_ids: Vec<i64>,
}

/// Request body for `set_plate_solve_config`. The frontend calls
/// `api.invoke('set_plate_solve_config', { config })` per the Tauri
/// named-arg convention used by every `api.invoke` call, so the HTTP body is
/// `{ "config": { ... } }`, not a bare `PlateSolveConfig`. See
/// `.superpowers/sdd/task-10-report.md` (Web wrapper rider).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPlateSolveConfigArgs {
    pub config: PlateSolveConfig,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_plate_solve_config(
    State(state): State<WebAppState>,
) -> Result<Json<PlateSolveConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    Ok(Json(config::load_config(&conn)))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_plate_solve_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetPlateSolveConfigArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    config::save_config(&conn, &args.config).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(()))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn reset_plate_solve_config(
    State(state): State<WebAppState>,
) -> Result<Json<PlateSolveConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    let default_config = PlateSolveConfig::default();
    config::save_config(&conn, &default_config).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(default_config))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn plate_solve_batch(
    State(state): State<WebAppState>,
    Json(args): Json<BatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let layer_caches = resolve_layer_caches(&state)?;
    let index_packs = resolve_index_packs(&state);
    let dso = get_dso_catalog(&state);
    let prepared = athenaeum_core::api::plate_solve::prepare_plate_solve_batch(
        &state.ctx,
        &args.frame_ids,
        layer_caches,
        index_packs,
        dso,
    )
    .map_err(api_err)?;

    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let ctx = state.ctx.clone();
    // Fire-and-forget by design: progress rides SSE and the run ends the batch
    // itself (handle removed, complete event) on every path, so the request
    // returns at once and only the run's error is left to log.
    tokio::task::spawn_blocking(move || {
        if let Err(e) = athenaeum_core::api::plate_solve::run_plate_solve_batch(ctx, emitter, prepared) {
            tracing::error!(error = %e, "plate solve batch ended with an error");
        }
    });

    Ok(Json(()))
}

#[tracing::instrument(skip_all)]
pub async fn cancel_plate_solve(
    State(state): State<WebAppState>,
) -> Json<()> {
    let solves = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = solves.get(&0) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Json(())
}

// ========== Autofind Object Routes ==========

/// Fill `frame.object` from stored RA/Dec for frames in the batch that have
/// coordinates but no object name. Tight 0.2° proximity tolerance; labels
/// are written as `"Autofind: <designation>"`.
///
/// Returns immediately; progress is streamed via SSE on event names
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveObjectNameArgs {
    pub name: String,
}

/// POST /api/resolve_object_name
///
/// Mirrors the Tauri command: does this object name mean anything to the
/// solver? Used by the metadata editor while the user types.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn resolve_object_name(
    State(state): State<WebAppState>,
    Json(args): Json<ResolveObjectNameArgs>,
) -> Result<
    Json<Option<athenaeum_core::plate_solve::dso_lookup::ResolvedObject>>,
    (StatusCode, String),
> {
    Ok(Json(athenaeum_core::api::objects::resolve_object_name(
        &state.ctx, args.name,
    )))
}

/// `autofind-objects-progress` and `autofind-objects-complete`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn autofind_objects_from_coordinates(
    State(state): State<WebAppState>,
    Json(args): Json<BatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let dso = get_dso_catalog(&state);
    let prepared = athenaeum_core::api::plate_solve::prepare_autofind_objects(&state.ctx, dso)
        .map_err(api_err)?;

    let emitter = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let ctx = state.ctx.clone();
    let frame_ids = args.frame_ids;
    // Fire-and-forget by design: progress rides SSE and the run always sends
    // its complete event and removes the cancel handle, so only its error is
    // left to log.
    tokio::task::spawn_blocking(move || {
        if let Err(e) =
            athenaeum_core::api::plate_solve::run_autofind_objects(ctx, emitter, prepared, frame_ids)
        {
            tracing::error!(error = %e, "autofind batch ended with an error");
        }
    });

    Ok(Json(()))
}

#[tracing::instrument(skip_all)]
pub async fn cancel_autofind_objects(
    State(state): State<WebAppState>,
) -> Json<()> {
    let handles = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = handles.get(&1) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Json(())
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_plate_solve_result(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<Option<storage::PlateSolveRecord>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    storage::get_plate_solve(&conn, args.frame_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Delete the `plate_solves` row for a frame. Mirror of the Tauri command;
/// used by the metadata pane's "Revert WCS to FITS header" flow so the
/// stored WCS doesn't outlive the cleared ra/dec/rotation/objctra/objctdec
/// columns.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_plate_solve_for_frame(
    State(state): State<WebAppState>,
    Json(args): Json<FrameIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    storage::delete_plate_solve(&conn, args.frame_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// Note: no `rename_all` — matches the Tauri side and the snake_case
// `CatalogStatusInfo` TS interface (`star_count_approx`, `min_fov_deg`, …).
#[derive(Clone, Serialize)]
pub struct CatalogStatusInfo {
    pub name: String,
    pub density: u32,
    pub installed: bool,
    pub epoch: f64,
    pub star_count_approx: u64,
    pub size_bytes: u64,
    pub min_fov_deg: f64,
    pub mag_limit: f32,
}

/// Return one `CatalogStatusInfo` per declared density tier, merged with
/// on-disk installed state. Returns an empty Vec when no manifest is available.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_catalog_status(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<CatalogStatusInfo>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".to_string(),
    ))?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".to_string()))?
        .to_path_buf();
    let rows = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::gaia_prebuilt::tier_status(&app_data)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("catalog status task failed: {e}")))?;
    Ok(Json(rows.into_iter().map(|t| CatalogStatusInfo {
        name: format!("Gaia tier {} (≤{:.2}° FOV)", t.density, t.min_fov_deg),
        density: t.density,
        installed: t.installed,
        epoch: t.epoch,
        star_count_approx: t.star_count,
        size_bytes: t.size_bytes,
        min_fov_deg: t.min_fov_deg,
        mag_limit: 19.0,
    }).collect()))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_fov_summary(
    State(state): State<WebAppState>,
) -> Result<Json<athenaeum_core::plate_solve::FovSummary>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".to_string()))?;
    let conn = db.conn();
    athenaeum_core::plate_solve::frame_fov_summary(&conn)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDensityArgs {
    target_density: u32,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn download_catalog_layers(
    State(state): State<WebAppState>,
    Json(args): Json<TargetDensityArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let emitter = Arc::new(crate::events::SseProgressEmitter::new(state.event_tx.clone()));
    athenaeum_core::api::plate_solve::download_catalog_layers(
        &state.ctx,
        emitter,
        args.target_density,
    )
    .await
    .map(Json)
    .map_err(crate::routes::api_err)
}

/// One row per declared quad-index pack (`base`, `narrow`), merged with the
/// on-disk installed state. Mirror of the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_index_status(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<athenaeum_core::catalog::index_prebuilt::IndexStatusInfo>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".to_string(),
    ))?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".to_string()))?
        .to_path_buf();
    let rows = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::index_prebuilt::index_status(&app_data)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("index status task failed: {e}")))?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexPackNamesArgs {
    names: Vec<String>,
}

/// Download and install the named quad-index packs; progress rides the
/// `index-download-progress` SSE event. Mirror of the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn download_index_packs(
    State(state): State<WebAppState>,
    Json(args): Json<IndexPackNamesArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let emitter = Arc::new(crate::events::SseProgressEmitter::new(state.event_tx.clone()));
    athenaeum_core::api::plate_solve::download_index_packs(&state.ctx, emitter, args.names)
        .await
        .map(Json)
        .map_err(crate::routes::api_err)
}

#[cfg(test)]
mod plate_solve_config_tests {
    use super::*;
    use crate::events::SseEvent;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// these tests exercise actual `settings` table reads/writes for the
    /// plate solve config. Mirrors `settings::logging_config_tests::test_state`.
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
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            zip_cache: std::sync::Arc::new(athenaeum_core::zip_store::cache::ZipCache::for_tests()),
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
        }
    }

    /// Regression guard for the real frontend payload: `api.invoke` sends
    /// `{ "config": { ... } }`, per the Tauri named-arg convention — not a
    /// bare `PlateSolveConfig`. Exercises the handler via the wrapped
    /// `SetPlateSolveConfigArgs` struct directly.
    #[tokio::test]
    async fn set_plate_solve_config_then_get_reflects_change() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let state = test_state(db);

        let mut cfg = PlateSolveConfig::default();
        cfg.sip_order = 2;

        let _ = set_plate_solve_config(State(state.clone()), Json(SetPlateSolveConfigArgs { config: cfg }))
            .await
            .expect("valid config must be accepted");

        let resp = get_plate_solve_config(State(state)).await.unwrap().0;
        assert_eq!(resp.sip_order, 2);
    }

    /// Pins the fix: the handler now requires the `{ "config": ... }`
    /// wrapper (matching `SetPlateSolveConfigArgs`). Deserializing a bare
    /// `PlateSolveConfig` JSON body must fail hard (serde error), not
    /// silently succeed with the wrong shape.
    #[test]
    fn bare_plate_solve_config_body_fails_to_deserialize_into_wrapped_args() {
        let bare = serde_json::to_value(PlateSolveConfig::default()).unwrap();

        let result: Result<SetPlateSolveConfigArgs, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "bare PlateSolveConfig body must NOT deserialize into SetPlateSolveConfigArgs — \
             this is what closes the silent-mismatch hole (axum returns 422/400 for this shape)"
        );
    }
}
