// Duplicate-detection and black-hole route handlers
// Mirrors the Tauri duplicates commands in athenaeum-tauri.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::WebAppState;
use crate::events::SseEvent;

// ── Request body types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveToBlackHoleArgs {
    pub file_id: i64,
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

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("duplicates error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    eprintln!("duplicates error: database not initialized");
    (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Return duplicate file groups.
///
/// Checks the settings to determine whether content hashes or metadata hashes
/// are used.  Serves from the warm cache when available; otherwise computes
/// on-the-fly (slow path, used before the first scan populates the cache).
pub async fn get_duplicates(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::DuplicateGroup>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let use_content_hash = state
        .ctx
        .settings
        .get_duplicates_use_content_hash(&conn)
        .unwrap_or(false);

    // Fast path: warm cache.
    if athenaeum_core::db::has_duplicate_cache(&conn, use_content_hash).unwrap_or(false) {
        let groups = athenaeum_core::db::get_cached_duplicates(&conn, use_content_hash)
            .map_err(db_err)?;
        return Ok(Json(groups));
    }

    // Slow path: compute now.
    let groups = athenaeum_core::db::find_duplicate_groups(&conn, use_content_hash)
        .map_err(db_err)?;
    Ok(Json(groups))
}

/// Soft-delete a file by adding it to the black hole.
///
/// Looks up the file's current path, then records it in the `black_hole` table
/// so it can be reviewed or permanently deleted later.
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

/// List files currently sitting in the black hole.
///
/// Pass `filter` to limit results to a specific `from_where` source label
/// (e.g. `"duplicates"`).
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
pub async fn get_blackholed_file_ids(
    State(state): State<WebAppState>,
    Json(args): Json<GetBlackholedFileIdsArgs>,
) -> Result<Json<Vec<i64>>, (StatusCode, String)> {
    if args.file_ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let placeholders: Vec<String> = args.file_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT file_id FROM black_hole WHERE file_id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).map_err(db_err)?;

    let params: Vec<&dyn rusqlite::ToSql> = args
        .file_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let blackholed: Vec<i64> = stmt
        .query_map(params.as_slice(), |row| row.get(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    Ok(Json(blackholed))
}

/// Restore a file from the black hole (removes it from the `black_hole` table;
/// does not move the file on disk).
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
pub async fn set_scan_root_unique_camera_flag(
    State(state): State<WebAppState>,
    Json(args): Json<SetScanRootUniqueCameraFlagArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::set_unique_camera_flag(&conn, args.id, args.enabled)
        .map_err(db_err)?;

    eprintln!("unique_camera flag set for root {}: enabled={}", args.id, args.enabled);
    Ok(Json(()))
}

/// Backfill fingerprints for existing FITS headers that are missing them.
pub async fn backfill_header_fingerprints(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare("SELECT id, header FROM fits_header WHERE header_fingerprint IS NULL")
        .map_err(db_err)?;

    let headers: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    let total = headers.len();
    eprintln!("Backfilling fingerprints for {} headers", total);

    for (id, header) in headers {
        let fingerprint = athenaeum_core::fingerprint::compute_header_fingerprint(&header);
        conn.execute(
            "UPDATE fits_header SET header_fingerprint = ?1 WHERE id = ?2",
            rusqlite::params![fingerprint, id],
        )
        .map_err(db_err)?;
    }

    Ok(Json(total))
}

/// Find directory pairs with a high proportion of duplicate files.
///
/// `threshold` is a similarity percentage (0–100); defaults to 70.0 if omitted.
/// Always computes fresh — the folder similarity cache cannot account for files
/// that have since been moved to the black hole.
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
