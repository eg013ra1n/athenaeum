// Zip routes that change the catalog — mirrors `athenaeum-tauri/src/commands/zip.rs`.
//
// Thin wrappers only: extraction + policy construction + handler call + error mapping. Business logic
// lives in `athenaeum_core::api::zip`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::api::zip as api;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::routes::scan_roots::allowed_roots_policy;
use crate::WebAppState;

pub use athenaeum_core::api::zip::{UnpackOutcome, UnpackTarget, ZipDirListing};
pub use athenaeum_core::zip_store::cache::ZipCacheStatus;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnpackZipFramesArgs {
    pub target: UnpackTarget,
    pub dest_dir: String,
}

/// POST /api/unpack_zip_frames
///
/// Extraction and catalog writes run under `spawn_blocking`; the destination folder is checked against
/// the host's allowed roots inside the shared handler.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn unpack_zip_frames(
    State(state): State<WebAppState>,
    Json(args): Json<UnpackZipFramesArgs>,
) -> Result<Json<UnpackOutcome>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let policy = allowed_roots_policy(&state.allowed_paths);
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    let outcome = tokio::task::spawn_blocking(move || {
        api::unpack_zip_frames(
            &ctx,
            &policy,
            args.target,
            &PathBuf::from(args.dest_dir),
            &emitter,
            &AtomicBool::new(false),
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Unpack task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(outcome))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListZipEntriesArgs {
    pub zip_path: String,
    #[serde(default)]
    pub inner_dir: String,
}

/// POST /api/list_zip_entries
///
/// The zip path is checked against the host's allowed roots inside the shared handler; the zip's directory
/// is read under `spawn_blocking`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_zip_entries(
    State(state): State<WebAppState>,
    Json(args): Json<ListZipEntriesArgs>,
) -> Result<Json<ZipDirListing>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let policy = allowed_roots_policy(&state.allowed_paths);
    let listing =
        tokio::task::spawn_blocking(move || api::list_zip_entries(&ctx, &policy, &args.zip_path, &args.inner_dir))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Zip listing task panicked: {}", e)))?
            .map_err(api_err)?;
    Ok(Json(listing))
}

/// POST /api/get_zip_cache_status
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_zip_cache_status(
    State(state): State<WebAppState>,
) -> Result<Json<ZipCacheStatus>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let status = tokio::task::spawn_blocking(move || api::get_zip_cache_status(&ctx))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Zip cache status task panicked: {}", e)))?
        .map_err(api_err)?;
    Ok(Json(status))
}

/// POST /api/clear_zip_cache
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_zip_cache(
    State(state): State<WebAppState>,
) -> Result<Json<ZipCacheStatus>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let status = tokio::task::spawn_blocking(move || api::clear_zip_cache(&ctx))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Zip cache clear task panicked: {}", e)))?
        .map_err(api_err)?;
    Ok(Json(status))
}
