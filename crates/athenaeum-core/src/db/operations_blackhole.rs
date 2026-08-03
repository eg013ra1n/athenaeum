// Black Hole (soft delete) operations

use crate::events::{emit_event, ProgressEmitter};
use crate::models::{BlackHoleEntry, BulkMoveResult, FolderSimilarity};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;

/// Progress event payload for `bulk-move-to-black-hole-progress`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BulkMoveProgressEvent {
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub current_file: Option<String>,
}

/// A master's file leaving the library through the generic delete path must not
/// strand its lineage (2026-08-02 audit C3): un-supersede the raw source set and
/// restore its consumer links, exactly like `api::masters::delete_master` does,
/// before the file is black-holed or voided. Without this, the raw set stays
/// superseded — invisible to the matcher — by a master whose file is gone, and
/// nothing in the UI can undo it.
///
/// Only the calibration rows are undone here; `files`/`frames` are left to the
/// caller, which is what each black-hole path wants: a black-holed ex-master
/// stays an ordinary catalog file (a restore or a later re-scan re-ingests it as
/// an *imported* master, with no provenance — the same honest outcome
/// `delete_master`'s warning describes), and `send_to_void` drops the rows
/// itself.
///
/// A no-op for ordinary files. A failed *lookup* is logged and treated as "not a
/// master": refusing to delete a file because one SELECT failed would be a worse
/// outcome than the stranding this guards against. A failed *unregister* is
/// returned — the caller decides (abort this file, keep the batch going).
fn unregister_master_if_any(conn: &Connection, file_id: i64) -> Result<()> {
    let master_set_id = match crate::db::master_unregister::master_set_id_for_file(conn, file_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(()),
        Err(e) => {
            tracing::error!(file_id, error = %e, "master lookup before black-hole/void failed");
            return Ok(());
        }
    };

    crate::db::master_unregister::unregister_master_set(conn, master_set_id).map_err(|e| {
        tracing::error!(file_id, master_set_id, error = %e,
            "failed to unregister master before black-hole/void");
        e
    })?;

    // The primitive logs the unregister itself, but not which file triggered it
    // — this line is the trail from "user deleted a file" to "lineage changed".
    tracing::info!(file_id, master_set_id, "master unregistered before black-hole/void");
    Ok(())
}

/// Add a file to the black hole (soft delete).
///
/// Idempotent: a `UNIQUE(file_id)` index guarantees one row per file, and
/// `INSERT OR IGNORE` makes re-blackholing an already-blackholed file a no-op
/// rather than a duplicate row. Returns the canonical row id either way.
///
/// If the file is a master library file, its registration is undone first (see
/// [`unregister_master_if_any`]) — still idempotent, because a repeat call finds
/// no master left to unregister.
///
/// Atomic: the unregister sequence and the black-hole insert are one savepoint,
/// so a mid-sequence failure leaves the calibration lineage exactly as it was.
/// The savepoint nests, so this composes inside a caller's transaction.
pub fn add_to_black_hole(
    conn: &Connection,
    file_id: i64,
    from_where: &str,
    original_path: &str,
) -> Result<i64> {
    // One atomic unit: the master-unregister sequence (6 statements, doc
    // contract: "runs in the CALLER's transaction") plus the black-hole
    // insert. Without this, a failure mid-unregister left the calibration
    // lineage permanently half-rewired (audit C5).
    let sp = crate::db::SavepointGuard::new(conn, "add_to_black_hole")?;

    unregister_master_if_any(conn, file_id)?;

    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO black_hole (file_id, from_where, moved_at, original_path)
         VALUES (?1, ?2, ?3, ?4)",
        params![file_id, from_where, now, original_path],
    )?;

    // `last_insert_rowid()` is stale when the insert is ignored (already present),
    // so resolve the row id explicitly — correct for both fresh and repeat calls.
    let id: i64 = conn.query_row(
        "SELECT id FROM black_hole WHERE file_id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    sp.commit()?;
    Ok(id)
}

