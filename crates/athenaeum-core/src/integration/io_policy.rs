//! The two I/O knobs an integration run needs, resolved together: how much memory a band may use, and how many
//! reads to keep in flight. `IoPolicy` lives in `starloom`; `resolve` reads them through Athenaeum's settings.

use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

use super::band_budget;
use super::storage_class;
use crate::settings::SettingsManager;

pub use starloom::integration::io_policy::*;

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
        prefetch_local: false,
    })
}
