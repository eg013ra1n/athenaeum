use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use super::models::{CacheEntry, CacheStats, StretchMode};

/// Initialize the cache database with schema
pub fn init_cache_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Create cache entries table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            file_path TEXT NOT NULL,
            file_modified_at TEXT NOT NULL,
            cache_filename TEXT NOT NULL UNIQUE,
            cache_version TEXT NOT NULL,
            stretch_mode TEXT NOT NULL,
            black_point INTEGER,
            white_point INTEGER,
            midtones REAL NOT NULL,
            resolution TEXT NOT NULL,
            image_width INTEGER NOT NULL,
            image_height INTEGER NOT NULL,
            is_color INTEGER NOT NULL,
            file_size INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            last_accessed_at TEXT NOT NULL,
            access_count INTEGER DEFAULT 0
        )",
        [],
    )?;

    // Migrate existing databases: add resolution column if it doesn't exist
    let _ = conn.execute(
        "ALTER TABLE cache_entries ADD COLUMN resolution TEXT NOT NULL DEFAULT 'preview'",
        [],
    );

    // Create indexes for fast lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cache_file_id ON cache_entries(file_id)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cache_file_path ON cache_entries(file_path)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cache_accessed ON cache_entries(last_accessed_at)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_cache_lookup ON cache_entries(
            file_path, cache_version, stretch_mode, black_point, white_point, midtones, resolution
        )",
        [],
    )?;

    // Create cache statistics table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache_stats (
            key TEXT PRIMARY KEY,
            value TEXT,
            updated_at TEXT NOT NULL
        )",
        [],
    )?;

    // Initialize stats if not present
    conn.execute(
        "INSERT OR IGNORE INTO cache_stats VALUES
            ('total_entries', '0', datetime('now')),
            ('total_size_bytes', '0', datetime('now')),
            ('max_size_bytes', '10737418240', datetime('now')),  -- 10 GB default
            ('cache_hits', '0', datetime('now')),
            ('cache_misses', '0', datetime('now'))",
        [],
    )?;

    // Create processing queue table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            file_path TEXT NOT NULL,
            stretch_mode TEXT NOT NULL,
            black_point INTEGER,
            white_point INTEGER,
            midtones REAL NOT NULL,
            priority INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_queue_status_priority ON cache_queue(status, priority)",
        [],
    )?;

    Ok(conn)
}

/// Find a cache entry matching the given parameters
pub fn find_cache_entry(
    conn: &Connection,
    file_path: &str,
    cache_version: &str,
    stretch_mode: &StretchMode,
    black_point: Option<u16>,
    white_point: Option<u16>,
    midtones: f32,
    resolution: &str,
) -> Result<Option<CacheEntry>> {
    let mode_str = stretch_mode.to_string();

    let mut stmt = conn.prepare(
        "SELECT * FROM cache_entries
         WHERE file_path = ?1
           AND cache_version = ?2
           AND stretch_mode = ?3
           AND (black_point IS ?4 OR black_point = ?4)
           AND (white_point IS ?5 OR white_point = ?5)
           AND midtones = ?6
           AND resolution = ?7",
    )?;

    let entry = stmt
        .query_row(
            params![
                file_path,
                cache_version,
                mode_str,
                black_point,
                white_point,
                midtones,
                resolution
            ],
            |row| {
                Ok(CacheEntry {
                    id: Some(row.get(0)?),
                    file_id: row.get(1)?,
                    file_path: row.get(2)?,
                    file_modified_at: row.get(3)?,
                    cache_filename: row.get(4)?,
                    cache_version: row.get(5)?,
                    stretch_mode: if row.get::<_, String>(6)? == "auto" {
                        StretchMode::Auto
                    } else {
                        StretchMode::Manual
                    },
                    black_point: row.get(7)?,
                    white_point: row.get(8)?,
                    midtones: row.get(9)?,
                    resolution: row.get(10)?,
                    image_width: row.get(11)?,
                    image_height: row.get(12)?,
                    is_color: row.get(13)?,
                    file_size: row.get(14)?,
                    created_at: row.get(15)?,
                    last_accessed_at: row.get(16)?,
                    access_count: row.get(17)?,
                })
            },
        )
        .optional()?;

    Ok(entry)
}

/// Insert a new cache entry
pub fn insert_cache_entry(conn: &Connection, entry: &CacheEntry) -> Result<i64> {
    let mode_str = entry.stretch_mode.to_string();

    conn.execute(
        "INSERT INTO cache_entries (
            file_id, file_path, file_modified_at, cache_filename, cache_version,
            stretch_mode, black_point, white_point, midtones, resolution,
            image_width, image_height, is_color, file_size,
            created_at, last_accessed_at, access_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            entry.file_id,
            entry.file_path,
            entry.file_modified_at,
            entry.cache_filename,
            entry.cache_version,
            mode_str,
            entry.black_point,
            entry.white_point,
            entry.midtones,
            entry.resolution,
            entry.image_width,
            entry.image_height,
            entry.is_color,
            entry.file_size,
            entry.created_at,
            entry.last_accessed_at,
            entry.access_count
        ],
    )?;

    // Update statistics
    update_stat(conn, "total_entries", |v| (v + 1).to_string())?;
    update_stat(conn, "total_size_bytes", |v| (v + entry.file_size).to_string())?;

    Ok(conn.last_insert_rowid())
}

