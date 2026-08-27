//! Background content-index pass: populates `files.content_hash` for the whole
//! catalog.
//!
//! The column has two consumers: the device-to-device transfer dedup handshake,
//! which matches a sender's sampling hashes against `files.content_hash` over
//! the receiver's whole catalog, and the Duplicates view, which groups by
//! content instead of by (size, date, filename) when
//! `duplicates.use_content_hash` is on. The scanner only writes the column when
//! that same setting is on (default off — hashing every scanned file is a
//! measurable performance regression), so for most libraries this pass is the
//! ordinary populator of the column, not a one-off migration: list every `files`
//! row still missing a hash, re-hash the ones present on disk with the same
//! [`compute_xxhash`](crate::duplicates::compute_xxhash) the scanner / ingest /
//! sender-offer all use, and UPDATE the row.
//!
//! Missing or unreadable files are skipped at `debug` and retried on the next
//! run, so the pass is idempotent and self-healing. A gentle per-chunk nap
//! keeps a multi-thousand-file catalog converging in the background without
//! starving the app's own IO.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::events::{emit_event, ProgressEmitter};

/// Rows hashed per chunk between throttle naps.
const CHUNK: usize = 64;

/// Nap between chunks. Gentle throttle so the backfill converges in the
/// background without monopolising disk IO: 64 files (~1.5 MB sampled each)
/// then a short pause. On a ~13.5k-file catalog this is ~210 chunks — a few
/// minutes of mostly-idle wall time, never a startup stall (it runs off-thread).
const CHUNK_SLEEP: Duration = Duration::from_millis(50);

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
    /// True if the pass stopped early because its cancel flag was set.
    pub cancelled: bool,
}

/// Per-chunk progress for the content-index job. UI data, not a log line — the
/// pass also logs its own `debug!` per chunk (ProgressEmitter events and
/// tracing stay separate concerns).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexProgress {
    pub done: usize,
    pub total: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// Per-file progress of the master strong-hash pass. UI data for the scan
/// progress surface, not a log line — the pass logs its own lines separately.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MasterHashProgress {
    pub done: usize,
    pub total: usize,
    pub path: String,
}

/// Terminal event. Emitted on EVERY exit path — normal completion, cancel,
/// the nothing-to-do early return, AND the row-listing failure — so the
/// sidebar card and the notification handler have exactly one place to close
/// on; none of them can hang open waiting for an event that never comes.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexFinished {
    pub updated: usize,
    pub skipped: usize,
    pub cancelled: bool,
    /// The pass could not enumerate its work (locked DB, schema mismatch,
    /// corrupt catalog). Without this the failure exit is indistinguishable
    /// from a clean nothing-to-do run, and the UI would cheerfully report
    /// "finished — 0 indexed" over a broken catalog.
    pub failed: bool,
}

/// Rows still missing a hash. Pure SQL — never touches the disk, so the status
/// command is safe to call from the UI on every Settings mount.
pub fn count_pending(db: &Database) -> usize {
    let conn = db.conn();
    conn.query_row("SELECT COUNT(*) FROM files WHERE content_hash IS NULL", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|n| n as usize)
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "content index: failed to count pending rows");
        0
    })
}

