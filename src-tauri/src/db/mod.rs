// Database operations module
// Handles SQLite catalog operations

pub mod schema;
mod operations;
mod operations_blackhole;
mod equipment;
pub mod calibration_links;

pub use schema::*;
pub use operations::*;
pub use operations_blackhole::*;
pub use equipment::*;

use rusqlite::{Connection, Result};
use std::path::PathBuf;
use std::sync::Mutex;

/// Database connection wrapper
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Create a new database connection
    pub fn new(path: PathBuf) -> Result<Self> {
        let conn = Connection::open(&path)?;

        // Performance tuning - safe with WAL mode
        // synchronous=NORMAL: 2x faster writes, crash-safe with WAL
        // cache_size=-64000: 64MB cache (default is 2MB)
        // temp_store=MEMORY: Store temp tables in memory
        // mmap_size=256MB: Memory-mapped I/O for faster reads
        conn.execute_batch(
            "PRAGMA busy_timeout = 1500;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;",
        )?;

        init_db(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Get a connection lock
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}
