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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkMoveProgressEvent {
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub current_file: Option<String>,
}

/// Add a file to the black hole (soft delete).
///
/// Idempotent: a `UNIQUE(file_id)` index guarantees one row per file, and
/// `INSERT OR IGNORE` makes re-blackholing an already-blackholed file a no-op
/// rather than a duplicate row. Returns the canonical row id either way.
pub fn add_to_black_hole(
    conn: &Connection,
    file_id: i64,
    from_where: &str,
    original_path: &str,
) -> Result<i64> {
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
    let query = if let Some(source) = filter_by_source {
        format!(
            "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
             FROM black_hole bh
             JOIN files f ON bh.file_id = f.id
             WHERE bh.from_where = '{}'
             ORDER BY bh.moved_at DESC",
            source
        )
    } else {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         ORDER BY bh.moved_at DESC"
            .to_string()
    };

    let mut stmt = conn.prepare(&query)?;

    let entries = stmt.query_map([], |row| {
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

/// Permanently delete a file from disk and database (send to void)
pub fn send_to_void(conn: &Connection, file_id: i64) -> Result<()> {
    // Get file path before deletion
    let path: String = conn.query_row(
        "SELECT path FROM files WHERE id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    // Delete physical file
    if std::path::Path::new(&path).exists() {
        std::fs::remove_file(&path)?;
    }

    // Delete from black_hole table (CASCADE will handle this, but explicit is clearer)
    conn.execute(
        "DELETE FROM black_hole WHERE file_id = ?1",
        params![file_id],
    )?;

    // Delete from files table (will cascade to frames, frame_tags, etc.)
    conn.execute(
        "DELETE FROM files WHERE id = ?1",
        params![file_id],
    )?;

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
}
