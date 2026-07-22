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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;

use crate::db::Database;

/// Rows hashed per chunk between throttle naps.
const CHUNK: usize = 64;

/// Nap between chunks. Gentle throttle so the backfill converges in the
/// background without monopolising disk IO: 64 files (~1.5 MB sampled each)
/// then a short pause. On a ~13.5k-file catalog this is ~210 chunks — a few
/// minutes of mostly-idle wall time, never a startup stall (it runs off-thread).
const CHUNK_SLEEP: Duration = Duration::from_millis(50);

/// Per-DB single-flight guard (review fix): at most one backfill per DATABASE
/// PATH per process launch. Keyed by path — a process-global bool would starve a
/// rebound catalog (dev-reset / DB-path change re-runs `initialize_database` with
/// a NEW db whose NULL-hash rows would then never backfill until a full restart).
/// A repeat spawn for the SAME path (StrictMode double-mount, a repeat
/// `initialize_database`) is still a no-op. Kept out of
/// [`backfill_content_hashes`] itself so tests can drive the pass repeatedly.
static BACKFILL_RAN_FOR: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

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
    let ran = BACKFILL_RAN_FOR.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let mut set = ran.lock().expect("backfill guard mutex poisoned");
        if !set.insert(db.path().to_path_buf()) {
            tracing::debug!(path = %db.path().display(), "content-hash backfill already ran for this catalog this launch; skipping");
            return;
        }
    }
    let _ = backfill_content_hashes(db);
}

/// Run one backfill pass (no single-flight guard — that lives in
/// [`backfill_content_hashes_once`]). Idempotent at the DB level: only NULL-hash
/// rows are visited, so a re-run converges and never re-hashes a done row.
pub fn backfill_content_hashes(db: &Database) -> BackfillSummary {
    // Snapshot the NULL-hash rows (with their recorded size/modified_at for the
    // stale-row check below) into a Vec up front, then DROP the connection: the
    // pool is small (max 8) and this pass runs for minutes — holding a checked-out
    // connection across the hashing IO and the chunk naps would pin a slot idle
    // and, under a concurrent burst, push other callers into the pool-exhaustion
    // panic (review fix). Each chunk checks a connection out only for its writes.
    let pending: Vec<(i64, String, i64, String)> = {
        let conn = db.conn();
        match conn
            .prepare("SELECT id, path, size, modified_at FROM files WHERE content_hash IS NULL")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
            }) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "content-hash backfill: failed to list pending rows");
                return BackfillSummary::default();
            }
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
        // Hash the chunk's files FIRST (no connection held), then check a pooled
        // connection out only for the chunk's UPDATEs.
        let mut hashed: Vec<(i64, String)> = Vec::with_capacity(chunk.len());
        for (id, path, db_size, db_modified) in chunk {
            let p = Path::new(path);
            // Stale-row guard (review fix): only hash a file whose on-disk
            // (size, modified_at) still MATCH the row — the same comparison the
            // scanner's unchanged-skip uses. A drifted file means the row's
            // metadata describes different bytes; hashing them here would stamp a
            // new-content hash onto a stale row and let the dedup handshake
            // wrongly report "already on peer" for content the catalog has never
            // described. Skip — the next scan's reparse rewrites hash AND
            // metadata together.
            let on_disk = std::fs::metadata(p).ok().map(|m| {
                let size = m.len() as i64;
                let modified = m
                    .modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
                (size, modified)
            });
            let matches_row = matches!(
                on_disk.as_ref(),
                Some((s, Some(m))) if s == db_size && m == db_modified
            );
            if !matches_row {
                skipped += 1;
                tracing::debug!(file_id = id, path = %path, "content-hash backfill: missing or drifted on disk; left for the next scan");
                continue;
            }
            match crate::duplicates::compute_xxhash(p) {
                Ok(hash) => hashed.push((*id, hash)),
                Err(e) => {
                    skipped += 1;
                    tracing::debug!(file_id = id, path = %path, error = %e, "content-hash backfill: unreadable; skipping");
                }
            }
        }
        if !hashed.is_empty() {
            let conn = db.conn();
            for (id, hash) in &hashed {
                match conn.execute(
                    "UPDATE files SET content_hash = ?1 WHERE id = ?2 AND content_hash IS NULL",
                    rusqlite::params![hash, id],
                ) {
                    Ok(_) => updated += 1,
                    Err(e) => {
                        skipped += 1;
                        tracing::warn!(file_id = id, error = %e, "content-hash backfill: UPDATE failed");
                    }
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

    /// Insert a `files` row with a NULL content_hash pointing at `path`, carrying
    /// the file's REAL on-disk (size, modified_at) — the stale-row guard compares
    /// them exactly like the scanner's unchanged-skip, so a fabricated stamp would
    /// make the backfill (correctly) skip the row. A missing file gets the caller's
    /// `size` and a fixed stamp (its row is expected to be skipped anyway).
    fn insert_null_row(conn: &rusqlite::Connection, path: &Path, name: &str, size: i64) {
        let (size, modified) = match std::fs::metadata(path) {
            Ok(m) => (
                m.len() as i64,
                m.modified()
                    .ok()
                    .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339())
                    .unwrap_or_else(|| "2026-07-11T00:00:00Z".to_string()),
            ),
            Err(_) => (size, "2026-07-11T00:00:00Z".to_string()),
        };
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
             VALUES (?1, ?2, ?3, ?4, 'FITS', '2026-07-11T00:00:00Z', NULL)",
            rusqlite::params![path.to_string_lossy().to_string(), name, size, modified],
        )
        .unwrap();
    }

    #[test]
    fn backfill_hashes_null_rows_skips_missing_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = tmp.path().join("catalog.db");
        let db = Database::new(catalog).unwrap();

        // Two files with real payload on disk (large enough to exercise all
        // three sampling positions), one row whose path is missing, and one whose
        // on-disk bytes DRIFTED from the row's recorded (size, modified_at) —
        // the stale-row guard must leave it for the next scan's reparse.
        let f1 = tmp.path().join("a.fits");
        let f2 = tmp.path().join("b.fits");
        let missing = tmp.path().join("gone.fits");
        let drifted = tmp.path().join("drifted.fits");
        std::fs::write(&f1, vec![0x11u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(&f2, vec![0x22u8; 2 * 1024 * 1024]).unwrap();
        std::fs::write(&drifted, vec![0x33u8; 1024]).unwrap();

        {
            let conn = db.conn();
            insert_null_row(&conn, &f1, "a.fits", 2 * 1024 * 1024);
            insert_null_row(&conn, &f2, "b.fits", 2 * 1024 * 1024);
            insert_null_row(&conn, &missing, "gone.fits", 0);
            // Drifted row: size deliberately disagrees with the on-disk file.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, 'drifted.fits', 999, '2026-07-11T00:00:00Z', 'FITS', '2026-07-11T00:00:00Z', NULL)",
                rusqlite::params![drifted.to_string_lossy().to_string()],
            )
            .unwrap();
        }

        let s1 = backfill_content_hashes(&db);
        assert_eq!(s1.pending, 4);
        assert_eq!(s1.updated, 2, "both matching on-disk files hashed");
        assert_eq!(s1.skipped, 2, "missing-path AND drifted rows skipped");

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
        assert_eq!(s2.pending, 2, "the missing-path and drifted rows remain NULL");
        assert_eq!(s2.updated, 0);
        assert_eq!(s2.skipped, 2);
    }

    #[test]
    fn backfill_empty_catalog_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let s = backfill_content_hashes(&db);
        assert_eq!(s, BackfillSummary::default());
    }
}
