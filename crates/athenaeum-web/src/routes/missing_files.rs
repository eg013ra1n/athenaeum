// Missing files route handlers — mirrors athenaeum-tauri/src/commands/missing_files.rs

use axum::{extract::State, http::StatusCode, Json};

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

// ── Error helpers ─────────────────────────────────────────────────────────────

// The raw stderr prints formerly here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on every caller below, which already logs each returned Err at
// the command boundary — see the T7 sweep report.
fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
}

// ── Response DTOs ─────────────────────────────────────────────────────────────

pub use athenaeum_core::api::missing_files::MissingFileRecord;

// ── Request body types ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootIdArgs {
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMissingFilesArgs {
    pub root_id: i64,
    pub file_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdArgs {
    pub file_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMissingFilesArgs {
    pub file_ids: Vec<i64>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/check_missing_files_in_scan_root
///
/// Check for files in the DB under this root that no longer exist on disk.
/// Emits `verifying` SSE progress before and after the filesystem check
/// (the shared core handler does both).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_missing_files_in_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<athenaeum_core::models::OrphanedFile>>, (StatusCode, String)> {
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    athenaeum_core::api::scan_roots::check_missing_files_in_scan_root(
        &state.ctx,
        args.root_id,
        &emitter,
    )
    .map(Json)
    .map_err(api_err)
}

/// POST /api/sync_missing_files
///
/// Sync the missing_files table after a rescan. Inserts new missing entries,
/// removes entries for files that now exist, preserves 'ignored' entries.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn sync_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<SyncMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();
    athenaeum_core::db::sync_missing_files(&conn, args.root_id, &args.file_ids).map_err(db_err)?;
    Ok(Json(()))
}

/// POST /api/get_missing_files
///
/// Return all missing file records for a scan root with full details.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::get_missing_files(&state.ctx, args.root_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/recheck_missing_files
///
/// Re-verify all missing files for a root on disk, removing entries for files
/// that have reappeared, then return the updated list.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn recheck_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<RootIdArgs>,
) -> Result<Json<Vec<MissingFileRecord>>, (StatusCode, String)> {
    athenaeum_core::api::missing_files::recheck_missing_files(&state.ctx, args.root_id)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/ignore_missing_file
#[tracing::instrument(skip_all, err(Debug))]
pub async fn ignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'ignored' WHERE file_id = ?1",
        [args.file_id],
    )
    .map_err(db_err)?;

    Ok(Json(()))
}

/// POST /api/unignore_missing_file
#[tracing::instrument(skip_all, err(Debug))]
pub async fn unignore_missing_file(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "UPDATE missing_files SET status = 'missing' WHERE file_id = ?1",
        [args.file_id],
    )
    .map_err(db_err)?;

    Ok(Json(()))
}

/// POST /api/delete_missing_files
///
/// Delete files from the database entirely (CASCADE handles frames, missing_files, etc.)
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_missing_files(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteMissingFilesArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    if args.file_ids.is_empty() {
        return Ok(Json(()));
    }

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    // Not a bare `DELETE FROM files`: a master's file needs its raw source set
    // un-superseded and its consumers repointed first, or the raw frames stay
    // invisible to the matcher with nothing left in the UI to undo it
    // (2026-08-02 audit C3). `delete_orphaned_files` is the one path that does
    // both — the Black Hole, void and orphan-purge flows all go through it.
    athenaeum_core::relinking::delete_orphaned_files(&conn, &args.file_ids).map_err(db_err)?;

    Ok(Json(()))
}

#[cfg(test)]
mod delete_missing_files_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Same shape as `analysis::analysis_config_tests::test_state` — a real
    /// temp-file database, because this handler is all SQL.
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

    /// Purging a MASTER's file through the missing-files panel must run the
    /// same un-supersede the orphan-purge path does (2026-08-02 audit C3):
    /// otherwise the raw source set keeps pointing at a master that exists
    /// neither on disk nor in the catalog, its frames stay invisible to the
    /// matcher forever, and nothing in the UI can undo it.
    #[tokio::test]
    async fn deleting_a_master_file_un_supersedes_its_raw_set() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();

        let (file_id, raw_set_id) = {
            let conn = db.conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES ('/lib/master_dark.fits', 'master_dark.fits', 100, '2026-09-01T00:00:00Z', 'FITS')",
                [],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
                rusqlite::params![file_id],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date, is_master_library)
                 VALUES ('MasterDark', '2026-09-01', 1)",
                [],
            )
            .unwrap();
            let master_set_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![master_set_id, frame_id],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-09-01')",
                [],
            )
            .unwrap();
            let raw_set_id = conn.last_insert_rowid();
            conn.execute(
                "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
                rusqlite::params![master_set_id, raw_set_id],
            )
            .unwrap();

            (file_id, raw_set_id)
        };

        let state = test_state(db);
        let _ = delete_missing_files(
            State(state.clone()),
            Json(DeleteMissingFilesArgs { file_ids: vec![file_id] }),
        )
        .await
        .expect("delete should succeed");

        let db = state.ctx.db.get().unwrap();
        let conn = db.conn();
        let still_superseded: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                rusqlite::params![raw_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_superseded, None,
            "raw set must be un-superseded when its master's file is purged"
        );

        let files_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files_left, 0, "the purged file row is gone");
    }
}