/// Run one content-index pass. No single-flight guard of its own — that (and
/// the trigger policy: the sync gate, compute-queue admission, the boot and
/// post-scan re-arms) lives in [`crate::api::content_index`]. Idempotent at the
/// DB level: only NULL-hash rows are visited, so a re-run converges and never
/// re-hashes a done row.
pub fn run_content_index(
    db: &Database,
    emitter: &dyn ProgressEmitter,
    cancel: Arc<AtomicBool>,
) -> BackfillSummary {
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
                tracing::error!(error = %e, "content index: failed to list pending rows");
                emit_event(
                    emitter,
                    "content-index-finished",
                    &ContentIndexFinished { updated: 0, skipped: 0, cancelled: false, failed: true },
                );
                return BackfillSummary::default();
            }
        }
    };

    let pending_count = pending.len();
    if pending_count == 0 {
        tracing::info!(pending = 0, "content index: nothing to do");
        emit_event(
            emitter,
            "content-index-finished",
            &ContentIndexFinished { updated: 0, skipped: 0, cancelled: false, failed: false },
        );
        return BackfillSummary::default();
    }
    tracing::info!(pending = pending_count, "content index started");

    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut cancelled = false;
    let chunk_total = pending_count.div_ceil(CHUNK);
    for (chunk_idx, chunk) in pending.chunks(CHUNK).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            tracing::info!(updated, skipped, "content index cancelled");
            break;
        }

        // Hash the chunk's files FIRST (no connection held), then check a pooled
        // connection out only for the chunk's UPDATEs.
        let mut hashed: Vec<(i64, String)> = Vec::with_capacity(chunk.len());
        for (id, path, db_size, db_modified) in chunk {
            // Poll per file as well as per chunk: a chunk is 64 files x ~1.5 MB
            // of sampled reads, so on the network storage this job exists to
            // protect, chunk granularity alone leaves seconds of dead time after
            // the user presses X. The partial chunk's `hashed` vec is DISCARDED
            // rather than flushed — those rows stay NULL and the next pass
            // retries them, which is both correct and simpler than a half-chunk
            // write.
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            let p = Path::new(path);
            // Stale-row guard (review fix): only hash a file whose on-disk
            // (size, modified_at) still MATCH the row — the same comparison the
            // scanner's unchanged-skip uses. A drifted file means the row's
            // metadata describes different bytes; hashing them here would stamp a
            // new-content hash onto a stale row and let the dedup handshake
            // wrongly report "already on peer" for content the catalog has never
            // described. Skip — the next scan's reparse rewrites hash AND
            // metadata together.
            if !disk_matches_row(p, *db_size, db_modified) {
                skipped += 1;
                tracing::debug!(file_id = id, path = %path, "content index: missing or drifted on disk; left for the next scan");
                continue;
            }
            match crate::duplicates::compute_xxhash(p) {
                Ok(hash) => hashed.push((*id, hash)),
                Err(e) => {
                    skipped += 1;
                    tracing::debug!(file_id = id, path = %path, error = %e, "content index: unreadable; skipping");
                }
            }
        }
        if cancelled {
            tracing::info!(updated, skipped, "content index cancelled");
            break;
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
                        tracing::warn!(file_id = id, error = %e, "content index: UPDATE failed");
                    }
                }
            }
        }
        tracing::debug!(chunk = chunk_idx + 1, of = chunk_total, updated, skipped, "content index chunk done");
        emit_event(
            emitter,
            "content-index-progress",
            &ContentIndexProgress {
                done: ((chunk_idx + 1) * CHUNK).min(pending_count),
                total: pending_count,
                updated,
                skipped,
            },
        );
        // Gentle throttle between chunks (skip the nap after the last chunk).
        if chunk_idx + 1 < chunk_total {
            std::thread::sleep(CHUNK_SLEEP);
        }
    }

    emit_event(
        emitter,
        "content-index-finished",
        &ContentIndexFinished { updated, skipped, cancelled, failed: false },
    );
    tracing::info!(pending = pending_count, updated, skipped, cancelled, "content index finished");
    BackfillSummary { pending: pending_count, updated, skipped, cancelled }
}

/// Fill `files.strong_hash` for every master or processed file the header key
/// shortlists into a group and that has no hash yet.
///
/// The shortlist is the whole economy of this pass. A master with a unique
/// header fingerprint is nobody's duplicate, so reading 300 MiB to prove it
/// would be pure waste; on the owner's catalog the shortlist is 61 files /
/// 7.5 GiB out of 381 / 89.4 GiB. Idempotent — only NULL-hash rows are
/// visited, so a re-run converges.
///
/// A file whose size or mtime no longer matches its row is skipped, not
/// hashed: the row is stale and the next scan will rewrite it (same stale-row
/// stance as [`run_content_index`]). A read failure is logged and skipped —
/// one unreadable master must not abandon the pass.
///
/// Takes `&Connection`, not `&Database`: the caller is the scanner's phase 4,
/// which already holds the connection for the whole scan, so there is no
/// pool slot to give back.
///
/// The `is_master` / `imagetyp` predicate below is a hand-copy of
/// [`DuplicateKey::Master::eligibility`](crate::db::DuplicateKey) and must stay
/// identical to it — including its treatment of a NULL `imagetyp`, which under
/// SQL's three-valued logic `NOT IN` rejects. An unclassified file is
/// deliberately hashed by neither this pass nor decided by either key; see that
/// method's doc comment for why widening it would be wrong.
/// True when the file at `path` still matches its catalog row's
/// `(size, modified_at)` — the shared staleness contract of every
/// `strong_hash`/`content_hash` write: drifted metadata means the row
/// describes different bytes, and stamping a fresh-content hash onto a stale
/// row would let a later reader trust a hash the scanner never vouched for.
/// A missing file or unreadable mtime counts as "does not match".
pub(crate) fn disk_matches_row(path: &std::path::Path, size: i64, modified_at: &str) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => {
            let on_disk_mtime = m
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
            m.len() as i64 == size && on_disk_mtime.as_deref() == Some(modified_at)
        }
        Err(_) => false,
    }
}

