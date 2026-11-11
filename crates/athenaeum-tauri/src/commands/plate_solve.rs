use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, State};

use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::plate_solve::dso_lookup::{DsoCatalog, ResolvedObject};
use athenaeum_core::plate_solve::storage;
use athenaeum_core::plate_solve::CatalogOpenError;

use super::AppState;

/// Resolve and open the additive density-tier catalog stack under
/// `<app-data>/catalogs/smac_gaia` (base → deepest), or a legacy single
/// `stars.smac` there as a 1-layer fallback. Opens fresh each call (mmap is
/// cheap); the batch path opens once and shares across workers. Errors if no
/// catalog is installed.
pub(super) fn resolve_layer_caches(
    state: &AppState,
) -> Result<Vec<Arc<solvemyastro::StarCache>>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let db_path = db.path().to_path_buf();
    let parent = db_path
        .parent()
        .ok_or("Cannot determine app data directory")?;
    athenaeum_core::plate_solve::open_catalog_layers(parent).map_err(|e| match e {
        CatalogOpenError::NotInstalled { root } => format!(
            "solvemyastro star catalog not found under {}. \
             Please download the Gaia DR3 prebuilt catalog from Settings → Plate Solving.",
            root.display()
        ),
        CatalogOpenError::Open { dir, error } => {
            format!("Failed to open star cache at {}: {error}", dir.display())
        }
    })
}

/// Open the installed quad-index packs under `<app-data>/catalogs/smac_index`
/// (empty when none are installed; a pack that does not open is skipped with a
/// warning). Opened once per batch, like the catalog layers.
pub(super) fn resolve_index_packs(state: &AppState) -> Vec<Arc<solvemyastro::QuadIndex>> {
    let Some(db) = state.ctx.db.get() else {
        return Vec::new();
    };
    let db_path = db.path().to_path_buf();
    match db_path.parent() {
        Some(parent) => athenaeum_core::plate_solve::open_index_packs(parent),
        None => Vec::new(),
    }
}

/// Lazy-load the DSO catalog once and cache it in the ServiceContext.
fn get_dso_catalog(state: &AppState) -> Option<Arc<DsoCatalog>> {
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

// ========== Config Commands ==========

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_plate_solve_config(
    state: State<'_, AppState>,
) -> Result<PlateSolveConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    Ok(config::load_config(&conn))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_plate_solve_config(
    state: State<'_, AppState>,
    config: PlateSolveConfig,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    config::save_config(&conn, &config).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reset_plate_solve_config(
    state: State<'_, AppState>,
) -> Result<PlateSolveConfig, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    let default_config = PlateSolveConfig::default();
    config::save_config(&conn, &default_config).map_err(|e| e.to_string())?;
    Ok(default_config)
}

// ========== Solve Commands ==========

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn plate_solve_batch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    frame_ids: Vec<i64>,
) -> Result<(), String> {
    let layer_caches = resolve_layer_caches(&state)?;
    let index_packs = resolve_index_packs(&state);
    let dso = get_dso_catalog(&state);
    let prepared = athenaeum_core::api::plate_solve::prepare_plate_solve_batch(
        &state.ctx,
        &frame_ids,
        layer_caches,
        index_packs,
        dso,
    )
    .map_err(|e| e.to_string())?;

    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    let ctx = Arc::clone(&state.ctx);
    tokio::task::spawn_blocking(move || {
        athenaeum_core::api::plate_solve::run_plate_solve_batch(ctx, emitter, prepared)
    })
    .await
    .map_err(|e| format!("plate solve batch panicked: {e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_plate_solve(state: State<'_, AppState>) -> Result<(), String> {
    let solves = state.ctx.active_plate_solves.lock().unwrap();
    tracing::debug!(active_keys = ?solves.keys().collect::<Vec<_>>(), "cancel_plate_solve invoked");
    if let Some(handle) = solves.get(&0) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
        tracing::info!("plate solve cancel flag set");
    } else {
        tracing::warn!("no active plate solve handle, cancel had no effect");
    }
    Ok(())
}

// ========== Autofind Object Commands ==========

/// Batch: fill `frame.object` from stored RA/Dec for frames that have
/// coordinates but no object name. Uses the bundled DSO catalog with a
/// tight 0.2° proximity tolerance. Labels are prefixed with "Autofind: "
/// so the origin is visible to the user.
///
/// Emits `autofind-objects-progress` for each frame and
/// `autofind-objects-complete` once finished. Does NOT return a value —
/// callers should listen for the complete event for the summary.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn autofind_objects_from_coordinates(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    frame_ids: Vec<i64>,
) -> Result<(), String> {
    let dso = get_dso_catalog(&state);
    let prepared = athenaeum_core::api::plate_solve::prepare_autofind_objects(&state.ctx, dso)
        .map_err(|e| e.to_string())?;

    // The whole batch is blocking work (per-frame DB reads + coordinate
    // math); running it inline would starve a tokio worker thread for the
    // batch's full duration while holding a pooled connection (audit C6).
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    let ctx = Arc::clone(&state.ctx);
    tokio::task::spawn_blocking(move || {
        athenaeum_core::api::plate_solve::run_autofind_objects(ctx, emitter, prepared, frame_ids)
    })
    .await
    .map_err(|e| format!("autofind batch panicked: {e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn cancel_autofind_objects(state: State<'_, AppState>) -> Result<(), String> {
    let handles = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = handles.get(&1) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Does this object name mean anything to the solver?
///
/// The metadata editor asks while the user types: naming a target is what
/// makes a coordinate-less frame solvable, and a name the catalog does not
/// carry will not help.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn resolve_object_name(
    name: String,
    state: State<'_, AppState>,
) -> Result<Option<ResolvedObject>, String> {
    Ok(athenaeum_core::api::objects::resolve_object_name(&state.ctx, name))
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_plate_solve_result(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<Option<storage::PlateSolveRecord>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    storage::get_plate_solve(&conn, frame_id).map_err(|e| e.to_string())
}

/// Delete the `plate_solves` row for a frame. Called by the metadata pane's
/// "Revert WCS to FITS header" action so the stored WCS doesn't outlive the
/// `frames.ra/dec/rotation/objctra/objctdec` columns that were just cleared
/// — otherwise the pane's Plate-solve result card would render stale data.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_plate_solve_for_frame(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    storage::delete_plate_solve(&conn, frame_id).map_err(|e| e.to_string())
}

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
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_catalog_status(
    state: State<'_, AppState>,
) -> Result<Vec<CatalogStatusInfo>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or("Cannot determine app data directory")?.to_path_buf();
    // tier_status does blocking file I/O + (first run) a blocking manifest fetch;
    // run it off the async runtime so the IPC call returns instead of stalling.
    let rows = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::gaia_prebuilt::tier_status(&app_data)
    })
    .await
    .map_err(|e| format!("catalog status task failed: {e}"))?;
    Ok(rows.into_iter().map(|t| CatalogStatusInfo {
        name: format!("Gaia tier {} (≤{:.2}° FOV)", t.density, t.min_fov_deg),
        density: t.density,
        installed: t.installed,
        epoch: t.epoch,
        star_count_approx: t.star_count,
        size_bytes: t.size_bytes,
        min_fov_deg: t.min_fov_deg,
        mag_limit: 19.0,
    }).collect())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_fov_summary(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::plate_solve::FovSummary, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    athenaeum_core::plate_solve::frame_fov_summary(&conn).map_err(|e| e.to_string())
}

