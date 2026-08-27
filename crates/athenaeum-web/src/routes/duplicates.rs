// Duplicate-detection and black-hole route handlers
// Mirrors the Tauri duplicates commands in athenaeum-tauri.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::WebAppState;
use crate::events::{SseEvent, SseProgressEmitter};

// ── Request body types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveToBlackHoleArgs {
    pub file_id: i64,
    pub from_where: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkMoveToBlackHoleArgs {
    pub file_ids: Vec<i64>,
    pub from_where: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBlackHoleFilesArgs {
    pub filter: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBlackholedFileIdsArgs {
    pub file_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdArgs {
    pub file_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDuplicateFoldersArgs {
    pub threshold: Option<f64>,
}

// ── Helper ────────────────────────────────────────────────────────────────────

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

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Return duplicate file groups.
///
/// The `duplicates.use_content_hash` setting selects the `DuplicateKey`s:
/// content hashes when on; when off, the union of the header key (raw
/// sub-frames) and the master key (masters and processed files, decided by a
/// full-file hash), whose eligibility clauses are complements over the
/// classified files. Serves
/// from the warm cache of EACH key when available; otherwise computes
/// on-the-fly (slow path, used before the first scan populates the cache).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_duplicates(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::DuplicateGroup>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    // Content mode is a single explicit key over every file. Otherwise the
    // view is Header (raw sub-frames, decided by their stored header) plus
    // Master (everything else, decided by a full-file hash) — the two
    // eligibility clauses are complements over the CLASSIFIED files, so no
    // file is decided twice (an unclassified `imagetyp IS NULL` frame is
    // deliberately in neither — see `DuplicateKey::eligibility`).
    let keys: &[athenaeum_core::db::DuplicateKey] = if state
        .ctx
        .settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false)
    {
        &[athenaeum_core::db::DuplicateKey::Content]
    } else {
        &[
            athenaeum_core::db::DuplicateKey::Header,
            athenaeum_core::db::DuplicateKey::Master,
        ]
    };

    let mut all = Vec::new();
    for &key in keys {
        let groups = if athenaeum_core::db::has_duplicate_cache(&conn, key).unwrap_or(false) {
            // Fast path: warm cache.
            athenaeum_core::db::get_cached_duplicates(&conn, key).map_err(db_err)?
        } else {
            // Slow path: compute now.
            athenaeum_core::db::find_duplicate_groups(&conn, key).map_err(db_err)?
        };
        all.extend(groups);
    }
    all.sort_by(|a, b| b.file_count.cmp(&a.file_count).then(b.size.cmp(&a.size)));
    Ok(Json(all))
}

/// Soft-delete a file by adding it to the black hole.
///
/// Looks up the file's current path, then records it in the `black_hole` table
/// so it can be reviewed or permanently deleted later.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn move_to_black_hole(
    State(state): State<WebAppState>,
    Json(args): Json<MoveToBlackHoleArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let original_path: String = conn
        .query_row(
            "SELECT path FROM files WHERE id = ?1",
            [args.file_id],
            |row| row.get(0),
        )
        .map_err(db_err)?;

    let id = athenaeum_core::db::add_to_black_hole(&conn, args.file_id, &args.from_where, &original_path)
        .map_err(db_err)?;

    let _ = state.event_tx.send(SseEvent {
        event_name: "blackhole-changed".to_string(),
        data: serde_json::json!({ "file_id": args.file_id, "action": "blackholed" }),
    });

    Ok(Json(id))
}

/// Move a batch of files to the black hole in a single transaction, emitting
/// SSE progress events as it runs. Mirrors the Tauri `bulk_move_to_black_hole`
/// command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn bulk_move_to_black_hole(
    State(state): State<WebAppState>,
    Json(args): Json<BulkMoveToBlackHoleArgs>,
) -> Result<Json<athenaeum_core::models::BulkMoveResult>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    let result = athenaeum_core::db::bulk_move_to_black_hole(
        &conn,
        &args.file_ids,
        &args.from_where,
        Some(&emitter),
    )
    .map_err(db_err)?;

    let _ = state.event_tx.send(SseEvent {
        event_name: "blackhole-changed".to_string(),
        data: serde_json::json!({ "action": "bulk-blackholed", "count": result.moved }),
    });

    Ok(Json(result))
}

/// List files currently sitting in the black hole.
///
/// Pass `filter` to limit results to a specific `from_where` source label
/// (e.g. `"duplicates"`).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_black_hole_files(
    State(state): State<WebAppState>,
    Json(args): Json<GetBlackHoleFilesArgs>,
) -> Result<Json<Vec<athenaeum_core::models::BlackHoleEntry>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let entries = athenaeum_core::db::get_black_hole_files(&conn, args.filter).map_err(db_err)?;
    Ok(Json(entries))
}