pub fn fill_master_strong_hashes(
    conn: &Connection,
    emitter: &dyn ProgressEmitter,
    cancel: &AtomicBool,
) -> usize {
    let pending: Vec<(i64, String, i64, String)> = {
        let sql = "SELECT f.id, f.path, f.size, f.modified_at
                   FROM files f
                   JOIN fits_header fh ON fh.file_id = f.id
                   JOIN frames fr ON fr.file_id = f.id
                   WHERE (f.strong_hash IS NULL OR f.strong_hash = '')
                     AND fh.header_fingerprint IS NOT NULL
                     AND fh.header_fingerprint <> ''
                     AND (COALESCE(fr.is_master, 0) = 1
                          OR fr.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat'))
                     AND EXISTS (
                        SELECT 1 FROM files f2
                        JOIN fits_header fh2 ON fh2.file_id = f2.id
                        JOIN frames fr2 ON fr2.file_id = f2.id
                        WHERE f2.id <> f.id
                          AND f2.size = f.size
                          AND fh2.header_fingerprint = fh.header_fingerprint
                          AND (COALESCE(fr2.is_master, 0) = 1
                               OR fr2.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat'))
                     )";
        match conn.prepare(sql).and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "master strong-hash: failed to list pending rows");
                return 0;
            }
        }
    };

    if pending.is_empty() {
        tracing::debug!(pending = 0, "master strong-hash: nothing to do");
        return 0;
    }
    tracing::info!(pending = pending.len(), "master strong-hash: pass starting");

    let total = pending.len();
    let mut written = 0usize;
    for (idx, (id, path, size, modified_at)) in pending.into_iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            tracing::info!(written, "master strong-hash: cancelled");
            break;
        }

        let p = std::path::Path::new(&path);
        // Stale-row check: hash only what the catalog still describes. An
        // unreachable file (unmounted volume) logs at debug, not warn — this
        // pass repeats every scan, and the owner runs detachable archives.
        if !disk_matches_row(p, size, &modified_at) {
            tracing::debug!(file_id = id, path = %path,
                "master strong-hash: row is stale or unreachable, skipping");
            continue;
        }

        let hash = match crate::duplicates::compute_full_xxhash(p) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(file_id = id, path = %path, error = %e,
                    "master strong-hash: read failed, skipping");
                continue;
            }
        };

        if let Err(e) = conn.execute(
            "UPDATE files SET strong_hash = ?2 WHERE id = ?1",
            rusqlite::params![id, hash],
        ) {
            tracing::error!(file_id = id, error = %e, "master strong-hash: write failed");
            continue;
        }
        written += 1;

        emit_event(
            emitter,
            "master-hash-progress",
            &MasterHashProgress { done: idx + 1, total, path: path.clone() },
        );
    }

    tracing::info!(written, total, "master strong-hash: pass finished");
    written
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

        let s1 = run_content_index(&db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
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
        let s2 = run_content_index(&db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
        assert_eq!(s2.pending, 2, "the missing-path and drifted rows remain NULL");
        assert_eq!(s2.updated, 0);
        assert_eq!(s2.skipped, 2);
    }

    #[test]
    fn backfill_empty_catalog_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let s = run_content_index(&db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
        assert_eq!(s, BackfillSummary::default());
    }

    struct CapturingEmitter(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

    impl crate::events::ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    /// N real files on disk with matching `files` rows and NULL content_hash.
    /// Real bytes because the pass's stale-row guard compares the row's
    /// (size, modified_at) against the file's — a fake row would be skipped.
    fn test_db_with_pending_rows(n: usize) -> (Database, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("t.db")).unwrap();
        let conn = db.conn();
        crate::db::schema::init_db(&conn).unwrap();
        for i in 0..n {
            let p = tmp.path().join(format!("f{i}.fits"));
            crate::archive::restore::tests::write_minimal_fits(&p);
            let meta = std::fs::metadata(&p).unwrap();
            let modified: chrono::DateTime<Utc> = meta.modified().unwrap().into();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'FITS', ?5)",
                rusqlite::params![
                    p.to_str().unwrap(),
                    format!("f{i}.fits"),
                    meta.len() as i64,
                    modified.to_rfc3339(),
                    Utc::now().to_rfc3339(),
                ],
            )
            .unwrap();
        }
        drop(conn);
        (db, tmp)
    }

    /// The pass reports progress and a terminal event, so the sidebar card and
    /// the completion notification have something to render.
    #[test]
    fn content_index_emits_progress_and_finish() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));

        let summary = run_content_index(&db, &emitter, Arc::new(AtomicBool::new(false)));

        assert_eq!(summary.updated, 3);
        assert!(!summary.cancelled);

        let events = emitter.0.lock().unwrap();
        assert!(
            events.iter().any(|(name, _)| name == "content-index-progress"),
            "expected at least one progress event"
        );
        let (_, finished) = events
            .iter()
            .find(|(name, _)| name == "content-index-finished")
            .expect("expected a terminal event");
        assert_eq!(finished["updated"], 3);
        assert_eq!(finished["cancelled"], false);
    }

    /// A pre-set cancel flag stops the pass before it hashes anything, and the
    /// terminal event says so — the sidebar's X must not look like a no-op.
    #[test]
    fn content_index_honours_cancel_flag() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));

        let summary = run_content_index(&db, &emitter, Arc::new(AtomicBool::new(true)));

        assert!(summary.cancelled, "pre-set flag must report cancelled");
        assert_eq!(summary.updated, 0, "cancelled before any chunk ran");

        let events = emitter.0.lock().unwrap();
        let (_, finished) = events
            .iter()
            .find(|(name, _)| name == "content-index-finished")
            .expect("a cancelled run still emits a terminal event");
        assert_eq!(finished["cancelled"], true);
    }

    /// Status needs a cheap count that does not walk the disk.
    #[test]
    fn count_pending_counts_null_hash_rows_only() {
        let (db, _tmp) = test_db_with_pending_rows(3);
        assert_eq!(count_pending(&db), 3);
        run_content_index(&db, &crate::events::NullEmitter, Arc::new(AtomicBool::new(false)));
        assert_eq!(count_pending(&db), 0);
    }

    /// A row-listing failure (locked DB, schema mismatch, corrupt catalog) is
    /// logged, not swallowed — but it must ALSO still emit exactly one
    /// terminal event with `failed: true`, or the sidebar card hangs open
    /// forever and the UI can't tell a broken catalog from a clean
    /// nothing-to-do run. Forces a real `prepare` failure (dropped table)
    /// rather than an injected one.
    #[test]
    fn content_index_emits_failed_when_listing_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        db.conn().execute("DROP TABLE files", []).unwrap();
        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));

        let summary = run_content_index(&db, &emitter, Arc::new(AtomicBool::new(false)));

        assert_eq!(summary, BackfillSummary::default());

        let events = emitter.0.lock().unwrap();
        let finished_events: Vec<_> = events
            .iter()
            .filter(|(name, _)| name == "content-index-finished")
            .collect();
        assert_eq!(finished_events.len(), 1, "exactly one terminal event, no progress events");
        assert_eq!(finished_events[0].1["failed"], true);
        assert_eq!(finished_events[0].1["cancelled"], false);
    }

    use std::io::Write as _;

    /// The shortlist is what keeps this affordable: only masters whose header
    /// already puts them in a group get hashed. A master with a unique header
    /// is nobody's duplicate, so reading 300 MiB to prove it would be waste.
    #[test]
    fn only_header_shortlisted_masters_are_hashed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        let write = |name: &str, body: &[u8]| -> String {
            let p = tmp.path().join(name);
            std::fs::File::create(&p).unwrap().write_all(body).unwrap();
            p.to_string_lossy().to_string()
        };
        let shared_a = write("a.xisf", b"AAAA");
        let shared_b = write("b.xisf", b"AAAA");
        let lonely = write("c.xisf", b"CCCC");

        for (id, path, header) in [
            (1i64, &shared_a, "HDR-SHARED"),
            (2, &shared_b, "HDR-SHARED"),
            (3, &lonely, "HDR-UNIQUE"),
        ] {
            let meta = std::fs::metadata(path).unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'x.xisf', ?3, ?4, 'XISF')",
                rusqlite::params![
                    id,
                    path,
                    meta.len() as i64,
                    chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap())
                        .to_rfc3339()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master)
                 VALUES (?1, ?1, 'MasterLight', 1)",
                rusqlite::params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO fits_header (file_id, header, header_fingerprint)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    id,
                    header,
                    crate::fingerprint::compute_header_fingerprint(header)
                ],
            )
            .unwrap();
        }

        let emitter = CapturingEmitter(std::sync::Mutex::new(Vec::new()));
        let never = std::sync::atomic::AtomicBool::new(false);
        let n = fill_master_strong_hashes(&conn, &emitter, &never);
        assert_eq!(n, 2, "only the two shortlisted masters are read");

        let hashed: Vec<(i64, Option<String>)> = conn
            .prepare("SELECT id, strong_hash FROM files ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(hashed[0].1, hashed[1].1, "identical bytes, identical hash");
        assert!(hashed[0].1.is_some());
        assert!(hashed[2].1.is_none(), "the lonely master must not be read");

        // Idempotent: a second pass hashes nothing.
        assert_eq!(fill_master_strong_hashes(&conn, &emitter, &never), 0);
    }
}