/// Move a batch of files to the black hole in a single transaction, emitting
/// progress events as each file is processed.
///
/// Files already in the black hole are skipped as idempotent no-ops (the
/// `UNIQUE(file_id)` index + `INSERT OR IGNORE` mean no duplicate row is
/// created) and are counted as neither `moved` nor `failed`. Genuine per-file
/// failures (e.g. file row missing) are logged to stderr and collected into
/// `BulkMoveResult::failed` — they do NOT abort the whole batch. A
/// connection-level error (transaction begin/commit fails) returns `Err` and
/// leaves the DB unchanged.
pub fn bulk_move_to_black_hole(
    conn: &Connection,
    file_ids: &[i64],
    from_where: &str,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<BulkMoveResult> {
    let total = file_ids.len();
    let now = Utc::now().to_rfc3339();

    conn.execute("BEGIN TRANSACTION", [])?;

    let mut moved: usize = 0;
    let mut failed: Vec<(i64, String)> = Vec::new();
    let mut last_emit = Instant::now();

    for (idx, file_id) in file_ids.iter().enumerate() {
        // Resolve the file's current path (for black_hole.original_path).
        let path: rusqlite::Result<String> = conn.query_row(
            "SELECT path FROM files WHERE id = ?1",
            params![file_id],
            |row| row.get(0),
        );

        let path = match path {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(file_id, error = %e, "bulk_move_to_black_hole: file not found");
                failed.push((*file_id, format!("file row not found: {}", e)));
                continue;
            }
        };

        // A master file gives up its registration before it is black-holed;
        // a failure there fails THIS file only, never the batch.
        if let Err(e) = unregister_master_if_any(conn, *file_id) {
            failed.push((*file_id, format!("master unregister failed: {}", e)));
            continue;
        }

        let insert = conn.execute(
            "INSERT OR IGNORE INTO black_hole (file_id, from_where, moved_at, original_path)
             VALUES (?1, ?2, ?3, ?4)",
            params![file_id, from_where, now, path],
        );

        match insert {
            // changed == 0 means the file was already in the black hole — a
            // silent idempotent no-op (not a move, not a failure).
            Ok(changed) => {
                if changed > 0 {
                    moved += 1;
                }
            }
            Err(e) => {
                tracing::error!(file_id, path = %path, error = %e, "bulk_move_to_black_hole: failed to move file");
                failed.push((*file_id, e.to_string()));
            }
        }

        // Throttle progress emission to ~10 Hz so 5k-file batches don't spam
        // the event bus, but always emit the final one.
        let now_i = Instant::now();
        let is_last = idx + 1 == total;
        if now_i.duration_since(last_emit).as_millis() >= 100 || is_last {
            if let Some(e) = emitter {
                let percent = if total > 0 {
                    ((idx + 1) as f64 / total as f64) * 100.0
                } else {
                    100.0
                };
                emit_event(
                    e,
                    "bulk-move-to-black-hole-progress",
                    &BulkMoveProgressEvent {
                        current: idx + 1,
                        total,
                        percent,
                        current_file: Some(path),
                    },
                );
            }
            last_emit = now_i;
        }
    }

    conn.execute("COMMIT", [])?;

    Ok(BulkMoveResult { moved, failed })
}

/// Get all files in the black hole, optionally filtered by source
pub fn get_black_hole_files(
    conn: &Connection,
    filter_by_source: Option<String>,
) -> Result<Vec<BlackHoleEntry>> {
    let query = if filter_by_source.is_some() {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         WHERE bh.from_where = ?1
         ORDER BY bh.moved_at DESC"
    } else {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         ORDER BY bh.moved_at DESC"
    };
    let params: Vec<rusqlite::types::Value> = match filter_by_source {
        Some(s) => vec![s.into()],
        None => vec![],
    };

    let mut stmt = conn.prepare(query)?;

    let entries = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let moved_at_str: String = row.get(5)?;
        let moved_at = chrono::DateTime::parse_from_rfc3339(&moved_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(BlackHoleEntry {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            filename: row.get(2)?,
            original_path: row.get(3)?,
            from_where: row.get(4)?,
            moved_at,
            file_size: row.get(6)?,
        })
    })?;

    entries.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Remove a file from the black hole (restore)
pub fn remove_from_black_hole(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM black_hole WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(())
}

