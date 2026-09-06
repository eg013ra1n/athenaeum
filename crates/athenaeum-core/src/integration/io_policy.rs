//! The two I/O knobs an integration run needs, resolved together: how much
//! memory a band may use, and how many reads to keep in flight. They are one
//! value because they are decided from the same two inputs — the machine and
//! the storage the frames actually live on — and because passing two loose
//! `usize`s through the engine invites transposing them.

use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

use super::band_budget;
use super::storage_class::{self, StorageClass};
use crate::settings::SettingsManager;

#[derive(Clone, Copy, Debug)]
pub struct IoPolicy {
    pub band_budget_bytes: usize,
    pub read_concurrency: usize,
    /// Reported in the build's log line so a later measurement on a NAS or an
    /// SSD can be read against the policy that produced it.
    pub storage: StorageClass,
}

pub fn resolve(
    conn: &Connection,
    settings: &SettingsManager,
    paths: &[PathBuf],
    pool_threads: usize,
) -> Result<IoPolicy> {
    let storage = storage_class::classify_all(paths);
    Ok(IoPolicy {
        band_budget_bytes: band_budget::resolve_budget_bytes(conn, settings)?,
        read_concurrency: storage_class::read_concurrency(
            storage,
            settings.get_integration_read_concurrency(conn)?,
            pool_threads,
        ),
        storage,
    })
}
