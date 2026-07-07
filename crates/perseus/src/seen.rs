//! Store-aware dedup: records which capture files have already been enqueued,
//! keyed by path + `(size, mtime)`, so a restart never re-baselines an
//! **un-synced** file into oblivion.
//!
//! # Why this exists (review finding, IMPORTANT #2)
//!
//! The original watcher marked every file present at startup as an
//! already-handled "baseline" unconditionally. A frame written while the agent
//! was down (crash, service restart, reboot) landed in that baseline and was
//! silently never synced — a correctness gap for an observatory agent, where
//! "we lost a sub-exposure and nobody noticed" is worse than a harmless
//! duplicate. This store makes "already handled" a durable, stat-aware fact
//! instead of an in-process assumption: a file is only skipped when its
//! *current* `(size, mtime)` matches what was recorded the last time Perseus
//! enqueued it. Anything new, changed, or never recorded is (re-)enqueued —
//! erring on the side of sending, never on the side of silently dropping,
//! because a re-sent duplicate is harmless (the receiver dedupes by
//! uuid-then-content-hash) while a silent skip is not recoverable.
//!
//! # Why a separate table, not part of `athenaeum-core`'s sync schema
//!
//! `perseus_seen` is Perseus-only bookkeeping — it has no meaning to the
//! primary or to `athenaeum-core`'s sync engine, so it does not belong in
//! `sync::store`'s DDL. It lives in the **same** `<data_dir>/perseus.db` file
//! via its own [`rusqlite::Connection`], which is safe because that file is
//! opened in WAL mode (multiple connections to one WAL database, even from
//! different processes, are an explicitly supported SQLite pattern) and this
//! agent is single-process.

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

const DDL: &str = "CREATE TABLE IF NOT EXISTS perseus_seen (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    enqueued_at TEXT NOT NULL
)";

/// Milliseconds-since-epoch rendering of a [`SystemTime`], `0` if unavailable
/// (a filesystem that doesn't report mtime). Consistent within and across runs
/// on the same host, which is all the `(size, mtime)` comparison needs.
pub fn mtime_millis(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Durable "have we already enqueued exactly this file?" record.
pub struct SeenStore {
    conn: Mutex<Connection>,
}

impl SeenStore {
    /// Open (creating if absent) the seen store at `path`, sharing pragmas with
    /// [`athenaeum_core::sync::store::StandaloneSyncStore`] so both connections
    /// to the same file cooperate under WAL.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("open seen store {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .context("configure seen store pragmas")?;
        conn.execute(DDL, []).context("create perseus_seen")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Whether `path` at `(size, mtime_ms)` should be (re-)enqueued: `true` when
    /// there is no recorded row, or the recorded row's stat differs from the
    /// current one (a real edit, or a different file recreated at the same
    /// path). A lookup error is never treated as "skip" — it errs on the side
    /// of enqueueing, since a duplicate send is harmless and a silent skip is
    /// not (see the module doc).
    pub fn should_enqueue(&self, path: &Path, size: u64, mtime_ms: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT size, mtime FROM perseus_seen WHERE path = ?1",
                params![path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("query perseus_seen")?;
        Ok(match row {
            None => true,
            Some((seen_size, seen_mtime)) => seen_size != size as i64 || seen_mtime != mtime_ms,
        })
    }

    /// Record `path` as enqueued at `(size, mtime_ms)` — insert or overwrite.
    /// Called once a file has cleared the watcher's write-stability window and
    /// has actually been handed to the sync engine (durable `Queued` row
    /// written), so a re-enqueue attempt for the identical stat is only ever
    /// skipped after a real, successful enqueue.
    pub fn mark_enqueued(&self, path: &Path, size: u64, mtime_ms: i64) -> Result<()> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        conn.execute(
            "INSERT INTO perseus_seen (path, size, mtime, enqueued_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                size = excluded.size,
                mtime = excluded.mtime,
                enqueued_at = excluded.enqueued_at",
            params![path.to_string_lossy(), size as i64, mtime_ms, now_iso()],
        )
        .context("upsert perseus_seen")?;
        Ok(())
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store() -> (tempfile::TempDir, SeenStore) {
        let tmp = tempfile::tempdir().unwrap();
        let s = SeenStore::open(tmp.path().join("perseus.db")).unwrap();
        (tmp, s)
    }

    #[test]
    fn unseen_path_should_enqueue() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        assert!(store.should_enqueue(&p, 100, 111).unwrap());
    }

    #[test]
    fn seen_unchanged_is_not_reenqueued() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111).unwrap();
        assert!(
            !store.should_enqueue(&p, 100, 111).unwrap(),
            "an unchanged, already-recorded file must not be re-enqueued"
        );
    }

    #[test]
    fn seen_but_size_changed_is_reenqueued() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111).unwrap();
        assert!(store.should_enqueue(&p, 200, 111).unwrap(), "size drift must re-enqueue");
    }

    #[test]
    fn seen_but_mtime_changed_is_reenqueued() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111).unwrap();
        assert!(store.should_enqueue(&p, 100, 222).unwrap(), "mtime drift must re-enqueue");
    }

    #[test]
    fn mark_enqueued_is_an_idempotent_upsert() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111).unwrap();
        store.mark_enqueued(&p, 200, 222).unwrap();
        assert!(!store.should_enqueue(&p, 200, 222).unwrap(), "latest stat wins");
        assert!(store.should_enqueue(&p, 100, 111).unwrap(), "stale stat no longer matches");
    }

    #[test]
    fn independent_paths_tracked_separately() {
        let (_tmp, store) = store();
        let a = PathBuf::from("/cap/a.fits");
        let b = PathBuf::from("/cap/b.fits");
        store.mark_enqueued(&a, 100, 111).unwrap();
        assert!(!store.should_enqueue(&a, 100, 111).unwrap());
        assert!(store.should_enqueue(&b, 100, 111).unwrap());
    }
}