/// Permanently delete a file from database and disk (send to void)
///
/// Catalog first, disk second — the same stance as `api::masters::delete_master`
/// — and every catalog write (the master unregister of
/// [`unregister_master_if_any`] included) is one savepoint. A crash between the
/// two halves leaves an orphan file on disk, which a later scan simply
/// re-ingests; it can never leave a catalog row pointing at a file that is gone
/// forever, nor a half-rewired calibration lineage. The savepoint nests, so this
/// composes inside a caller's transaction.
pub fn send_to_void(conn: &Connection, file_id: i64) -> Result<()> {
    // Get file path before deletion
    let path: String = conn.query_row(
        "SELECT path FROM files WHERE id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    // Catalog first, disk second (same stance as api::masters::delete_master):
    // the benign crash leftover is an orphan file on disk — which a later
    // scan simply re-ingests — never a catalog row pointing at a file that
    // is gone forever. All catalog writes are one atomic unit so the
    // master-unregister sequence can't half-commit (audit C5/I3).
    let sp = crate::db::SavepointGuard::new(conn, "send_to_void")?;
    unregister_master_if_any(conn, file_id)?;
    conn.execute(
        "DELETE FROM black_hole WHERE file_id = ?1",
        params![file_id],
    )?;
    // files delete cascades to frames, frame_tags, etc.
    conn.execute("DELETE FROM files WHERE id = ?1", params![file_id])?;
    sp.commit()?;

    if std::path::Path::new(&path).exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::error!(file_id, path = %path, error = %e,
                "send_to_void: catalog rows removed but disk delete failed; file remains on disk");
            return Err(e.into());
        }
    }

    Ok(())
}

/// Permanently delete all files in black hole (send all to void)
pub fn send_all_to_void(conn: &Connection) -> Result<usize> {
    // Get all file IDs in black hole
    let mut stmt = conn.prepare("SELECT file_id FROM black_hole")?;
    let file_ids: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let count = file_ids.len();

    // Delete each file
    for file_id in file_ids {
        // Ignore errors (file might already be gone)
        let _ = send_to_void(conn, file_id);
    }

    Ok(count)
}

