//! One-shot background backfill of `files.content_hash` for the whole catalog
//! (Task E, item 5).
//!
//! The device-to-device transfer dedup handshake matches a sender's sampling
//! hashes against `files.content_hash` over the receiver's whole catalog. Before
//! Task E that column was written only by sync-ingest and by the scanner when
//! `duplicates.use_content_hash` was on (default off), so a scanned-but-never-
//! synced library was effectively invisible to dedup — the owner's field DB had
//! `content_hash` on 2 of 13,566 rows. The scanner now hashes every new/changed
//! file unconditionally; this backfill closes the gap for rows written *before*
//! that change.
//!
//! It runs once per process launch (single-flight guard) on a background thread:
//! list every `files` row still missing a hash, re-hash the ones present on disk
//! with the same [`compute_xxhash`](crate::duplicates::compute_xxhash) the
//! scanner / ingest / sender-offer all use, and UPDATE the row. Missing or
//! unreadable files are skipped at `debug` and retried on the next launch, so
//! the pass is idempotent and self-healing. A gentle per-chunk nap keeps a
//! multi-thousand-file catalog converging in the background without starving the
//! app's own IO.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::db::Database;

/// Rows hashed per chunk between throttle naps.
const CHUNK: usize = 64;

/// Nap between chunks. Gentle throttle so the backfill converges in the
/// background without monopolising disk IO: 64 files (~1.5 MB sampled each)
/// then a short pause. On a ~13.5k-file catalog this is ~210 chunks — a few
/// minutes of mostly-idle wall time, never a startup stall (it runs off-thread).
const CHUNK_SLEEP: Duration = Duration::from_millis(50);

/// Per-process single-flight guard: the backfill runs at most once per launch.
/// A second spawn (StrictMode double-mount, a repeat `initialize_database`) is a
/// no-op. Kept out of [`backfill_content_hashes`] itself so tests can drive the
/// pass repeatedly to assert idempotency.
static BACKFILL_RAN: AtomicBool = AtomicBool::new(false);

/// Outcome of one backfill pass. Returned for tests/logging; hosts ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillSummary {
    /// Rows that were NULL-hash at the start of the pass.
    pub pending: usize,
    /// Rows successfully hashed and UPDATEd this pass.
    pub updated: usize,
    /// Rows skipped (missing on disk, unreadable, or UPDATE failed) — still NULL,
    /// retried on the next launch.
    pub skipped: usize,
}

/// Host entry point: run the whole-library content-hash backfill at most once
/// per process. Spawn it on a background thread post-init (blocking file IO).
/// Errors are logged, never fatal.
pub fn backfill_content_hashes_once(db: &Database) {
    if BACKFILL_RAN.swap(true, Ordering::SeqCst) {
        tracing::debug!("content-hash backfill already ran this launch; skipping");
        return;
    }
    let _ = backfill_content_hashes(db);
}

