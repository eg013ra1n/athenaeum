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

pub use athenaeum_core::api::zip::{RescanAfterVoid, UnpackOutcome, UnpackTarget, ZipDirListing, ZipEntryHeader};
pub use athenaeum_core::zip_store::cache::ZipCacheStatus;
pub use athenaeum_core::zip_store::materialize::ExtractionEstimate;

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipEntryArgs {
    pub zip_path: String,
    pub entry: String,
}

/// POST /api/get_zip_entry_header
///
/// The header and frame metadata of a frame entry the catalog holds no row for, read live from the zip
/// (`spawn_blocking`); the zip path is checked against the host's allowed roots. Writes nothing.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_zip_entry_header(
    State(state): State<WebAppState>,
    Json(args): Json<ZipEntryArgs>,
) -> Result<Json<ZipEntryHeader>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    let header = tokio::task::spawn_blocking(move || api::get_zip_entry_header(&policy, &args.zip_path, &args.entry))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Zip entry header task panicked: {}", e)))?
        .map_err(api_err)?;
    Ok(Json(header))
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateZipExtractionArgs {
    #[serde(default)]
    pub file_ids: Vec<i64>,
    #[serde(default)]
    pub frame_ids: Vec<i64>,
    #[serde(default)]
    pub frame_set_id: Option<i64>,
    #[serde(default)]
    pub export_mode: Option<athenaeum_core::export::models::ExportMode>,
    #[serde(default)]
    pub with_linked_masters: bool,
}

/// POST /api/estimate_zip_extraction
#[tracing::instrument(skip_all, err(Debug))]
pub async fn estimate_zip_extraction(
    State(state): State<WebAppState>,
    Json(args): Json<EstimateZipExtractionArgs>,
) -> Result<Json<ExtractionEstimate>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let estimate = tokio::task::spawn_blocking(move || {
        api::estimate_zip_extraction(
            &ctx,
            &args.file_ids,
            &args.frame_ids,
            args.frame_set_id,
            args.export_mode,
            args.with_linked_masters,
        )
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Zip extraction estimate task panicked: {}", e)))?
    .map_err(api_err)?;
    Ok(Json(estimate))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanAfterVoidArgs {
    #[serde(default)]
    pub dirs: Vec<String>,
}

/// POST /api/rescan_after_void
///
/// Folders outside the host's allowed roots are skipped by the shared handler; the scan runs under
/// `spawn_blocking`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rescan_after_void(
    State(state): State<WebAppState>,
    Json(args): Json<RescanAfterVoidArgs>,
) -> Result<Json<RescanAfterVoid>, (StatusCode, String)> {
    let ctx = state.ctx.clone();
    let policy = allowed_roots_policy(&state.allowed_paths);
    let done = tokio::task::spawn_blocking(move || api::rescan_after_void(&ctx, args.dirs, &policy))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Rescan after void task panicked: {}", e)))?
        .map_err(api_err)?;
    Ok(Json(done))
}

#[cfg(test)]
mod rescan_after_void_tests {
    use super::*;
    use athenaeum_core::db::Database;
    use tempfile::TempDir;

    /// `routes::tests::test_state` leaves the database unset; this one gets a real catalog with one scan root.
    fn state_with_root(tmp: &TempDir, root: &std::path::Path, allowed: Vec<PathBuf>) -> WebAppState {
        let mut state = crate::routes::tests::test_state(None);
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        db.conn().execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()]).unwrap();
        let ctx = std::sync::Arc::get_mut(&mut state.ctx).expect("the test state owns its context");
        ctx.db.set(db).ok().expect("the test state has no database yet");
        state.allowed_paths = allowed;
        state
    }

    #[tokio::test]
    async fn the_route_rescans_a_folder_under_a_root_and_skips_one_outside_the_allowed_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let dir = root.to_string_lossy().into_owned();

        let open = state_with_root(&tmp, &root, Vec::new());
        let Json(done) = rescan_after_void(State(open), Json(RescanAfterVoidArgs { dirs: vec![dir.clone()] }))
            .await
            .expect("rescan");
        assert_eq!(done, RescanAfterVoid { directories: 1, zips: 0 });
        let db_dir2 = TempDir::new().unwrap();
        let Json(done) = rescan_after_void(
            State(state_with_root(&db_dir2, &root, Vec::new())),
            Json(RescanAfterVoidArgs { dirs: vec![tmp.path().join("elsewhere").to_string_lossy().into_owned()] }),
        )
        .await
        .expect("rescan");
        assert_eq!(done, RescanAfterVoid::default(), "a folder under no scan root is skipped, not an error");

        let sandbox = TempDir::new().unwrap();
        let db_dir3 = TempDir::new().unwrap();
        let confined = state_with_root(&db_dir3, &root, vec![sandbox.path().to_path_buf()]);
        let Json(done) = rescan_after_void(State(confined), Json(RescanAfterVoidArgs { dirs: vec![dir] }))
            .await
            .expect("rescan");
        assert_eq!(done, RescanAfterVoid::default(), "a folder outside the allowed paths is skipped");
    }
}