/// Find folders with high similarity (many duplicate files)
pub fn find_duplicate_folders(
    conn: &Connection,
    similarity_threshold: f64,
) -> Result<Vec<FolderSimilarity>> {
    // Get all unique folder paths from files
    let mut folder_files: HashMap<String, Vec<(i64, String, i64)>> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT id, path, metadata_hash, size
         FROM files
         WHERE metadata_hash IS NOT NULL
         AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = files.id)"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    for row in rows {
        let (id, path, hash, size) = row?;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let folder = parent.to_string_lossy().to_string();
            folder_files
                .entry(folder)
                .or_insert_with(Vec::new)
                .push((id, hash, size));
        }
    }

    let mut similarities = Vec::new();
    let folders: Vec<_> = folder_files.keys().cloned().collect();

    // Compare all folder pairs
    for i in 0..folders.len() {
        for j in (i + 1)..folders.len() {
            let folder_a = &folders[i];
            let folder_b = &folders[j];

            let files_a = &folder_files[folder_a];
            let files_b = &folder_files[folder_b];

            // Find common hashes
            let hashes_a: HashMap<_, _> = files_a.iter().map(|(id, hash, size)| (hash.clone(), (*id, *size))).collect();
            let hashes_b: HashMap<_, _> = files_b.iter().map(|(id, hash, size)| (hash.clone(), (*id, *size))).collect();

            let mut shared_count = 0;
            let mut shared_size = 0i64;
            let mut shared_file_ids = Vec::new();

            for (hash, (id_a, size)) in &hashes_a {
                if hashes_b.contains_key(hash) {
                    shared_count += 1;
                    shared_size += size;
                    shared_file_ids.push(*id_a);
                }
            }

            if shared_count > 0 {
                let total_unique = (files_a.len() + files_b.len() - shared_count) as f64;
                let similarity_percent = (shared_count as f64 / total_unique) * 100.0;

                if similarity_percent >= similarity_threshold {
                    similarities.push(FolderSimilarity {
                        folder_a: folder_a.clone(),
                        folder_b: folder_b.clone(),
                        similarity_percent,
                        shared_files: shared_count as i32,
                        shared_size,
                        unique_a: (files_a.len() - shared_count) as i32,
                        unique_b: (files_b.len() - shared_count) as i32,
                        shared_file_ids,
                    });
                }
            }
        }
    }

    // Sort by similarity percentage (highest first)
    similarities.sort_by(|a, b| {
        b.similarity_percent
            .partial_cmp(&a.similarity_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(similarities)
}

/// Rebuild the folder similarity cache table
/// This clears existing cache and recomputes all folder similarities
pub fn rebuild_folder_similarity_cache(
    conn: &Connection,
    similarity_threshold: f64,
) -> Result<usize> {
    // Start transaction
    conn.execute("BEGIN TRANSACTION", [])?;

    // Clear existing cache
    conn.execute("DELETE FROM folder_similarity", [])?;

    // Compute folder similarities (reuse existing logic)
    let similarities = find_duplicate_folders(conn, similarity_threshold)?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut count = 0;

    for sim in similarities {
        conn.execute(
            "INSERT INTO folder_similarity
             (folder_a, folder_b, shared_files, shared_size, unique_a, unique_b, similarity_percent, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                sim.folder_a,
                sim.folder_b,
                sim.shared_files,
                sim.shared_size,
                sim.unique_a,
                sim.unique_b,
                sim.similarity_percent,
                now
            ],
        )?;
        count += 1;
    }

    conn.execute("COMMIT", [])?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, '/tmp/a.fits', 'a.fits', 10, '2024-01-01T00:00:00Z', 'FITS')",
            [],
        )
        .unwrap();
        conn
    }

    /// Re-blackholing the same file must not create a duplicate row and must
    /// return the same canonical row id.
    #[test]
    fn add_to_black_hole_is_idempotent() {
        let conn = setup();

        let id1 = add_to_black_hole(&conn, 1, "light", "/tmp/a.fits").unwrap();
        let id2 = add_to_black_hole(&conn, 1, "light", "/tmp/a.fits").unwrap();
        assert_eq!(id1, id2, "repeat blackhole must return the same row id");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM black_hole WHERE file_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "must not create a duplicate black_hole row");
    }

    /// A bulk move over a mix of fresh and already-blackholed files counts only
    /// the genuinely new ones as `moved`, never duplicates, never fails the dupes.
    #[test]
    fn bulk_move_skips_already_blackholed() {
        let conn = setup();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (2, '/tmp/b.fits', 'b.fits', 10, '2024-01-01T00:00:00Z', 'FITS')",
            [],
        )
        .unwrap();

        // File 1 is already blackholed; the bulk move includes it again.
        add_to_black_hole(&conn, 1, "light", "/tmp/a.fits").unwrap();

        let res = bulk_move_to_black_hole(&conn, &[1, 2], "light", None).unwrap();
        assert_eq!(res.moved, 1, "only the fresh file counts as moved");
        assert!(res.failed.is_empty(), "already-blackholed is a no-op, not a failure");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM black_hole", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "one row per file, no duplicates");
    }

    // ── Master interception (audit C3) ───────────────────────────────────────

    /// A `files` + `frames` pair, returning `(file_id, frame_id)`. FK
    /// enforcement is on, so both rows must exist before any calibration
    /// junction row can reference them.
    fn seed_file(conn: &Connection, path: &str, imagetyp: &str) -> (i64, i64) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 100, '2026-08-01T00:00:00Z', 'FITS')",
            params![path, path.rsplit('/').next().unwrap()],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
            params![file_id, imagetyp],
        )
        .unwrap();
        (file_id, conn.last_insert_rowid())
    }

    struct MasterFixture {
        raw_set_id: i64,
        master_set_id: i64,
        master_file_id: i64,
        consumer_frame_id: i64,
    }

    /// The end state `register_master` leaves behind: a raw source set
    /// superseded by a master set that owns one file at `master_path`, carries
    /// provenance, and has inherited the raw set's consumer link.
    fn seed_registered_master(conn: &Connection, master_path: &str) -> MasterFixture {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let raw_set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library)
             VALUES ('MasterDark', '2026-08-01', 1)",
            [],
        )
        .unwrap();
        let master_set_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            params![master_set_id, raw_set_id],
        )
        .unwrap();

        let (master_file_id, master_frame_id) = seed_file(conn, master_path, "MASTERDARK");
        conn.execute(
            "UPDATE frames SET is_master = 1 WHERE id = ?1",
            params![master_frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![master_set_id, master_frame_id],
        )
        .unwrap();

        // The raw members the master was integrated from — untouched by any of
        // this, and the reason the raw set is worth restoring.
        let (_, raw_frame_id) = seed_file(conn, "/lib/raw1.fits", "DARK");
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![raw_set_id, raw_frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'h', '2026-08-02T00:00:00Z')",
            params![master_set_id, raw_set_id],
        )
        .unwrap();

        let (_, consumer_frame_id) = seed_file(conn, "/lib/light.fits", "LIGHT");
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
            params![consumer_frame_id, master_set_id],
        )
        .unwrap();

        MasterFixture {
            raw_set_id,
            master_set_id,
            master_file_id,
            consumer_frame_id,
        }
    }

    /// Everything `unregister_master_set` is supposed to have undone by the
    /// time the black-hole/void path proceeds with the file itself.
    fn assert_lineage_restored(conn: &Connection, fx: &MasterFixture) {
        let target: i64 = conn
            .query_row(
                "SELECT calibration_set_id FROM calibration_set_to_frames
                  WHERE source_id = ?1 AND source_type = 'frame'",
                [fx.consumer_frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            target, fx.raw_set_id,
            "consumer link repointed back to the raw set"
        );

        let sup: Option<i64> = conn
            .query_row(
                "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
                [fx.raw_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sup, None, "raw set is matchable again");

        let masters: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [fx.master_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(masters, 0, "master shell row is gone");

        let prov: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM master_provenance WHERE master_set_id = ?1",
                [fx.master_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov, 0, "master provenance dropped");
    }

    /// Voiding a master's file must restore the lineage first — otherwise the
    /// raw set stays superseded by a set (and a file) that no longer exists.
    #[test]
    fn send_to_void_unregisters_a_master_first() {
        let dir = tempfile::tempdir().unwrap();
        let master_path = dir.path().join("master_dark.fits");
        std::fs::write(&master_path, b"master").unwrap();

        let conn = setup();
        let fx = seed_registered_master(&conn, master_path.to_str().unwrap());

        send_to_void(&conn, fx.master_file_id).unwrap();

        assert_lineage_restored(&conn, &fx);

        let files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [fx.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files, 0, "voided file row is gone");
        assert!(!master_path.exists(), "voided file is gone from disk");
    }

    /// Black-holing a master's file restores the lineage too, but the file row
    /// SURVIVES: the ex-master is now an ordinary catalog file sitting in the
    /// Black Hole, restorable like any other.
    #[test]
    fn bulk_move_to_black_hole_unregisters_a_master_first() {
        let conn = setup();
        let fx = seed_registered_master(&conn, "/lib/master_dark.fits");

        let res = bulk_move_to_black_hole(&conn, &[fx.master_file_id], "test", None).unwrap();
        assert_eq!(res.moved, 1);
        assert!(res.failed.is_empty(), "{:?}", res.failed);

        assert_lineage_restored(&conn, &fx);

        let files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [fx.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files, 1, "black-holed file row survives");

        let bh: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM black_hole WHERE file_id = ?1",
                [fx.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bh, 1, "the file really is in the black hole");
    }

    /// The single-file path (`move_to_black_hole` command → `add_to_black_hole`)
    /// intercepts identically, and stays idempotent on a repeat call.
    #[test]
    fn add_to_black_hole_unregisters_a_master_first() {
        let conn = setup();
        let path = "/lib/master_dark.fits";
        let fx = seed_registered_master(&conn, path);

        let id1 = add_to_black_hole(&conn, fx.master_file_id, "test", path).unwrap();
        assert_lineage_restored(&conn, &fx);

        // Second call: no master left to unregister, still the same row.
        let id2 = add_to_black_hole(&conn, fx.master_file_id, "test", path).unwrap();
        assert_eq!(id1, id2, "repeat blackhole of an ex-master is still idempotent");

        let files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE id = ?1",
                [fx.master_file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(files, 1, "black-holed file row survives");
    }

    /// The guard is a no-op for ordinary catalog files: an ordinary calibration
    /// set losing a member to the Black Hole (or the void) keeps its row, its
    /// remaining members and its consumer link exactly as they were.
    #[test]
    fn non_master_files_pass_through_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let voided = dir.path().join("d3.fits");
        std::fs::write(&voided, b"dark").unwrap();

        let conn = setup();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-08-01')",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        let mut member_files = Vec::new();
        for path in ["/lib/d1.fits", "/lib/d2.fits", voided.to_str().unwrap()] {
            let (file_id, frame_id) = seed_file(&conn, path, "DARK");
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
            member_files.push(file_id);
        }
        let (_, consumer_frame_id) = seed_file(&conn, "/lib/light.fits", "LIGHT");
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            params![consumer_frame_id, set_id],
        )
        .unwrap();

        // One member down each of the three delete paths.
        add_to_black_hole(&conn, member_files[0], "test", "/lib/d1.fits").unwrap();
        let res = bulk_move_to_black_hole(&conn, &[member_files[1]], "test", None).unwrap();
        assert!(res.failed.is_empty(), "{:?}", res.failed);
        send_to_void(&conn, member_files[2]).unwrap();

        let alive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive, 1, "an ordinary calibration set is never unregistered");

        let target: i64 = conn
            .query_row(
                "SELECT calibration_set_id FROM calibration_set_to_frames
                  WHERE source_id = ?1 AND source_type = 'frame'",
                [consumer_frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, set_id, "consumer link untouched");

        // Only the voided member's membership row goes (files CASCADE); the two
        // black-holed ones keep theirs.
        let members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(members, 2, "black-holed members keep their membership rows");
    }

    // ── Atomicity / nesting (audit C5 + I3) ──────────────────────────────────

    /// Both single-file delete paths must compose inside a caller's
    /// transaction: they wrap their catalog writes in a savepoint, and a
    /// savepoint nests where a raw `BEGIN` would error with "cannot start a
    /// transaction within a transaction". The outer rollback must take the
    /// inner writes with it.
    #[test]
    fn black_hole_and_void_nest_inside_an_outer_transaction() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/t/v.fits','v.fits',1,'2026-01-01T00:00:00Z','FITS')",
            [],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();

        // Raw BEGIN inside the functions would error with "cannot start a
        // transaction within a transaction"; savepoints must nest.
        let tx = conn.unchecked_transaction().unwrap();
        add_to_black_hole(&conn, fid, "duplicates", "/t/v.fits").unwrap();
        send_to_void(&conn, fid).unwrap();
        drop(tx); // rollback

        // The outer rollback must take the inner writes with it.
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "outer rollback must restore the files row");
        let bh: i64 = conn
            .query_row("SELECT COUNT(*) FROM black_hole", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bh, 0);
    }

    // ── Source filter is data, not SQL (audit C1) ────────────────────────────

    /// The `from_where` filter arrives from the UI (and, on the web build, from
    /// an HTTP request body): it must reach SQLite as a bound value, never as
    /// query text.
    #[test]
    fn black_hole_filter_is_bound_not_spliced() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/t/a.fits','a.fits',1,'2026-01-01T00:00:00Z','FITS')",
            [],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();
        add_to_black_hole(&conn, fid, "duplicates", "/t/a.fits").unwrap();

        // A single quote in the filter must be data, not syntax: no SQL error,
        // zero rows (no source is literally named this).
        let evil = "x' UNION SELECT 1,1,'p','p','w','2026-01-01T00:00:00Z',1 --".to_string();
        let rows = get_black_hole_files(&conn, Some(evil)).unwrap();
        assert!(
            rows.is_empty(),
            "injection text must match nothing: {rows:?}"
        );

        // And a legitimate filter still works.
        let rows = get_black_hole_files(&conn, Some("duplicates".into())).unwrap();
        assert_eq!(rows.len(), 1);
    }
}
