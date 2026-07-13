use rusqlite::{Connection, Result};

pub const UUID_TABLES: [&str; 7] = [
    "files", "frames", "frames_set", "sessions",
    "calibration_set", "tags", "export_templates",
];

/// (Re)create the `calibration_set_empty_prune` trigger (B2). Factored out so
/// `api::calibration::refresh_calibration_library_inner` can suspend it around
/// its clear-memberships + recluster window and restore the identical
/// definition afterwards.
///
/// Exemptions in the WHEN clause:
/// - master library sets (`is_master_library = 1`): intrinsically
///   single-frame; the sole member may legitimately be removed via re-import
///   without the parent row being garbage.
/// - superseded raw sets / `master_provenance.source_set_id` targets: frozen
///   provenance history. `source_set_id` is an FK with no ON DELETE action,
///   so pruning such a set would abort the caller's own DELETE with a
///   foreign-key failure (and losing the row would orphan the master's
///   provenance anchor even if it didn't).
pub(crate) fn create_calibration_set_empty_prune_trigger(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS calibration_set_empty_prune
         AFTER DELETE ON calibration_set_frames
         FOR EACH ROW
         WHEN NOT EXISTS (
            SELECT 1 FROM calibration_set_frames
             WHERE set_id = OLD.set_id
         )
           AND EXISTS (
            SELECT 1 FROM calibration_set
             WHERE id = OLD.set_id
               AND COALESCE(is_master_library, 0) = 0
               AND superseded_by_set_id IS NULL
         )
           AND NOT EXISTS (
            SELECT 1 FROM master_provenance
             WHERE source_set_id = OLD.set_id
         )
         BEGIN
            DELETE FROM calibration_set WHERE id = OLD.set_id;
         END",
        [],
    )?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, col],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn backfill_identity(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for t in UUID_TABLES {
        let ids: Vec<i64> = {
            let mut st = tx.prepare(&format!("SELECT id FROM {t} WHERE uuid IS NULL"))?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        if !ids.is_empty() {
            let mut up = tx.prepare(&format!("UPDATE {t} SET uuid = ?1 WHERE id = ?2"))?;
            for id in &ids {
                up.execute(rusqlite::params![uuid::Uuid::new_v4().to_string(), id])?;
            }
        }
        // best-available timestamp source per table; NULL-safe
        let src = match t {
            "files" | "sessions" => "COALESCE(created_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            _ => "strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        };
        tx.execute(
            &format!("UPDATE {t} SET updated_at = {src} WHERE updated_at IS NULL"),
            [],
        )?;
    }
    tx.commit()
}

/// Initialize the database schema
pub fn init_db(conn: &Connection) -> Result<()> {
    // Files table - includes metadata hash for quick duplicate detection and content_hash for xxhash-based detection
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            filename TEXT NOT NULL,
            size INTEGER NOT NULL,
            modified_at TEXT NOT NULL,
            format TEXT NOT NULL CHECK(format IN ('FITS', 'XISF')),
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            metadata_hash TEXT,
            content_hash TEXT
        )",
        [],
    )?;

    // Frames table - expanded with astronomical coordinates and new fields
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
            gain REAL,
            offset REAL,
            binning TEXT,
            xbinning INTEGER,
            ybinning INTEGER,
            ccd_temp REAL,
            set_temp REAL,
            focallen REAL,
            xpixsz REAL,
            ypixsz REAL,
            naxis1 INTEGER,
            naxis2 INTEGER,
            ra REAL,
            dec REAL,
            sitelat REAL,
            lat_obs REAL,
            sitelong REAL,
            long_obs REAL,
            objctra TEXT,
            objctdec TEXT,
            override INTEGER NOT NULL DEFAULT 0,
            imagetyp TEXT,
            is_master INTEGER NOT NULL DEFAULT 0,
            swcreate TEXT,
            rotation REAL,
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
            find_duplicates INTEGER NOT NULL DEFAULT 1,
            unique_camera INTEGER NOT NULL DEFAULT 0,
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

    // Calibration set table (renamed from calibration_sets)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_set (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            imagetyp TEXT NOT NULL,
            exptime REAL,
            filter TEXT,
            ccd_temp REAL,
            gain REAL,
            binning TEXT,
            instrume TEXT,
            telescop TEXT,
            date TEXT NOT NULL,
            date_start TEXT,
            date_end TEXT,
            temp_min REAL,
            temp_max REAL,
            offset REAL,
            frame_count INTEGER DEFAULT 0,
            is_master_library INTEGER NOT NULL DEFAULT 0,
            focallen REAL
        )",
        [],
    )?;

    // Calibration set frames junction table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_set_frames (
            set_id INTEGER NOT NULL,
            frame_id INTEGER NOT NULL,
            PRIMARY KEY (set_id, frame_id),
            FOREIGN KEY (set_id) REFERENCES calibration_set(id) ON DELETE CASCADE,
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

    // Projects table - for organizing imaging projects
    conn.execute(
        "CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )",
        [],
    )?;

    // Frames set table - imaging sessions
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frames_set (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT,
            is_custom INTEGER NOT NULL DEFAULT 0,
            date_obs_start TEXT,
            date_obs_end TEXT,
            objctra TEXT,
            objctdec TEXT,
            total_exp_time REAL
        )",
        [],
    )?;

    // Imaging nights table - top-level grouping by observation night
    conn.execute(
        "CREATE TABLE IF NOT EXISTS imaging_nights (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER NOT NULL,
            start_time TEXT NOT NULL,
            end_time TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Sessions table - grouping by instrume within an imaging night
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            imaging_night_id INTEGER NOT NULL,
            instrume TEXT NOT NULL,
            frame_count INTEGER NOT NULL DEFAULT 0,
            total_exp_time REAL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (imaging_night_id) REFERENCES imaging_nights(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Session members junction table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS session_members (
            session_id INTEGER NOT NULL,
            frame_id INTEGER NOT NULL,
            PRIMARY KEY (session_id, frame_id),
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // FITS header table - stores complete original FITS header
    conn.execute(
        "CREATE TABLE IF NOT EXISTS fits_header (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            header TEXT NOT NULL,
            header_fingerprint TEXT,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Settings table - stores application configuration
    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT,
            updated_at TEXT
        )",
        [],
    )?;

    // Black hole table - soft delete tracking
    conn.execute(
        "CREATE TABLE IF NOT EXISTS black_hole (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            from_where TEXT NOT NULL,
            moved_at TEXT NOT NULL,
            original_path TEXT NOT NULL,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        )",
        [],
    )?;
    // One-time cleanup: collapse duplicate black_hole rows. The table historically
    // had no UNIQUE(file_id) constraint and a plain INSERT could duplicate a row
    // when a file was blackholed twice. Keep the earliest row per file_id. Must
    // run BEFORE the unique index is created, or index creation fails on a dirty DB.
    conn.execute(
        "DELETE FROM black_hole
         WHERE id NOT IN (SELECT MIN(id) FROM black_hole GROUP BY file_id)",
        [],
    )?;
    // Guarantee one black_hole row per file going forward. Doubles as the file_id
    // lookup index used by get_blackholed_file_ids / restore.
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_black_hole_file_id ON black_hole(file_id)",
        [],
    )?;

    // Missing files table - tracks files that no longer exist on disk
    conn.execute(
        "CREATE TABLE IF NOT EXISTS missing_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL UNIQUE,
            scan_root_id INTEGER NOT NULL,
            detected_at TEXT NOT NULL,
            last_checked_at TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'missing',
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
            FOREIGN KEY (scan_root_id) REFERENCES scan_roots(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Calibration set to frames - links frames/sets to their required calibration sets
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_set_to_frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id INTEGER NOT NULL,
            source_type TEXT NOT NULL CHECK(source_type IN ('frame', 'calibration_set')),
            calibration_set_id INTEGER NOT NULL,
            calibration_type TEXT NOT NULL CHECK(calibration_type IN ('Dark', 'Flat', 'Bias', 'DarkFlat')),
            matched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            match_score REAL,
            date_warning INTEGER DEFAULT 0,
            temp_warning INTEGER DEFAULT 0,
            is_manual_override INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE,
            UNIQUE(source_id, source_type, calibration_type)
        )",
        [],
    )?;

    // Duplicate groups cache - stores pre-computed duplicate file groups
    conn.execute(
        "CREATE TABLE IF NOT EXISTS duplicate_groups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            hash TEXT NOT NULL,
            hash_type TEXT NOT NULL CHECK(hash_type IN ('content', 'metadata')),
            size INTEGER NOT NULL,
            file_count INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(hash, hash_type)
        )",
        [],
    )?;

    // Duplicate group files - links files to duplicate groups
    conn.execute(
        "CREATE TABLE IF NOT EXISTS duplicate_group_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            group_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            FOREIGN KEY (group_id) REFERENCES duplicate_groups(id) ON DELETE CASCADE,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
            UNIQUE(group_id, file_id)
        )",
        [],
    )?;

    // Folder similarity cache - stores pre-computed folder similarity results
    conn.execute(
        "CREATE TABLE IF NOT EXISTS folder_similarity (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            folder_a TEXT NOT NULL,
            folder_b TEXT NOT NULL,
            shared_files INTEGER NOT NULL,
            shared_size INTEGER NOT NULL,
            unique_a INTEGER NOT NULL,
            unique_b INTEGER NOT NULL,
            similarity_percent REAL NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(folder_a, folder_b)
        )",
        [],
    )?;

    // Archive roots - user-configured destination folders for ZIP archives
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_roots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            label TEXT,
            is_default INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )?;

    // Archive operations - one row per archive operation (ZIP archive feature).
    // frames_set_id is nullable + calibration_set_id exists from birth on fresh
    // DBs so the one-time rebuild in the migrations section below is skipped
    // (it's gated on the absence of calibration_set_id).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER,
            calibration_set_id INTEGER,
            archive_root_path TEXT NOT NULL,
            flats_disposition TEXT,
            darks_disposition TEXT,
            bias_disposition TEXT,
            darkflats_disposition TEXT,
            compression TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            error_message TEXT,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE,
            FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Archive operation files - frozen plan: one row per file the operation will touch
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operation_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            file_id INTEGER,
            source_path TEXT NOT NULL,
            target_zip_path TEXT NOT NULL,
            target_path_in_zip TEXT NOT NULL,
            expected_hash TEXT NOT NULL,
            disposition TEXT NOT NULL,
            frame_role TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Archive operation steps - audit log: one row per (file, stage) pair
    conn.execute(
        "CREATE TABLE IF NOT EXISTS archive_operation_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            operation_file_id INTEGER,
            stage TEXT NOT NULL,
            status TEXT NOT NULL,
            actual_hash TEXT,
            error_message TEXT,
            started_at TEXT,
            completed_at TEXT,
            FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE,
            FOREIGN KEY (operation_file_id) REFERENCES archive_operation_files(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // File operations - one row per dual-pane Move or Delete operation
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_operations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK(kind IN ('move','delete')),
            status TEXT NOT NULL,
            source_root TEXT,
            dest_dir TEXT,
            total_files INTEGER NOT NULL DEFAULT 0,
            total_bytes INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            started_at TEXT,
            finished_at TEXT,
            error_message TEXT
        )",
        [],
    )?;

    // File operation files - frozen plan: one row per file the operation will touch
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_operation_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            source_path TEXT NOT NULL,
            dest_path TEXT,
            strategy TEXT NOT NULL,
            catalog_file_id INTEGER,
            expected_hash TEXT,
            file_size_bytes INTEGER NOT NULL DEFAULT 0,
            disposition TEXT NOT NULL DEFAULT 'planned',
            FOREIGN KEY (operation_id) REFERENCES file_operations(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // File operation steps - audit log: one row per (file, stage) pair for resume + rollback
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_operation_steps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation_id INTEGER NOT NULL,
            operation_file_id INTEGER,
            stage TEXT NOT NULL,
            status TEXT NOT NULL,
            actual_hash TEXT,
            error_message TEXT,
            started_at TEXT,
            completed_at TEXT,
            FOREIGN KEY (operation_id) REFERENCES file_operations(id) ON DELETE CASCADE,
            FOREIGN KEY (operation_file_id) REFERENCES file_operation_files(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Create indexes for common queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_filename ON files(filename)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_metadata_hash ON files(metadata_hash)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash)",
        [],
    )?;
    // Path index for LIKE prefix queries (e.g., path LIKE '/foo/bar%')
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_path ON files(path)",
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
        "CREATE INDEX IF NOT EXISTS idx_frames_instrume ON frames(instrume)",
        [],
    )?;
    // Covering index for Equipment page query (get_all_cameras)
    // Includes instrume, exptime, date_obs to avoid table lookups for aggregations
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_instrume_stats ON frames(instrume, exptime, date_obs)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_ra ON frames(ra)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_dec ON frames(dec)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_objctra ON frames(objctra)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_objctdec ON frames(objctdec)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_exptime ON frames(exptime)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_filter ON frames(filter)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_imagetyp ON frames(imagetyp)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frames_is_master ON frames(is_master)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calibration_set_instrume ON calibration_set(instrume)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calibration_set_is_master ON calibration_set(is_master_library)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_black_hole_from_where ON black_hole(from_where)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_black_hole_moved_at ON black_hole(moved_at)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_missing_files_scan_root ON missing_files(scan_root_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_missing_files_status ON missing_files(status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_fits_header_fingerprint ON fits_header(header_fingerprint)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calib_link_source ON calibration_set_to_frames(source_id, source_type)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calib_link_set ON calibration_set_to_frames(calibration_set_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calib_link_type ON calibration_set_to_frames(calibration_type)",
        [],
    )?;

    // Indexes for duplicate cache tables
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_dup_group_hash ON duplicate_groups(hash, hash_type)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_dup_group_files_group ON duplicate_group_files(group_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_dup_group_files_file ON duplicate_group_files(file_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_files_op ON archive_operation_files(operation_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_steps_op ON archive_operation_steps(operation_id, status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_ops_status ON archive_operations(status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_ops_frames_set ON archive_operations(frames_set_id)",
        [],
    )?;
    // idx_archive_ops_calibration_set is NOT created here: on a pre-existing
    // (legacy) database this early section runs before the Phase 2 rebuild
    // below has added the calibration_set_id column, and CREATE INDEX on a
    // nonexistent column fails immediately, hard-crashing init_db() on every
    // upgrade. It's created once, unconditionally, right after the rebuild
    // section instead — by then the column is guaranteed to exist on both
    // fresh and migrated databases.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_op_files_op ON file_operation_files(operation_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_op_steps_op_file ON file_operation_steps(operation_id, operation_file_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_op_steps_status ON file_operation_steps(status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_ops_status ON file_operations(status)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_folder_sim_percent ON folder_similarity(similarity_percent DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_session_members_frame ON session_members(frame_id)",
        [],
    )?;

    // Registration results — stacking-preparation frame-to-reference alignment.
    // One row per (frames_set_id, frame_id) pair. The reference frame carries
    // is_reference=1 and an identity-like transform; every other member carries
    // the affine sub→reference transform + a refined WCS derived by composing
    // the reference WCS with that transform.  The table is additive and never
    // touches frames.ra/dec or plate_solves.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS registration_results (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER NOT NULL,
            frame_id INTEGER NOT NULL,
            reference_frame_id INTEGER NOT NULL,
            is_reference INTEGER NOT NULL DEFAULT 0,
            crpix1 REAL,
            crpix2 REAL,
            crval1 REAL,
            crval2 REAL,
            cd1_1 REAL,
            cd1_2 REAL,
            cd2_1 REAL,
            cd2_2 REAL,
            affine_a1 REAL,
            affine_b1 REAL,
            affine_c1 REAL,
            affine_a2 REAL,
            affine_b2 REAL,
            affine_c2 REAL,
            matched_stars INTEGER NOT NULL,
            rms_residual_px REAL NOT NULL,
            rms_residual_arcsec REAL,
            status TEXT NOT NULL,
            error TEXT,
            compute_time_ms INTEGER NOT NULL,
            registered_at TEXT NOT NULL,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE,
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE,
            UNIQUE(frames_set_id, frame_id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_registration_results_set ON registration_results(frames_set_id)",
        [],
    )?;

    // User-chosen reference frame for registration, keyed per frame set.
    // One row per frame set; INSERT OR REPLACE on every update.
    // Both FKs have ON DELETE CASCADE so stale rows are auto-removed when a
    // frame set or its member frame is deleted.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frame_set_reference (
            frames_set_id INTEGER PRIMARY KEY,
            reference_frame_id INTEGER NOT NULL,
            set_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE,
            FOREIGN KEY (reference_frame_id) REFERENCES frames(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // A5: keep `calibration_set_to_frames.source_id` consistent. The
    // `calibration_set_id` column has a FK with ON DELETE CASCADE, but
    // `source_id` does not — when a calibration_set is deleted, any sub-cal
    // links where `source_type = 'calibration_set' AND source_id = OLD.id`
    // would be left dangling, silently losing the user's hierarchy. Install
    // a trigger that prunes them on parent delete, and run a one-shot sweep
    // to clean any orphans that already exist in older databases.
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS calibration_set_subcal_cleanup
         AFTER DELETE ON calibration_set
         FOR EACH ROW
         BEGIN
            DELETE FROM calibration_set_to_frames
             WHERE source_type = 'calibration_set'
               AND source_id = OLD.id;
         END",
        [],
    )?;
    conn.execute(
        "DELETE FROM calibration_set_to_frames
         WHERE source_type = 'calibration_set'
           AND source_id NOT IN (SELECT id FROM calibration_set)",
        [],
    )?;

    // B2 (empty-set prune trigger + startup sweep) lives further down, after
    // the Phase 2 migration block: its WHEN clause references
    // `superseded_by_set_id` and `master_provenance`, which must exist first.

    // Migrations - add columns to existing tables if they don't exist
    // Add find_duplicates to scan_roots table (migration for existing databases)
    let has_find_duplicates: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('scan_roots') WHERE name='find_duplicates'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_find_duplicates {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN find_duplicates INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }

    // Add header_fingerprint to fits_header table (migration for existing databases)
    let has_header_fingerprint: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('fits_header') WHERE name='header_fingerprint'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_header_fingerprint {
        conn.execute(
            "ALTER TABLE fits_header ADD COLUMN header_fingerprint TEXT",
            [],
        )?;
    }

    // Self-heal: fill fingerprints for any legacy rows still missing them, so
    // relinking works on databases created before fingerprinting existed.
    // Idempotent (a no-op once all rows are fingerprinted); non-fatal so a
    // backfill hiccup never blocks startup.
    match super::operations::backfill_null_header_fingerprints(conn) {
        Ok(n) if n > 0 => tracing::info!(table = "fits_header", count = n, "backfilled legacy header fingerprints"),
        Ok(_) => {}
        Err(e) => tracing::warn!(table = "fits_header", error = %e, "header-fingerprint backfill skipped (non-fatal)"),
    }

    // Add content_hash to files table (migration for existing databases)
    let has_content_hash: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='content_hash'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_content_hash {
        conn.execute(
            "ALTER TABLE files ADD COLUMN content_hash TEXT",
            [],
        )?;
    }

    // Add date_obs_start to frames_set table (migration for existing databases)
    let has_date_obs_start: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='date_obs_start'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_date_obs_start {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN date_obs_start TEXT",
            [],
        )?;
    }

    // Add date_obs_end to frames_set table (migration for existing databases)
    let has_date_obs_end: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='date_obs_end'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_date_obs_end {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN date_obs_end TEXT",
            [],
        )?;
    }

    // Add flat_pattern to frames_set table (migration for existing databases)
    let has_flat_pattern: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='flat_pattern'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_flat_pattern {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN flat_pattern TEXT",
            [],
        )?;
    }

    // Add is_manual_override to calibration_set_to_frames table (migration for existing databases)
    let has_is_manual_override: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('calibration_set_to_frames') WHERE name='is_manual_override'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_is_manual_override {
        conn.execute(
            "ALTER TABLE calibration_set_to_frames ADD COLUMN is_manual_override INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // Add telescop to calibration_set table (migration for existing databases)
    // Used for telescope-based calibration matching
    let has_telescop: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('calibration_set') WHERE name='telescop'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_telescop {
        conn.execute(
            "ALTER TABLE calibration_set ADD COLUMN telescop TEXT",
            [],
        )?;
    }

    // Add unique_camera to scan_roots table (migration for existing databases)
    let has_unique_camera: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('scan_roots') WHERE name='unique_camera'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_unique_camera {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN unique_camera INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // Add bayerpat to frames table (migration for existing databases)
    // Used to distinguish OSC (one-shot color) cameras from mono cameras
    // OSC cameras have a Bayer pattern (e.g., "RGGB", "BGGR")
    let has_bayerpat: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames') WHERE name='bayerpat'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_bayerpat {
        conn.execute(
            "ALTER TABLE frames ADD COLUMN bayerpat TEXT",
            [],
        )?;
    }

    // Add rotation to frames table (migration for existing databases)
    // Stores image position angle in degrees (N through E), extracted from CROTA2 or CD matrix
    let has_rotation: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames') WHERE name='rotation'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_rotation {
        conn.execute(
            "ALTER TABLE frames ADD COLUMN rotation REAL",
            [],
        )?;
    }

    // Add rotation stats to frames_set table (migration for existing databases)
    let has_avg_rotation: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='avg_rotation'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_avg_rotation {
        conn.execute("ALTER TABLE frames_set ADD COLUMN avg_rotation REAL", [])?;
        conn.execute("ALTER TABLE frames_set ADD COLUMN min_rotation REAL", [])?;
        conn.execute("ALTER TABLE frames_set ADD COLUMN max_rotation REAL", [])?;
    }

    // Add last_scan_errors to scan_roots table (migration for existing databases)
    let has_last_scan_errors: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('scan_roots') WHERE name='last_scan_errors'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_last_scan_errors {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN last_scan_errors TEXT",
            [],
        )?;
    }

    // Add is_archived to frames_set table (migration for existing databases)
    let has_is_archived: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='is_archived'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_is_archived {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN is_archived INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // Add monitor_enabled to scan_roots table (migration for existing databases)
    // Enables per-root opt-in background polling for new files.
    let has_monitor_enabled: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('scan_roots') WHERE name='monitor_enabled'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_monitor_enabled {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN monitor_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // Frame set merge log - persistent audit history for auto-merge operations
    // (both button-triggered and monitor-triggered). Survives indefinitely; row
    // contains JSON blobs with the full per-frame breakdown for drill-down.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frame_set_merge_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER NOT NULL,
            occurred_at TEXT NOT NULL,
            source TEXT NOT NULL CHECK(source IN ('button', 'monitor')),
            threshold_arcmin REAL NOT NULL,
            frames_added_json TEXT NOT NULL,
            frames_skipped_json TEXT NOT NULL,
            added_count INTEGER NOT NULL,
            skipped_count INTEGER NOT NULL,
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_merge_log_set ON frame_set_merge_log(frames_set_id, occurred_at DESC)",
        [],
    )?;

    // Excluded frames table - stores frames excluded during auto-generation
    conn.execute(
        "CREATE TABLE IF NOT EXISTS excluded_frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            reason TEXT NOT NULL,
            excluded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Frame analysis table - star detection and image quality metrics
    conn.execute(
        "CREATE TABLE IF NOT EXISTS frame_analysis (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frame_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            stars_detected INTEGER NOT NULL,
            median_fwhm REAL NOT NULL,
            median_eccentricity REAL NOT NULL,
            median_snr REAL NOT NULL,
            median_hfr REAL NOT NULL,
            frame_snr REAL NOT NULL,
            snr_weight REAL NOT NULL,
            psf_signal REAL NOT NULL,
            background REAL NOT NULL,
            noise REAL NOT NULL,
            detection_threshold REAL NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            source_channels INTEGER NOT NULL,
            trail_r_squared REAL NOT NULL DEFAULT 0.0,
            possibly_trailed INTEGER NOT NULL DEFAULT 0,
            median_beta REAL,
            quality_score REAL,
            config_hash TEXT,
            analyzed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
            UNIQUE(frame_id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frame_analysis_frame_id ON frame_analysis(frame_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_frame_analysis_file_id ON frame_analysis(file_id)",
        [],
    )?;

    // Per-star metrics table — individual star detection results for overlay rendering
    conn.execute(
        "CREATE TABLE IF NOT EXISTS star_metrics (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frame_analysis_id INTEGER NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            peak REAL NOT NULL,
            flux REAL NOT NULL,
            fwhm REAL NOT NULL,
            fwhm_x REAL NOT NULL,
            fwhm_y REAL NOT NULL,
            eccentricity REAL NOT NULL,
            snr REAL NOT NULL,
            hfr REAL NOT NULL,
            theta REAL NOT NULL,
            beta REAL,
            fit_method TEXT NOT NULL,
            fit_residual REAL NOT NULL,
            FOREIGN KEY (frame_analysis_id) REFERENCES frame_analysis(id) ON DELETE CASCADE
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_star_metrics_analysis_id ON star_metrics(frame_analysis_id)",
        [],
    )?;

    // Add trail_r_squared and possibly_trailed to frame_analysis (migration for existing databases)
    let has_trail: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frame_analysis') WHERE name='trail_r_squared'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_trail {
        conn.execute(
            "ALTER TABLE frame_analysis ADD COLUMN trail_r_squared REAL NOT NULL DEFAULT 0.0",
            [],
        )?;
        conn.execute(
            "ALTER TABLE frame_analysis ADD COLUMN possibly_trailed INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    // Add median_beta to frame_analysis (migration for existing databases)
    let has_median_beta: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frame_analysis') WHERE name='median_beta'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_median_beta {
        conn.execute(
            "ALTER TABLE frame_analysis ADD COLUMN median_beta REAL",
            [],
        )?;
    }

    // Migration: rename snr_db → frame_snr (rustafits 0.7.1)
    let has_snr_db: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frame_analysis') WHERE name='snr_db'",
        [],
        |row| row.get(0),
    );
    if let Ok(1) = has_snr_db {
        conn.execute("ALTER TABLE frame_analysis RENAME COLUMN snr_db TO frame_snr", [])?;
    }

    // Calibration set originals table - stores original metadata values before custom edits
    // Used to backup original FITS header values when user edits calibration set metadata
    conn.execute(
        "CREATE TABLE IF NOT EXISTS calibration_set_originals (
            set_id INTEGER PRIMARY KEY,
            ccd_temp REAL,
            temp_min REAL,
            temp_max REAL,
            gain REAL,
            offset REAL,
            binning TEXT,
            exptime REAL,
            saved_at TEXT NOT NULL,
            FOREIGN KEY (set_id) REFERENCES calibration_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Plate solve results - WCS solutions from plate solving
    conn.execute(
        "CREATE TABLE IF NOT EXISTS plate_solves (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            frame_id INTEGER NOT NULL UNIQUE,
            crpix1 REAL NOT NULL,
            crpix2 REAL NOT NULL,
            crval1 REAL NOT NULL,
            crval2 REAL NOT NULL,
            cd1_1 REAL NOT NULL,
            cd1_2 REAL NOT NULL,
            cd2_1 REAL NOT NULL,
            cd2_2 REAL NOT NULL,
            sip_order INTEGER,
            sip_a_coeffs TEXT,
            sip_b_coeffs TEXT,
            sip_ap_coeffs TEXT,
            sip_bp_coeffs TEXT,
            matched_stars INTEGER NOT NULL,
            total_detected INTEGER NOT NULL,
            rms_residual_px REAL NOT NULL,
            rms_residual_arcsec REAL NOT NULL,
            pixel_scale_arcsec REAL NOT NULL,
            field_rotation_deg REAL NOT NULL,
            solve_time_ms INTEGER NOT NULL,
            catalog_used TEXT NOT NULL,
            algorithm_used TEXT NOT NULL,
            solved_at TEXT NOT NULL,
            expected_catalog_stars_in_fov INTEGER,
            inlier_ratio REAL,
            FOREIGN KEY (frame_id) REFERENCES frames(id) ON DELETE CASCADE
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_plate_solves_frame_id ON plate_solves(frame_id)",
        [],
    )?;

    // Migration: add density-aware confidence metrics to plate_solves
    // (rustafits v1.1.0 / Phase 2 density-aware acceptance).
    let has_expected_in_fov: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('plate_solves') WHERE name='expected_catalog_stars_in_fov'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_expected_in_fov {
        conn.execute(
            "ALTER TABLE plate_solves ADD COLUMN expected_catalog_stars_in_fov INTEGER",
            [],
        )?;
        conn.execute(
            "ALTER TABLE plate_solves ADD COLUMN inlier_ratio REAL",
            [],
        )?;
    }

    // Add archived_at to frames_set table (ZIP archive feature)
    let has_archived_at: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='archived_at'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archived_at {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN archived_at TEXT",
            [],
        )?;
    }

    // Add archive_operation_id to frames_set table (ZIP archive feature)
    let has_archive_op_id: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name='archive_operation_id'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_op_id {
        conn.execute(
            "ALTER TABLE frames_set ADD COLUMN archive_operation_id INTEGER",
            [],
        )?;
    }

    // Add archived_in_operation to files table (ZIP archive feature)
    let has_archived_in_op: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archived_in_operation'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archived_in_op {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archived_in_operation INTEGER",
            [],
        )?;
    }

    // Add archive_zip_path to files table (ZIP archive feature)
    let has_archive_zip_path: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archive_zip_path'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_zip_path {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archive_zip_path TEXT",
            [],
        )?;
    }

    // Add archive_path_in_zip to files table (ZIP archive feature)
    let has_archive_path_in_zip: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name='archive_path_in_zip'",
        [],
        |row| row.get(0),
    );
    if let Ok(0) = has_archive_path_in_zip {
        conn.execute(
            "ALTER TABLE files ADD COLUMN archive_path_in_zip TEXT",
            [],
        )?;
    }

    // ---- Phase 2: calibration library ----

    // scan_roots.kind: 'normal' | 'calibration_library'
    if !column_exists(conn, "scan_roots", "kind")? {
        conn.execute(
            "ALTER TABLE scan_roots ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal'",
            [],
        )?;
    }

    // calibration_set.superseded_by_set_id: set once a master replaced this raw set
    if !column_exists(conn, "calibration_set", "superseded_by_set_id")? {
        conn.execute(
            "ALTER TABLE calibration_set ADD COLUMN superseded_by_set_id INTEGER REFERENCES calibration_set(id)",
            [],
        )?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_calibration_set_superseded ON calibration_set(superseded_by_set_id)",
        [],
    )?;

    // master_provenance: row EXISTS = master built by Athenaeum
    conn.execute(
        "CREATE TABLE IF NOT EXISTS master_provenance (
            master_set_id      INTEGER PRIMARY KEY REFERENCES calibration_set(id) ON DELETE CASCADE,
            source_set_id      INTEGER REFERENCES calibration_set(id),
            recipe_json        TEXT NOT NULL,
            member_frame_uuids TEXT NOT NULL,
            member_hash        TEXT NOT NULL,
            created_at         TEXT NOT NULL
        )",
        [],
    )?;

    // B5: light_calibrations — sole tracking record for calibrated-light FITS
    // artifacts, which live OUTSIDE the catalog and are never registered in
    // `files`/`frames` (design spec 2026-07-05-light-calibration-design.md
    // §5). `frame_id` is nullable + UNIQUE: NULL only for an "adopted" row
    // whose source frame isn't cataloged yet, UNIQUE because a frame has at
    // most one tracked calibrated output. `dark_set_id`/`flat_set_id`/
    // `bias_set_id` are `ON DELETE SET NULL` (unlike
    // `master_provenance.master_set_id`'s CASCADE): deleting a calibration set
    // — including a master a calibrated light references — must NOT delete the
    // calibrated-light history AND must NOT abort the delete with a
    // FOREIGN KEY constraint failure (as a no-action FK would once any light is
    // calibrated). A NULLed set id makes `derive_status` yield Partial/Stale,
    // i.e. an honest "re-calibrate" prompt.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS light_calibrations (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            frame_id          INTEGER UNIQUE REFERENCES frames(id) ON DELETE CASCADE,
            source_uuid       TEXT,
            source_filename   TEXT,
            output_path       TEXT NOT NULL UNIQUE,
            dark_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
            flat_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
            bias_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
            calstat           TEXT NOT NULL,
            flat_norm_applied INTEGER NOT NULL,
            flat_norm_mode    TEXT NOT NULL DEFAULT 'centralThird',
            cal_params        TEXT NOT NULL DEFAULT '{}',
            output_hash       TEXT NOT NULL,
            engine_version    INTEGER NOT NULL,
            created_at        TEXT NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_light_cal_source_uuid ON light_calibrations(source_uuid)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_light_cal_source_filename ON light_calibrations(source_filename)",
        [],
    )?;

    // B5 (2026-07-06): selectable flat-normalization statistic. v0.2.5 is
    // unreleased, but the owner's dev DB already carries `light_calibrations`
    // rows, so ADD COLUMN with a DEFAULT preserves them (an existing row's
    // statistic was central-third — today's only behavior). Must run BEFORE the
    // FK rebuild below so that rebuild's explicit-column INSERT..SELECT finds the
    // column on the source table.
    if !column_exists(conn, "light_calibrations", "flat_norm_mode")? {
        conn.execute(
            "ALTER TABLE light_calibrations ADD COLUMN flat_norm_mode TEXT NOT NULL DEFAULT 'centralThird'",
            [],
        )?;
    }

    // B5 (2026-07-06): advanced per-run parameters (trim fraction / output
    // pedestal / bias-fallback policy) as one JSON blob. Same rationale as the
    // flat_norm_mode ADD COLUMN above — v0.2.5 is unreleased but the owner's dev
    // DB already carries `light_calibrations` rows, so DEFAULT '{}' preserves
    // them (an existing row predates every advanced param, so '{}' decodes to
    // `LightCalParams::default()` = today's behavior). Must run BEFORE the FK
    // rebuild below so that rebuild's explicit-column INSERT..SELECT finds the
    // column on the source table.
    if !column_exists(conn, "light_calibrations", "cal_params")? {
        conn.execute(
            "ALTER TABLE light_calibrations ADD COLUMN cal_params TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }

    // Guarded migration for dev DBs created before the `ON DELETE SET NULL`
    // fix (v0.2.5 is unreleased, so only dev DBs can carry the old no-action
    // FK). Detect via the pragma FK list: if any of the three set-id FKs
    // reference calibration_set WITHOUT `SET NULL`, rebuild the table in place
    // preserving rows (SQLite can't alter an FK action). Same FK-off / 12-step
    // recipe the archive_operations rebuild below uses.
    let light_cal_needs_rebuild = {
        let mut stmt = conn.prepare("PRAGMA foreign_key_list('light_calibrations')")?;
        // columns: 0 id, 1 seq, 2 table, 3 from, 4 to, 5 on_update, 6 on_delete, 7 match
        let fks = stmt
            .query_map([], |r| Ok((r.get::<_, String>(3)?, r.get::<_, String>(6)?)))?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
        fks.iter().any(|(from, on_delete)| {
            matches!(from.as_str(), "dark_set_id" | "flat_set_id" | "bias_set_id")
                && on_delete != "SET NULL"
        })
    };
    if light_cal_needs_rebuild {
        // FK enforcement OFF around the rebuild (a no-op inside a transaction,
        // so it must be issued OUTSIDE the BEGIN/COMMIT batch). light_calibrations
        // is a leaf — no other table FK-references its id — so DROP TABLE fires
        // no cascades and id preservation via the explicit-column INSERT
        // (SQLite re-seeds sqlite_sequence from the max copied id) is safe.
        conn.pragma_update(None, "foreign_keys", false)?;
        let rebuild = conn.execute_batch(
            "BEGIN;
             CREATE TABLE light_calibrations_new (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                frame_id          INTEGER UNIQUE REFERENCES frames(id) ON DELETE CASCADE,
                source_uuid       TEXT,
                source_filename   TEXT,
                output_path       TEXT NOT NULL UNIQUE,
                dark_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
                flat_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
                bias_set_id       INTEGER REFERENCES calibration_set(id) ON DELETE SET NULL,
                calstat           TEXT NOT NULL,
                flat_norm_applied INTEGER NOT NULL,
                flat_norm_mode    TEXT NOT NULL DEFAULT 'centralThird',
                cal_params        TEXT NOT NULL DEFAULT '{}',
                output_hash       TEXT NOT NULL,
                engine_version    INTEGER NOT NULL,
                created_at        TEXT NOT NULL
             );
             INSERT INTO light_calibrations_new
                (id, frame_id, source_uuid, source_filename, output_path, dark_set_id,
                 flat_set_id, bias_set_id, calstat, flat_norm_applied, flat_norm_mode, cal_params,
                 output_hash, engine_version, created_at)
                SELECT id, frame_id, source_uuid, source_filename, output_path, dark_set_id,
                       flat_set_id, bias_set_id, calstat, flat_norm_applied, flat_norm_mode, cal_params,
                       output_hash, engine_version, created_at
                FROM light_calibrations;
             DROP TABLE light_calibrations;
             ALTER TABLE light_calibrations_new RENAME TO light_calibrations;
             COMMIT;",
        );
        // Re-enable FK enforcement whether or not the rebuild succeeded, then
        // surface the rebuild error — never leave the connection unenforced.
        let re_enable = conn.pragma_update(None, "foreign_keys", true);
        rebuild?;
        re_enable?;
        // Step 10 of the recipe: verify the rebuild didn't orphan any row.
        let violations: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_check('light_calibrations')",
            [],
            |r| r.get(0),
        )?;
        if violations > 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "light_calibrations rebuild broke foreign-key integrity: \
                     {violations} violation(s)"
                )),
            ));
        }
        // Recreate the two indexes the DROP removed.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_light_cal_source_uuid ON light_calibrations(source_uuid)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_light_cal_source_filename ON light_calibrations(source_filename)",
            [],
        )?;
    }

    // B2: prune empty `calibration_set` rows automatically. The
    // `calibration_set_frames` junction table CASCADE-deletes rows when
    // either side is removed (e.g., a frame is deleted from `frames` or the
    // parent set itself is deleted). But a parent that loses its last
    // member through frame deletion is never cleaned up — the
    // `bulk_update_frame_metadata` cascade in `db/operations.rs` does it in
    // application code, but other deletion paths (raw frame delete from
    // file_op cleanup, archive operations, etc.) leave empty parent rows.
    // This trigger handles every path uniformly. DROP + re-create (instead of
    // CREATE IF NOT EXISTS) so databases that already carry an older
    // definition pick up the current WHEN clause on next startup.
    conn.execute("DROP TRIGGER IF EXISTS calibration_set_empty_prune", [])?;
    create_calibration_set_empty_prune_trigger(conn)?;
    // One-shot startup sweep for empty sets that pre-date the trigger, with
    // the same exemptions the trigger applies (see helper doc).
    conn.execute(
        "DELETE FROM calibration_set
         WHERE COALESCE(is_master_library, 0) = 0
           AND superseded_by_set_id IS NULL
           AND id NOT IN (SELECT source_set_id FROM master_provenance
                           WHERE source_set_id IS NOT NULL)
           AND NOT EXISTS (
            SELECT 1 FROM calibration_set_frames
             WHERE set_id = calibration_set.id
         )",
        [],
    )?;

    // archive_operations: frames_set_id nullable + calibration_set_id.
    // SQLite can't drop NOT NULL via ALTER — rebuild once, detected by the
    // absence of the calibration_set_id column.
    if !column_exists(conn, "archive_operations", "calibration_set_id")? {
        // FK enforcement MUST be disabled around the rebuild. With
        // foreign_keys=ON (this codebase's connection default — rusqlite's
        // bundled sqlite3 is compiled with SQLITE_DEFAULT_FOREIGN_KEYS=1),
        // `DROP TABLE archive_operations` performs an implicit DELETE of
        // every row before dropping, which fires the ON DELETE CASCADE on
        // archive_operation_files.operation_id and
        // archive_operation_steps.operation_id — silently wiping every
        // operation's file manifest and audit log while the copied parent
        // rows survive. This is step 1 of the official SQLite 12-step ALTER
        // recipe. The pragma is a no-op inside an open transaction, so it
        // must be issued here, OUTSIDE the BEGIN/COMMIT batch below.
        conn.pragma_update(None, "foreign_keys", false)?;
        let rebuild = conn.execute_batch(
            "BEGIN;
             CREATE TABLE archive_operations_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                frames_set_id INTEGER,
                calibration_set_id INTEGER,
                archive_root_path TEXT NOT NULL,
                flats_disposition TEXT,
                darks_disposition TEXT,
                bias_disposition TEXT,
                darkflats_disposition TEXT,
                compression TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                error_message TEXT,
                FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE,
                FOREIGN KEY (calibration_set_id) REFERENCES calibration_set(id) ON DELETE CASCADE
             );
             INSERT INTO archive_operations_new
                (id, frames_set_id, archive_root_path, flats_disposition, darks_disposition,
                 bias_disposition, darkflats_disposition, compression, status, started_at,
                 finished_at, error_message)
                SELECT id, frames_set_id, archive_root_path, flats_disposition, darks_disposition,
                       bias_disposition, darkflats_disposition, compression, status, started_at,
                       finished_at, error_message
                FROM archive_operations;
             -- Carry forward the AUTOINCREMENT high-water mark. The INSERT above
             -- only seeds archive_operations_new's sqlite_sequence row from the
             -- ids it actually copied — if a higher id was ever deleted from the
             -- old table (e.g. via the frames_set(id) ON DELETE CASCADE FK, which
             -- fires in normal use), that history lives only in the OLD table's
             -- sqlite_sequence row and would otherwise be lost, letting a future
             -- insert reuse a retired id. Raise the new row to match, or seed it
             -- from scratch if the table was emptied entirely before rebuild.
             UPDATE sqlite_sequence
                SET seq = COALESCE((SELECT seq FROM sqlite_sequence WHERE name='archive_operations'), 0)
              WHERE name='archive_operations_new'
                AND seq < COALESCE((SELECT seq FROM sqlite_sequence WHERE name='archive_operations'), 0);
             INSERT INTO sqlite_sequence (name, seq)
                SELECT 'archive_operations_new', seq FROM sqlite_sequence
                 WHERE name='archive_operations'
                   AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name='archive_operations_new');
             DROP TABLE archive_operations;
             ALTER TABLE archive_operations_new RENAME TO archive_operations;
             COMMIT;",
        );
        // Re-enable FK enforcement whether or not the rebuild succeeded, then
        // surface the rebuild error (if any) — never leave the connection
        // running unenforced.
        let re_enable = conn.pragma_update(None, "foreign_keys", true);
        rebuild?;
        re_enable?;
        // Step 10 of the 12-step recipe: verify the rebuild didn't break FK
        // integrity (e.g. children left orphaned, or copied rows referencing
        // missing parents). Scoped to the three tables the rebuild touches —
        // FK constraints live on the child side, so checking these covers
        // both directions — rather than a whole-DB check, so an unrelated
        // pre-existing violation elsewhere can't brick catalog startup.
        for table in ["archive_operations", "archive_operation_files", "archive_operation_steps"] {
            let violations: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_foreign_key_check('{table}')"),
                [],
                |r| r.get(0),
            )?;
            if violations > 0 {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(format!(
                        "archive_operations rebuild broke foreign-key integrity: \
                         {violations} violation(s) reported by foreign_key_check on {table}"
                    )),
                ));
            }
        }
        // Recreate the two indexes the rebuild dropped
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_status ON archive_operations(status)", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_archive_ops_frames_set ON archive_operations(frames_set_id)", [])?;
    }
    // calibration_set_id is guaranteed to exist by this point on both a
    // freshly-created database (present in the CREATE TABLE from birth) and a
    // migrated legacy one (just added by the rebuild above) — safe to create
    // unconditionally here, unlike in the early index section further up.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_archive_ops_calibration_set ON archive_operations(calibration_set_id)",
        [],
    )?;

    // ---- Collaboration Stage 1: catalog identity (Phase 1) ----
    conn.execute(
        "CREATE TABLE IF NOT EXISTS catalog_meta (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            catalog_uuid TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            created_at TEXT NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO catalog_meta (id, catalog_uuid, schema_version, created_at)
         VALUES (1, ?1, 1, ?2)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;

    // Personal-sync tables (Stage I). The DDL text is owned by `sync::store`
    // (single source, shared with the standalone Perseus store) — this call
    // just materialises those tables in the app catalog so the primary-side
    // receiver (task A7) can record outbound state, transfer history, and
    // per-frame receipts alongside the catalog it ingests into.
    {
        use crate::sync::store::{
            DDL_HISTORY, DDL_INDEXES, DDL_OUTBOUND, DDL_RECEIPTS, DDL_SYNC_SOURCES,
        };
        conn.execute(DDL_OUTBOUND, [])?;
        conn.execute(DDL_HISTORY, [])?;
        conn.execute(DDL_RECEIPTS, [])?;
        conn.execute(DDL_SYNC_SOURCES, [])?;
        for idx in DDL_INDEXES {
            conn.execute(idx, [])?;
        }
    }

    // Add uuid and updated_at columns to the 7 entity tables
    for t in UUID_TABLES {
        if !column_exists(conn, t, "uuid")? {
            conn.execute(&format!("ALTER TABLE {t} ADD COLUMN uuid TEXT"), [])?;
        }
        if !column_exists(conn, t, "updated_at")? {
            conn.execute(&format!("ALTER TABLE {t} ADD COLUMN updated_at TEXT"), [])?;
        }
    }
    backfill_identity(conn)?;
    for t in UUID_TABLES {
        conn.execute(
            &format!("CREATE UNIQUE INDEX IF NOT EXISTS idx_{t}_uuid ON {t}(uuid)"),
            [],
        )?;
    }

    // Create identity and touch triggers for all UUID tables
    const UUID_V4_SQL: &str = "lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || \
        substr(hex(randomblob(2)),2) || '-' || substr('89ab', abs(random()) % 4 + 1, 1) || \
        substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6)))";
    // NOTE: trigger DDL is DROP+CREATE (not CREATE ... IF NOT EXISTS) so it is
    // authoritative on every init_db run. This is the trigger-evolution
    // mechanism: dev/test DBs created under an older trigger definition get
    // upgraded in place instead of ossifying with stale trigger bodies.
    for t in UUID_TABLES {
        conn.execute(&format!("DROP TRIGGER IF EXISTS {t}_identity"), [])?;
        conn.execute(
            &format!(
                "CREATE TRIGGER {t}_identity AFTER INSERT ON {t}
                 FOR EACH ROW WHEN NEW.uuid IS NULL OR NEW.updated_at IS NULL
                 BEGIN
                     UPDATE {t} SET uuid = COALESCE(NEW.uuid, {UUID_V4_SQL}),
                         updated_at = COALESCE(NEW.updated_at, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     WHERE id = NEW.id;
                 END"
            ),
            [],
        )?;
        conn.execute(&format!("DROP TRIGGER IF EXISTS {t}_touch"), [])?;
        conn.execute(
            &format!(
                "CREATE TRIGGER {t}_touch AFTER UPDATE ON {t}
                 FOR EACH ROW WHEN NEW.updated_at IS OLD.updated_at AND NEW.uuid IS OLD.uuid
                 BEGIN
                     UPDATE {t} SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE id = NEW.id;
                 END"
            ),
            [],
        )?;
    }

    // ---- Stage II collaboration (slice 3): project poll cache + local links ----
    // Poll cache of my collaboration projects, one row per project. Holds the RAW
    // signed snapshot (payload+signature, base64) so slice-4's project
    // PeerAuthorizer can re-verify offline. Refreshed wholesale on each poll.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS collab_projects (
            project_id             TEXT PRIMARY KEY,
            slug                   TEXT NOT NULL,
            title                  TEXT NOT NULL,
            data_role              TEXT NOT NULL,
            is_coordinator         INTEGER NOT NULL DEFAULT 0,
            require_approval       INTEGER NOT NULL DEFAULT 0,
            pending_announcements  INTEGER NOT NULL DEFAULT 0,
            project_status         TEXT NOT NULL DEFAULT 'active',
            target_name            TEXT NOT NULL,
            target_ra_deg          REAL NOT NULL,
            target_dec_deg         REAL NOT NULL,
            target_radius_deg      REAL NOT NULL,
            membership_version     INTEGER NOT NULL,
            snapshot_payload_b64   TEXT NOT NULL,
            snapshot_signature_b64 TEXT NOT NULL,
            members_json           TEXT NOT NULL,
            thresholds_version     INTEGER,
            thresholds_rules_json  TEXT,
            fetched_at             TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;

    // Local project↔frame-set links. NEVER sent to the hub (spec §7).
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_links (
            project_id    TEXT    NOT NULL,
            frames_set_id INTEGER NOT NULL,
            created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (project_id, frames_set_id),
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // "Publish as project" deep-link intents: when the portal /new form was
    // prefilled from this set, the next poll auto-links the newly appeared
    // project whose target matches (spec §8 "source set auto-linked").
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_link_intents (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            frames_set_id INTEGER NOT NULL,
            ra_deg        REAL    NOT NULL,
            dec_deg       REAL    NOT NULL,
            created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // ---- Stage II collaboration (slice 4): known packages + received contributions ----
    // Every collaboration package I know about — mine, received, or merely seen in
    // the hub list. Mirrors the hub-anchored decision state (`state`) alongside
    // local-only progress (`local_status`, `local_dir`, `manifest_ndjson`). The
    // retained `manifest_ndjson` bytes (mine + fully-received) let me re-serve a
    // package byte-identically (Д2) without re-deriving the manifest.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_packages (
            package_id      TEXT PRIMARY KEY,
            project_id      TEXT NOT NULL,
            announcement_id TEXT NOT NULL UNIQUE,
            publisher_display TEXT NOT NULL,
            own             INTEGER NOT NULL DEFAULT 0,
            root_hash       TEXT NOT NULL,
            byte_size       INTEGER NOT NULL,
            frame_count     INTEGER NOT NULL,
            manifest_xxh3   TEXT,
            aggregate_stats TEXT NOT NULL DEFAULT '{}',
            supersedes      TEXT NOT NULL DEFAULT '[]',
            state           TEXT NOT NULL,
            reject_reason   TEXT,
            superseded      INTEGER NOT NULL DEFAULT 0,
            origin          TEXT NOT NULL,
            local_dir       TEXT,
            manifest_ndjson BLOB,
            local_status    TEXT NOT NULL DEFAULT 'none',
            holder_count    INTEGER NOT NULL DEFAULT 0,
            online_count    INTEGER NOT NULL DEFAULT 0,
            created_at      TEXT NOT NULL,
            decided_at      TEXT,
            fetched_at      TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_project_packages_project ON project_packages(project_id)",
        [],
    )?;

    // Received frames (contributions) — these NEVER enter `files`/`frames`; they
    // land under a managed per-project root and are tracked here only. Cascades
    // away with its parent package.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_contributions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id      TEXT NOT NULL,
            package_id      TEXT NOT NULL REFERENCES project_packages(package_id) ON DELETE CASCADE,
            frame_uuid      TEXT NOT NULL,
            publisher_display TEXT NOT NULL,
            rel_path        TEXT NOT NULL,
            landed_path     TEXT NOT NULL UNIQUE,
            byte_size       INTEGER NOT NULL,
            xxh3            TEXT NOT NULL,
            frame_meta      TEXT NOT NULL DEFAULT '{}',
            analysis        TEXT,
            superseded      INTEGER NOT NULL DEFAULT 0,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_project_contributions_project ON project_contributions(project_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_project_contributions_uuid ON project_contributions(frame_uuid)",
        [],
    )?;

    Ok(())
}

#[cfg(test)]
mod identity_schema_tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn catalog_meta_seeded_once_and_stable() {
        let conn = mem_db();
        let (uuid1, ver): (String, i64) = conn
            .query_row("SELECT catalog_uuid, schema_version FROM catalog_meta WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ver, 1);
        assert_eq!(uuid1.len(), 36, "catalog_uuid must be a hyphenated UUID");
        // re-running init_db must NOT regenerate the uuid
        init_db(&conn).unwrap();
        let uuid2: String = conn
            .query_row("SELECT catalog_uuid FROM catalog_meta WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uuid1, uuid2);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM catalog_meta", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn identity_columns_and_indexes_exist_on_all_seven_tables() {
        let conn = mem_db();
        for t in super::UUID_TABLES {
            for col in ["uuid", "updated_at"] {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                        rusqlite::params![t, col],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{t}.{col} missing");
            }
            let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
                    rusqlite::params![format!("idx_{t}_uuid")],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(idx, 1, "unique index on {t}.uuid missing");
        }
    }

    #[test]
    fn legacy_rows_get_backfilled() {
        // Simulate a legacy catalog: rows inserted while triggers/columns are absent is
        // impossible after init_db, so emulate by clearing uuid on an inserted row and
        // re-running init_db (backfill must repair NULL uuids idempotently).
        let conn = mem_db();
        conn.execute(
            "INSERT INTO tags (name, color) VALUES ('legacy', NULL)", [],
        ).unwrap();
        conn.execute("UPDATE tags SET uuid = NULL, updated_at = NULL", []).unwrap();
        init_db(&conn).unwrap();
        let (u, ts): (Option<String>, Option<String>) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='legacy'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let u = u.expect("uuid backfilled");
        assert_eq!(u.len(), 36);
        assert!(ts.is_some(), "updated_at backfilled");
    }

    fn assert_v4_shape(u: &str) {
        assert_eq!(u.len(), 36);
        let b: Vec<char> = u.chars().collect();
        for i in [8, 13, 18, 23] { assert_eq!(b[i], '-', "dash at {i} in {u}"); }
        assert_eq!(b[14], '4', "version nibble in {u}");
        assert!("89ab".contains(b[19]), "variant nibble in {u}");
    }

    #[test]
    fn insert_trigger_fills_uuid_and_updated_at() {
        let conn = mem_db();
        conn.execute("INSERT INTO tags (name, color) VALUES ('t1', NULL)", []).unwrap();
        let (u, ts): (String, String) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='t1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_v4_shape(&u);
        assert!(ts.ends_with('Z') && ts.contains('T'));
    }

    #[test]
    fn update_trigger_bumps_updated_at_but_respects_explicit_set() {
        let conn = mem_db();
        conn.execute("INSERT INTO tags (name, color) VALUES ('t2', NULL)", []).unwrap();
        let ts0: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        conn.execute("UPDATE tags SET color = 'red' WHERE name='t2'", []).unwrap();
        let ts1: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        assert_ne!(ts0, ts1, "touch trigger must bump updated_at");
        // explicit set wins (future sync import path)
        conn.execute("UPDATE tags SET color='blue', updated_at='2020-01-01T00:00:00.000Z' WHERE name='t2'", []).unwrap();
        let ts2: String = conn.query_row("SELECT updated_at FROM tags WHERE name='t2'", [], |r| r.get(0)).unwrap();
        assert_eq!(ts2, "2020-01-01T00:00:00.000Z");
    }

    #[test]
    fn insert_with_explicit_identity_is_preserved() {
        let conn = mem_db();
        conn.execute(
            "INSERT INTO tags (name, color, uuid, updated_at)
             VALUES ('sync-import', NULL, 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee', '2021-06-01T12:00:00.000Z')",
            [],
        )
        .unwrap();
        let (u, ts): (String, String) = conn
            .query_row(
                "SELECT uuid, updated_at FROM tags WHERE name='sync-import'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(u, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", "identity trigger must not clobber explicit uuid");
        assert_eq!(ts, "2021-06-01T12:00:00.000Z", "identity trigger must preserve explicit updated_at");
    }

    #[test]
    fn insert_with_explicit_updated_at_but_no_uuid_preserves_timestamp() {
        let conn = mem_db();
        conn.execute(
            "INSERT INTO tags (name, color, updated_at) VALUES ('q3', NULL, '2022-03-03T03:03:03.000Z')",
            [],
        ).unwrap();
        let (u, ts): (Option<String>, String) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='q3'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!(u.is_some(), "uuid generated");
        assert_eq!(ts, "2022-03-03T03:03:03.000Z", "explicit updated_at must survive the trigger cascade");
    }

    #[test]
    fn insert_with_explicit_uuid_but_no_updated_at_gets_timestamp() {
        let conn = mem_db();
        conn.execute(
            "INSERT INTO tags (name, color, uuid) VALUES ('q2', NULL, 'bbbbbbbb-cccc-4ddd-8eee-ffffffffffff')",
            [],
        ).unwrap();
        let (u, ts): (String, Option<String>) = conn
            .query_row("SELECT uuid, updated_at FROM tags WHERE name='q2'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(u, "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff");
        assert!(ts.is_some(), "updated_at must be filled at insert, not at next startup");
    }
}

#[cfg(test)]
mod archive_schema_tests {
    use super::*;

    #[test]
    fn test_archive_tables_created() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for table in &["archive_operations", "archive_operation_files", "archive_operation_steps"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected table {} to exist", table);
        }
    }

    #[test]
    fn test_file_op_tables_created() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for table in &["file_operations", "file_operation_files", "file_operation_steps"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected table {} to exist", table);
        }
    }

    #[test]
    fn test_archive_columns_added() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // frames_set columns
        for col in &["archived_at", "archive_operation_id"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('frames_set') WHERE name=?1",
                [col],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected frames_set.{} to exist", col);
        }

        // files columns
        for col in &["archived_in_operation", "archive_zip_path", "archive_path_in_zip"] {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('files') WHERE name=?1",
                [col],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(count, 1, "expected files.{} to exist", col);
        }
    }

    fn insert_dummy_calibration_set(conn: &Connection, id: i64, imagetyp: &str) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (?1, ?2, '2025-01-01')",
            rusqlite::params![id, imagetyp],
        ).unwrap();
    }

    fn insert_subcal_link(conn: &Connection, source_id: i64, target_id: i64, calibration_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'calibration_set', ?2, ?3, '2025-01-01T00:00:00Z')",
            rusqlite::params![source_id, target_id, calibration_type],
        ).unwrap();
    }

    #[test]
    fn deleting_calibration_set_prunes_subcal_links_via_trigger() {
        // A5 regression: deleting a parent calibration_set must remove every
        // sub-calibration link whose source_id pointed at that set, otherwise
        // the user's manually-assigned hierarchy silently disappears.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        insert_dummy_calibration_set(&conn, 100, "Flat");
        insert_dummy_calibration_set(&conn, 200, "Dark");
        insert_subcal_link(&conn, 100, 200, "Dark");

        // Sanity: the link exists.
        let before: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_id=100 AND source_type='calibration_set'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(before, 1, "sub-cal link should be present before delete");

        conn.execute("DELETE FROM calibration_set WHERE id = 100", []).unwrap();

        let after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_id=100 AND source_type='calibration_set'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(after, 0, "trigger must have removed the orphaned sub-cal link");
    }

    fn insert_dummy_frame(conn: &Connection, id: i64) {
        // Minimal frame row for the calibration_set_frames junction.
        // FKs are enforced — populate every NOT NULL column on `files`.
        let file_id: i64 = id + 100_000;
        conn.execute(
            "INSERT OR IGNORE INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![
                file_id,
                format!("/test/dummy_{}.fits", id),
                format!("dummy_{}.fits", id),
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id) VALUES (?1, ?2)",
            rusqlite::params![id, file_id],
        ).unwrap();
    }

    #[test]
    fn deleting_last_member_frame_prunes_empty_calibration_set() {
        // B2 regression: when a calibration_set's last member frame is
        // deleted, the parent set must be removed too. The CASCADE on
        // calibration_set_frames takes care of the junction row; the new
        // trigger handles the parent.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // calibration_set requires `imagetyp` + `date`; nothing else is FK-bound.
        insert_dummy_calibration_set(&conn, 500, "Dark");
        insert_dummy_frame(&conn, 7000);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 7000)",
            [],
        ).unwrap();

        // Sanity: the set is present.
        let before: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 500",
            [], |r| r.get(0)).unwrap();
        assert_eq!(before, 1);

        // Deleting the lone member triggers the prune.
        conn.execute(
            "DELETE FROM calibration_set_frames WHERE set_id = 500 AND frame_id = 7000",
            [],
        ).unwrap();

        let after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 500",
            [], |r| r.get(0)).unwrap();
        assert_eq!(after, 0, "trigger must prune empty parent set");
    }

    #[test]
    fn pruning_does_not_touch_master_library_sets() {
        // Master library sets are intrinsically single-frame. If their member
        // is removed (e.g., during re-import), the parent shouldn't be
        // garbage-collected — the trigger explicitly skips is_master_library=1.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (501, 'MasterDark', '2025-01-01', 1)",
            [],
        ).unwrap();
        insert_dummy_frame(&conn, 7001);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (501, 7001)",
            [],
        ).unwrap();

        conn.execute(
            "DELETE FROM calibration_set_frames WHERE set_id = 501",
            [],
        ).unwrap();

        let still_there: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 501",
            [], |r| r.get(0)).unwrap();
        assert_eq!(still_there, 1, "master library sets must survive losing their member");
    }

    #[test]
    fn startup_sweep_prunes_existing_empty_calibration_sets() {
        // B2 startup migration: any empty non-master sets that pre-date the
        // trigger should be cleaned at next init_db.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert an empty non-master set bypassing the trigger by simply not
        // adding any membership rows.
        insert_dummy_calibration_set(&conn, 502, "Bias");

        let before: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 502",
            [], |r| r.get(0)).unwrap();
        assert_eq!(before, 1);

        // Re-run init_db to fire the sweep.
        init_db(&conn).unwrap();

        let after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 502",
            [], |r| r.get(0)).unwrap();
        assert_eq!(after, 0, "startup sweep should remove the empty set");
    }

    /// Superseded raw set (frozen provenance history) losing its last member
    /// — e.g. the raw files get deleted via file_op — must survive the prune:
    /// `master_provenance.source_set_id` references it with an FK that has no
    /// ON DELETE action, so pre-hardening the trigger's DELETE aborted the
    /// caller's own statement with "FOREIGN KEY constraint failed".
    fn seed_superseded_set(conn: &Connection, raw_id: i64, master_id: i64, frame_id: i64) {
        insert_dummy_calibration_set(conn, raw_id, "Dark");
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, 'MasterDark', '2025-01-01', 1)",
            [master_id],
        ).unwrap();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            rusqlite::params![master_id, raw_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'hash', '2025-01-01T00:00:00Z')",
            rusqlite::params![master_id, raw_id],
        ).unwrap();
        insert_dummy_frame(conn, frame_id);
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![raw_id, frame_id],
        ).unwrap();
    }

    #[test]
    fn empty_prune_trigger_spares_provenance_referenced_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_superseded_set(&conn, 510, 511, 7010);

        // Must neither FK-error nor prune the frozen set.
        conn.execute("DELETE FROM calibration_set_frames WHERE set_id = 510", []).unwrap();

        let still_there: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 510",
            [], |r| r.get(0)).unwrap();
        assert_eq!(still_there, 1, "superseded set must survive losing its members");
    }

    #[test]
    fn startup_sweep_spares_empty_superseded_sets() {
        // Same guard for the init_db bulk sweep: an already-empty superseded
        // set must not be swept (pre-hardening this made init_db itself fail
        // with a FK error → app refused to start).
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_superseded_set(&conn, 512, 513, 7011);
        conn.execute("DELETE FROM calibration_set_frames WHERE set_id = 512", []).unwrap();

        init_db(&conn).unwrap();

        let still_there: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set WHERE id = 512",
            [], |r| r.get(0)).unwrap();
        assert_eq!(still_there, 1, "startup sweep must spare superseded sets");
    }

    #[test]
    fn startup_orphan_sweep_cleans_existing_dangling_subcal_links() {
        // A5 startup migration: if an older DB already has orphan rows (e.g.,
        // from a delete that happened before the trigger existed), running
        // init_db should remove them.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Manually insert an orphan: a sub-cal link where source_id has no
        // matching calibration_set row.
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (999, 'calibration_set', 200, 'Bias', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap_err();
        // The line above fails because calibration_set_id=200 doesn't exist
        // (FK CASCADE requires it). Set up a valid target first.
        insert_dummy_calibration_set(&conn, 200, "Bias");
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (999, 'calibration_set', 200, 'Bias', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let orphans_before: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames
             WHERE source_type='calibration_set'
               AND source_id NOT IN (SELECT id FROM calibration_set)",
            [], |r| r.get(0)).unwrap();
        assert_eq!(orphans_before, 1, "test set up an orphan to be cleaned");

        // Re-run init_db (idempotent) to trigger the sweep.
        init_db(&conn).unwrap();

        let orphans_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames
             WHERE source_type='calibration_set'
               AND source_id NOT IN (SELECT id FROM calibration_set)",
            [], |r| r.get(0)).unwrap();
        assert_eq!(orphans_after, 0, "init_db should have swept orphans away");
    }

    #[test]
    fn scan_roots_kind_column_and_default() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/a')", []).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE path='/data/a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "normal");
    }

    #[test]
    fn scan_roots_kind_migrates_existing_table() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulate a legacy scan_roots without kind
        conn.execute(
            "CREATE TABLE scan_roots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                enabled INTEGER NOT NULL DEFAULT 1,
                find_duplicates INTEGER NOT NULL DEFAULT 1,
                unique_camera INTEGER NOT NULL DEFAULT 0,
                last_scan TEXT
            )",
            [],
        ).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/old')", []).unwrap();
        init_db(&conn).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE path='/old'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "normal");
    }

    #[test]
    fn calibration_set_superseded_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')", [],
        ).unwrap();
        let v: Option<i64> = conn
            .query_row("SELECT superseded_by_set_id FROM calibration_set WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn master_provenance_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library) VALUES ('MasterDark','2026-01-01',1)", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO master_provenance (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (1, NULL, '{}', '[]', 'abc', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM master_provenance", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn archive_operations_accepts_calibration_subject() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // calibration_set_id carries a real FK to calibration_set(id); this
        // connection enforces foreign keys (rusqlite's bundled sqlite3 defaults
        // `PRAGMA foreign_keys = ON`), so row 42 must exist before it can be
        // referenced. Mirrors the frames_set fixture insert in the sibling
        // rebuild test below.
        insert_dummy_calibration_set(&conn, 42, "MasterDark");
        // NULL frames_set_id + calibration_set_id must be insertable
        conn.execute(
            "INSERT INTO archive_operations
             (frames_set_id, calibration_set_id, archive_root_path, compression, status, started_at)
             VALUES (NULL, 42, '/arch', 'store', 'planning', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let (fs, cs): (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT frames_set_id, calibration_set_id FROM archive_operations WHERE id=1",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((fs, cs), (None, Some(42)));
    }

    #[test]
    fn archive_operations_rebuild_preserves_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO frames_set (name) VALUES ('M31')", [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_operations
             (frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (1, '/arch', 'store', 'completed', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        // Re-running init_db (idempotent) must keep the row intact
        init_db(&conn).unwrap();
        let fs: Option<i64> = conn
            .query_row("SELECT frames_set_id FROM archive_operations WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fs, Some(1));
    }

    // Not one of the brief's 6 given tests — added during self-review. The
    // legacy-shape rebuild's `INSERT ... SELECT` only seeds the new table's
    // AUTOINCREMENT counter from ids that still exist; it silently drops any
    // higher id the OLD table's sqlite_sequence remembered from a row that
    // was deleted before the rebuild ran (which happens in real use whenever
    // a `frames_set` row is deleted — archive_operations.frames_set_id
    // cascades). Without the sqlite_sequence carry-forward fix, a
    // post-rebuild insert would silently reuse a retired id.
    #[test]
    fn archive_operations_rebuild_preserves_autoincrement_high_water_mark() {
        let conn = Connection::open_in_memory().unwrap();
        // Manually build the pre-migration (legacy) shape so init_db must take
        // the rebuild path rather than skip it (the guard column is absent).
        conn.execute(
            "CREATE TABLE frames_set (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT,
                is_custom INTEGER NOT NULL DEFAULT 0,
                date_obs_start TEXT,
                date_obs_end TEXT,
                objctra TEXT,
                objctdec TEXT,
                total_exp_time REAL
            )",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE archive_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                frames_set_id INTEGER NOT NULL,
                archive_root_path TEXT NOT NULL,
                flats_disposition TEXT,
                darks_disposition TEXT,
                bias_disposition TEXT,
                darkflats_disposition TEXT,
                compression TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                error_message TEXT,
                FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
            )",
            [],
        ).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO archive_operations (id, frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (1, 1, '/arch1', 'store', 'completed', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_operations (id, frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (5, 1, '/arch2', 'store', 'completed', '2026-01-02T00:00:00Z')",
            [],
        ).unwrap();
        // Retire id 5 (e.g. its frames_set was deleted elsewhere, cascading
        // here) — leaves a gap between the surviving max id (1) and the
        // historical high-water mark (5).
        conn.execute("DELETE FROM archive_operations WHERE id = 5", []).unwrap();

        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO archive_operations (frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (1, '/arch3', 'store', 'planning', '2026-01-03T00:00:00Z')",
            [],
        ).unwrap();
        let new_id: i64 = conn
            .query_row("SELECT id FROM archive_operations WHERE archive_root_path='/arch3'", [], |r| r.get(0))
            .unwrap();
        assert!(new_id > 5, "post-rebuild insert must not reuse a retired id (got {new_id})");
    }

    // Regression pin for the FK cascade wipe (review finding): with
    // foreign_keys=ON — this codebase's connection default, rusqlite's
    // bundled sqlite3 is compiled with SQLITE_DEFAULT_FOREIGN_KEYS=1 — the
    // rebuild's `DROP TABLE archive_operations` performs an implicit DELETE
    // of every row before dropping, which fires the ON DELETE CASCADE on
    // archive_operation_files.operation_id and
    // archive_operation_steps.operation_id. On a real catalog with archive
    // history that silently and irrecoverably wipes every operation's file
    // manifest and audit log while the copied parent rows survive. The
    // rebuild must therefore run with FK enforcement disabled (the official
    // SQLite 12-step ALTER recipe's step 1).
    #[test]
    fn archive_operations_rebuild_preserves_child_rows() {
        let conn = Connection::open_in_memory().unwrap();
        // Manufacture the legacy (pre-migration) schema by hand so init_db
        // takes the rebuild path: the FK parent (frames_set), the legacy
        // archive_operations shape, and both child tables.
        conn.execute(
            "CREATE TABLE frames_set (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT,
                is_custom INTEGER NOT NULL DEFAULT 0,
                date_obs_start TEXT,
                date_obs_end TEXT,
                objctra TEXT,
                objctdec TEXT,
                total_exp_time REAL
            )",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE archive_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                frames_set_id INTEGER NOT NULL,
                archive_root_path TEXT NOT NULL,
                flats_disposition TEXT,
                darks_disposition TEXT,
                bias_disposition TEXT,
                darkflats_disposition TEXT,
                compression TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                error_message TEXT,
                FOREIGN KEY (frames_set_id) REFERENCES frames_set(id) ON DELETE CASCADE
            )",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE archive_operation_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id INTEGER NOT NULL,
                file_id INTEGER,
                source_path TEXT NOT NULL,
                target_zip_path TEXT NOT NULL,
                target_path_in_zip TEXT NOT NULL,
                expected_hash TEXT NOT NULL,
                disposition TEXT NOT NULL,
                frame_role TEXT NOT NULL,
                file_size_bytes INTEGER NOT NULL,
                FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE
            )",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE archive_operation_steps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id INTEGER NOT NULL,
                operation_file_id INTEGER,
                stage TEXT NOT NULL,
                status TEXT NOT NULL,
                actual_hash TEXT,
                error_message TEXT,
                started_at TEXT,
                completed_at TEXT,
                FOREIGN KEY (operation_id) REFERENCES archive_operations(id) ON DELETE CASCADE,
                FOREIGN KEY (operation_file_id) REFERENCES archive_operation_files(id) ON DELETE CASCADE
            )",
            [],
        ).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO archive_operations (id, frames_set_id, archive_root_path, compression, status, started_at)
             VALUES (1, 1, '/arch', 'store', 'completed', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_operation_files
             (id, operation_id, source_path, target_zip_path, target_path_in_zip,
              expected_hash, disposition, frame_role, file_size_bytes)
             VALUES (1, 1, '/data/L1.fits', '/arch/M31.zip', 'L1.fits', 'aabb', 'archive', 'light', 1024)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_operation_steps (id, operation_id, operation_file_id, stage, status)
             VALUES (1, 1, 1, 'copy', 'completed')",
            [],
        ).unwrap();

        // Trigger the rebuild.
        init_db(&conn).unwrap();

        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_operation_files WHERE operation_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "rebuild must not cascade-wipe the operation's file manifest");
        let steps: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_operation_steps WHERE operation_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(steps, 1, "rebuild must not cascade-wipe the operation's audit log");
        // And the whole parent→children JOIN chain must still resolve.
        let joined: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_operations ao
             JOIN archive_operation_files aof ON aof.operation_id = ao.id
             JOIN archive_operation_steps aos ON aos.operation_id = ao.id
             WHERE ao.id = 1",
            [], |r| r.get(0)).unwrap();
        assert_eq!(joined, 1, "parent and both children must survive the rebuild joinable");
    }
}
