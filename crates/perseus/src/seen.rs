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

/// A resolved *live* source-file linkage for a confirmed package: the capture
/// file path plus the `(size, mtime_ms)` perseus recorded when it enqueued that
/// file.
///
/// The recorded stat is retention's last-line TOCTOU guard (review IMPORTANT
/// #2): a concurrent re-enqueue could rewrite the same path between resolving
/// this linkage and actually removing the file, so the caller must re-stat the
/// file immediately before deletion and compare against `size`/`mtime_ms` —
/// carried here so [`source_for_package`](SeenStore::source_for_package) is the
/// single place that reads both facts together (no separate round-trip that
/// could itself race).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLink {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
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
    /// there is no recorded row, when the row is a **deleted** one, or when the
    /// recorded row's stat differs from the current one (a real edit, or a
    /// different file recreated at the same path). A lookup error is never
    /// treated as "skip" — it errs on the side of enqueueing, since a duplicate
    /// send is harmless and a silent skip is not (see the module doc).
    ///
    /// The `deleted_at` arm is spec §2's "file reappears" row (0.5.1 T9). A
    /// stamped row is the audit trail of a file that is *gone* — retention
    /// removed it, or the operator deleted it from the Library tab — so anything
    /// found at that path afterwards is a NEW capture, typically the same frame
    /// re-copied from the camera media, which reproduces the original
    /// `(size, mtime)` exactly. Comparing stats against a corpse would call that
    /// "already sent" and drop it silently, the quietest data loss this agent
    /// can have; the row is history, not a live dedup key. A real re-enqueue
    /// clears the stamp ([`mark_enqueued`](Self::mark_enqueued)), so this arm
    /// fires once per deletion, never on a loop.
    pub fn should_enqueue(&self, path: &Path, size: u64, mtime_ms: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let row: Option<(i64, i64, Option<String>)> = conn
            .query_row(
                "SELECT size, mtime, deleted_at FROM perseus_seen WHERE path = ?1",
                params![path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context("query perseus_seen")?;
        Ok(match row {
            None => true,
            Some((_, _, Some(_deleted))) => true,
            Some((seen_size, seen_mtime, None)) => {
                seen_size != size as i64 || seen_mtime != mtime_ms
            }
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

    /// Whether a **live** (`deleted_at IS NULL`) seen row exists for `path` —
    /// i.e. Perseus has handed exactly this capture file to the sync engine at
    /// least once and retention has not since removed it.
    ///
    /// This is the library listing's last status arm (T4): a file with no batch
    /// participation that still resolves here left the node under an older
    /// bookkeeping shape, so it is honestly `Sent` rather than `Unsent`. A
    /// retention-deleted row is deliberately excluded — it is an audit trail of a
    /// file that is *gone*, and a file freshly re-captured at the same path is a
    /// new frame (`mark_enqueued` clears `deleted_at`, making it live again).
    ///
    /// `path` must be the same spelling the watcher recorded (canonicalized); a
    /// different spelling of the same file reads `false`, never a wrong `true`.
    pub fn is_recorded(&self, path: &Path) -> Result<bool> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM perseus_seen WHERE path = ?1 AND deleted_at IS NULL",
                params![path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()
            .context("query perseus_seen is_recorded")?;
        Ok(found.is_some())
    }

    /// The `package_ref` of the **live** (`deleted_at IS NULL`) seen row for
    /// `path`: the one package whose confirm can ever cause this file to be
    /// deleted. `None` when there is no live row (never enqueued, or deleted and
    /// the capture reappeared), and `None` too for a legacy row predating the
    /// retention columns, whose `package_ref` is NULL.
    ///
    /// This is the exact inverse of
    /// [`sources_for_package`](Self::sources_for_package), and it exists because
    /// that is the ONLY direction retention travels: `path` is the PRIMARY KEY
    /// and [`mark_enqueued`](Self::mark_enqueued) overwrites `package_ref` on
    /// every re-enqueue, so at most ONE package can resolve back to a given file.
    /// The library's retention fate line reads this to anchor its clock on the
    /// linkage that can actually fire, rather than on an older package that
    /// carried the same bytes and lost the linkage when the file was re-sent.
    pub fn package_for_path(&self, path: &Path) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        // `package_ref` is nullable, so the row itself is `Option<Option<_>>`:
        // outer = is there a live row, inner = does it carry a linkage.
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT package_ref FROM perseus_seen WHERE path = ?1 AND deleted_at IS NULL",
                params![path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()
            .context("query perseus_seen package_for_path")?;
        Ok(row.flatten())
    }

    /// Resolve the *live* source capture file for a confirmed package
    /// (`package_ref` = `sync_outbound.package_ref`), or `None` when there is no
    /// live linkage. Returns the recorded `(size, mtime_ms)` alongside the path
    /// (see [`SourceLink`]) — the caller's last-line guard against a concurrent
    /// re-enqueue rewriting this exact path before the delete happens.
    ///
    /// "Live" means `deleted_at IS NULL`: a row whose source retention has
    /// already deleted, or whose path was since re-enqueued under a **newer**
    /// package (the `path` PRIMARY KEY overwrote `package_ref` to the new
    /// package), does not surface. This is exactly the safety property retention
    /// needs — it will only ever be handed a source that (a) belongs to this
    /// confirmed package and (b) has not already been handled.
    pub fn source_for_package(&self, package_ref: &str) -> Result<Option<SourceLink>> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let row: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT path, size, mtime FROM perseus_seen WHERE package_ref = ?1 AND deleted_at IS NULL",
                params![package_ref],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context("query source_for_package")?;
        Ok(row.map(|(path, size, mtime_ms)| SourceLink {
            path: PathBuf::from(path),
            size: size.max(0) as u64,
            mtime_ms,
        }))
    }

    /// Every *live* (`deleted_at IS NULL`) source linkage of `package_ref`, ordered
    /// by path. The batcher records one row per packaged file, so a batch package
    /// resolves to MANY links — [`source_for_package`](Self::source_for_package)
    /// (singular, `query_row`) sees only the first and exists for the legacy
    /// one-file-per-package callers.
    pub fn sources_for_package(&self, package_ref: &str) -> Result<Vec<SourceLink>> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT path, size, mtime FROM perseus_seen
                 WHERE package_ref = ?1 AND deleted_at IS NULL ORDER BY path",
            )
            .context("prepare sources_for_package")?;
        let rows = stmt
            .query_map(params![package_ref], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
            })
            .context("query sources_for_package")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect sources_for_package")?;
        Ok(rows
            .into_iter()
            .map(|(path, size, mtime_ms)| SourceLink {
                path: PathBuf::from(path),
                size: size.max(0) as u64,
                mtime_ms,
            })
            .collect())
    }

    /// Re-point every *live* (`deleted_at IS NULL`) linkage from `old_ref` to
    /// `new_ref`, returning how many rows moved. Used by the divert path
    /// (`resend declined as a new transfer`): the payload gets a fresh package
    /// identity, and retention must follow it — a linkage left on the old ref
    /// would never see the new transfer's confirm. Rows already handled by
    /// retention (`deleted_at` set) are audit history and stay put.
    pub fn relink_package(&self, old_ref: &str, new_ref: &str) -> Result<usize> {
        let conn = self.conn.lock().expect("seen store mutex poisoned");
        let n = conn
            .execute(
                "UPDATE perseus_seen SET package_ref = ?2
                 WHERE package_ref = ?1 AND deleted_at IS NULL",
                params![old_ref, new_ref],
            )
            .context("relink perseus_seen package_ref")?;
        Ok(n)
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

    /// Spec §2, the "file reappears" row — and the quietest possible data loss
    /// if it is wrong.
    ///
    /// A `deleted_at` row is the audit trail of a file that is **gone**. When an
    /// identical `(size, mtime)` file turns up at that path again it is a NEW
    /// capture (the camera media was re-copied), not the one that was deleted —
    /// so it must be enqueued. Matching the stat of a corpse and calling it
    /// "already sent" would silently drop a frame nobody ever synced.
    #[test]
    fn a_deleted_row_reenqueues_an_identical_recreation() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        assert!(
            !store.should_enqueue(&p, 100, 111).unwrap(),
            "the live row still dedups its own file"
        );
        store.mark_deleted(&p).unwrap();
        assert!(
            store.should_enqueue(&p, 100, 111).unwrap(),
            "a file re-created at a deleted row's path is NEW, whatever its stat"
        );
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

    // ── library status join (task T4) ────────────────────────────────────────

    #[test]
    fn is_recorded_tracks_only_live_rows() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        assert!(
            !store.is_recorded(&p).unwrap(),
            "a never-enqueued file is not recorded"
        );
        store.mark_enqueued(&p, 100, 111, "/pkg/a").unwrap();
        assert!(store.is_recorded(&p).unwrap(), "an enqueued file is recorded");
        store.mark_deleted(&p).unwrap();
        assert!(
            !store.is_recorded(&p).unwrap(),
            "a retention-deleted row is audit history, not a live record"
        );
        // A re-capture at the same path clears deleted_at → live again.
        store.mark_enqueued(&p, 200, 222, "/pkg/b").unwrap();
        assert!(store.is_recorded(&p).unwrap());
        assert!(
            !store.is_recorded(&PathBuf::from("/cap/other.fits")).unwrap(),
            "paths are tracked independently"
        );
    }

    /// The linkage the library's retention fate line anchors on: exactly the
    /// package `sources_for_package` would hand the deleter back, and nothing
    /// else. A re-enqueue MOVES it (the `path` PRIMARY KEY overwrites
    /// `package_ref`), a deletion hides it, and a legacy row without a linkage
    /// reports none.
    #[test]
    fn package_for_path_tracks_the_live_linkage_only() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/a.fits");
        assert_eq!(store.package_for_path(&p).unwrap(), None, "never enqueued");

        store.mark_enqueued(&p, 100, 111, "/pkg/one").unwrap();
        assert_eq!(
            store.package_for_path(&p).unwrap(),
            Some("/pkg/one".to_string())
        );

        // A re-send overwrites the linkage: only the NEW package can delete it.
        store.mark_enqueued(&p, 100, 111, "/pkg/two").unwrap();
        assert_eq!(
            store.package_for_path(&p).unwrap(),
            Some("/pkg/two".to_string()),
            "the re-enqueue moved the linkage off /pkg/one"
        );
        assert!(
            store.sources_for_package("/pkg/one").unwrap().is_empty(),
            "…and the two directions agree"
        );

        store.mark_deleted(&p).unwrap();
        assert_eq!(
            store.package_for_path(&p).unwrap(),
            None,
            "a deleted row is audit history, not a live linkage"
        );
    }

    // ── retention source-mapping (task A8) ───────────────────────────────────

    #[test]
    fn source_for_package_resolves_the_enqueued_file() {
        let (_tmp, store) = store();
        let p = PathBuf::from("/cap/light-0001.fits");
        store.mark_enqueued(&p, 100, 111, "/data/packages/uuid-1").unwrap();
        assert_eq!(
            store.source_for_package("/data/packages/uuid-1").unwrap(),
            Some(SourceLink { path: p, size: 100, mtime_ms: 111 }),
            "a confirmed package's source capture file (with its recorded stat) must be resolvable"
        );
    }

    #[test]
    fn sources_for_package_returns_every_live_file() {
        let (_tmp, store) = store();
        store.mark_enqueued(&PathBuf::from("/cap/a.fits"), 10, 1, "/pkg/uuid-1").unwrap();
        store.mark_enqueued(&PathBuf::from("/cap/b.fits"), 20, 2, "/pkg/uuid-1").unwrap();
        store.mark_enqueued(&PathBuf::from("/cap/c.fits"), 30, 3, "/pkg/uuid-2").unwrap();
        store.mark_deleted(&PathBuf::from("/cap/b.fits")).unwrap();
        let live = store.sources_for_package("/pkg/uuid-1").unwrap();
        assert_eq!(live.len(), 1, "deleted linkage must be excluded");
        assert_eq!(live[0].path, PathBuf::from("/cap/a.fits"));
        // Undelete-free check on the multi-row case:
        store.mark_enqueued(&PathBuf::from("/cap/d.fits"), 40, 4, "/pkg/uuid-1").unwrap();
        let live = store.sources_for_package("/pkg/uuid-1").unwrap();
        assert_eq!(live.len(), 2, "every live row of the package is returned");
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
    fn relink_package_moves_live_rows_only() {
        let (_tmp, store) = store();
        let live = PathBuf::from("/cap/light-0001.fits");
        let dead = PathBuf::from("/cap/light-0002.fits");
        store.mark_enqueued(&live, 100, 111, "/pkg/old").unwrap();
        store.mark_enqueued(&dead, 200, 222, "/pkg/old").unwrap();
        store.mark_deleted(&dead).unwrap();

        assert_eq!(store.relink_package("/pkg/old", "/pkg/new").unwrap(), 1);
        assert_eq!(
            store.source_for_package("/pkg/new").unwrap(),
            Some(SourceLink { path: live, size: 100, mtime_ms: 111 }),
            "the live linkage follows the diverted package"
        );
        assert_eq!(
            store.source_for_package("/pkg/old").unwrap(),
            None,
            "no live linkage remains on the old ref (the deleted row is audit-only)"
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
            Some(SourceLink { path: p, size: 200, mtime_ms: 222 }),
            "a re-enqueue clears deleted_at and relinks to the new package"
        );
        assert_eq!(
            store.source_for_package("/data/packages/uuid-1").unwrap(),
            None,
            "the stale (superseded) package_ref no longer resolves the path"
        );
    }
}
