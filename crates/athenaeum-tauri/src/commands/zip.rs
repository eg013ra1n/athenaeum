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
    inner_dir: String,
) -> Result<ZipDirListing, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_path, &inner_dir))
        .await
        .map_err(|e| format!("Zip listing task panicked: {}", e))?
        .map_err(|e| e.to_string())
}
