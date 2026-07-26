//! Send-batch bookkeeping: one durable row per package Perseus hands to the sync
//! engine, recording whether it was sent **auto** (watcher-driven) or **manual**
//! (an explicit operator "send now"), when, and how many files it carried.
//!
//! # Why this exists
//!
//! The web history page groups sent packages by batch and by day, and has to
//! distinguish auto-fired sends from manual ones. The sync engine's own
//! `sync_outbound` rows don't carry that intent, and [`crate::seen`] tracks
//! per-*file* dedup, not per-*batch* provenance. `perseus_batch` is that missing
//! per-batch record — the batcher (Task 4) writes a row as each package is
//! formed; the web page (Task 6) lists them newest-first.
//!
//! # Why a separate table, not part of `athenaeum-core`'s sync schema
//!
//! Like [`crate::seen`], this is Perseus-only bookkeeping with no meaning to the
//! primary or to `athenaeum-core`'s sync engine, so it does not belong in
//! `sync::store`'s DDL. It lives in the **same** `<data_dir>/perseus.db` file via
//! its own [`rusqlite::Connection`], which is safe because that file is opened in
//! WAL mode (multiple connections to one WAL database are an explicitly supported
//! SQLite pattern) and this agent is single-process.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

const DDL: &str = "CREATE TABLE IF NOT EXISTS perseus_batch (
    package_ref      TEXT PRIMARY KEY,
    mode             TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    file_count       INTEGER NOT NULL,
    files_deleted_at TEXT
)";

/// Per-file source linkage for a packaged batch: which capture file each
/// manifest `rel_path` was copied from. This is what a confirmed-package
/// rebuild ([`crate::resend`]) resolves sources through — after confirm the
/// payload copies are cleaned and only the manifest survives, so re-sending a
/// confirmed batch has to re-read the ORIGINAL capture files. Additive table
/// (`CREATE IF NOT EXISTS`): batches recorded before this shipped simply have
/// no rows here and fall back to the rebuild's reverse-mapping.
const DDL_FILES: &str = "CREATE TABLE IF NOT EXISTS perseus_batch_files (
    package_ref TEXT NOT NULL,
    rel_path    TEXT NOT NULL,
    source_path TEXT NOT NULL,
    PRIMARY KEY (package_ref, rel_path)
)";

/// Reverse-lookup index for [`BatchStore::batches_for_source`]. The library
/// listing (T4) runs ONE `source_path =` lookup per file in the browsed
/// directory, so without this the status join degrades to a full table scan per
/// row — quadratic on a node with a long send history. Additive and idempotent
/// (`IF NOT EXISTS`): an existing agent DB gains it on the next open.
const DDL_FILES_SOURCE_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_perseus_batch_files_source
    ON perseus_batch_files(source_path)";

/// Agent-scoped key/value scratch table — one row per fact the agent must
/// remember across restarts that is not *about* a batch. The scheduler (0.5.1
/// §3) is the first and only client: it stores [`KEY_LAST_SCHEDULED_FIRE`] so a
/// restart can tell "we already sent at 06:00" from "we slept through 06:00".
///
/// A generic KV rather than a `last_scheduled_fire` column somewhere: the fact
/// has no natural owning row (it belongs to the *agent*, not to any batch), and
/// the next such fact should not each cost a table. Values are plain strings —
/// the caller owns the encoding (this key uses RFC-3339). Additive DDL, same
/// `IF NOT EXISTS` pattern as the tables above.
const DDL_META: &str = "CREATE TABLE IF NOT EXISTS perseus_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
)";

/// `perseus_meta` key holding the RFC-3339 timestamp of the last
/// scheduled-mode fire. Named here so the writer (the batcher's scheduled arm)
/// and the reader (the startup catch-up check) cannot drift apart on a literal.
pub const KEY_LAST_SCHEDULED_FIRE: &str = "last_scheduled_fire";

/// One recorded send-batch: the package it belongs to, whether it was sent
/// `auto` or `manual`, the RFC-3339 timestamp the batcher stamped, and the number
/// of files the package carried.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    pub package_ref: String,
    pub mode: String,
    pub created_at: String,
    pub file_count: i64,
    /// RFC-3339 timestamp the batch's payload copies were deleted from disk, or
    /// `None` while the files are still present. Set by [`BatchStore::mark_files_deleted`].
    pub files_deleted_at: Option<String>,
}

