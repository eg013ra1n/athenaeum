// Zip commands that change the catalog.
//
// Thin wrappers only: extraction + handler call + error mapping. Business logic lives in
// `athenaeum_core::api::zip`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, State};

use athenaeum_core::api::zip as api;
use athenaeum_core::api::PathPolicy;

use crate::tauri_events::TauriProgressEmitter;

use super::AppState;

pub use athenaeum_core::api::zip::{UnpackOutcome, UnpackTarget, ZipDirListing};
pub use athenaeum_core::zip_store::cache::ZipCacheStatus;
pub use athenaeum_core::zip_store::materialize::ExtractionEstimate;

/// Move the zip-resident frames of a frame set or calibration set out of their foreign zips into
/// `dest_dir`, keeping every catalog row. Progress arrives as `zip-unpack-progress`; the end as
/// `archive-finished` with `kind: "unpack"`. Extraction and catalog writes run under `spawn_blocking`.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn unpack_zip_frames(
    app: AppHandle,
    state: State<'_, AppState>,
    target: UnpackTarget,
    dest_dir: String,
) -> Result<UnpackOutcome, String> {
    let ctx = state.ctx.clone();
    let emitter = TauriProgressEmitter(app);
    tokio::task::spawn_blocking(move || {
        api::unpack_zip_frames(
            &ctx,
            &PathPolicy::AllowAll,
            target,
            &PathBuf::from(dest_dir),
            &emitter,
            &AtomicBool::new(false),
        )
    })
    .await
    .map_err(|e| format!("Unpack task panicked: {}", e))?
    .map_err(|e| e.to_string())
}

/// One level of a zip for the browser's zip view: sub-folders, cataloged frames with their metadata, and
/// the entries that are not frames. Reads the zip's directory only (`spawn_blocking`); never opens or
/// modifies an entry.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_zip_entries(
    state: State<'_, AppState>,
    zip_path: String,
    inner_dir: Option<String>,
) -> Result<ZipDirListing, String> {
    let ctx = state.ctx.clone();
    // Optional like the web route's `#[serde(default)]`: no folder means the top of the zip.
    let inner_dir = inner_dir.unwrap_or_default();
    tokio::task::spawn_blocking(move || api::list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_path, &inner_dir))
        .await
        .map_err(|e| format!("Zip listing task panicked: {}", e))?
        .map_err(|e| e.to_string())
}

/// Size, entry count and limit of the zip extraction cache.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_zip_cache_status(state: State<'_, AppState>) -> Result<ZipCacheStatus, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_zip_cache_status(&ctx))
        .await
        .map_err(|e| format!("Zip cache status task panicked: {}", e))?
        .map_err(|e| e.to_string())
}

/// Removes every cached extraction nothing is using; returns the cache status afterwards.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn clear_zip_cache(state: State<'_, AppState>) -> Result<ZipCacheStatus, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::clear_zip_cache(&ctx))
        .await
        .map_err(|e| format!("Zip cache clear task panicked: {}", e))?
        .map_err(|e| e.to_string())
}

/// How many of the named frames would be extracted from zips (not cached yet) and their total size. The
/// batch is named by file ids, frame ids and/or a frame set (lights + calibration frames). Reads the
/// catalog and the zips' state only (`spawn_blocking`); extracts nothing.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn estimate_zip_extraction(
    state: State<'_, AppState>,
    file_ids: Option<Vec<i64>>,
    frame_ids: Option<Vec<i64>>,
    frame_set_id: Option<i64>,
) -> Result<ExtractionEstimate, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || {
        api::estimate_zip_extraction(
            &ctx,
            &file_ids.unwrap_or_default(),
            &frame_ids.unwrap_or_default(),
            frame_set_id,
        )
    })
    .await
    .map_err(|e| format!("Zip extraction estimate task panicked: {}", e))?
    .map_err(|e| e.to_string())
}
