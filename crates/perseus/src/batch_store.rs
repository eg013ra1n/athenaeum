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
use rusqlite::{params, Connection};

const DDL: &str = "CREATE TABLE IF NOT EXISTS perseus_batch (
    package_ref TEXT PRIMARY KEY,
    mode        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    file_count  INTEGER NOT NULL
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

/// One recorded send-batch: the package it belongs to, whether it was sent
/// `auto` or `manual`, the RFC-3339 timestamp the batcher stamped, and the number
/// of files the package carried.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    pub package_ref: String,
    pub mode: String,
    pub created_at: String,
    pub file_count: i64,
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
                "SELECT package_ref, mode, created_at, file_count FROM perseus_batch
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
                })
            })
            .context("query perseus_batch")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect perseus_batch rows")?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
