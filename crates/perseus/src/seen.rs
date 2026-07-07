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

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

const DDL: &str = "CREATE TABLE IF NOT EXISTS perseus_seen (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    enqueued_at TEXT NOT NULL,
    package_ref TEXT,
    deleted_at TEXT
)";

/// Add `column` to `table` if it is not already present. SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so this reads `PRAGMA table_info` first. Idempotent
/// — the retention columns (`package_ref`, `deleted_at`) were added to
/// `perseus_seen` after the A6 shape shipped, so an existing agent DB is migrated
/// in place on the next open.
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("prepare table_info({table})"))?;
    let present = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .with_context(|| format!("query table_info({table})"))?
        .filter_map(|c| c.ok())
        .any(|c| c == column);
    if !present {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])
            .with_context(|| format!("add column {table}.{column}"))?;
    }
    Ok(())
}

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
        // Migrate an existing (A6-shape) table in place: add the retention
        // linkage columns if they are missing.
        ensure_column(&conn, "perseus_seen", "package_ref", "TEXT")?;
        ensure_column(&conn, "perseus_seen", "deleted_at", "TEXT")?;
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

    /// Record `path` as enqueued at `(size, mtime_ms)` for the package at
    /// `package_ref` — insert or overwrite. Called once a file has cleared the
    /// watcher's write-stability window and has actually been handed to the sync
    /// engine (durable `Queued` row written), so a re-enqueue attempt for the
    /// identical stat is only ever skipped after a real, successful enqueue.
    ///
    /// `package_ref` is the sync engine's `sync_outbound.package_ref` (the
    /// package directory) for this file — the linkage retention later joins on to
    /// resolve a *confirmed package* back to its original *source capture file*.
    /// A re-enqueue clears any prior `deleted_at`: the file is a live capture
    /// again (a new package supersedes the old linkage), so retention must not
    /// treat the earlier deletion as still standing.
    pub fn mark_enqueued(
        &self,
        path: &Path,
        size: u64,
        mtime_ms: i64,
        package_ref: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        conn.execute(
            "INSERT INTO perseus_seen (path, size, mtime, enqueued_at, package_ref, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT(path) DO UPDATE SET
                size = excluded.size,
                mtime = excluded.mtime,
                enqueued_at = excluded.enqueued_at,
                package_ref = excluded.package_ref,
                deleted_at = NULL",
            params![
                path.to_string_lossy(),
                size as i64,
                mtime_ms,
                now_iso(),
                package_ref
            ],
        )
        .context("upsert perseus_seen")?;
        Ok(())
    }

    /// Resolve the *live* source capture file for a confirmed package
    /// (`package_ref` = `sync_outbound.package_ref`), or `None` when there is no
    /// live linkage.
    ///
    /// "Live" means `deleted_at IS NULL`: a row whose source retention has
    /// already deleted, or whose path was since re-enqueued under a **newer**
    /// package (the `path` PRIMARY KEY overwrote `package_ref` to the new
    /// package), does not surface. This is exactly the safety property retention
    /// needs — it will only ever be handed a source that (a) belongs to this
    /// confirmed package and (b) has not already been handled.
    pub fn source_for_package(&self, package_ref: &str) -> Result<Option<PathBuf>> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let row: Option<String> = conn
            .query_row(
                "SELECT path FROM perseus_seen WHERE package_ref = ?1 AND deleted_at IS NULL",
                params![package_ref],
                |r| r.get(0),
            )
            .optional()
            .context("query source_for_package")?;
        Ok(row.map(PathBuf::from))
    }

    /// Stamp a source row `deleted_at = now` after retention removed it, so it
    /// never surfaces via [`source_for_package`](Self::source_for_package) again
    /// (the row itself is retained as a durable audit trail).
    pub fn mark_deleted(&self, path: &Path) -> Result<()> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        conn.execute(
            "UPDATE perseus_seen SET deleted_at = ?2 WHERE path = ?1",
            params![path.to_string_lossy(), now_iso()],
        )
        .context("mark perseus_seen row deleted")?;
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
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        assert!(
            !store.should_enqueue(&p, 100, 111).unwrap(),
            "an unchanged, already-recorded file must not be re-enqueued"
        );
    }

    #[test]
    fn seen_but_size_changed_is_reenqueued() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        assert!(store.should_enqueue(&p, 200, 111).unwrap(), "size drift must re-enqueue");
    }

    #[test]
    fn seen_but_mtime_changed_is_reenqueued() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        assert!(store.should_enqueue(&p, 100, 222).unwrap(), "mtime drift must re-enqueue");
    }

    #[test]
    fn mark_enqueued_is_an_idempotent_upsert() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        store.mark_enqueued(&p, 200, 222, "/pkg/a2").unwrap();
        assert!(!store.should_enqueue(&p, 200, 222).unwrap(), "latest stat wins");
        assert!(store.should_enqueue(&p, 100, 111).unwrap(), "stale stat no longer matches");
    }

    #[test]
    fn independent_paths_tracked_separately() {
        let (_tmp, store) = store();
        let a = PathBuf::from("/cap/a.fits");
        let b = PathBuf::from("/cap/b.fits");
        store.mark_enqueued(&a, 100, 111, "/pkg/a").unwrap();
        assert!(!store.should_enqueue(&a, 100, 111).unwrap());
        assert!(store.should_enqueue(&b, 100, 111).unwrap());
    }

    // ── retention source-mapping (task A8) ───────────────────────────────────

    #[test]
    fn source_for_package_resolves_the_enqueued_file() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/light-0001.fits");
        store.mark_enqueued(&p, 100, 111, "/data/packages/uuid-1").unwrap();
        assert_eq!(
            store.source_for_package("/data/packages/uuid-1").unwrap(),
            Some(p),
            "a confirmed package's source capture file must be resolvable"
        );
    }

    #[test]
    fn source_for_unknown_package_is_none() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/light-0001.fits");
        store.mark_enqueued(&p, 100, 111, "/data/packages/uuid-1").unwrap();
        assert_eq!(
            store.source_for_package("/data/packages/never-enqueued").unwrap(),
            None,
            "a package that was never enqueued resolves to no source"
        );
    }

    #[test]
    fn deleted_source_no_longer_surfaces() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/light-0001.fits");
        store.mark_enqueued(&p, 100, 111, "/data/packages/uuid-1").unwrap();
        store.mark_deleted(&p).unwrap();
        assert_eq!(
            store.source_for_package("/data/packages/uuid-1").unwrap(),
            None,
            "once retention has deleted a source it must never surface again"
        );
    }

    #[test]
    fn reenqueue_after_delete_makes_source_live_again() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/light-0001.fits");
        store.mark_enqueued(&p, 100, 111, "/data/packages/uuid-1").unwrap();
        store.mark_deleted(&p).unwrap();
        // The same path is captured again (a brand-new file), enqueued as a new
        // package: the linkage is live again under the NEW package_ref.
        store.mark_enqueued(&p, 200, 222, "/data/packages/uuid-2").unwrap();
        assert_eq!(
            store.source_for_package("/data/packages/uuid-2").unwrap(),
            Some(p),
            "a re-enqueue clears deleted_at and relinks to the new package"
        );
        assert_eq!(
            store.source_for_package("/data/packages/uuid-1").unwrap(),
            None,
            "the stale (superseded) package_ref no longer resolves the path"
        );
    }
}
