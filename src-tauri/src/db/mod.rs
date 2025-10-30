// Database operations module
// Handles SQLite catalog operations

mod schema;
mod operations;
mod equipment;

pub use schema::*;
pub use operations::*;
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
        let conn = Connection::open(path)?;
        init_db(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Get a connection lock
    pub fn conn(&self) -> std::sync::MutexGuard<Connection> {
        self.conn.lock().unwrap()
    }
}