/// Durable per-batch send record, keyed by `package_ref`.
pub struct BatchStore {
    conn: Mutex<Connection>,
}

impl BatchStore {
    /// Open (creating if absent) the batch store at `path`, sharing pragmas with
    /// [`crate::seen::SeenStore::open`] so both connections to the same
    /// `perseus.db` file cooperate under WAL.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("open batch store {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .context("configure batch store pragmas")?;
        conn.execute(DDL, []).context("create perseus_batch")?;
        conn.execute(DDL_FILES, [])
            .context("create perseus_batch_files")?;
        conn.execute(DDL_FILES_SOURCE_INDEX, [])
            .context("create idx_perseus_batch_files_source")?;
        conn.execute(DDL_META, []).context("create perseus_meta")?;

        // files_deleted_at (UI v2 §4.1): guarded ALTER — CREATE IF NOT EXISTS never
        // adds a column to an existing table. Additive; pre-upgrade rows read NULL.
        let has_col: bool = conn
            .prepare("PRAGMA table_info(perseus_batch)")
            .context("prepare table_info(perseus_batch)")?
            .query_map([], |r| r.get::<_, String>(1))
            .context("query table_info(perseus_batch)")?
            .filter_map(|c| c.ok())
            .any(|c| c == "files_deleted_at");
        if !has_col {
            conn.execute("ALTER TABLE perseus_batch ADD COLUMN files_deleted_at TEXT", [])
                .context("add perseus_batch.files_deleted_at")?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Record (or overwrite) the batch row for `package_ref`. A repeat call for
    /// the same `package_ref` is an idempotent upsert — the last write wins,
    /// never a duplicate row — so a batcher retry can't inflate the history.
    pub fn record(
        &self,
        package_ref: &str,
        mode: &str,
        created_at: &str,
        file_count: usize,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        conn.execute(
            "INSERT INTO perseus_batch (package_ref, mode, created_at, file_count)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(package_ref) DO UPDATE SET
                mode = excluded.mode,
                created_at = excluded.created_at,
                file_count = excluded.file_count",
            params![package_ref, mode, created_at, file_count as i64],
        )
        .context("upsert perseus_batch")?;
        Ok(())
    }

    /// Record (or refresh) the `rel_path → source capture file` linkage for a
    /// packaged batch, one row per packaged file, in one transaction. Idempotent
    /// upsert on `(package_ref, rel_path)` — a batcher retry can't duplicate
    /// rows. Best-effort at the call sites (a failed write only degrades a
    /// future rebuild to the reverse-mapping fallback, never fails the send).
    pub fn record_files(&self, package_ref: &str, files: &[(String, PathBuf)]) -> Result<()> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let tx = conn
            .unchecked_transaction()
            .context("begin record_files")?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO perseus_batch_files (package_ref, rel_path, source_path)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(package_ref, rel_path) DO UPDATE SET
                        source_path = excluded.source_path",
                )
                .context("prepare upsert perseus_batch_files")?;
            for (rel_path, source_path) in files {
                stmt.execute(params![
                    package_ref,
                    rel_path,
                    source_path.to_string_lossy()
                ])
                .with_context(|| format!("upsert perseus_batch_files {rel_path}"))?;
            }
        }
        tx.commit().context("commit record_files")
    }

    /// The recorded `rel_path → source capture file` pairs for `package_ref`,
    /// ordered by `rel_path`. Empty for a batch recorded before the table
    /// shipped (the rebuild then reverse-maps against the capture dirs).
    pub fn files_for(&self, package_ref: &str) -> Result<Vec<(String, PathBuf)>> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT rel_path, source_path FROM perseus_batch_files
                 WHERE package_ref = ?1 ORDER BY rel_path ASC",
            )
            .context("prepare files_for")?;
        let rows = stmt
            .query_map(params![package_ref], |r| {
                Ok((r.get::<_, String>(0)?, PathBuf::from(r.get::<_, String>(1)?)))
            })
            .context("query perseus_batch_files")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect perseus_batch_files rows")?;
        Ok(rows)
    }

    /// Copy every `perseus_batch_files` row of `old_ref` under `new_ref`
    /// (upsert), returning how many rows were cloned. The old rows are kept —
    /// the divert path (`resend as new transfer`) leaves the declined batch as
    /// history, and on a fan-out the old package dir is still live for sibling
    /// targets.
    pub fn clone_files(&self, old_ref: &str, new_ref: &str) -> Result<usize> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let n = conn
            .execute(
                "INSERT INTO perseus_batch_files (package_ref, rel_path, source_path)
                 SELECT ?2, rel_path, source_path FROM perseus_batch_files
                 WHERE package_ref = ?1
                 ON CONFLICT(package_ref, rel_path) DO UPDATE SET
                    source_path = excluded.source_path",
                params![old_ref, new_ref],
            )
            .context("clone perseus_batch_files")?;
        Ok(n)
    }

    /// List every recorded batch newest-first (`created_at` DESC, then
    /// `package_ref` DESC as a stable tiebreak for equal timestamps).
    pub fn list(&self) -> Result<Vec<BatchRow>> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT package_ref, mode, created_at, file_count, files_deleted_at FROM perseus_batch
                 ORDER BY created_at DESC, package_ref DESC",
            )
            .context("prepare list perseus_batch")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BatchRow {
                    package_ref: r.get(0)?,
                    mode: r.get(1)?,
                    created_at: r.get(2)?,
                    file_count: r.get(3)?,
                    files_deleted_at: r.get(4)?,
                })
            })
            .context("query perseus_batch")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect perseus_batch rows")?;
        Ok(rows)
    }

    /// Stamp the RFC-3339 `at` timestamp as when `package_ref`'s payload copies
    /// were deleted from disk. A no-op if the row does not exist.
    pub fn mark_files_deleted(&self, package_ref: &str, at: &str) -> Result<()> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        conn.execute(
            "UPDATE perseus_batch SET files_deleted_at = ?2 WHERE package_ref = ?1",
            params![package_ref, at],
        )
        .context("mark perseus_batch files_deleted_at")?;
        Ok(())
    }

    /// Delete the whole batch — its `perseus_batch` row and every
    /// `perseus_batch_files` linkage row — in one transaction.
    pub fn delete(&self, package_ref: &str) -> Result<()> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let tx = conn.unchecked_transaction().context("begin batch delete")?;
        tx.execute("DELETE FROM perseus_batch_files WHERE package_ref = ?1", params![package_ref])
            .context("delete perseus_batch_files")?;
        tx.execute("DELETE FROM perseus_batch WHERE package_ref = ?1", params![package_ref])
            .context("delete perseus_batch")?;
        tx.commit().context("commit batch delete")
    }

    /// Every batch ONE source capture file rode in, as DISTINCT `package_ref`s
    /// sorted ascending. The library listing's per-file join (T4): a file's
    /// participation count, and the set of outbound rows whose newest attempt
    /// decides its status.
    ///
    /// `source` is the path spelling the batcher recorded, which is the
    /// **canonicalized** capture path (the watcher canonicalizes both the root and
    /// each discovered file before it emits them). A caller that hands over a
    /// non-canonical spelling of the same file gets an empty result — not a wrong
    /// one. The statement is cached: this runs once per listed file, not once per
    /// request.
    pub fn batches_for_source(&self, source: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let mut stmt = conn
            .prepare_cached(
                "SELECT DISTINCT package_ref FROM perseus_batch_files
                 WHERE source_path = ?1 ORDER BY package_ref ASC",
            )
            .context("prepare batches_for_source")?;
        let refs = stmt
            .query_map(params![source], |r| r.get::<_, String>(0))
            .context("query batches_for_source")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect batches_for_source rows")?;
        Ok(refs)
    }

    /// DISTINCT package_refs that reference ANY of `sources` — a file's full set of
    /// batch participations (original + divert copies), the obligation verdict's
    /// cross-batch input. Chunked IN-list (999 SQLite param cap).
    pub fn packages_for_sources(&self, sources: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let mut out: Vec<String> = Vec::new();
        for chunk in sources.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT DISTINCT package_ref FROM perseus_batch_files WHERE source_path IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).context("prepare packages_for_sources")?;
            let refs = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |r| r.get::<_, String>(0))
                .context("query packages_for_sources")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("collect packages_for_sources")?;
            out.extend(refs);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Read a `perseus_meta` value. `None` = the key was never
    /// written — a first run, distinct from a written-then-empty value, which is
    /// exactly the distinction the scheduler's catch-up check needs ("never
    /// fired" must not read as "fired at the epoch").
    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        let mut stmt = conn
            .prepare_cached("SELECT value FROM perseus_meta WHERE key = ?1")
            .context("prepare meta_get")?;
        let value = stmt
            .query_row(params![key], |r| r.get::<_, String>(0))
            .optional()
            .context("query meta_get")?;
        Ok(value)
    }

    /// Write a `perseus_meta` value, replacing any previous one for
    /// the key (upsert — the table holds current facts, not a history).
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().expect("batch store mutex poisoned");
        conn.execute(
            "INSERT INTO perseus_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .context("write perseus_meta")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh on-disk store in a throwaway tempdir. The `TempDir` guard is
    /// returned first so the caller can keep it alive for the test's duration.
    fn store() -> (tempfile::TempDir, BatchStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BatchStore::open(dir.path().join("perseus.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn files_deleted_at_roundtrips_and_defaults_null() {
        let (_tmp, store) = store();
        store.record("/pkg/u1", "auto", "2026-07-23T10:00:00Z", 3).unwrap();
        assert_eq!(store.list().unwrap()[0].files_deleted_at, None);
        store.mark_files_deleted("/pkg/u1", "2026-07-23T11:00:00Z").unwrap();
        assert_eq!(store.list().unwrap()[0].files_deleted_at.as_deref(), Some("2026-07-23T11:00:00Z"));
    }

    /// The `perseus_meta` KV (0.5.1 T12): a never-written key reads `None` — the
    /// distinction the scheduler's catch-up needs between "never fired" and
    /// "fired at some stored time" — and a write is an upsert, so the row holds
    /// the current fact rather than accumulating history.
    #[test]
    fn meta_kv_roundtrips_and_upserts() {
        let (_tmp, store) = store();
        assert_eq!(store.meta_get(KEY_LAST_SCHEDULED_FIRE).unwrap(), None);

        store
            .meta_set(KEY_LAST_SCHEDULED_FIRE, "2026-07-26T06:00:00+02:00")
            .unwrap();
        assert_eq!(
            store.meta_get(KEY_LAST_SCHEDULED_FIRE).unwrap().as_deref(),
            Some("2026-07-26T06:00:00+02:00")
        );

        store
            .meta_set(KEY_LAST_SCHEDULED_FIRE, "2026-07-27T06:00:00+02:00")
            .unwrap();
        assert_eq!(
            store.meta_get(KEY_LAST_SCHEDULED_FIRE).unwrap().as_deref(),
            Some("2026-07-27T06:00:00+02:00"),
            "the second write replaces the first"
        );

        // Keys are independent, and an unrelated key stays absent.
        store.meta_set("other", "x").unwrap();
        assert_eq!(store.meta_get("other").unwrap().as_deref(), Some("x"));
        assert_eq!(store.meta_get("never-written").unwrap(), None);
    }

    /// The meta row survives a reopen of the same file — it is the *persistence*
    /// that makes catch-up possible across a restart.
    #[test]
    fn meta_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perseus.db");
        {
            let store = BatchStore::open(&path).unwrap();
            store
                .meta_set(KEY_LAST_SCHEDULED_FIRE, "2026-07-26T06:00:00Z")
                .unwrap();
        }
        let reopened = BatchStore::open(&path).unwrap();
        assert_eq!(
            reopened.meta_get(KEY_LAST_SCHEDULED_FIRE).unwrap().as_deref(),
            Some("2026-07-26T06:00:00Z")
        );
    }

    #[test]
    fn delete_removes_batch_row_and_linkage() {
        let (_tmp, store) = store();
        store.record("/pkg/u1", "auto", "2026-07-23T10:00:00Z", 1).unwrap();
        store.record_files("/pkg/u1", &[("a.fits".into(), PathBuf::from("/cap/a.fits"))]).unwrap();
        store.delete("/pkg/u1").unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(store.files_for("/pkg/u1").unwrap().is_empty());
    }

    #[test]
    fn packages_for_sources_finds_every_participation() {
        let (_tmp, store) = store();
        store.record_files("/pkg/u1", &[("a.fits".into(), PathBuf::from("/cap/a.fits"))]).unwrap();
        store.record_files("/pkg/u2", &[("a.fits".into(), PathBuf::from("/cap/a.fits")),
                                         ("b.fits".into(), PathBuf::from("/cap/b.fits"))]).unwrap();
        store.record_files("/pkg/u3", &[("c.fits".into(), PathBuf::from("/cap/c.fits"))]).unwrap();
        let mut refs = store.packages_for_sources(&["/cap/a.fits".to_string()]).unwrap();
        refs.sort();
        assert_eq!(refs, vec!["/pkg/u1".to_string(), "/pkg/u2".to_string()]);
    }

    #[test]
    fn batches_for_source_returns_only_that_sources_participations() {
        let (_tmp, store) = store();
        store
            .record_files("/pkg/u1", &[("a.fits".into(), PathBuf::from("/cap/a.fits"))])
            .unwrap();
        store
            .record_files(
                "/pkg/u2",
                &[
                    ("a.fits".into(), PathBuf::from("/cap/a.fits")),
                    ("b.fits".into(), PathBuf::from("/cap/b.fits")),
                ],
            )
            .unwrap();
        assert_eq!(
            store.batches_for_source("/cap/a.fits").unwrap(),
            vec!["/pkg/u1".to_string(), "/pkg/u2".to_string()],
            "every batch this file rode in, sorted"
        );
        assert_eq!(
            store.batches_for_source("/cap/b.fits").unwrap(),
            vec!["/pkg/u2".to_string()]
        );
        assert!(
            store.batches_for_source("/cap/never.fits").unwrap().is_empty(),
            "an unpackaged file has no participations"
        );
    }

    /// The listing route runs one lookup per file in a directory, so the
    /// `source_path` index is load-bearing, not cosmetic.
    #[test]
    fn open_creates_the_source_path_index() {
        let (_tmp, store) = store();
        let conn = store.conn.lock().unwrap();
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_perseus_batch_files_source'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "perseus_batch_files(source_path) must be indexed");
    }

    #[test]
    fn record_and_list_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = BatchStore::open(dir.path().join("perseus.db")).unwrap();
        s.record("pkg-a", "auto", "2026-07-12T01:00:00Z", 3).unwrap();
        s.record("pkg-b", "manual", "2026-07-12T02:00:00Z", 5).unwrap();
        let rows = s.list().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].package_ref, "pkg-b"); // newest first
        assert_eq!(rows[0].mode, "manual");
        assert_eq!(rows[1].file_count, 3);
        // idempotent upsert on the same package_ref
        s.record("pkg-b", "manual", "2026-07-12T02:00:00Z", 5).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
    }

    #[test]
    fn record_files_roundtrip_and_idempotent_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let s = BatchStore::open(dir.path().join("perseus.db")).unwrap();
        let files = vec![
            ("a/light-1.fits".to_string(), PathBuf::from("/cap/a/light-1.fits")),
            ("a/light-2.fits".to_string(), PathBuf::from("/cap/a/light-2.fits")),
        ];
        s.record_files("/pkg/one", &files).unwrap();
        assert_eq!(s.files_for("/pkg/one").unwrap(), files);
        assert!(s.files_for("/pkg/unknown").unwrap().is_empty());

        // Re-record with a moved source: upsert, never a duplicate row.
        let moved = vec![(
            "a/light-1.fits".to_string(),
            PathBuf::from("/cap2/a/light-1.fits"),
        )];
        s.record_files("/pkg/one", &moved).unwrap();
        let rows = s.files_for("/pkg/one").unwrap();
        assert_eq!(rows.len(), 2, "upsert must not duplicate (package_ref, rel_path)");
        assert_eq!(rows[0].1, PathBuf::from("/cap2/a/light-1.fits"), "last write wins");
    }

    #[test]
    fn clone_files_copies_rows_and_keeps_originals() {
        let dir = tempfile::tempdir().unwrap();
        let s = BatchStore::open(dir.path().join("perseus.db")).unwrap();
        let files = vec![
            ("x.fits".to_string(), PathBuf::from("/cap/x.fits")),
            ("y.fits".to_string(), PathBuf::from("/cap/y.fits")),
        ];
        s.record_files("/pkg/old", &files).unwrap();
        assert_eq!(s.clone_files("/pkg/old", "/pkg/new").unwrap(), 2);
        assert_eq!(s.files_for("/pkg/new").unwrap(), files, "clone carries the linkage");
        assert_eq!(
            s.files_for("/pkg/old").unwrap(),
            files,
            "the declined batch keeps its rows as history"
        );
        // Cloning again is an idempotent upsert.
        assert_eq!(s.clone_files("/pkg/old", "/pkg/new").unwrap(), 2);
        assert_eq!(s.files_for("/pkg/new").unwrap().len(), 2);
    }
}
