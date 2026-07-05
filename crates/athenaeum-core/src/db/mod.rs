// Database operations module
// Handles SQLite catalog operations

pub mod schema;
mod operations;
mod operations_blackhole;
mod equipment;
pub mod calibration_links;
pub mod analysis;
pub mod master_provenance;
pub mod light_calibrations;

pub use schema::*;
pub use operations::*;
pub use operations_blackhole::*;
pub use equipment::*;

use r2d2::{ManageConnection, Pool, PooledConnection};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, Result};
use std::path::{Path, PathBuf};

/// Custom r2d2 connection manager for rusqlite 0.32.
///
/// Each new connection gets PRAGMAs applied and SIN/COS functions registered.
pub struct SqliteConnectionManager {
    path: PathBuf,
}

impl SqliteConnectionManager {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Apply PRAGMAs and register custom functions on a connection.
    fn setup_connection(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;",
        )?;

        conn.create_scalar_function("SIN", 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
            let val: Option<f64> = ctx.get(0)?;
            Ok(val.map(f64::sin))
        })?;

        conn.create_scalar_function("COS", 1, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
            let val: Option<f64> = ctx.get(0)?;
            Ok(val.map(f64::cos))
        })?;

        Ok(())
    }
}

impl ManageConnection for SqliteConnectionManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        Self::setup_connection(&conn)?;
        Ok(conn)
    }

    fn is_valid(&self, conn: &mut Connection) -> std::result::Result<(), rusqlite::Error> {
        conn.execute_batch("SELECT 1").map_err(Into::into)
    }

    fn has_broken(&self, _conn: &mut Connection) -> bool {
        false
    }
}

/// Database connection pool wrapper.
///
/// Hands out pooled connections that each have PRAGMAs and SIN/COS already set up.
/// The pool is `Send + Sync`, so `ServiceContext` no longer needs a `Mutex` around it.
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
    path: PathBuf,
}

impl Database {
    /// Create a new database connection pool and initialise the schema.
    pub fn new(path: PathBuf) -> Result<Self> {
        let manager = SqliteConnectionManager::new(&path);

        let pool = Pool::builder()
            .max_size(8)
            .build(manager)
            .map_err(|e| rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("Pool creation failed: {}", e)),
            ))?;

        // Initialise schema on first connection
        let conn = pool.get().map_err(|e| rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("Failed to get initial connection: {}", e)),
        ))?;
        init_db(&conn)?;

        Ok(Self { pool, path })
    }

    /// Get a pooled connection (replaces the old `MutexGuard<Connection>`).
    ///
    /// Defensive cleanup: a few call sites still use raw `BEGIN`/`COMMIT`
    /// with `?` propagation in between, which leaks the transaction onto
    /// the pooled connection if any intermediate query fails. The next
    /// caller to grab that connection would then see "cannot start a
    /// transaction within a transaction". Roll back any leftover
    /// transaction on checkout so a poisoned connection can't take down
    /// later commands. We log when we find one so the underlying call site
    /// is fixable.
    pub fn conn(&self) -> PooledConnection<SqliteConnectionManager> {
        let conn = self
            .pool
            .get()
            .expect("Failed to get DB connection from pool");
        if !conn.is_autocommit() {
            tracing::warn!("pooled connection had an open transaction on checkout, rolling back");
            if let Err(e) = conn.execute("ROLLBACK", []) {
                tracing::error!(error = %e, "defensive rollback failed");
            }
        }
        conn
    }

    /// Get the database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
