use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use athenaeum_core::plate_solve::config::{self, PlateSolveConfig};
use athenaeum_core::api::plate_solve::PlateSolveActivity;
use athenaeum_core::plate_solve::dso_lookup::ResolvedObject;
use athenaeum_core::plate_solve::storage;
use athenaeum_core::services::PlateSolveOrigin;

use super::AppState;

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
    use athenaeum_core::api::plate_solve as core;
    let layer_caches = core::resolve_layer_caches(&state.ctx).map_err(|e| e.to_string())?;
    let index_packs = core::resolve_index_packs(&state.ctx);
    let dso = core::dso_catalog(&state.ctx);
    let prepared = core::prepare_plate_solve_batch(
        &state.ctx,
        &frame_ids,
        layer_caches,
        index_packs,
        dso,
        PlateSolveOrigin::Ui,
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
    athenaeum_core::api::plate_solve::cancel_plate_solve(&state.ctx);
    Ok(())
}

/// What the plate-solve slot is doing now: idle, or the running batch's origin,
/// run id and progress.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_plate_solve_activity(
    state: State<'_, AppState>,
) -> Result<PlateSolveActivity, String> {
    Ok(athenaeum_core::api::plate_solve::plate_solve_activity(&state.ctx))
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
    let dso = athenaeum_core::api::plate_solve::dso_catalog(&state.ctx);
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
    let emitter = Arc::new(crate::tauri_events::TauriProgressEmitter(app));
    athenaeum_core::api::plate_solve::download_index_packs(&state.ctx, emitter, names)
        .await
        .map_err(|e| e.to_string())
}
