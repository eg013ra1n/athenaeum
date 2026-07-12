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

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

const DDL: &str = "CREATE TABLE IF NOT EXISTS perseus_batch (
    package_ref TEXT PRIMARY KEY,
    mode        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    file_count  INTEGER NOT NULL
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
}