/// Update access time and count for a cache entry
pub fn update_cache_access(conn: &Connection, cache_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE cache_entries
         SET last_accessed_at = datetime('now'),
             access_count = access_count + 1
         WHERE id = ?1",
        params![cache_id],
    )?;

    // Update hit counter
    update_stat(conn, "cache_hits", |v| (v + 1).to_string())?;

    Ok(())
}

/// Update miss counter
pub fn increment_cache_miss(conn: &Connection) -> Result<()> {
    update_stat(conn, "cache_misses", |v| (v + 1).to_string())?;
    Ok(())
}

/// Delete a cache entry
pub fn delete_cache_entry(conn: &Connection, cache_id: i64) -> Result<()> {
    // Get file size before deletion for stats update
    let file_size: u64 = conn.query_row(
        "SELECT file_size FROM cache_entries WHERE id = ?1",
        params![cache_id],
        |row| row.get(0),
    )?;

    conn.execute("DELETE FROM cache_entries WHERE id = ?1", params![cache_id])?;

    // Update statistics
    update_stat(conn, "total_entries", |v| (v.saturating_sub(1)).to_string())?;
    update_stat(conn, "total_size_bytes", |v| {
        (v.saturating_sub(file_size)).to_string()
    })?;

    Ok(())
}

/// Delete all cache entries
pub fn clear_all_cache_entries(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM cache_entries", [])?;

    // Reset statistics
    conn.execute(
        "UPDATE cache_stats SET value = '0', updated_at = datetime('now')
         WHERE key IN ('total_entries', 'total_size_bytes')",
        [],
    )?;

    Ok(())
}

/// Get cache statistics
pub fn get_cache_stats(conn: &Connection) -> Result<CacheStats> {
    let total_entries = get_stat(conn, "total_entries")?.parse().unwrap_or(0);
    let total_size_bytes = get_stat(conn, "total_size_bytes")?.parse().unwrap_or(0);
    let max_size_bytes = get_stat(conn, "max_size_bytes")?.parse().unwrap_or(10737418240);
    let cache_hits: u64 = get_stat(conn, "cache_hits")?.parse().unwrap_or(0);
    let cache_misses: u64 = get_stat(conn, "cache_misses")?.parse().unwrap_or(0);

    let hit_rate = if cache_hits + cache_misses > 0 {
        cache_hits as f64 / (cache_hits + cache_misses) as f64
    } else {
        0.0
    };

    Ok(CacheStats {
        total_entries,
        total_size_bytes,
        cache_hits,
        cache_misses,
        hit_rate,
        max_size_bytes,
    })
}

/// Get oldest entries for LRU eviction
pub fn get_oldest_entries(conn: &Connection, limit: usize) -> Result<Vec<CacheEntry>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM cache_entries
         ORDER BY last_accessed_at ASC
         LIMIT ?1",
    )?;

    let entries = stmt
        .query_map(params![limit], |row| {
            Ok(CacheEntry {
                id: Some(row.get(0)?),
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                file_modified_at: row.get(3)?,
                cache_filename: row.get(4)?,
                cache_version: row.get(5)?,
                stretch_mode: if row.get::<_, String>(6)? == "auto" {
                    StretchMode::Auto
                } else {
                    StretchMode::Manual
                },
                black_point: row.get(7)?,
                white_point: row.get(8)?,
                midtones: row.get(9)?,
                resolution: row.get(10)?,
                image_width: row.get(11)?,
                image_height: row.get(12)?,
                is_color: row.get(13)?,
                file_size: row.get(14)?,
                created_at: row.get(15)?,
                last_accessed_at: row.get(16)?,
                access_count: row.get(17)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

/// Helper function to get a stat value
fn get_stat(conn: &Connection, key: &str) -> Result<String> {
    conn.query_row(
        "SELECT value FROM cache_stats WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .context("Failed to get cache stat")
}

/// Helper function to update a stat value
fn update_stat<F>(conn: &Connection, key: &str, updater: F) -> Result<()>
where
    F: FnOnce(u64) -> String,
{
    let current: u64 = get_stat(conn, key)?.parse().unwrap_or(0);
    let new_value = updater(current);

    conn.execute(
        "UPDATE cache_stats SET value = ?1, updated_at = datetime('now') WHERE key = ?2",
        params![new_value, key],
    )?;

    Ok(())
}