// ========== Catalog Download ==========

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn download_catalog_layers(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    target_density: u32,
) -> Result<String, String> {
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    athenaeum_core::api::plate_solve::download_catalog_layers(&state.ctx, emitter, target_density)
        .await
        .map_err(|e| e.to_string())
}

// ========== Quad Index Packs ==========

/// One row per declared quad-index pack (`base`, `narrow`), merged with the
/// on-disk installed state; the installed packs alone when no manifest is
/// reachable. The packs speed up blind and scale-free solves.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_index_status(
    state: State<'_, AppState>,
) -> Result<Vec<athenaeum_core::catalog::index_prebuilt::IndexStatusInfo>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let app_data = db.path().to_path_buf().parent()
        .ok_or("Cannot determine app data directory")?.to_path_buf();
    // index_status does blocking file I/O + (first run) a blocking manifest fetch;
    // run it off the async runtime so the IPC call returns instead of stalling.
    let rows = tokio::task::spawn_blocking(move || {
        athenaeum_core::catalog::index_prebuilt::index_status(&app_data)
    })
    .await
    .map_err(|e| format!("index status task failed: {e}"))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Download and install the named quad-index packs; progress rides the
/// `index-download-progress` event. Already-installed packs are skipped.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn download_index_packs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<String, String> {
    use athenaeum_core::catalog::index_prebuilt::{self, IndexProgressMapper};
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let app_data_dir = db.path().to_path_buf().parent()
        .ok_or("Cannot determine app data directory")?.to_path_buf();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let mapper = Arc::new(IndexProgressMapper::new());
    let (app_clone, mapper_clone) = (app.clone(), mapper.clone());

    let result = tokio::task::spawn_blocking(move || {
        index_prebuilt::download_index_packs(&app_data_dir, &names, cancel_flag, &|progress| {
            let _ = app_clone.emit("index-download-progress", mapper_clone.map(progress));
        })
    })
    .await
    .map_err(|e| format!("download task panicked: {e}"))?;

    match result {
        Ok(p) => Ok(p.display().to_string()),
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = app.emit("index-download-progress", mapper.error(msg.clone()));
            Err(msg)
        }
    }
}
