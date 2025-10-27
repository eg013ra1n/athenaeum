use rusqlite::{Connection, Result};

/// Initialize the database schema
pub fn init_db(conn: &Connection) -> Result<()> {
    // Files table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            filename TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL,
            modified_at TEXT NOT NULL,
            format TEXT NOT NULL CHECK(format IN ('FITS', 'XISF')),
            content_hash TEXT NOT NULL DEFAULT '',
            duplicate_group_id INTEGER,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )?;

    // Add filename column to existing tables (migration)
    let _ = conn.execute(
        "ALTER TABLE files ADD COLUMN filename TEXT NOT NULL DEFAULT ''",
        [],
    );

    // Backfill filename from path for existing records
    conn.execute(
        "UPDATE files SET filename = SUBSTR(path, INSTR(path, '/') + 1) WHERE filename = ''",
        [],
    )?;

    // Frames table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            object TEXT,
            date_obs TEXT,
            telescop TEXT,
            instrume TEXT,
            exptime REAL,
            filter TEXT,
            imagetyp TEXT,
            gain REAL,
            offset REAL,
            binning TEXT,
            xbinning INTEGER,
            ybinning INTEGER,
            ccd_temp REAL,
            set_temp REAL,
            focal_length REAL,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Scan roots table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS scan_roots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1,
            last_scan TEXT
        )",
        [],
    )?;

    // Tags table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            color TEXT
        )",
        [],
    )?;

    // Frame tags junction table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frame_tags (
            frame_id INTEGER NOT NULL,
            tag_id INTEGER NOT NULL,
            PRIMARY KEY (frame_id, tag_id),
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE,
            FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Calibration sets table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_sets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            imagetyp TEXT NOT NULL,
            exptime REAL,
            filter TEXT,
            ccd_temp REAL,
            gain REAL,
            binning TEXT,
            instrume TEXT,
            date TEXT NOT NULL
        )",
        [],
    )?;

    // Calibration set frames junction table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_set_frames (
            set_id INTEGER NOT NULL,
            frame_id INTEGER NOT NULL,
            PRIMARY KEY (set_id, frame_id),
            FOREIGN KEY (set_id) REFERENCES calibration_sets(id) ON DELETE CASCADE,
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Export templates table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS export_templates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            template TEXT NOT NULL,
            description TEXT
        )",
        [],
    )?;

    // Create indexes for common queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_filename ON files(filename)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_size ON files(size)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_date_obs ON frames(date_obs)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_object ON frames(object)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_telescop ON frames(telescop)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_instrume ON frames(instrume)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_imagetyp ON frames(imagetyp)",
        [],
    )?;

    Ok(())
}