/// Given a list of file IDs, return the subset that are currently blackholed.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_blackholed_file_ids(
    State(state): State<WebAppState>,
    Json(args): Json<GetBlackholedFileIdsArgs>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let blackholed =
        athenaeum_core::db::get_blackholed_file_ids(&conn, &args.file_ids).map_err(db_err)?;

    Ok(Json(blackholed))
}

/// Restore a file from the black hole (removes it from the `black_hole` table;
/// does not move the file on disk).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn restore_from_black_hole(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::remove_from_black_hole(&conn, args.file_id).map_err(db_err)?;

    let _ = state.event_tx.send(SseEvent {
        event_name: "blackhole-changed".to_string(),
        data: serde_json::json!({ "file_id": args.file_id, "action": "restored" }),
    });

    Ok(Json(()))
}

/// Permanently delete a single blackholed file from disk and the database.
///
/// WARNING: this is irreversible.  The file is removed from the filesystem and
/// all related database rows (frames, black_hole, etc.) are deleted via CASCADE.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn send_to_void(
    State(state): State<WebAppState>,
    Json(args): Json<FileIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::send_to_void(&conn, args.file_id).map_err(db_err)?;
    Ok(Json(()))
}

/// Permanently delete all files currently in the black hole.
///
/// WARNING: this is irreversible.  Returns the number of files deleted.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn send_all_to_void(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let n = athenaeum_core::db::send_all_to_void(&conn).map_err(db_err)?;
    Ok(Json(n))
}

// ── Scan root flags ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScanRootDuplicatesFlagArgs {
    pub id: i64,
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScanRootUniqueCameraFlagArgs {
    pub id: i64,
    pub enabled: bool,
}

/// Update the allow_duplicates flag for a scan root.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_scan_root_duplicates_flag(
    State(state): State<WebAppState>,
    Json(args): Json<SetScanRootDuplicatesFlagArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::update_scan_root_duplicates_flag(&conn, args.id, args.enabled)
        .map_err(db_err)?;
    Ok(Json(()))
}

/// Toggle unique_camera flag for a scan root.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_scan_root_unique_camera_flag(
    State(state): State<WebAppState>,
    Json(args): Json<SetScanRootUniqueCameraFlagArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::set_unique_camera_flag(&conn, args.id, args.enabled)
        .map_err(db_err)?;

    tracing::info!(root_id = args.id, enabled = args.enabled, "unique_camera flag set");
    Ok(Json(()))
}

/// Deep-verify two CATALOG files, banking the read (mirror of the Tauri
/// `verify_duplicate_pair`): an identical pair's full-content hash lands in
/// `files.strong_hash`, and a later verify of the same pair is decided from
/// the stored hashes without reading either file.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDuplicatePairArgs {
    pub file_a: i64,
    pub file_b: i64,
}

#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn verify_duplicate_pair(
    State(state): State<WebAppState>,
    Json(args): Json<VerifyDuplicatePairArgs>,
) -> Result<Json<athenaeum_core::duplicates::VerifyPairResult>, (StatusCode, String)> {
    athenaeum_core::api::files::verify_duplicate_pair(&state.ctx, args.file_a, args.file_b)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Deep-verify two files are byte-identical. Opt-in safety net before
/// destructive operations on duplicates — `compute_xxhash` only samples
/// 3×512 KiB regions, so two genuinely different large files can collide.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFilesByteIdenticalArgs {
    pub path1: String,
    pub path2: String,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn verify_files_byte_identical(
    Json(args): Json<VerifyFilesByteIdenticalArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let p1 = std::path::PathBuf::from(&args.path1);
    let p2 = std::path::PathBuf::from(&args.path2);
    athenaeum_core::duplicates::verify_byte_identical(&p1, &p2)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Find directory pairs with a high proportion of duplicate files.
///
/// `threshold` is a similarity percentage (0–100); defaults to 70.0 if omitted.
/// Always computes fresh — the folder similarity cache cannot account for files
/// that have since been moved to the black hole.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_duplicate_folders(
    State(state): State<WebAppState>,
    Json(args): Json<GetDuplicateFoldersArgs>,
) -> Result<Json<Vec<athenaeum_core::models::FolderSimilarity>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let threshold = args.threshold.unwrap_or(70.0);
    let folders = athenaeum_core::db::find_duplicate_folders(&conn, threshold).map_err(db_err)?;
    Ok(Json(folders))
}
