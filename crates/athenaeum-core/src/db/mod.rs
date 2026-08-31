// Database operations module
// Handles SQLite catalog operations

pub mod schema;
pub mod repair;
mod operations;
mod operations_blackhole;
mod equipment;
pub mod calibration_links;
pub mod analysis;
pub mod master_provenance;
pub mod master_unregister;
pub mod collab;
pub mod collab_exchange;

pub use schema::*;
pub use operations::*;
pub use operations_blackhole::*;
pub use equipment::*;

use r2d2::{ManageConnection, Pool, PooledConnection};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, Result};
use std::path::{Path, PathBuf};

/// Custom r2d2 connection manager for rusqlite 0.40.
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
        // foreign_keys is already the bundled build's compile-time default;
        // stating it makes enforcement survive a switch to a system SQLite or
        // a build-flag change.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;",
        )?;

        register_math_functions(conn)?;

        Ok(())
    }
}

/// Register the trigonometric scalar functions the catalog SQL relies on
/// (`api::spatial::get_imaging_locations` computes a circular mean of rotation
/// angles). Bundled SQLite is built without `SQLITE_ENABLE_MATH_FUNCTIONS`, so
/// `SIN`/`COS` only exist because we install them per connection. Pooled
/// connections get this via `setup_connection`; tests that open a bare
/// `Connection` must call it themselves.
pub fn register_math_functions(conn: &Connection) -> Result<()> {
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
///
/// `Clone` yields an independent handle onto the **same** underlying pool (r2d2
/// `Pool` is internally `Arc`-backed) — a cheap, shared, `'static` DB handle the
/// sync receiver's per-package landing resolver captures without borrowing
/// `ServiceContext`.
#[derive(Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pooled connection must enforce foreign keys — the whole catalog's
    /// cascade discipline (calibration links, archive manifests, session
    /// members) is built on it. Pinned on a real pooled checkout rather than
    /// trusting the bundled build's compile-time default.
    #[test]
    fn pooled_connections_enforce_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("fk.db")).unwrap();

        // Take two checkouts so the pragma is verified on a freshly-made
        // connection, not just the schema-init one.
        for _ in 0..2 {
            let conn = db.conn();
            let on: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            assert_eq!(on, 1, "foreign_keys must be ON on a pooled connection");
        }

        // And that it actually bites: an FK-violating insert is rejected.
        let conn = db.conn();
        let err = conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (999999, 999999)",
            [],
        );
        assert!(err.is_err(), "FK violation must be rejected, got {err:?}");
    }
}