/// Run one backfill pass (no single-flight guard — that lives in
/// [`backfill_content_hashes_once`]). Idempotent at the DB level: only NULL-hash
/// rows are visited, so a re-run converges and never re-hashes a done row.
pub fn backfill_content_hashes(db: &Database) -> BackfillSummary {
    let conn = db.conn();

    // Snapshot the NULL-hash rows into a Vec up front so we don't hold a live
    // statement/cursor open across the hashing IO and the per-row UPDATEs.
    let pending: Vec<(i64, String)> = match conn
        .prepare("SELECT id, path FROM files WHERE content_hash IS NULL")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "content-hash backfill: failed to list pending rows");
            return BackfillSummary::default();
        }
    };

    let pending_count = pending.len();
    if pending_count == 0 {
        tracing::info!(pending = 0, "content-hash backfill: nothing to do");
        return BackfillSummary::default();
    }
    tracing::info!(pending = pending_count, "content-hash backfill started");

    let mut updated = 0usize;
    let mut skipped = 0usize;
    let chunk_total = pending_count.div_ceil(CHUNK);
    for (chunk_idx, chunk) in pending.chunks(CHUNK).enumerate() {
        for (id, path) in chunk {
            let p = Path::new(path);
            if !p.exists() {
                skipped += 1;
                tracing::debug!(file_id = id, path = %path, "content-hash backfill: file missing on disk; skipping");
                continue;
            }
            match crate::duplicates::compute_xxhash(p) {
                Ok(hash) => {
                    match conn.execute(
                        "UPDATE files SET content_hash = ?1 WHERE id = ?2",
                        rusqlite::params![hash, id],
                    ) {
                        Ok(_) => updated += 1,
                        Err(e) => {
                            skipped += 1;
                            tracing::warn!(file_id = id, error = %e, "content-hash backfill: UPDATE failed");
                        }
                    }
                }
                Err(e) => {
                    skipped += 1;
                    tracing::debug!(file_id = id, path = %path, error = %e, "content-hash backfill: unreadable; skipping");
                }
            }
        }
        tracing::debug!(chunk = chunk_idx + 1, of = chunk_total, updated, skipped, "content-hash backfill chunk done");
        // Gentle throttle between chunks (skip the nap after the last chunk).
        if chunk_idx + 1 < chunk_total {
            std::thread::sleep(CHUNK_SLEEP);
        }
    }

    tracing::info!(pending = pending_count, updated, skipped, "content-hash backfill finished");
    BackfillSummary { pending: pending_count, updated, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Insert a `files` row with a NULL content_hash pointing at `path`.
    fn insert_null_row(conn: &rusqlite::Connection, path: &Path, name: &str, size: i64) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
             VALUES (?1, ?2, ?3, '2026-07-11T00:00:00Z', 'FITS', '2026-07-11T00:00:00Z', NULL)",
            rusqlite::params![path.to_string_lossy().to_string(), name, size],
        )
        .unwrap();
    }

    #[test]
    fn backfill_hashes_null_rows_skips_missing_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = tmp.path().join("catalog.db");
        let db = Database::new(catalog).unwrap();

        // Two files with real payload on disk (large enough to exercise all
        // three sampling positions), plus one row whose path is missing.
        let f1 = tmp.path().join("a.fits");
        let f2 = tmp.path().join("b.fits");
        let missing = tmp.path().join("gone.fits");
        std::fs::write(&f1, vec![0x11u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(&f2, vec![0x22u8; 2 * 1024 * 1024]).unwrap();

        {
            let conn = db.conn();
            insert_null_row(&conn, &f1, "a.fits", 2 * 1024 * 1024);
            insert_null_row(&conn, &f2, "b.fits", 2 * 1024 * 1024);
            insert_null_row(&conn, &missing, "gone.fits", 0);
        }

        let s1 = backfill_content_hashes(&db);
        assert_eq!(s1.pending, 3);
        assert_eq!(s1.updated, 2, "both on-disk files hashed");
        assert_eq!(s1.skipped, 1, "missing-path row skipped");

        // Hashed rows carry exactly the compute_xxhash value; missing stays NULL.
        {
            let conn = db.conn();
            let h1: Option<String> = conn
                .query_row("SELECT content_hash FROM files WHERE filename='a.fits'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                h1.as_deref(),
                Some(crate::duplicates::compute_xxhash(&f1).unwrap().as_str()),
                "backfill hash must equal the scanner/offer compute_xxhash"
            );
            let h2: Option<String> = conn
                .query_row("SELECT content_hash FROM files WHERE filename='b.fits'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(h2.as_deref(), Some(crate::duplicates::compute_xxhash(&f2).unwrap().as_str()));
            let hg: Option<String> = conn
                .query_row("SELECT content_hash FROM files WHERE filename='gone.fits'", [], |r| r.get(0))
                .unwrap();
            assert!(hg.is_none(), "missing-path row must stay NULL");
        }

        // Idempotent: a second pass sees only the still-NULL missing row and
        // hashes nothing new.
        let s2 = backfill_content_hashes(&db);
        assert_eq!(s2.pending, 1, "only the missing-path row remains NULL");
        assert_eq!(s2.updated, 0);
        assert_eq!(s2.skipped, 1);
    }

    #[test]
    fn backfill_empty_catalog_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let s = backfill_content_hashes(&db);
        assert_eq!(s, BackfillSummary::default());
    }
}
