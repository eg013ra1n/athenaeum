// Database CRUD operations

use crate::fingerprint::compute_header_fingerprint;
use crate::models::*;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result};

/// Upper bound of the byte range that matches exactly the strings with
/// `prefix` as a literal prefix: `[prefix, upper)`. Returns `None` when no
/// finite upper bound exists — the caller then filters on the lower bound
/// alone (`path >= prefix`). That happens only when:
/// - `prefix` is empty (no upper bound can express "everything");
/// - every char in `prefix` is `char::MAX` (the carry runs off the front) —
///   unreachable for a real filesystem path, but handled for defensive
///   correctness. When this fires for a non-empty prefix, it's logged via
///   `tracing::warn!` so the (lower-bound-only) fallback is observable rather
///   than silently swallowed.
///
/// All stored paths are valid UTF-8 (they come from Rust `String`s), and
/// SQLite's default `TEXT` collation (`BINARY`) compares by `memcmp` on the
/// UTF-8 bytes. For a prefix `P` ending in char `c`, replacing the last char
/// with `succ(c)` (its Unicode scalar successor, skipping the UTF-16
/// surrogate gap `D800..=DFFF` — `char` can never itself be a surrogate, so
/// skipping keeps `succ` total over all `char`s below `char::MAX`) yields a
/// string that is byte-wise greater than every valid-UTF-8 string with
/// prefix `P`, and no valid-UTF-8 string sorts strictly between them: any
/// such string would need to differ from `P` before `succ(c)`'s position
/// while still comparing above `P ++ c`, which requires an invalid
/// continuation-byte sequence. This is why the fix operates on `char`s (via
/// `prefix.chars()`), not raw bytes — incrementing the *last byte* of a
/// multi-byte UTF-8 char (the previous implementation) can land on an
/// invalid byte sequence (any char whose last encoded byte is `0xBF`, e.g.
/// `ÿ`/`¿`/Cyrillic `п`) and silently degrade to the unbounded
/// lower-bound-only fallback.
///
/// Unlike `LIKE`, this predicate is exact-case and treats every character
/// (including `%`/`_`) literally.
pub(crate) fn path_prefix_upper(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(&last) = chars.last() {
        // Successor of the last char, skipping the surrogate gap (chars
        // themselves are never surrogates, but the scalar value right after
        // 0xD7FF is, so jump straight to 0xE000).
        let mut next = last as u32 + 1;
        if (0xD800..=0xDFFF).contains(&next) {
            next = 0xE000;
        }
        match char::from_u32(next) {
            Some(c) => {
                *chars.last_mut().unwrap() = c;
                return Some(chars.into_iter().collect());
            }
            None => {
                // last char was char::MAX — no successor; carry into the
                // previous char instead (drop this one and retry).
                chars.pop();
            }
        }
    }
    if !prefix.is_empty() {
        tracing::warn!(
            prefix = %prefix,
            "no finite upper bound for prefix (all chars are char::MAX), falling back to lower-bound-only match"
        );
    }
    None
}

/// The native separator a stored catalog path uses. Catalog paths are always
/// absolute native paths (WalkDir + canonicalized roots): POSIX ones start
/// with `/`, Windows ones with a drive letter or UNC `\\` — the leading
/// character decides, not the build OS, which keeps the predicate builders
/// testable with Windows-shaped fixtures from any host.
pub(crate) fn native_separator_of(path: &str) -> char {
    if path.starts_with('/') {
        '/'
    } else {
        '\\'
    }
}

/// Fold '/'-spelled separators in a user/tool-supplied Windows path to the
/// native '\' form before it participates in catalog-path comparisons — the
/// filesystem accepts `C:/Astro/Old` but every stored row spells it
/// `C:\Astro\Old`, so a '/'-spelled input silently updated zero rows.
/// Lossless on Windows (filenames cannot contain '/' or '\'); the identity on
/// POSIX hosts ('\' is a legal filename char there — never rewrite it).
pub fn normalize_separators(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_string()
    }
}

/// Build a `(col >= ? AND (? IS NULL OR col < ?)) OR ...` SQL fragment that
/// matches any row whose `column` falls under one of `roots` as an
/// exact-case byte-range prefix, plus the values to bind (three per root, in
/// placeholder order — pass them to `rusqlite::params_from_iter`).
///
/// An empty `roots` list produces the always-false fragment `"0"` with no
/// params: "under one of zero eligible roots" matches nothing, mirroring the
/// original `EXISTS (SELECT 1 FROM scan_roots sr WHERE ...)` semantics when
/// no scan root satisfies the caller's filter.
///
/// Separator-strict, same pattern as `frame_ids_under_paths` and the three
/// destructive sites above: each per-root prefix carries a trailing native
/// separator, so a name-prefix sibling root (`/data/M31` vs `/data/M31_Ha`)
/// never falls inside another root's range. No `files.path` row is ever
/// equal to a root path (roots are directories), so requiring the separator
/// loses nothing legitimate.
///
/// `pub` for the Axum mirrors, which inline this query shape.
pub fn scan_root_prefix_predicate(
    column: &str,
    roots: &[String],
) -> (String, Vec<rusqlite::types::Value>) {
    if roots.is_empty() {
        return ("0".to_string(), Vec::new());
    }
    let mut clauses = Vec::with_capacity(roots.len());
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(roots.len() * 3);
    for root in roots {
        let sep = native_separator_of(root);
        let prefix = format!("{}{}", root.trim_end_matches(sep), sep);
        clauses.push(format!(
            "({col} >= ? AND (? IS NULL OR {col} < ?))",
            col = column
        ));
        values.push(rusqlite::types::Value::Text(prefix.clone()));
        let hi = path_prefix_upper(&prefix)
            .map(rusqlite::types::Value::Text)
            .unwrap_or(rusqlite::types::Value::Null);
        values.push(hi.clone());
        values.push(hi);
    }
    (clauses.join(" OR "), values)
}

/// RAII savepoint: nest-safe transaction scope over a shared `&Connection`.
/// `rusqlite::Connection::savepoint()` needs `&mut Connection`, which the
/// pool only ever lends out as `&Connection` — so nest-safety here is done
/// by hand with `SAVEPOINT` / `RELEASE` / `ROLLBACK TO` (same reasoning as
/// `scanner::reparse_and_update_in_place`, see `scanner/mod.rs:619-630`).
/// Unlike raw `BEGIN`, `SAVEPOINT` nests inside an already-open transaction
/// instead of erroring with "cannot start a transaction within a
/// transaction", and behaves like a normal transaction when none is open.
///
/// Drop without calling `commit()` ⇒ `ROLLBACK TO` + `RELEASE`, so the
/// savepoint's changes are undone on any early return (`?`) or panic-unwind,
/// not just the explicit error paths.
pub(crate) struct SavepointGuard<'c> {
    conn: &'c Connection,
    name: &'static str,
    done: bool,
}

impl<'c> SavepointGuard<'c> {
    pub(crate) fn new(conn: &'c Connection, name: &'static str) -> rusqlite::Result<Self> {
        conn.execute_batch(&format!("SAVEPOINT {name}"))?;
        Ok(Self {
            conn,
            name,
            done: false,
        })
    }

    /// Persist the savepoint's changes (`RELEASE`). Consumes `self` so the
    /// subsequent `Drop` sees `done == true` and is a no-op.
    pub(crate) fn commit(mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch(&format!("RELEASE {}", self.name))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for SavepointGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            // Never swallow errors: if the rollback itself fails, log it
            // rather than silently leaving the savepoint open.
            if let Err(e) = self
                .conn
                .execute_batch(&format!("ROLLBACK TO {0}; RELEASE {0}", self.name))
            {
                tracing::error!(savepoint = self.name, error = %e, "SavepointGuard rollback failed");
            }
        }
    }
}

/// Parse a stored RFC3339 timestamp defensively: a malformed value logs a
/// warning and falls back to the UNIX epoch instead of panicking every read
/// of that row (2026-08-03 audit I6 — `frames.date_obs` was already parsed
/// defensively; `files.modified_at`/`created_at` were not, so one bad string
/// took down every listing, search and metadata read that touched the row).
pub(crate) fn parse_stored_ts(field: &'static str, raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            tracing::warn!(field, raw, error = %e, "malformed stored timestamp; substituting epoch");
            DateTime::<Utc>::UNIX_EPOCH
        })
}

/// Insert a new file record
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_file(conn: &Connection, file: &File) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at, metadata_hash, content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    stmt.execute(params![
        file.path,
        file.filename,
        file.size,
        file.modified_at.to_rfc3339(),
        format!("{:?}", file.format),
        file.created_at.to_rfc3339(),
        file.metadata_hash,
        file.content_hash,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Insert FITS header with computed fingerprint
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_fits_header(conn: &Connection, file_id: i64, header: &str) -> Result<i64> {
    let fingerprint = compute_header_fingerprint(header);
    let mut stmt = conn.prepare_cached(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
    )?;
    stmt.execute(params![file_id, header, fingerprint])?;
    Ok(conn.last_insert_rowid())
}

/// Fill `header_fingerprint` for any `fits_header` rows still missing it
/// (legacy rows created before fingerprinting existed). Idempotent — a no-op
/// once every row has a fingerprint. Returns the number of rows updated.
pub fn backfill_null_header_fingerprints(conn: &Connection) -> Result<usize> {
    let rows: Vec<(i64, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, header FROM fits_header WHERE header_fingerprint IS NULL")?;
        let collected = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>>>()?;
        collected
    };
    for (id, header) in &rows {
        conn.execute(
            "UPDATE fits_header SET header_fingerprint = ?1 WHERE id = ?2",
            params![compute_header_fingerprint(header), id],
        )?;
    }
    Ok(rows.len())
}

/// Insert a new frame record
/// Uses prepare_cached() for better performance during bulk inserts
pub fn insert_frame(conn: &Connection, frame: &Frame) -> Result<i64> {
    let imagetyp_str = frame.imagetyp.as_ref().map(|t| format!("{:?}", t));
    let date_obs_str = frame.date_obs.as_ref().map(|d| d.to_rfc3339());
    let override_int = if frame.override_ { 1 } else { 0 };

    tracing::debug!(
        file_id = frame.file_id,
        object = ?frame.object,
        date_obs = ?date_obs_str,
        "inserting frame"
    );

    let is_master_int = if frame.is_master { 1 } else { 0 };

    let mut stmt = conn.prepare_cached(
        "INSERT INTO frames (file_id, object, date_obs, telescop, instrume, exptime, filter, imagetyp, is_master,
         gain, offset, binning, xbinning, ybinning, ccd_temp, set_temp, focallen, xpixsz, ypixsz,
         naxis1, naxis2, ra, dec, sitelat, lat_obs, sitelong, long_obs, objctra, objctdec, override, swcreate, bayerpat, rotation,
         xbayroff, ybayroff, roworder)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
         ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36)",
    )?;
    stmt.execute(params![
        frame.file_id,
        frame.object,
        date_obs_str,
        frame.telescop,
        frame.instrume,
        frame.exptime,
        frame.filter,
        imagetyp_str,
        is_master_int,
        frame.gain,
        frame.offset,
        frame.binning,
        frame.xbinning,
        frame.ybinning,
        frame.ccd_temp,
        frame.set_temp,
        frame.focallen,
        frame.xpixsz,
        frame.ypixsz,
        frame.naxis1,
        frame.naxis2,
        frame.ra,
        frame.dec,
        frame.sitelat,
        frame.lat_obs,
        frame.sitelong,
        frame.long_obs,
        frame.objctra,
        frame.objctdec,
        override_int,
        frame.swcreate,
        frame.bayerpat,
        frame.rotation,
        frame.xbayroff,
        frame.ybayroff,
        frame.roworder,
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Get all scan roots
pub fn get_scan_roots(conn: &Connection) -> Result<Vec<ScanRoot>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, enabled, find_duplicates, unique_camera, last_scan, last_scan_errors, monitor_enabled, kind FROM scan_roots ORDER BY path"
    )?;

    let roots = stmt.query_map([], |row| {
        let last_scan_str: Option<String> = row.get(5)?;
        let last_scan = last_scan_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let last_scan_errors_str: Option<String> = row.get(6)?;
        let last_scan_errors =
            last_scan_errors_str.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok());

        Ok(ScanRoot {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            enabled: row.get::<_, i32>(2)? == 1,
            find_duplicates: row.get::<_, i32>(3)? == 1,
            unique_camera: row.get::<_, i32>(4)? == 1,
            last_scan,
            last_scan_errors,
            monitor_enabled: row.get::<_, i32>(7)? == 1,
            kind: row.get(8)?,
        })
    })?;

    roots.collect()
}

/// Set monitor_enabled flag for a scan root
pub fn set_scan_root_monitor_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET monitor_enabled = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
    )?;
    Ok(())
}

/// Insert or update a scan root. `kind` applies only on INSERT — an existing
/// row's kind is never silently changed by re-adding the same path.
pub fn upsert_scan_root(conn: &Connection, path: &str, kind: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_roots (path, enabled, find_duplicates, kind) VALUES (?1, 1, 1, ?2)
         ON CONFLICT(path) DO NOTHING",
        params![path, kind],
    )?;

    let id: i64 = conn.query_row(
        "SELECT id FROM scan_roots WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;

    Ok(id)
}

/// Count scan roots of a given kind (uniqueness guard for calibration_library).
pub fn count_scan_roots_of_kind(conn: &Connection, kind: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM scan_roots WHERE kind = ?1",
        params![kind],
        |row| row.get(0),
    )
}

/// The path of the (unique) scan root of a given special `kind`, if one is
/// designated. Drives the sync receiver's live per-package landing resolver:
/// `sync_incoming` designated → land there; `None` → caller's app-data fallback.
pub fn scan_root_path_of_kind(conn: &Connection, kind: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT path FROM scan_roots WHERE kind = ?1 LIMIT 1",
        params![kind],
        |row| row.get(0),
    )
    .optional()
}

/// Update scan root find_duplicates flag
pub fn update_scan_root_duplicates_flag(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET find_duplicates = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, id],
    )?;
    Ok(())
}

/// Toggle unique_camera flag with cascade: updates frames.instrume, deletes affected
/// calibration sets, clears calibration links, and updates sessions in a single transaction.
/// Result of reconciling unique_camera instrume suffix state
#[derive(Debug)]
pub struct ReconcileResult {
    pub frames_renamed: usize,
    pub calibration_sets_deleted: usize,
    pub sessions_updated: usize,
}

/// Set unique_camera flag for a scan root (flag-only, no cascade)
pub fn set_unique_camera_flag(conn: &Connection, root_id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE scan_roots SET unique_camera = ?1 WHERE id = ?2",
        params![if enabled { 1 } else { 0 }, root_id],
    )?;
    Ok(())
}

/// Delete calibration sets for frames under a scan root
pub fn delete_calibration_sets_for_root(conn: &Connection, root_id: i64) -> Result<usize> {
    // Get root path
    let root_path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        params![root_id],
        |row| row.get(0),
    )?;

    // Byte-range bounds must carry the trailing separator: without it the
    // range for /lib_old also swallows sibling /lib_old2/* (see the
    // name_prefix_sibling tests). Same pattern as frame_ids_under_paths.
    let sep = native_separator_of(&root_path);
    let root_prefix = format!("{}{}", root_path.trim_end_matches(sep), sep);
    let path_hi = path_prefix_upper(&root_prefix);

    // Find affected calibration set IDs (sets containing frames under this root).
    //
    // The lineage guard mirrors `prune_orphaned_calibration_sets` (db/schema.rs)
    // exemption-for-exemption: master shells, superseded raw sets, and
    // provenance-anchored sets are frozen lineage and must never be swept up
    // here. Both `calibration_set.superseded_by_set_id` and
    // `master_provenance.source_set_id` are NO-ACTION FKs, so deleting one end
    // while the other survives (the real-world shape: raw darks under the
    // toggled imaging root, their master in the Calibration Library root) aborts
    // with "FOREIGN KEY constraint failed" — and since this runs inside
    // `reconcile_unique_camera_instrume`'s savepoint, the instrume rename rolls
    // back too and EVERY subsequent scan of the root fails identically. Where no
    // FK fires (an imported master whose own file lives under the root),
    // `archive_operations.calibration_set_id` (ON DELETE CASCADE) would instead
    // destroy the archive audit rows silently.
    let affected_set_ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT csf.set_id
             FROM calibration_set_frames csf
             JOIN frames fr ON csf.frame_id = fr.id
             JOIN files f ON fr.file_id = f.id
             JOIN calibration_set cs ON cs.id = csf.set_id
             WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
               AND COALESCE(cs.is_master_library, 0) = 0
               AND cs.superseded_by_set_id IS NULL
               AND cs.id NOT IN (SELECT source_set_id FROM master_provenance
                                  WHERE source_set_id IS NOT NULL)",
        )?;
        let rows = stmt.query_map(params![root_prefix, path_hi], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let calibration_sets_deleted = affected_set_ids.len();

    // Explicit cascade delete for affected calibration sets
    if !affected_set_ids.is_empty() {
        let placeholders: String = affected_set_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let set_values: Vec<rusqlite::types::Value> = affected_set_ids
            .iter()
            .map(|id| rusqlite::types::Value::Integer(*id))
            .collect();

        // calibration_set_frames
        let sql = format!(
            "DELETE FROM calibration_set_frames WHERE set_id IN ({})",
            placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_to_frames — as target (calibration_set_id)
        let sql = format!(
            "DELETE FROM calibration_set_to_frames WHERE calibration_set_id IN ({})",
            placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_to_frames — as source (source_type='calibration_set')
        let sql = format!(
            "DELETE FROM calibration_set_to_frames WHERE source_type = 'calibration_set' AND source_id IN ({})",
            placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set_originals
        let sql = format!(
            "DELETE FROM calibration_set_originals WHERE set_id IN ({})",
            placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;

        // calibration_set
        let sql = format!("DELETE FROM calibration_set WHERE id IN ({})", placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(set_values.iter()))?;
    }

    // Clear calibration links for affected light frames (source_type='frame')
    conn.execute(
        "DELETE FROM calibration_set_to_frames
         WHERE source_type = 'frame'
           AND source_id IN (
             SELECT fr.id FROM frames fr
             JOIN files f ON fr.file_id = f.id
             WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
           )",
        params![root_prefix, path_hi],
    )?;

    Ok(calibration_sets_deleted)
}

/// Reconcile unique_camera instrume suffix state for a scan root
/// Idempotent: if frames already match the flag, returns all zeros
pub fn reconcile_unique_camera_instrume(
    conn: &Connection,
    root_id: i64,
) -> Result<ReconcileResult> {
    // Read current flag and path
    let (unique_camera, root_path): (bool, String) = conn.query_row(
        "SELECT unique_camera, path FROM scan_roots WHERE id = ?1",
        params![root_id],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
    )?;

    let suffix = format!(" N{}", root_id);
    // Byte-range bounds must carry the trailing separator: without it the
    // range for /lib_old also swallows sibling /lib_old2/* (see the
    // name_prefix_sibling tests). Same pattern as frame_ids_under_paths.
    let sep = native_separator_of(&root_path);
    let root_prefix = format!("{}{}", root_path.trim_end_matches(sep), sep);
    let path_hi = path_prefix_upper(&root_prefix);

    let sp = SavepointGuard::new(conn, "reconcile_unique_camera")?;

    // Update frames.instrume based on flag state
    // (the `instrume LIKE '%' || ?` checks below match a literal suffix, not
    // a path prefix — out of scope for this fix, left untouched)
    let frames_renamed = if unique_camera {
        // Add suffix — skip NULL instrume and already-suffixed
        conn.execute(
            "UPDATE frames SET instrume = instrume || ?1
             WHERE instrume IS NOT NULL
               AND instrume NOT LIKE '%' || ?1
               AND id IN (
                 SELECT fr.id FROM frames fr
                 JOIN files f ON fr.file_id = f.id
                 WHERE f.path >= ?2 AND (?3 IS NULL OR f.path < ?3)
               )",
            params![suffix, root_prefix, path_hi],
        )?
    } else {
        // Strip suffix — only from frames that have it
        let suffix_len = suffix.len() as i64;
        conn.execute(
            "UPDATE frames SET instrume = SUBSTR(instrume, 1, LENGTH(instrume) - ?1)
             WHERE instrume IS NOT NULL
               AND instrume LIKE '%' || ?2
               AND id IN (
                 SELECT fr.id FROM frames fr
                 JOIN files f ON fr.file_id = f.id
                 WHERE f.path >= ?3 AND (?4 IS NULL OR f.path < ?4)
               )",
            params![suffix_len, suffix, root_prefix, path_hi],
        )?
    };

    let mut calibration_sets_deleted = 0;
    let mut sessions_updated = 0;

    // Only cascade if frames were actually changed
    if frames_renamed > 0 {
        // Delete affected calibration sets
        calibration_sets_deleted = delete_calibration_sets_for_root(conn, root_id)?;

        // Update sessions.instrume to match updated frame values
        sessions_updated = conn.execute(
            "UPDATE sessions SET instrume = (
               SELECT fr.instrume FROM session_members sm
               JOIN frames fr ON sm.frame_id = fr.id
               WHERE sm.session_id = sessions.id AND fr.instrume IS NOT NULL
               LIMIT 1
             )
             WHERE id IN (
               SELECT DISTINCT sm.session_id FROM session_members sm
               JOIN frames fr ON sm.frame_id = fr.id
               JOIN files f ON fr.file_id = f.id
               WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
             )",
            params![root_prefix, path_hi],
        )?;
    }

    sp.commit()?;

    Ok(ReconcileResult {
        frames_renamed,
        calibration_sets_deleted,
        sessions_updated,
    })
}

/// Reconcile the `missing_files` table with a scan's still-missing list.
///
/// Extracted from the (formerly duplicated) Tauri/Axum command bodies so the
/// multi-statement reconcile is atomic: any failure rolls the whole pass
/// back via the savepoint instead of leaking an open raw transaction onto
/// the pooled connection (2026-08-03 audit I2). Semantics unchanged:
/// files absent from `file_ids` lose their 'missing' rows ('ignored' rows
/// are kept), present files get last_checked_at bumped or a fresh row.
pub fn sync_missing_files(conn: &Connection, root_id: i64, file_ids: &[i64]) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let sp = SavepointGuard::new(conn, "sync_missing_files")?;

    if file_ids.is_empty() {
        // All files are present — drop every 'missing' row for this root.
        conn.execute(
            "DELETE FROM missing_files WHERE scan_root_id = ?1 AND status = 'missing'",
            [root_id],
        )?;
    } else {
        // Files that are no longer missing lose their row; 'ignored' rows stay.
        let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let delete_sql = format!(
            "DELETE FROM missing_files
             WHERE scan_root_id = ?1 AND status = 'missing' AND file_id NOT IN ({placeholders})"
        );
        let mut params: Vec<rusqlite::types::Value> = vec![root_id.into()];
        for id in file_ids {
            params.push((*id).into());
        }
        conn.execute(&delete_sql, rusqlite::params_from_iter(params))?;

        for file_id in file_ids {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM missing_files WHERE file_id = ?1",
                    [file_id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if exists {
                // Bump the check timestamp only — an 'ignored' status stands.
                conn.execute(
                    "UPDATE missing_files SET last_checked_at = ?1 WHERE file_id = ?2",
                    params![&now, file_id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO missing_files (file_id, scan_root_id, detected_at, last_checked_at, status)
                     VALUES (?1, ?2, ?3, ?4, 'missing')",
                    params![file_id, root_id, &now, &now],
                )?;
            }
        }
    }

    sp.commit()?;
    Ok(())
}

/// Update scan root last_scan timestamp
pub fn update_scan_root_timestamp(conn: &Connection, id: i64) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE scan_roots SET last_scan = ?1 WHERE id = ?2",
        params![now, id],
    )?;
    Ok(())
}

/// Persist scan errors for a scan root (stored as JSON, cleared if empty)
pub fn update_scan_root_errors(
    conn: &Connection,
    id: i64,
    errors: &[String],
) -> anyhow::Result<()> {
    let json_val: Option<String> = if errors.is_empty() {
        None
    } else {
        Some(serde_json::to_string(errors).map_err(|e| anyhow::anyhow!(e))?)
    };
    conn.execute(
        "UPDATE scan_roots SET last_scan_errors = ?1 WHERE id = ?2",
        params![json_val, id],
    )?;
    Ok(())
}

/// Delete a scan root and all associated files
/// Optimized: uses transaction, pre-computes IDs, deletes child tables explicitly
pub fn delete_scan_root(conn: &Connection, id: i64) -> Result<()> {
    // Get the path of the scan root
    let path: String = conn.query_row(
        "SELECT path FROM scan_roots WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;

    // Start transaction for atomicity and performance
    let sp = SavepointGuard::new(conn, "delete_scan_root")?;

    // Pre-compute file IDs to delete (single byte-range query). The prefix
    // carries the trailing separator so name-prefix siblings (/lib_old vs
    // /lib_old2) are never swept — see the name_prefix_sibling tests.
    let file_ids: Vec<i64> = {
        let sep = native_separator_of(&path);
        let prefix = format!("{}{}", path.trim_end_matches(sep), sep);
        let path_hi = path_prefix_upper(&prefix);
        let mut stmt =
            conn.prepare("SELECT id FROM files WHERE path >= ?1 AND (?2 IS NULL OR path < ?2)")?;
        let rows = stmt.query_map(params![prefix, path_hi], |row| row.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if !file_ids.is_empty() {
        // Pre-compute frame IDs for these files
        let frame_ids: Vec<i64> = {
            let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id FROM frames WHERE file_id IN ({})", placeholders);
            let mut stmt = conn.prepare(&sql)?;
            let params_vec: Vec<rusqlite::types::Value> = file_ids
                .iter()
                .map(|id| rusqlite::types::Value::Integer(*id))
                .collect();
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
                row.get(0)
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        // Delete child tables explicitly (avoiding cascade overhead)
        if !frame_ids.is_empty() {
            let frame_placeholders: String =
                frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let frame_values: Vec<rusqlite::types::Value> = frame_ids
                .iter()
                .map(|id| rusqlite::types::Value::Integer(*id))
                .collect();

            // 1. session_members
            let sql = format!(
                "DELETE FROM session_members WHERE frame_id IN ({})",
                frame_placeholders
            );
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 2. calibration_set_frames
            let sql = format!(
                "DELETE FROM calibration_set_frames WHERE frame_id IN ({})",
                frame_placeholders
            );
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 3. frame_tags
            let sql = format!(
                "DELETE FROM frame_tags WHERE frame_id IN ({})",
                frame_placeholders
            );
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;

            // 4. calibration_set_to_frames (source_id refers to frame_id when source_type='frame')
            let sql = format!("DELETE FROM calibration_set_to_frames WHERE source_id IN ({}) AND source_type = 'frame'", frame_placeholders);
            conn.execute(&sql, rusqlite::params_from_iter(frame_values.iter()))?;
        }

        // Delete by file_id
        let file_placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let file_values: Vec<rusqlite::types::Value> = file_ids
            .iter()
            .map(|id| rusqlite::types::Value::Integer(*id))
            .collect();

        // 5. fits_header
        let sql = format!(
            "DELETE FROM fits_header WHERE file_id IN ({})",
            file_placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 6. black_hole
        let sql = format!(
            "DELETE FROM black_hole WHERE file_id IN ({})",
            file_placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 7. frames
        let sql = format!(
            "DELETE FROM frames WHERE file_id IN ({})",
            file_placeholders
        );
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;

        // 8. files
        let sql = format!("DELETE FROM files WHERE id IN ({})", file_placeholders);
        conn.execute(&sql, rusqlite::params_from_iter(file_values.iter()))?;
    }

    // 9. Delete orphaned calibration sets. Guarded prune: a raw set that fed a
    // Phase-2 master build is now member-less (its frames went in step 2/7) but
    // is still referenced by the NO-ACTION FKs `master_provenance.source_set_id`
    // and `calibration_set.superseded_by_set_id`. An unguarded delete of such a
    // set aborts this whole transaction with a FOREIGN KEY failure — the shared
    // helper preserves those lineage shells (same exemptions as the trigger).
    crate::db::schema::prune_orphaned_calibration_sets(conn)?;

    // 10. Delete orphaned sessions and imaging_nights first
    conn.execute(
        "DELETE FROM sessions WHERE id NOT IN (
            SELECT DISTINCT session_id FROM session_members
        )",
        [],
    )?;

    conn.execute(
        "DELETE FROM imaging_nights WHERE id NOT IN (
            SELECT DISTINCT imaging_night_id FROM sessions
        )",
        [],
    )?;

    // 11. Delete orphaned frame sets
    conn.execute(
        "DELETE FROM frames_set WHERE id NOT IN (
            SELECT DISTINCT frames_set_id FROM imaging_nights
        )",
        [],
    )?;

    // 12. Delete the scan root
    conn.execute("DELETE FROM scan_roots WHERE id = ?1", params![id])?;

    // Commit transaction
    sp.commit()?;

    Ok(())
}

/// Get a file by its path
pub fn get_file_by_path(conn: &Connection, path: &str) -> Result<File> {
    conn.query_row(
        "SELECT id, path, filename, size, modified_at, format, created_at, metadata_hash, content_hash,
                archived_in_operation, archive_zip_path, archive_path_in_zip, uuid, updated_at
         FROM files WHERE path = ?1",
        params![path],
        |row| {
            Ok(File {
                id: row.get(0)?,
                path: row.get(1)?,
                filename: row.get(2)?,
                size: row.get(3)?,
                modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
                format: match row.get::<_, String>(5)?.as_str() {
                    "FITS" => FileFormat::FITS,
                    "XISF" => FileFormat::XISF,
                    _ => FileFormat::FITS,
                },
                created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
                metadata_hash: row.get(7)?,
                content_hash: row.get(8)?,
                archived_in_operation: row.get(9)?,
                archive_zip_path: row.get(10)?,
                archive_path_in_zip: row.get(11)?,
                uuid: row.get(12)?,
                updated_at: row.get(13)?,
            })
        },
    )
}

/// Get all files with optional filters
pub fn get_files(conn: &Connection, limit: Option<usize>) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation,
                f.uuid, f.updated_at, fr.uuid, fr.updated_at
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         ORDER BY f.created_at DESC
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map([], |row| {
        let file = File {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
            format: if row.get::<_, String>(5)? == "FITS" {
                FileFormat::FITS
            } else {
                FileFormat::XISF
            },
            created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
            uuid: row.get(45)?,
            updated_at: row.get(46)?,
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(12) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(13).ok(),
                date_obs: row
                    .get::<_, Option<String>>(14)
                    .ok()
                    .flatten()
                    .and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                telescop: row.get(15).ok(),
                instrume: row.get(16).ok(),
                exptime: row.get(17).ok(),
                filter: row.get(18).ok(),
                imagetyp: row
                    .get::<_, Option<String>>(19)
                    .ok()
                    .flatten()
                    .and_then(|s| ImageType::from_str(&s)),
                is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(21).ok(),
                offset: row.get(22).ok(),
                binning: row.get(23).ok(),
                xbinning: row.get(24).ok(),
                ybinning: row.get(25).ok(),
                ccd_temp: row.get(26).ok(),
                set_temp: row.get(27).ok(),
                focallen: row.get(28).ok(),
                xpixsz: row.get(29).ok(),
                ypixsz: row.get(30).ok(),
                naxis1: row.get(31).ok(),
                naxis2: row.get(32).ok(),
                ra: row.get(33).ok(),
                dec: row.get(34).ok(),
                sitelat: row.get(35).ok(),
                lat_obs: row.get(36).ok(),
                sitelong: row.get(37).ok(),
                long_obs: row.get(38).ok(),
                objctra: row.get(39).ok(),
                objctdec: row.get(40).ok(),
                override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
                swcreate: row.get(42).ok(),
                bayerpat: row.get(43).ok(),
                xbayroff: None,
                ybayroff: None,
                roworder: None,
                rotation: row.get(44).ok(),
                uuid: row.get(47).ok(),
                updated_at: row.get(48).ok(),
            })
        } else {
            None
        };

        Ok((file, frame))
    })?;

    results.collect()
}

/// Get files in a specific directory
pub fn get_files_by_directory(
    conn: &Connection,
    directory_path: &str,
    limit: Option<usize>,
) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    // Find files that are directly in this directory (not in subdirectories).
    // Data-derived separator (native_separator_of doc: the leading character
    // decides, not the build OS) + trailing-separator trim so a drive root
    // (`D:\`) or `/` still lists its direct children.
    let sep = native_separator_of(directory_path).to_string();
    let dir = directory_path.trim_end_matches(['/', '\\']);
    let path_prefix = format!("{}{}", dir, sep);
    let path_hi = path_prefix_upper(&path_prefix);
    let expected_depth = dir.matches(sep.as_str()).count() as i64 + 1;

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation,
                f.uuid, f.updated_at, fr.uuid, fr.updated_at
         FROM files f
         LEFT JOIN frames fr ON f.id = fr.file_id
         WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
           AND (LENGTH(f.path) - LENGTH(REPLACE(f.path, ?3, ''))) = ?4
         ORDER BY f.filename
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map(params![path_prefix, path_hi, sep, expected_depth], |row| {
        let file = File {
            id: Some(row.get(0)?),
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
            format: if row.get::<_, String>(5)? == "FITS" {
                FileFormat::FITS
            } else {
                FileFormat::XISF
            },
            created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
            uuid: row.get(45)?,
            updated_at: row.get(46)?,
        };

        let frame = if let Ok(frame_id) = row.get::<_, Option<i64>>(12) {
            frame_id.map(|fid| Frame {
                id: Some(fid),
                file_id: file.id.unwrap(),
                object: row.get(13).ok(),
                date_obs: row
                    .get::<_, Option<String>>(14)
                    .ok()
                    .flatten()
                    .and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                telescop: row.get(15).ok(),
                instrume: row.get(16).ok(),
                exptime: row.get(17).ok(),
                filter: row.get(18).ok(),
                imagetyp: row
                    .get::<_, Option<String>>(19)
                    .ok()
                    .flatten()
                    .and_then(|s| ImageType::from_str(&s)),
                is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(21).ok(),
                offset: row.get(22).ok(),
                binning: row.get(23).ok(),
                xbinning: row.get(24).ok(),
                ybinning: row.get(25).ok(),
                ccd_temp: row.get(26).ok(),
                set_temp: row.get(27).ok(),
                focallen: row.get(28).ok(),
                xpixsz: row.get(29).ok(),
                ypixsz: row.get(30).ok(),
                naxis1: row.get(31).ok(),
                naxis2: row.get(32).ok(),
                ra: row.get(33).ok(),
                dec: row.get(34).ok(),
                sitelat: row.get(35).ok(),
                lat_obs: row.get(36).ok(),
                sitelong: row.get(37).ok(),
                long_obs: row.get(38).ok(),
                objctra: row.get(39).ok(),
                objctdec: row.get(40).ok(),
                override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
                swcreate: row.get(42).ok(),
                bayerpat: row.get(43).ok(),
                xbayroff: None,
                ybayroff: None,
                roworder: None,
                rotation: row.get(44).ok(),
                uuid: row.get(47).ok(),
                updated_at: row.get(48).ok(),
            })
        } else {
            None
        };

        Ok((file, frame))
    })?;

    results.collect()
}

/// Get files in a specific directory filtered by camera (instrume)
pub fn get_files_by_directory_for_camera(
    conn: &Connection,
    directory_path: &str,
    instrume: &str,
    limit: Option<usize>,
) -> Result<Vec<(File, Option<Frame>)>> {
    let limit_clause = match limit {
        Some(n) => format!("LIMIT {}", n),
        None => String::new(),
    };

    // Same data-derived separator + trailing-separator trim as
    // `get_files_by_directory` above.
    let sep = native_separator_of(directory_path).to_string();
    let dir = directory_path.trim_end_matches(['/', '\\']);
    let path_prefix = format!("{}{}", dir, sep);
    let path_hi = path_prefix_upper(&path_prefix);
    let expected_depth = dir.matches(sep.as_str()).count() as i64 + 1;

    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
                fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
                fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
                fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation,
                f.uuid, f.updated_at, fr.uuid, fr.updated_at
         FROM files f
         JOIN frames fr ON f.id = fr.file_id
         WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
           AND (LENGTH(f.path) - LENGTH(REPLACE(f.path, ?3, ''))) = ?4
           AND fr.instrume = ?5
         ORDER BY f.filename
         {}",
        limit_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let results = stmt.query_map(
        params![path_prefix, path_hi, sep, expected_depth, instrume],
        |row| {
            let file = File {
                id: Some(row.get(0)?),
                path: row.get(1)?,
                filename: row.get(2)?,
                size: row.get(3)?,
                modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
                format: if row.get::<_, String>(5)? == "FITS" {
                    FileFormat::FITS
                } else {
                    FileFormat::XISF
                },
                created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
                metadata_hash: row.get(7)?,
                content_hash: row.get(8)?,
                archived_in_operation: row.get(9)?,
                archive_zip_path: row.get(10)?,
                archive_path_in_zip: row.get(11)?,
                uuid: row.get(45)?,
                updated_at: row.get(46)?,
            };

            let frame = Some(Frame {
                id: Some(row.get(12)?),
                file_id: file.id.unwrap(),
                object: row.get(13).ok(),
                date_obs: row
                    .get::<_, Option<String>>(14)
                    .ok()
                    .flatten()
                    .and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                telescop: row.get(15).ok(),
                instrume: row.get(16).ok(),
                exptime: row.get(17).ok(),
                filter: row.get(18).ok(),
                imagetyp: row
                    .get::<_, Option<String>>(19)
                    .ok()
                    .flatten()
                    .and_then(|s| ImageType::from_str(&s)),
                is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
                gain: row.get(21).ok(),
                offset: row.get(22).ok(),
                binning: row.get(23).ok(),
                xbinning: row.get(24).ok(),
                ybinning: row.get(25).ok(),
                ccd_temp: row.get(26).ok(),
                set_temp: row.get(27).ok(),
                focallen: row.get(28).ok(),
                xpixsz: row.get(29).ok(),
                ypixsz: row.get(30).ok(),
                naxis1: row.get(31).ok(),
                naxis2: row.get(32).ok(),
                ra: row.get(33).ok(),
                dec: row.get(34).ok(),
                sitelat: row.get(35).ok(),
                lat_obs: row.get(36).ok(),
                sitelong: row.get(37).ok(),
                long_obs: row.get(38).ok(),
                objctra: row.get(39).ok(),
                objctdec: row.get(40).ok(),
                override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
                swcreate: row.get(42).ok(),
                bayerpat: row.get(43).ok(),
                xbayroff: None,
                ybayroff: None,
                roworder: None,
                rotation: row.get(44).ok(),
                uuid: row.get(47).ok(),
                updated_at: row.get(48).ok(),
            });

            Ok((file, frame))
        },
    )?;

    results.collect()
}

/// Get frames with missing metadata
/// category: "all", "coordinates", "object", "datetime", "instrument", "frametype"
///
/// Each returned row includes a `has_duplicate` flag that is true when another
/// file in the catalog shares either the same `content_hash` or the same
/// `metadata_hash`. That covers both exact-byte duplicates (content hash) and
/// "same header/size/filename" near-duplicates (metadata hash), matching how
/// the rest of the app treats duplicates.
/// SELECT clause for `MissingMetadataRow` rows.
const MISSING_METADATA_SELECT: &str =
    "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
            f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
            fr.id, fr.object, fr.date_obs, fr.telescop, fr.instrume, fr.exptime, fr.filter, fr.imagetyp, fr.is_master,
            fr.gain, fr.offset, fr.binning, fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp,
            fr.focallen, fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs, fr.sitelong,
            fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.swcreate, fr.bayerpat, fr.rotation,
            f.uuid, f.updated_at, fr.uuid, fr.updated_at,
            CASE WHEN dgf.file_id IS NOT NULL THEN 1 ELSE 0 END AS has_duplicate";

/// Map a row from a SELECT that uses `MISSING_METADATA_SELECT`.
fn map_missing_metadata_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MissingMetadataRow> {
    let file = File {
        id: Some(row.get(0)?),
        path: row.get(1)?,
        filename: row.get(2)?,
        size: row.get(3)?,
        modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
        format: if row.get::<_, String>(5)? == "FITS" {
            FileFormat::FITS
        } else {
            FileFormat::XISF
        },
        created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
        metadata_hash: row.get(7)?,
        content_hash: row.get(8)?,
        archived_in_operation: row.get(9)?,
        archive_zip_path: row.get(10)?,
        archive_path_in_zip: row.get(11)?,
        uuid: row.get(45)?,
        updated_at: row.get(46)?,
    };

    let frame = Frame {
        id: Some(row.get(12)?),
        file_id: file.id.unwrap(),
        object: row.get(13).ok(),
        date_obs: row
            .get::<_, Option<String>>(14)
            .ok()
            .flatten()
            .and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }),
        telescop: row.get(15).ok(),
        instrume: row.get(16).ok(),
        exptime: row.get(17).ok(),
        filter: row.get(18).ok(),
        imagetyp: row
            .get::<_, Option<String>>(19)
            .ok()
            .flatten()
            .and_then(|s| ImageType::from_str(&s)),
        is_master: row.get::<_, i32>(20).ok().map(|v| v == 1).unwrap_or(false),
        gain: row.get(21).ok(),
        offset: row.get(22).ok(),
        binning: row.get(23).ok(),
        xbinning: row.get(24).ok(),
        ybinning: row.get(25).ok(),
        ccd_temp: row.get(26).ok(),
        set_temp: row.get(27).ok(),
        focallen: row.get(28).ok(),
        xpixsz: row.get(29).ok(),
        ypixsz: row.get(30).ok(),
        naxis1: row.get(31).ok(),
        naxis2: row.get(32).ok(),
        ra: row.get(33).ok(),
        dec: row.get(34).ok(),
        sitelat: row.get(35).ok(),
        lat_obs: row.get(36).ok(),
        sitelong: row.get(37).ok(),
        long_obs: row.get(38).ok(),
        objctra: row.get(39).ok(),
        objctdec: row.get(40).ok(),
        override_: row.get::<_, i32>(41).ok().map(|v| v == 1).unwrap_or(false),
        swcreate: row.get(42).ok(),
        bayerpat: row.get(43).ok(),
        xbayroff: None,
        ybayroff: None,
        roworder: None,
        rotation: row.get(44).ok(),
        uuid: row.get(47).ok(),
        updated_at: row.get(48).ok(),
    };

    let has_duplicate: i32 = row.get(49)?;

    Ok(MissingMetadataRow {
        file,
        frame,
        has_duplicate: has_duplicate != 0,
    })
}

pub fn get_frames_with_missing_metadata(
    conn: &Connection,
    category: &str,
) -> Result<Vec<MissingMetadataRow>> {
    // "Missing coordinates" catches three sentinel patterns that FITS
    // pipelines write instead of leaving fields NULL:
    //   1. numeric ra/dec = NULL
    //   2. numeric (ra, dec) = (0, 0) — literal zero, no one images at 0,0
    //   3. sexagesimal objctra/objctdec that stringify to all-zero after
    //      stripping whitespace, signs, and colons — e.g. "00 00 00",
    //      "+00 00 00", "00:00:00". Thousands of LIGHT frames in typical
    //      libraries have these because the acquisition software wrote a
    //      placeholder rather than the real RA/Dec.
    //
    // A frame counts as "missing coordinates" when BOTH the numeric side
    // and the sexagesimal side are in one of the above states. The same
    // clause is inlined into the "all" branch below.
    let coords_missing_clause =
        "((fr.ra IS NULL AND fr.dec IS NULL) OR (fr.ra = 0 AND fr.dec = 0)) \
         AND (\
             fr.objctra IS NULL OR fr.objctdec IS NULL \
             OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctra, ' ', ''), '+', ''), '-', ''), ':', '') = '000000' \
             OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctdec, ' ', ''), '+', ''), '-', ''), ':', '') = '000000'\
         )";
    // LIGHT-only categories: coordinates and object. Darks/flats/bias legitimately
    // have no RA/Dec and no target OBJECT — surfacing them under those chips would
    // be noise. For instrument/datetime/frametype, all frame types qualify because
    // the user may need to fix a missing camera/date/type on any kind of frame.
    let (missing_clause, imagetyp_filter) = match category {
        "coordinates" => (coords_missing_clause, "UPPER(fr.imagetyp) = 'LIGHT' AND"),
        "object" => ("fr.object IS NULL OR fr.object = ''", "UPPER(fr.imagetyp) = 'LIGHT' AND"),
        "datetime" => ("fr.date_obs IS NULL", ""),
        "instrument" => ("fr.instrume IS NULL OR fr.instrume = ''", ""),
        "frametype" => ("(fr.imagetyp IS NULL OR fr.imagetyp = '')", ""),
        _ => (
            "(UPPER(fr.imagetyp) = 'LIGHT' AND ((((fr.ra IS NULL AND fr.dec IS NULL) OR (fr.ra = 0 AND fr.dec = 0)) AND (fr.objctra IS NULL OR fr.objctdec IS NULL OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctra, ' ', ''), '+', ''), '-', ''), ':', '') = '000000' OR REPLACE(REPLACE(REPLACE(REPLACE(fr.objctdec, ' ', ''), '+', ''), '-', ''), ':', '') = '000000')) OR (fr.object IS NULL OR fr.object = ''))) OR fr.date_obs IS NULL OR (fr.instrume IS NULL OR fr.instrume = '') OR (fr.imagetyp IS NULL OR fr.imagetyp = '')",
            ""
        ),
    };

    // `has_duplicate` is a LEFT JOIN against the cached `duplicate_group_files`
    // table (populated by the existing duplicate-detection pipeline). No row
    // means the file isn't in any detected duplicate group; any match means it
    // is. Uses the `idx_dup_group_files_file` index for O(1) lookups.
    let query = format!(
        "{select}
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         LEFT JOIN duplicate_group_files dgf ON dgf.file_id = f.id
         WHERE {imagetyp_filter} ({missing_clause})
         GROUP BY f.id
         ORDER BY f.modified_at DESC",
        select = MISSING_METADATA_SELECT,
        imagetyp_filter = imagetyp_filter,
        missing_clause = missing_clause,
    );

    let mut stmt = conn.prepare(&query)?;
    let results = stmt.query_map([], map_missing_metadata_row)?;
    results.collect()
}

/// Bulk-update selected metadata fields on the given frames.
///
/// Used by the Missing Metadata page's Set Camera / Set Date / Set Frame Type
/// actions. DB-only update — the underlying FITS files on disk are NOT touched.
/// Every row that gets any field updated also has `override = 1` set, matching
/// the existing pattern so re-scanners know the DB row beats the on-disk header.
///
/// Only fields that are `Some` in `edits` are written. If `edits` has no fields,
/// the call is a no-op and returns 0.
///
/// `imagetyp` is normalized via `ImageType::from_str` when recognized so the
/// stored string matches what `insert_frame` produces (the Debug form of the
/// enum variant, e.g. `"Dark"`, `"MasterDark"`). Unknown strings are stored
/// as-given.
pub fn bulk_update_frame_metadata(
    conn: &Connection,
    frame_ids: &[i64],
    edits: &FrameMetadataEdits,
) -> Result<usize> {
    if frame_ids.is_empty() {
        return Ok(0);
    }
    if edits.instrume.is_none()
        && edits.date_obs.is_none()
        && edits.imagetyp.is_none()
        && edits.is_master.is_none()
        && edits.object.is_none()
        && edits.filter.is_none()
        && edits.telescop.is_none()
        && edits.focallen.is_none()
        && edits.xpixsz.is_none()
        && edits.gain.is_none()
        && edits.offset.is_none()
        && edits.binning.is_none()
        && edits.exptime.is_none()
        && edits.ccd_temp.is_none()
        && edits.ra.is_none()
        && edits.dec.is_none()
        && edits.rotation.is_none()
    {
        return Ok(0);
    }

    // Build the SET clauses and collect parameters in order.
    use rusqlite::types::Value;
    let mut set_clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Value> = Vec::new();

    // Every nullable field uses `Option<Option<T>>` so the wire format can
    // tell "no edit" (outer None — skip), "clear to NULL" (Some(None) —
    // emit `SET col = NULL`), and "set to v" (Some(Some(v))) apart. The
    // metadata-pane revert button depends on this distinction; a plain
    // `Option<T>` collapses the first two and silently no-ops the clear.
    // Helpers keep the body readable.
    fn text_value(maybe: &Option<String>) -> Value {
        match maybe {
            Some(s) => Value::Text(s.clone()),
            None => Value::Null,
        }
    }
    fn real_value(maybe: Option<f64>) -> Value {
        match maybe {
            Some(v) => Value::Real(v),
            None => Value::Null,
        }
    }

    if let Some(maybe_instrume) = &edits.instrume {
        set_clauses.push("instrume = ?");
        values.push(text_value(maybe_instrume));
    }
    if let Some(maybe_date_obs) = &edits.date_obs {
        set_clauses.push("date_obs = ?");
        values.push(text_value(maybe_date_obs));
    }
    // imagetyp + is_master are paired: setting imagetyp also writes the
    // matching is_master bool (true for any Master* variant, false for the
    // base variants). This keeps Equipment's master detection (which filters
    // on the imagetyp string) and any code that reads frames.is_master in
    // sync — no UI checkbox is needed for the toggle. Clearing imagetyp to
    // NULL leaves is_master alone (no sensible default for a typeless row).
    let mut imagetyp_set_master_flag: Option<bool> = None;
    if let Some(maybe_imagetyp) = &edits.imagetyp {
        match maybe_imagetyp {
            Some(imagetyp) => {
                let parsed = ImageType::from_str(imagetyp);
                let normalized = parsed
                    .as_ref()
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| imagetyp.clone());
                set_clauses.push("imagetyp = ?");
                values.push(Value::Text(normalized));
                if let Some(t) = parsed {
                    imagetyp_set_master_flag = Some(t.is_master());
                }
            }
            None => {
                set_clauses.push("imagetyp = ?");
                values.push(Value::Null);
            }
        }
    }
    // Derived is_master from imagetyp wins over an explicit edits.is_master
    // when both are present (older callers pass them separately; the dropdown
    // never does). Otherwise honour edits.is_master, otherwise leave alone.
    let final_is_master: Option<bool> = imagetyp_set_master_flag.or(edits.is_master);
    if let Some(is_master) = final_is_master {
        set_clauses.push("is_master = ?");
        values.push(Value::Integer(if is_master { 1 } else { 0 }));
    }
    if let Some(maybe_object) = &edits.object {
        set_clauses.push("object = ?");
        values.push(text_value(maybe_object));
    }
    if let Some(maybe_filter) = &edits.filter {
        set_clauses.push("filter = ?");
        values.push(text_value(maybe_filter));
    }
    if let Some(maybe_telescop) = &edits.telescop {
        set_clauses.push("telescop = ?");
        values.push(text_value(maybe_telescop));
    }
    if let Some(maybe_focallen) = edits.focallen {
        set_clauses.push("focallen = ?");
        values.push(real_value(maybe_focallen));
    }
    if let Some(maybe_xpixsz) = edits.xpixsz {
        set_clauses.push("xpixsz = ?");
        values.push(real_value(maybe_xpixsz));
    }
    if let Some(maybe_gain) = edits.gain {
        set_clauses.push("gain = ?");
        values.push(real_value(maybe_gain));
    }
    if let Some(maybe_offset) = edits.offset {
        set_clauses.push("offset = ?");
        values.push(real_value(maybe_offset));
    }
    if let Some(maybe_binning) = &edits.binning {
        match maybe_binning {
            Some(binning) => {
                // Validate "AxB" with positive integers; store xbinning/ybinning
                // in lockstep so calibration-matching consumers see consistent
                // values.
                let parts: Vec<&str> = binning.split('x').collect();
                if parts.len() != 2 {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        format!(
                            "binning must be in 'AxB' format (e.g. '1x1'), got '{}'",
                            binning
                        )
                        .into(),
                    ));
                }
                let xb: i64 = parts[0].parse().map_err(|_| {
                    rusqlite::Error::ToSqlConversionFailure(
                        format!("binning x component must be an integer, got '{}'", parts[0])
                            .into(),
                    )
                })?;
                let yb: i64 = parts[1].parse().map_err(|_| {
                    rusqlite::Error::ToSqlConversionFailure(
                        format!("binning y component must be an integer, got '{}'", parts[1])
                            .into(),
                    )
                })?;
                if xb < 1 || yb < 1 || xb > 16 || yb > 16 {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        format!("binning components must be 1-16, got '{}'", binning).into(),
                    ));
                }
                set_clauses.push("binning = ?");
                values.push(Value::Text(binning.clone()));
                set_clauses.push("xbinning = ?");
                values.push(Value::Integer(xb));
                set_clauses.push("ybinning = ?");
                values.push(Value::Integer(yb));
            }
            None => {
                // Clear all three columns in lockstep — leaving xbinning/ybinning
                // populated when binning string is NULL would confuse downstream
                // consumers.
                set_clauses.push("binning = ?");
                values.push(Value::Null);
                set_clauses.push("xbinning = ?");
                values.push(Value::Null);
                set_clauses.push("ybinning = ?");
                values.push(Value::Null);
            }
        }
    }
    if let Some(maybe_exptime) = edits.exptime {
        if let Some(v) = maybe_exptime {
            if !v.is_finite() || v <= 0.0 {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("exptime must be > 0 seconds, got {}", v).into(),
                ));
            }
        }
        set_clauses.push("exptime = ?");
        values.push(real_value(maybe_exptime));
    }
    if let Some(maybe_ccd_temp) = edits.ccd_temp {
        if let Some(v) = maybe_ccd_temp {
            if !v.is_finite() {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("ccd_temp must be a finite number, got {}", v).into(),
                ));
            }
        }
        set_clauses.push("ccd_temp = ?");
        values.push(real_value(maybe_ccd_temp));
    }
    // RA / DEC: when set, also mirror to sexagesimal OBJCTRA/OBJCTDEC so
    // calibration-matching, sky-chart and clustering consumers stay in sync.
    // When cleared, NULL both columns in lockstep.
    if let Some(maybe_ra) = edits.ra {
        match maybe_ra {
            Some(ra) => {
                if !ra.is_finite() || !(0.0..360.0).contains(&ra) {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        format!("ra must be in [0, 360) degrees, got {}", ra).into(),
                    ));
                }
                set_clauses.push("ra = ?");
                values.push(Value::Real(ra));
                set_clauses.push("objctra = ?");
                values.push(Value::Text(crate::coordinates::format_ra_sexagesimal(ra)));
            }
            None => {
                set_clauses.push("ra = ?");
                values.push(Value::Null);
                set_clauses.push("objctra = ?");
                values.push(Value::Null);
            }
        }
    }
    if let Some(maybe_dec) = edits.dec {
        match maybe_dec {
            Some(dec) => {
                if !dec.is_finite() || !(-90.0..=90.0).contains(&dec) {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        format!("dec must be in [-90, 90] degrees, got {}", dec).into(),
                    ));
                }
                set_clauses.push("dec = ?");
                values.push(Value::Real(dec));
                set_clauses.push("objctdec = ?");
                values.push(Value::Text(crate::coordinates::format_dec_sexagesimal(dec)));
            }
            None => {
                set_clauses.push("dec = ?");
                values.push(Value::Null);
                set_clauses.push("objctdec = ?");
                values.push(Value::Null);
            }
        }
    }
    if let Some(maybe_rotation) = edits.rotation {
        if let Some(v) = maybe_rotation {
            if !v.is_finite() || !(-360.0..=360.0).contains(&v) {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("rotation must be in [-360, 360] degrees, got {}", v).into(),
                ));
            }
        }
        set_clauses.push("rotation = ?");
        values.push(real_value(maybe_rotation));
    }
    // Always flag touched rows as override so rescans preserve the edit.
    set_clauses.push("override = 1");

    let id_placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    for id in frame_ids {
        values.push(Value::Integer(*id));
    }

    let sql = format!(
        "UPDATE frames SET {} WHERE id IN ({})",
        set_clauses.join(", "),
        id_placeholders
    );

    // One atomic unit: the frames UPDATE (override stamp included), the
    // three cascade DELETEs, and the empty-set prune. A failure partway
    // previously left edited frames with a partial subset of their stale
    // links — silently violating the "ANY edit unlinks" invariant below
    // (audit I1).
    let sp = SavepointGuard::new(conn, "bulk_update_frame_metadata")?;

    let count = conn
        .execute(&sql, rusqlite::params_from_iter(values.iter()))
        .map_err(|e| {
            tracing::error!(table = "frames", count = frame_ids.len(), error = %e, "bulk_update_frame_metadata failed");
            e
        })?;

    // ANY metadata edit invalidates the frame's existing relations to
    // calibration sets (as a member or as a consumer) and to sessions /
    // imaging nights / frame sets. The user has chosen to be conservative:
    // editing CCD-TEMP, FILTER, OBJECT, IMAGETYP — anything — unlinks. The
    // UI must warn before calling this (see CountFrameMetadataRelations).
    //
    // Calibration sets that lose their last member are pruned (otherwise a
    // single-frame "master" set would survive as a zombie pointing nowhere).
    // Sessions / imaging_nights / frames_sets are intentionally left in
    // place even when empty — same policy as elsewhere.
    {
        let id_placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let frame_id_params: Vec<rusqlite::types::Value> = frame_ids
            .iter()
            .map(|id| rusqlite::types::Value::Integer(*id))
            .collect();

        // calibration_set_frames: was a member of a calibration set.
        let sql_csf = format!(
            "DELETE FROM calibration_set_frames WHERE frame_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_csf, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                tracing::error!(table = "calibration_set_frames", error = %e, "bulk_update_frame_metadata cascade delete failed");
                e
            })?;

        // calibration_set_to_frames: consumer link from frame → calibration set.
        let sql_cstf = format!(
            "DELETE FROM calibration_set_to_frames WHERE source_type = 'frame' AND source_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_cstf, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                tracing::error!(table = "calibration_set_to_frames", error = %e, "bulk_update_frame_metadata cascade delete failed");
                e
            })?;

        // session_members: frame's seat in a session/imaging-night/frame-set.
        let sql_sm = format!(
            "DELETE FROM session_members WHERE frame_id IN ({})",
            id_placeholders
        );
        conn.execute(&sql_sm, rusqlite::params_from_iter(frame_id_params.iter()))
            .map_err(|e| {
                tracing::error!(table = "session_members", error = %e, "bulk_update_frame_metadata cascade delete failed");
                e
            })?;

        // Prune calibration sets that now have zero member frames. The shared
        // helper garbage-collects every empty set in one guarded statement,
        // EXCEPT master-library / superseded / provenance-anchor sets — deleting
        // one of those would abort this transaction on a NO-ACTION FK
        // (`master_provenance.source_set_id` / `superseded_by_set_id`). Consumer
        // links (`calibration_set_to_frames`) are removed automatically by the
        // FK CASCADE on calibration_set_to_frames.calibration_set_id.
        crate::db::schema::prune_orphaned_calibration_sets(conn).map_err(|e| {
            tracing::error!(table = "calibration_set", error = %e, "bulk_update_frame_metadata prune of empty sets failed");
            e
        })?;
    }

    sp.commit()?;

    // After writing the edits the override flag is on for every touched
    // frame. If the user just reverted everything back to its original
    // file-header values, drop the flag so the row stops looking edited.
    // Errors here aren't fatal — the edits themselves succeeded already.
    if let Err(e) = recompute_override_flag_for_frames(conn, frame_ids) {
        tracing::warn!(count = frame_ids.len(), error = %e, "recompute_override_flag_for_frames after bulk_update failed, edits already committed");
    }

    Ok(count)
}

/// Return the distinct non-empty INSTRUME values seen in the `frames` table,
/// alphabetically sorted. Used by the Set Camera modal to populate its dropdown.
pub fn get_distinct_instrumes(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT instrume FROM frames \
         WHERE instrume IS NOT NULL AND TRIM(instrume) != '' \
         ORDER BY instrume COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Find duplicates by filename and metadata, or by content hash
/// Only includes files from scan roots where find_duplicates = 1
///
/// If use_content_hash is true, groups by content_hash (xxhash).
/// If use_content_hash is false, groups by metadata_hash (size + modified + filename).
pub fn find_duplicate_groups(
    conn: &Connection,
    use_content_hash: bool,
) -> Result<Vec<DuplicateGroup>> {
    let hash_column = if use_content_hash {
        "content_hash"
    } else {
        "metadata_hash"
    };

    // Scan roots eligible for duplicate detection, fetched once in Rust so
    // the path predicate can be bound as byte-range params per root instead
    // of a per-row SQL-side `LIKE .. || '%'` concat. The original query also
    // OR'd in `sr.path || '/%'` as a second alternative, but that's redundant
    // — `X%` is already a superset of `X/%` — so only the plain-prefix
    // alternative is preserved here.
    let roots: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM scan_roots WHERE find_duplicates = 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let (root_predicate, root_values) = scan_root_prefix_predicate("f.path", &roots);

    let query = format!(
        "SELECT f.{}, f.size, COUNT(*) as count, GROUP_CONCAT(f.path, '|') as paths, GROUP_CONCAT(f.id, '|') as ids
         FROM files f
         WHERE f.{} IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         AND ({})
         GROUP BY f.{}, f.size
         HAVING count > 1
         ORDER BY count DESC, f.size DESC",
        hash_column, hash_column, root_predicate, hash_column
    );

    let mut stmt = conn.prepare(&query)?;

    let mut groups: Vec<DuplicateGroup> = stmt
        .query_map(rusqlite::params_from_iter(root_values.iter()), |row| {
            let paths_str: String = row.get(3)?;
            let file_paths: Vec<String> = paths_str.split('|').map(|s| s.to_string()).collect();

            let ids_str: String = row.get(4)?;
            let file_ids: Vec<i64> = ids_str
                .split('|')
                .filter_map(|s| s.parse::<i64>().ok())
                .collect();

            Ok(DuplicateGroup {
                id: None,
                size: row.get(1)?,
                content_hash: row.get(0)?,
                file_count: row.get(2)?,
                file_paths,
                file_ids,
                files: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    enrich_duplicate_groups(conn, &mut groups)?;
    Ok(groups)
}

/// Fetch per-file metadata (modified_at, scan-root assignment) for every file
/// in `groups` and fill in each group's `files` field. Scan-root assignment is
/// a longest-prefix match against `scan_roots.path` computed in Rust so the
/// caller doesn't need to add a join per row.
fn enrich_duplicate_groups(conn: &Connection, groups: &mut [DuplicateGroup]) -> Result<()> {
    if groups.is_empty() {
        return Ok(());
    }

    // Load scan roots once. Small table; a fresh read every call is cheaper
    // than caching and getting stale.
    let mut sr_stmt = conn.prepare("SELECT id, path FROM scan_roots ORDER BY length(path) DESC")?;
    let scan_roots: Vec<(i64, String)> = sr_stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // One SELECT covering every file id across every group. Pulls the frame
    // row (imagetyp, date_obs, filename) via LEFT JOIN so files without an
    // extracted frame still come through with None-y frame fields.
    let mut all_ids: Vec<i64> = groups
        .iter()
        .flat_map(|g| g.file_ids.iter().copied())
        .collect();
    all_ids.sort_unstable();
    all_ids.dedup();
    if all_ids.is_empty() {
        return Ok(());
    }
    let placeholders: String = std::iter::repeat("?")
        .take(all_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.modified_at, fr.imagetyp, fr.date_obs
         FROM files f
         LEFT JOIN frames fr ON fr.file_id = f.id
         WHERE f.id IN ({})",
        placeholders
    );
    let params_vec: Vec<rusqlite::types::Value> = all_ids
        .iter()
        .map(|i| rusqlite::types::Value::Integer(*i))
        .collect();
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let filename: String = row.get(2)?;
        let modified_at_str: String = row.get(3)?;
        let imagetyp: Option<String> = row.get(4)?;
        let date_obs: Option<String> = row.get(5)?;
        Ok((id, path, filename, modified_at_str, imagetyp, date_obs))
    })?;

    struct Row {
        path: String,
        filename: String,
        modified_at: chrono::DateTime<chrono::Utc>,
        imagetyp: Option<String>,
        date_obs: Option<String>,
    }
    let mut by_id: std::collections::HashMap<i64, Row> = std::collections::HashMap::new();
    for row in rows {
        let (id, path, filename, modified_at_str, imagetyp, date_obs) = row?;
        let modified_at = chrono::DateTime::parse_from_rfc3339(&modified_at_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        by_id.insert(
            id,
            Row {
                path,
                filename,
                modified_at,
                imagetyp,
                date_obs,
            },
        );
    }

    for group in groups.iter_mut() {
        let mut files: Vec<crate::models::DuplicateFile> = Vec::with_capacity(group.file_ids.len());
        for file_id in &group.file_ids {
            let Some(r) = by_id.get(file_id) else {
                continue;
            };
            let (scan_root_id, scan_root_path) = scan_roots
                .iter()
                // Separator-boundary-safe (scanner helper): with roots /data
                // and /data/M31x, the file /data/M31xyz/a.fits belongs to
                // /data, not /data/M31x — a bare starts_with mis-attributed it.
                .find(|(_, sr_path)| crate::scanner::path_has_root_prefix(&r.path, sr_path))
                .map(|(id, sr_path)| (Some(*id), Some(sr_path.clone())))
                .unwrap_or((None, None));
            files.push(crate::models::DuplicateFile {
                file_id: *file_id,
                path: r.path.clone(),
                filename: r.filename.clone(),
                modified_at: r.modified_at,
                scan_root_id,
                scan_root_path,
                imagetyp: r.imagetyp.clone(),
                date_obs: r.date_obs.clone(),
            });
        }
        group.files = files;
    }

    Ok(())
}

/// Rebuild the duplicate groups cache tables
/// This clears existing cache and recomputes all duplicate groups
pub fn rebuild_duplicate_groups_cache(conn: &Connection, use_content_hash: bool) -> Result<usize> {
    let hash_type = if use_content_hash {
        "content"
    } else {
        "metadata"
    };
    let hash_column = if use_content_hash {
        "content_hash"
    } else {
        "metadata_hash"
    };

    // Start transaction
    let sp = SavepointGuard::new(conn, "rebuild_dup_groups_cache")?;

    // Clear existing cache for this hash type
    conn.execute(
        "DELETE FROM duplicate_group_files WHERE group_id IN (SELECT id FROM duplicate_groups WHERE hash_type = ?1)",
        params![hash_type],
    )?;
    conn.execute(
        "DELETE FROM duplicate_groups WHERE hash_type = ?1",
        params![hash_type],
    )?;

    // Scan roots eligible for duplicate detection, fetched once in Rust
    // (shared by both queries below) so path matching becomes byte-range
    // binds per root instead of a per-row SQL-side `LIKE .. || '%'` concat.
    let roots: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM scan_roots WHERE find_duplicates = 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let (root_predicate, root_values) = scan_root_prefix_predicate("f.path", &roots);

    // Find duplicate files (groups with count > 1)
    let query = format!(
        "SELECT f.{}, f.size, COUNT(*) as count
         FROM files f
         WHERE f.{} IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id
         )
         AND ({})
         GROUP BY f.{}, f.size
         HAVING count > 1",
        hash_column, hash_column, root_predicate, hash_column
    );

    let mut stmt = conn.prepare(&query)?;
    let rows: Vec<(String, i64, i64)> = stmt
        .query_map(rusqlite::params_from_iter(root_values.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let now = chrono::Utc::now().to_rfc3339();
    let mut groups_created = 0;

    // Same root list, rebound against `files.path` (the loop body's table
    // alias) — the predicate text and its values are loop-invariant, so
    // build them once and just prepend the per-group hash/size each pass.
    // All-unnumbered placeholders throughout this statement (deliberately —
    // mixing `?1`/`?2` with the predicate's unnumbered `?`s would rely on
    // SQLite's left-to-right auto-numbering continuing past the explicit
    // indices, which is correct but easy to get subtly wrong; one scheme
    // keeps every bind position unambiguous by construction).
    let (files_root_predicate, files_root_values) =
        scan_root_prefix_predicate("files.path", &roots);
    let files_query = format!(
        "SELECT id FROM files WHERE {} = ? AND size = ?
         AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = files.id)
         AND ({})",
        hash_column, files_root_predicate
    );

    for (hash, size, file_count) in rows {
        // Insert the group
        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![hash, hash_type, size, file_count, now],
        )?;
        let group_id = conn.last_insert_rowid();

        // Find and insert all files in this group
        let mut files_stmt = conn.prepare(&files_query)?;
        let mut bind_values: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Text(hash.clone()),
            rusqlite::types::Value::Integer(size),
        ];
        bind_values.extend(files_root_values.iter().cloned());
        let file_ids: Vec<i64> = files_stmt
            .query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
                row.get(0)
            })?
            .filter_map(|r| r.ok())
            .collect();

        for file_id in file_ids {
            conn.execute(
                "INSERT OR IGNORE INTO duplicate_group_files (group_id, file_id) VALUES (?1, ?2)",
                params![group_id, file_id],
            )?;
        }

        groups_created += 1;
    }

    sp.commit()?;
    Ok(groups_created)
}

/// Get duplicate groups from cache
/// Returns cached duplicate groups with file paths and IDs
pub fn get_cached_duplicates(
    conn: &Connection,
    use_content_hash: bool,
) -> Result<Vec<DuplicateGroup>> {
    let hash_type = if use_content_hash {
        "content"
    } else {
        "metadata"
    };

    let mut stmt = conn.prepare(
        "SELECT dg.id, dg.hash, dg.size, dg.file_count
         FROM duplicate_groups dg
         WHERE dg.hash_type = ?1
         ORDER BY dg.file_count DESC, dg.size DESC",
    )?;

    let groups: Vec<(i64, String, i64, i64)> = stmt
        .query_map(params![hash_type], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut result = Vec::with_capacity(groups.len());

    for (group_id, hash, size, _file_count) in groups {
        // Get files for this group, excluding files in black_hole
        let mut files_stmt = conn.prepare(
            "SELECT f.id, f.path
             FROM duplicate_group_files dgf
             JOIN files f ON f.id = dgf.file_id
             WHERE dgf.group_id = ?1
             AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = f.id)
             ORDER BY f.path",
        )?;

        let files: Vec<(i64, String)> = files_stmt
            .query_map(params![group_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // Skip if fewer than 2 files (no longer duplicates after black_hole filtering)
        if files.len() < 2 {
            continue;
        }

        let file_ids: Vec<i64> = files.iter().map(|(id, _)| *id).collect();
        let file_paths: Vec<String> = files.iter().map(|(_, path)| path.clone()).collect();

        result.push(DuplicateGroup {
            id: Some(group_id),
            size,
            content_hash: hash,
            file_count: files.len() as i32, // Use actual count after filtering
            file_paths,
            file_ids,
            files: Vec::new(),
        });
    }

    enrich_duplicate_groups(conn, &mut result)?;
    Ok(result)
}

/// Check if duplicate cache exists and has data
pub fn has_duplicate_cache(conn: &Connection, use_content_hash: bool) -> Result<bool> {
    let hash_type = if use_content_hash {
        "content"
    } else {
        "metadata"
    };
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM duplicate_groups WHERE hash_type = ?1",
        params![hash_type],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Check if file already exists in database
pub fn file_exists(conn: &Connection, path: &str) -> Result<bool> {
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// ============================================================================
// Settings Operations
// ============================================================================

/// Get a setting value by key
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let result: Result<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    );

    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Set a setting value (insert or update)
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        params![key, value, now],
    )?;
    Ok(())
}

/// Delete a setting by key
pub fn delete_setting(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

// ============================================================================
// Frames Set Operations
// ============================================================================

/// Create a new frames_set and return its ID
pub fn create_frames_set(
    conn: &Connection,
    name: Option<&str>,
    is_custom: bool,
    date_obs_start: Option<&str>,
    date_obs_end: Option<&str>,
    objctra: Option<&str>,
    objctdec: Option<&str>,
    total_exp_time: Option<f64>,
    avg_rotation: Option<f64>,
    min_rotation: Option<f64>,
    max_rotation: Option<f64>,
) -> Result<i64> {
    let is_custom_int = if is_custom { 1 } else { 0 };
    conn.execute(
        "INSERT INTO frames_set (name, is_custom, date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, avg_rotation, min_rotation, max_rotation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![name, is_custom_int, date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, avg_rotation, min_rotation, max_rotation],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all frames sets with member counts
pub fn get_frames_sets_by_project(
    conn: &Connection,
    _project_id: i64, // Kept for backwards compatibility, but ignored
) -> Result<Vec<(crate::models::FramesSet, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT fs.id, fs.name, fs.is_custom, fs.date_obs_start, fs.date_obs_end, fs.objctra, fs.objctdec, fs.total_exp_time, fs.flat_pattern,
                COUNT(DISTINCT sm.frame_id) as member_count, fs.avg_rotation, fs.min_rotation, fs.max_rotation, fs.is_archived,
                fs.archived_at, fs.archive_operation_id, fs.uuid, fs.updated_at
         FROM frames_set fs
         LEFT JOIN imaging_nights in_tbl ON fs.id = in_tbl.frames_set_id
         LEFT JOIN sessions s ON in_tbl.id = s.imaging_night_id
         LEFT JOIN session_members sm ON s.id = sm.session_id
         GROUP BY fs.id
         ORDER BY fs.date_obs_start DESC, fs.name ASC"
    )?;

    let sets = stmt.query_map(params![], |row| {
        let set = crate::models::FramesSet {
            id: Some(row.get(0)?),
            name: row.get(1)?,
            is_custom: row.get::<_, i32>(2)? == 1,
            date_obs_start: row.get(3)?,
            date_obs_end: row.get(4)?,
            objctra: row.get(5)?,
            objctdec: row.get(6)?,
            total_exp_time: row.get(7)?,
            flat_pattern: row.get(8)?,
            avg_rotation: row.get(10)?,
            min_rotation: row.get(11)?,
            max_rotation: row.get(12)?,
            is_archived: row.get::<_, i32>(13).unwrap_or(0) == 1,
            archived_at: row.get(14)?,
            archive_operation_id: row.get(15)?,
            uuid: row.get(16)?,
            updated_at: row.get(17)?,
        };
        let member_count: i32 = row.get(9)?;
        Ok((set, member_count as usize))
    })?;

    sets.collect()
}

/// Get all frame IDs that are already members of any frames_set
pub fn get_all_frames_set_member_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
         ORDER BY sm.frame_id",
    )?;

    let frame_ids = stmt.query_map(params![], |row| row.get(0))?;

    frame_ids.collect()
}

/// Delete a frames_set (cascade will delete members)
pub fn delete_frames_set(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM frames_set WHERE id = ?1", params![id])?;
    Ok(())
}

/// Delete all auto-generated frames_sets (where is_custom = 0)
pub fn delete_auto_generated_frame_sets(conn: &Connection) -> Result<usize> {
    let count = conn.execute("DELETE FROM frames_set WHERE is_custom = 0", params![])?;
    Ok(count)
}

/// Update frames_set name
pub fn update_frames_set_name(conn: &Connection, id: i64, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE frames_set SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

/// Update frames_set metadata
pub fn update_frames_set_metadata(
    conn: &Connection,
    id: i64,
    date_obs_start: Option<&str>,
    date_obs_end: Option<&str>,
    objctra: Option<&str>,
    objctdec: Option<&str>,
    total_exp_time: Option<f64>,
    is_custom: bool,
    avg_rotation: Option<f64>,
    min_rotation: Option<f64>,
    max_rotation: Option<f64>,
) -> Result<()> {
    let is_custom_int = if is_custom { 1 } else { 0 };
    conn.execute(
        "UPDATE frames_set
         SET date_obs_start = ?1, date_obs_end = ?2, objctra = ?3, objctdec = ?4,
             total_exp_time = ?5, is_custom = ?6, avg_rotation = ?7, min_rotation = ?8, max_rotation = ?9
         WHERE id = ?10",
        params![date_obs_start, date_obs_end, objctra, objctdec, total_exp_time, is_custom_int, avg_rotation, min_rotation, max_rotation, id],
    )?;
    Ok(())
}

/// Set the archived status of a frames_set
pub fn set_frame_set_archived(conn: &Connection, id: i64, archived: bool) -> Result<()> {
    let archived_int = if archived { 1 } else { 0 };
    conn.execute(
        "UPDATE frames_set SET is_archived = ?1 WHERE id = ?2",
        params![archived_int, id],
    )?;
    Ok(())
}

/// Get all LIGHT frames for a project (for clustering)
pub fn get_light_frames_for_project(
    conn: &Connection,
    _project_id: i64,
) -> Result<Vec<(i64, crate::models::Frame)>> {
    // For now, we'll get all LIGHT frames regardless of project
    // In the future, we can add project filtering at the frame level
    let mut stmt = conn.prepare(
        "SELECT f.id, fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override, fr.uuid, fr.updated_at
         FROM files f
         INNER JOIN frames fr ON f.id = fr.file_id
         WHERE fr.imagetyp = 'Light' OR fr.imagetyp IS NULL
         ORDER BY f.id"
    )?;

    let results = stmt.query_map(params![], |row| {
        let file_id: i64 = row.get(0)?;

        let date_obs_str: Option<String> = row.get(4)?;
        let date_obs = date_obs_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });

        let imagetyp_str: Option<String> = row.get(9)?;
        let imagetyp = imagetyp_str.and_then(|s| crate::models::ImageType::from_str(&s));

        let frame = crate::models::Frame {
            id: Some(row.get(1)?),
            file_id: row.get(2)?,
            object: row.get(3)?,
            date_obs,
            telescop: row.get(5)?,
            instrume: row.get(6)?,
            exptime: row.get(7)?,
            filter: row.get(8)?,
            imagetyp,
            is_master: row.get::<_, i32>(10)? == 1,
            gain: row.get(11)?,
            offset: row.get(12)?,
            binning: row.get(13)?,
            xbinning: row.get(14)?,
            ybinning: row.get(15)?,
            ccd_temp: row.get(16)?,
            set_temp: row.get(17)?,
            focallen: row.get(18)?,
            xpixsz: row.get(19)?,
            ypixsz: row.get(20)?,
            naxis1: row.get(21)?,
            naxis2: row.get(22)?,
            ra: row.get(23)?,
            dec: row.get(24)?,
            sitelat: row.get(25)?,
            lat_obs: row.get(26)?,
            sitelong: row.get(27)?,
            long_obs: row.get(28)?,
            objctra: row.get(29)?,
            objctdec: row.get(30)?,
            override_: row.get::<_, i32>(31)? == 1,
            swcreate: None,
            bayerpat: None,
            xbayroff: None,
            ybayroff: None,
            roworder: None,
            rotation: None,
            uuid: row.get(32)?,
            updated_at: row.get(33)?,
        };

        Ok((file_id, frame))
    })?;

    results.collect()
}

// ============================================================================
// Imaging Nights and Sessions Operations
// ============================================================================

/// Create a new imaging night
pub fn create_imaging_night(
    conn: &Connection,
    frames_set_id: i64,
    start_time: &str,
    end_time: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
         VALUES (?1, ?2, ?3)",
        params![frames_set_id, start_time, end_time],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a new session within an imaging night
pub fn create_session(
    conn: &Connection,
    imaging_night_id: i64,
    instrume: &str,
    frame_count: i32,
    total_exp_time: Option<f64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sessions (imaging_night_id, instrume, frame_count, total_exp_time)
         VALUES (?1, ?2, ?3, ?4)",
        params![imaging_night_id, instrume, frame_count, total_exp_time],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert session members (bulk insert).
///
/// Tolerates being called either standalone (caller passes a fresh `&conn`)
/// or from inside an outer transaction (caller passes `&tx`, which derefs to
/// `&Connection`). Standalone callers get a local batch transaction for
/// fsync amortization; in-transaction callers join the existing tx so we
/// don't trigger SQLite's "cannot start a transaction within a transaction".
pub fn insert_session_members(conn: &Connection, session_id: i64, frame_ids: &[i64]) -> Result<()> {
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        for frame_id in frame_ids {
            tx.execute(
                "INSERT OR IGNORE INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
        }
        tx.commit()?;
    } else {
        for frame_id in frame_ids {
            conn.execute(
                "INSERT OR IGNORE INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
        }
    }
    Ok(())
}

/// Check if sessions exist for a frame set
pub fn sessions_exist_for_frame_set(conn: &Connection, frames_set_id: i64) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = ?1",
        params![frames_set_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Reassign an imaging night to a different frame set
pub fn reassign_imaging_night_to_frame_set(
    conn: &Connection,
    night_id: i64,
    new_frames_set_id: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE imaging_nights SET frames_set_id = ?1 WHERE id = ?2",
        params![new_frames_set_id, night_id],
    )?;
    Ok(())
}

/// Move sessions from one night to another.
///
/// Tolerates outer-transaction callers (see `insert_session_members` above
/// for the same pattern and rationale).
pub fn move_sessions_to_night(
    conn: &Connection,
    session_ids: &[i64],
    target_night_id: i64,
) -> Result<()> {
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        for session_id in session_ids {
            tx.execute(
                "UPDATE sessions SET imaging_night_id = ?1 WHERE id = ?2",
                params![target_night_id, session_id],
            )?;
        }
        tx.commit()?;
    } else {
        for session_id in session_ids {
            conn.execute(
                "UPDATE sessions SET imaging_night_id = ?1 WHERE id = ?2",
                params![target_night_id, session_id],
            )?;
        }
    }
    Ok(())
}

/// Update imaging night time range
pub fn update_imaging_night_time_range(
    conn: &Connection,
    night_id: i64,
    start_time: &str,
    end_time: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE imaging_nights SET start_time = ?1, end_time = ?2 WHERE id = ?3",
        params![start_time, end_time, night_id],
    )?;
    Ok(())
}

/// Deduplicate session members within a frame set
/// Three phases:
/// 1. Per-session: remove identical frame_id duplicates within a single session
/// 2. Cross-session: remove different frame_ids that reference the same physical file (same files.path)
/// 3. Session consolidation: merge sessions with the same instrume within the same imaging night
pub fn deduplicate_session_members_in_set(conn: &Connection, frames_set_id: i64) -> Result<usize> {
    let mut total_removed = 0;

    // All three phases plus the trailing count refresh are one unit: Phase 1
    // deletes-then-reinserts session_members rows, so a failure part-way
    // through used to leave members dropped and counts stale (audit M1).
    // A savepoint nests, so an outer-transaction caller keeps working.
    let sp = SavepointGuard::new(conn, "dedup_session_members")?;

    // --- Phase 1: Per-session frame_id dedup (original logic) ---
    let session_ids: Vec<i64> = conn
        .prepare(
            "SELECT s.id
             FROM sessions s
             JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
             WHERE in_tbl.frames_set_id = ?1",
        )?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for session_id in &session_ids {
        let duplicates: Vec<(i64, i32)> = conn
            .prepare(
                "SELECT frame_id, COUNT(*) as count
                 FROM session_members
                 WHERE session_id = ?1
                 GROUP BY frame_id
                 HAVING count > 1",
            )?
            .query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        for (frame_id, count) in duplicates {
            conn.execute(
                "DELETE FROM session_members WHERE session_id = ?1 AND frame_id = ?2",
                params![session_id, frame_id],
            )?;
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )?;
            total_removed += (count - 1) as usize;
        }
    }

    // --- Phase 2: Cross-session dedup by file path ---
    // Different frame_ids can reference the same physical file (same files.path)
    // after a scan root is removed and re-added. Keep the lowest frame_id, remove others.
    let path_members: Vec<(i64, i64, String)> = conn
        .prepare(
            "SELECT sm.frame_id, sm.session_id, f.path
             FROM session_members sm
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             JOIN frames fr ON sm.frame_id = fr.id
             JOIN files f ON fr.file_id = f.id
             WHERE n.frames_set_id = ?1
             ORDER BY f.path, sm.frame_id",
        )?
        .query_map(params![frames_set_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut seen_paths: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for (frame_id, session_id, path) in &path_members {
        if let Some(&kept_id) = seen_paths.get(path) {
            if *frame_id != kept_id {
                conn.execute(
                    "DELETE FROM session_members WHERE session_id = ?1 AND frame_id = ?2",
                    params![session_id, frame_id],
                )?;
                total_removed += 1;
            }
        } else {
            seen_paths.insert(path.clone(), *frame_id);
        }
    }

    // --- Phase 3: Session consolidation ---
    // Merge sessions with the same instrume within the same imaging night.
    let night_ids: Vec<i64> = conn
        .prepare("SELECT id FROM imaging_nights WHERE frames_set_id = ?1")?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for night_id in &night_ids {
        // Get sessions ordered by instrume then by member count descending (keep largest)
        let sessions: Vec<(i64, String, i64)> = conn
            .prepare(
                "SELECT s.id, COALESCE(s.instrume, ''), COUNT(sm.frame_id) as cnt
                 FROM sessions s
                 LEFT JOIN session_members sm ON s.id = sm.session_id
                 WHERE s.imaging_night_id = ?1
                 GROUP BY s.id, s.instrume
                 ORDER BY COALESCE(s.instrume, ''), cnt DESC",
            )?
            .query_map(params![night_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut instrume_primary: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for (session_id, instrume, _count) in &sessions {
            if let Some(&primary_id) = instrume_primary.get(instrume) {
                // Move members from duplicate session to primary (ignore conflicts from PK)
                conn.execute(
                    "UPDATE OR IGNORE session_members SET session_id = ?1 WHERE session_id = ?2",
                    params![primary_id, session_id],
                )?;
                // Delete any that couldn't be moved (already exist in primary)
                conn.execute(
                    "DELETE FROM session_members WHERE session_id = ?1",
                    params![session_id],
                )?;
                // Delete the now-empty session
                conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
            } else {
                instrume_primary.insert(instrume.clone(), *session_id);
            }
        }
    }

    // --- Update frame_count and total_exp_time on surviving sessions ---
    let surviving_sessions: Vec<i64> = conn
        .prepare(
            "SELECT s.id
             FROM sessions s
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             WHERE n.frames_set_id = ?1",
        )?
        .query_map(params![frames_set_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for session_id in &surviving_sessions {
        conn.execute(
            "UPDATE sessions SET
                frame_count = (SELECT COUNT(*) FROM session_members WHERE session_id = ?1),
                total_exp_time = COALESCE(
                    (SELECT SUM(fr.exptime) FROM session_members sm
                     JOIN frames fr ON sm.frame_id = fr.id
                     WHERE sm.session_id = ?1), 0.0)
             WHERE id = ?1",
            params![session_id],
        )?;
    }

    sp.commit()?;
    Ok(total_removed)
}

/// Get all imaging nights for a frame set (simplified, just nights without sessions)
pub fn get_imaging_nights_for_set(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<crate::models::ImagingNight>> {
    let mut stmt = conn.prepare(
        "SELECT id, frames_set_id, start_time, end_time, created_at
         FROM imaging_nights
         WHERE frames_set_id = ?1
         ORDER BY start_time ASC",
    )?;

    let nights = stmt.query_map(params![frames_set_id], |row| {
        let created_at_str: Option<String> = row.get(4)?;
        let created_at = created_at_str
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(crate::models::ImagingNight {
            id: Some(row.get(0)?),
            frames_set_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            created_at,
        })
    })?;

    nights.collect()
}

/// Get all sessions for an imaging night
pub fn get_sessions_for_night(
    conn: &Connection,
    night_id: i64,
) -> Result<Vec<crate::models::Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, imaging_night_id, instrume, frame_count, total_exp_time, created_at, uuid, updated_at
         FROM sessions
         WHERE imaging_night_id = ?1
         ORDER BY created_at ASC"
    )?;

    let sessions = stmt.query_map(params![night_id], |row| {
        let created_at_str: Option<String> = row.get(5)?;
        let created_at = created_at_str
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(crate::models::Session {
            id: Some(row.get(0)?),
            imaging_night_id: row.get(1)?,
            instrume: row.get(2)?,
            frame_count: row.get(3)?,
            total_exp_time: row.get(4)?,
            created_at,
            uuid: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;

    sessions.collect()
}

/// Get frames with their files by frame IDs (for session generation during auto-clustering)
pub fn get_frames_with_files_by_ids(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Vec<(i64, crate::models::File, crate::models::Frame)>> {
    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build the query with placeholders for each ID
    let placeholders = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                f.uuid, f.updated_at, fr.uuid, fr.updated_at
         FROM frames fr
         JOIN files f ON fr.file_id = f.id
         WHERE fr.id IN ({})
         ORDER BY fr.date_obs ASC",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;

    // Convert frame_ids to params
    let params_vec: Vec<&dyn rusqlite::ToSql> = frame_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let results = stmt.query_map(params_vec.as_slice(), |row| {
        let file = crate::models::File {
            id: row.get(0)?,
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
            format: if row.get::<_, String>(5)? == "FITS" {
                crate::models::FileFormat::FITS
            } else {
                crate::models::FileFormat::XISF
            },
            created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
            metadata_hash: row.get(7)?,
            content_hash: row.get(8)?,
            archived_in_operation: row.get(9)?,
            archive_zip_path: row.get(10)?,
            archive_path_in_zip: row.get(11)?,
            uuid: row.get(43)?,
            updated_at: row.get(44)?,
        };

        let date_obs_raw: Option<String> = row.get(15)?;

        let frame = crate::models::Frame {
            id: row.get(12)?,
            file_id: row.get(13)?,
            object: row.get(14)?,
            date_obs: date_obs_raw.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }),
            telescop: row.get(16)?,
            instrume: row.get(17)?,
            exptime: row.get(18)?,
            filter: row.get(19)?,
            imagetyp: row
                .get::<_, Option<String>>(20)?
                .and_then(|s| crate::models::ImageType::from_str(&s)),
            is_master: row.get::<_, i32>(21)? == 1,
            gain: row.get(22)?,
            offset: row.get(23)?,
            binning: row.get(24)?,
            xbinning: row.get(25)?,
            ybinning: row.get(26)?,
            ccd_temp: row.get(27)?,
            set_temp: row.get(28)?,
            focallen: row.get(29)?,
            xpixsz: row.get(30)?,
            ypixsz: row.get(31)?,
            naxis1: row.get(32)?,
            naxis2: row.get(33)?,
            ra: row.get(34)?,
            dec: row.get(35)?,
            sitelat: row.get(36)?,
            lat_obs: row.get(37)?,
            sitelong: row.get(38)?,
            long_obs: row.get(39)?,
            objctra: row.get(40)?,
            objctdec: row.get(41)?,
            override_: row.get::<_, i32>(42)? == 1,
            swcreate: None,
            bayerpat: None,
            xbayroff: None,
            ybayroff: None,
            roworder: None,
            rotation: None,
            uuid: row.get(45)?,
            updated_at: row.get(46)?,
        };

        let file_id: i64 = row.get(0)?;
        Ok((file_id, file, frame))
    })?;

    results.collect()
}

/// Get imaging nights with sessions for a frame set
pub fn get_imaging_nights_with_sessions(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<crate::models::ImagingNightWithSessions>> {
    // Get all imaging nights for this frame set
    let mut nights_stmt = conn.prepare(
        "SELECT id, frames_set_id, start_time, end_time, created_at
         FROM imaging_nights
         WHERE frames_set_id = ?1
         ORDER BY start_time ASC",
    )?;

    let nights = nights_stmt.query_map(params![frames_set_id], |row| {
        Ok(crate::models::ImagingNight {
            id: row.get(0)?,
            frames_set_id: row.get(1)?,
            start_time: row.get(2)?,
            end_time: row.get(3)?,
            created_at: row.get::<_, Option<String>>(4)?.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }),
        })
    })?;

    let mut result = Vec::new();

    for night in nights {
        let night = night?;
        let night_id = night.id.unwrap();

        // Get sessions for this night
        let mut sessions_stmt = conn.prepare(
            "SELECT id, imaging_night_id, instrume, frame_count, total_exp_time, created_at, uuid, updated_at
             FROM sessions
             WHERE imaging_night_id = ?1
             ORDER BY instrume ASC",
        )?;

        let sessions = sessions_stmt.query_map(params![night_id], |row| {
            Ok(crate::models::Session {
                id: row.get(0)?,
                imaging_night_id: row.get(1)?,
                instrume: row.get(2)?,
                frame_count: row.get(3)?,
                total_exp_time: row.get(4)?,
                created_at: row.get::<_, Option<String>>(5)?.and_then(|s| {
                    DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                }),
                uuid: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;

        let mut sessions_with_frames = Vec::new();

        for session in sessions {
            let session = session?;
            let session_id = session.id.unwrap();

            // Get frames for this session
            let mut frames_stmt = conn.prepare(
                "SELECT f.id, f.path, f.filename, f.size, f.modified_at, f.format, f.created_at, f.metadata_hash, f.content_hash,
                        f.archived_in_operation, f.archive_zip_path, f.archive_path_in_zip,
                        fr.id, fr.file_id, fr.object, fr.date_obs, fr.telescop, fr.instrume,
                        fr.exptime, fr.filter, fr.imagetyp, fr.is_master, fr.gain, fr.offset, fr.binning,
                        fr.xbinning, fr.ybinning, fr.ccd_temp, fr.set_temp, fr.focallen,
                        fr.xpixsz, fr.ypixsz, fr.naxis1, fr.naxis2, fr.ra, fr.dec, fr.sitelat, fr.lat_obs,
                        fr.sitelong, fr.long_obs, fr.objctra, fr.objctdec, fr.override,
                        f.uuid, f.updated_at, fr.uuid, fr.updated_at
                 FROM session_members sm
                 JOIN frames fr ON sm.frame_id = fr.id
                 JOIN files f ON fr.file_id = f.id
                 WHERE sm.session_id = ?1
                 ORDER BY fr.date_obs ASC",
            )?;

            let frames = frames_stmt.query_map(params![session_id], |row| {
                let file = crate::models::File {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    filename: row.get(2)?,
                    size: row.get(3)?,
                    modified_at: parse_stored_ts("files.modified_at", &row.get::<_, String>(4)?),
                    format: if row.get::<_, String>(5)? == "FITS" {
                        crate::models::FileFormat::FITS
                    } else {
                        crate::models::FileFormat::XISF
                    },
                    created_at: parse_stored_ts("files.created_at", &row.get::<_, String>(6)?),
                    metadata_hash: row.get(7)?,
                    content_hash: row.get(8)?,
                    archived_in_operation: row.get(9)?,
                    archive_zip_path: row.get(10)?,
                    archive_path_in_zip: row.get(11)?,
                    uuid: row.get(43)?,
                    updated_at: row.get(44)?,
                };

                let frame = crate::models::Frame {
                    id: row.get(12)?,
                    file_id: row.get(13)?,
                    object: row.get(14)?,
                    date_obs: row.get::<_, Option<String>>(15)?.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                    telescop: row.get(16)?,
                    instrume: row.get(17)?,
                    exptime: row.get(18)?,
                    filter: row.get(19)?,
                    imagetyp: row
                        .get::<_, Option<String>>(20)?
                        .and_then(|s| crate::models::ImageType::from_str(&s)),
                    is_master: row.get::<_, i32>(21)? == 1,
                    gain: row.get(22)?,
                    offset: row.get(23)?,
                    binning: row.get(24)?,
                    xbinning: row.get(25)?,
                    ybinning: row.get(26)?,
                    ccd_temp: row.get(27)?,
                    set_temp: row.get(28)?,
                    focallen: row.get(29)?,
                    xpixsz: row.get(30)?,
                    ypixsz: row.get(31)?,
                    naxis1: row.get(32)?,
                    naxis2: row.get(33)?,
                    ra: row.get(34)?,
                    dec: row.get(35)?,
                    sitelat: row.get(36)?,
                    lat_obs: row.get(37)?,
                    sitelong: row.get(38)?,
                    long_obs: row.get(39)?,
                    objctra: row.get(40)?,
                    objctdec: row.get(41)?,
                    override_: row.get::<_, i32>(42)? == 1,
                    swcreate: None,
                    bayerpat: None,
                    xbayroff: None,
                    ybayroff: None,
                    roworder: None,
                    rotation: None,
                    uuid: row.get(45)?,
                    updated_at: row.get(46)?,
                };

                Ok(crate::models::FileWithFrame {
                    file,
                    frame: Some(frame),
                })
            })?;

            let frames_vec: Result<Vec<_>> = frames.collect();
            sessions_with_frames.push(crate::models::SessionWithFrames {
                session,
                frames: frames_vec?,
            });
        }

        result.push(crate::models::ImagingNightWithSessions {
            imaging_night: night,
            sessions: sessions_with_frames,
        });
    }

    Ok(result)
}

// ============================================================================
// Custom Frames Set Operations
// ============================================================================

/// Get frame IDs for a given session
pub fn get_frame_ids_for_session(conn: &Connection, session_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT frame_id FROM session_members WHERE session_id = ?1")?;

    let frame_ids = stmt.query_map(params![session_id], |row| row.get(0))?;
    frame_ids.collect()
}

// ============================================================================
// Excluded Frames Operations
// ============================================================================

/// Clear all excluded frames
pub fn clear_excluded_frames(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM excluded_frames", [])?;
    Ok(())
}

/// Insert excluded frames in batch
///
/// Tolerates outer-transaction callers: the local batch transaction is only
/// opened when the connection is in autocommit, same pattern (and reason) as
/// `insert_session_members` above — an unconditional `unchecked_transaction`
/// here would commit the CALLER's transaction early (audit M2).
pub fn insert_excluded_frames(conn: &Connection, entries: &[(i64, String)]) -> Result<()> {
    let tx = if conn.is_autocommit() {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    let mut stmt =
        conn.prepare_cached("INSERT INTO excluded_frames (file_id, reason) VALUES (?1, ?2)")?;
    for (file_id, reason) in entries {
        stmt.execute(params![file_id, reason])?;
    }
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(())
}

/// Get count of excluded frames
pub fn get_excluded_frames_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM excluded_frames", [], |row| row.get(0))
}

/// Map a set of selected paths (files and/or folders) to the catalog frame
/// ids they cover. For each `path`, a frame qualifies when its file is either
/// an exact `files.path` match (the path names a file) or lives under
/// `"<path>"` + the path's native separator (the path names a folder —
/// matched recursively at any depth).
///
/// Results are DISTINCT and returned in ascending, stable order: a
/// `BTreeSet` dedupes across paths, so an overlapping file + parent-folder
/// selection yields each frame once. Empty input or non-cataloged paths yield
/// an empty vec.
///
/// Path form: matches the STORED `files.path` verbatim — same contract as
/// `get_file_by_path`. No macOS `/Volumes` ↔ `/private/Volumes` normalization
/// is applied here; the dual-pane browser selects the same path form the
/// scanner recorded, so an exact/prefix compare against the stored string is
/// correct. The folder branch uses an exact-case, wildcard-safe byte-range
/// prefix (`path >= "<path>" + native separator AND path < upper`, via
/// `path_prefix_upper`) —
/// the same compare `rename_files_path_prefix` uses. Unlike the
/// ASCII-case-insensitive `LIKE` it replaced, `>=`/`<` on TEXT are
/// case-SENSITIVE (so a `/M31` select never sweeps a `/m31` sibling on a
/// case-sensitive FS — Linux desktop + Docker/web) and treat `_`/`%` in the
/// folder's own name literally (so `/M31_L` never sweeps a `/M31xL` sibling).
/// The trailing `/` still stops a name-prefix sibling like `/M31extra` from
/// matching `/M31`, and this now agrees with the case-sensitive exact-file
/// branch.
pub fn frame_ids_under_paths(conn: &Connection, paths: &[String]) -> Result<Vec<i64>> {
    use std::collections::BTreeSet;

    let mut ids: BTreeSet<i64> = BTreeSet::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fr.id
           FROM files f
           JOIN frames fr ON fr.file_id = f.id
          WHERE f.path = ?1
             OR (f.path >= ?2 AND (?3 IS NULL OR f.path < ?3))",
    )?;
    for path in paths {
        // Descendants of a selected FOLDER: `[prefix, upper)` byte-range where
        // `prefix` carries the trailing separator and `upper` is its exclusive
        // upper bound (NULL only when the prefix is all `char::MAX` — then the
        // guard falls back to a lower-bound-only match).
        let sep = native_separator_of(path);
        let prefix = format!("{}{}", path.trim_end_matches(sep), sep);
        let upper = path_prefix_upper(&prefix);
        let rows = stmt.query_map(params![path, prefix, upper], |row| row.get::<_, i64>(0))?;
        for r in rows {
            ids.insert(r?);
        }
    }
    Ok(ids.into_iter().collect())
}

/// Delete excluded frames by file IDs
pub fn delete_excluded_frames_by_file_ids(conn: &Connection, file_ids: &[i64]) -> Result<usize> {
    if file_ids.is_empty() {
        return Ok(0);
    }
    let placeholders: String = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let values: Vec<rusqlite::types::Value> = file_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    let sql = format!(
        "DELETE FROM excluded_frames WHERE file_id IN ({})",
        placeholders
    );
    let deleted = conn.execute(&sql, rusqlite::params_from_iter(values.iter()))?;
    Ok(deleted)
}

/// Per-frame-set entry in a memberships summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetMembership {
    pub frame_set_id: i64,
    pub name: Option<String>,
    /// How many of the queried `frame_ids` are members of this frame set.
    pub selected_count: i64,
}

/// Per-calibration-set entry in a memberships summary, used for both
/// "member of" (the frame is a Dark/Flat/Bias/DarkFlat in this set) and
/// "consumer of" (the frame is a LIGHT/etc that *uses* this set's
/// calibrations) listings. `calibration_type` is set only for the
/// consumer case.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationSetMembership {
    pub calibration_set_id: i64,
    pub imagetyp: String,
    pub calibration_type: Option<String>,
    pub instrume: Option<String>,
    pub gain: Option<f64>,
    pub binning: Option<String>,
    pub exptime: Option<f64>,
    pub filter: Option<String>,
    pub ccd_temp: Option<f64>,
    pub date: String,
    pub selected_count: i64,
}

/// "Used in frame set" — for the calibration sets the queried frames are
/// MEMBERS of, this resolves the chain calibration_set_to_frames →
/// session_members → sessions → imaging_nights → frames_set so the user
/// sees which Objects ultimately get calibrated by these frames.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedInFrameSetMembership {
    pub frame_set_id: i64,
    pub name: Option<String>,
    /// Calibration role the selection's calibration set fills for this
    /// frame_set (Dark / Flat / Bias / DarkFlat).
    pub calibration_type: String,
    /// Distinct consumer frames (lights/etc.) in `frame_set_id` that
    /// reference one of the queried frames' calibration sets.
    pub used_for_frame_count: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMembershipsSummary {
    pub frame_sets: Vec<FrameSetMembership>,
    pub calibration_set_member: Vec<CalibrationSetMembership>,
    pub calibration_set_consumer: Vec<CalibrationSetMembership>,
    pub used_in_frame_sets: Vec<UsedInFrameSetMembership>,
    pub frame_count_total: i64,
    pub frame_count_unaffiliated: i64,
}

/// Aggregate the framesets a list of frames is a part of: the top-level
/// frame_set ("Object") via session_members, the calibration sets the
/// frames are members OF, and the calibration sets the frames CONSUME.
/// Counts are deduped per (frame_id, set_id) so a frame that appears in
/// multiple sessions of the same frame_set still counts once.
pub fn get_frame_memberships_summary(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<FrameMembershipsSummary> {
    let mut summary = FrameMembershipsSummary {
        frame_count_total: frame_ids.len() as i64,
        ..Default::default()
    };
    if frame_ids.is_empty() {
        return Ok(summary);
    }

    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    // Frame sets (Objects) — distinct frame_id within each frame_set.
    let sql = format!(
        "SELECT fs.id, fs.name, COUNT(DISTINCT sm.frame_id) AS selected_count
         FROM frames_set fs
         JOIN imaging_nights inight ON inight.frames_set_id = fs.id
         JOIN sessions s ON s.imaging_night_id = inight.id
         JOIN session_members sm ON sm.session_id = s.id
         WHERE sm.frame_id IN ({})
         GROUP BY fs.id, fs.name
         ORDER BY selected_count DESC, fs.name",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(FrameSetMembership {
            frame_set_id: row.get(0)?,
            name: row.get(1)?,
            selected_count: row.get(2)?,
        })
    })?;
    for r in rows {
        summary.frame_sets.push(r?);
    }

    // Calibration set MEMBER (frame is a calibration frame in this set).
    let sql_member = format!(
        "SELECT cs.id, cs.imagetyp, cs.instrume, cs.gain, cs.binning, cs.exptime,
                cs.filter, cs.ccd_temp, cs.date,
                COUNT(DISTINCT csf.frame_id) AS selected_count
         FROM calibration_set cs
         JOIN calibration_set_frames csf ON csf.set_id = cs.id
         WHERE csf.frame_id IN ({})
         GROUP BY cs.id
         ORDER BY selected_count DESC, cs.date DESC",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_member)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(CalibrationSetMembership {
            calibration_set_id: row.get(0)?,
            imagetyp: row.get(1)?,
            calibration_type: None,
            instrume: row.get(2)?,
            gain: row.get(3)?,
            binning: row.get(4)?,
            exptime: row.get(5)?,
            filter: row.get(6)?,
            ccd_temp: row.get(7)?,
            date: row.get(8)?,
            selected_count: row.get(9)?,
        })
    })?;
    for r in rows {
        summary.calibration_set_member.push(r?);
    }

    // Calibration set CONSUMER (frame uses this set as Dark/Flat/Bias/DarkFlat).
    let sql_consumer = format!(
        "SELECT cs.id, cs.imagetyp, cstf.calibration_type, cs.instrume, cs.gain,
                cs.binning, cs.exptime, cs.filter, cs.ccd_temp, cs.date,
                COUNT(DISTINCT cstf.source_id) AS selected_count
         FROM calibration_set_to_frames cstf
         JOIN calibration_set cs ON cs.id = cstf.calibration_set_id
         WHERE cstf.source_type = 'frame' AND cstf.source_id IN ({})
         GROUP BY cs.id, cstf.calibration_type
         ORDER BY cstf.calibration_type, selected_count DESC",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_consumer)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(CalibrationSetMembership {
            calibration_set_id: row.get(0)?,
            imagetyp: row.get(1)?,
            calibration_type: row.get(2)?,
            instrume: row.get(3)?,
            gain: row.get(4)?,
            binning: row.get(5)?,
            exptime: row.get(6)?,
            filter: row.get(7)?,
            ccd_temp: row.get(8)?,
            date: row.get(9)?,
            selected_count: row.get(10)?,
        })
    })?;
    for r in rows {
        summary.calibration_set_consumer.push(r?);
    }

    // "Used in frame set" — for each calibration set the queried frames are
    // members of, find the LIGHTs that consume that cal set and roll them up
    // to their frame_set ("Object"). Group by (frame_set, calibration_type)
    // so a Dark whose set is shared between two Objects shows two rows.
    let sql_used_in = format!(
        "SELECT fs.id, fs.name, cstf.calibration_type,
                COUNT(DISTINCT cstf.source_id) AS used_count
         FROM calibration_set_frames csf
         JOIN calibration_set_to_frames cstf
              ON cstf.calibration_set_id = csf.set_id AND cstf.source_type = 'frame'
         JOIN session_members sm   ON sm.frame_id = cstf.source_id
         JOIN sessions s           ON s.id = sm.session_id
         JOIN imaging_nights inight ON inight.id = s.imaging_night_id
         JOIN frames_set fs        ON fs.id = inight.frames_set_id
         WHERE csf.frame_id IN ({})
         GROUP BY fs.id, fs.name, cstf.calibration_type
         ORDER BY used_count DESC, fs.name",
        placeholders
    );
    let mut stmt = conn.prepare(&sql_used_in)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        Ok(UsedInFrameSetMembership {
            frame_set_id: row.get(0)?,
            name: row.get(1)?,
            calibration_type: row.get(2)?,
            used_for_frame_count: row.get(3)?,
        })
    })?;
    for r in rows {
        summary.used_in_frame_sets.push(r?);
    }

    // Frames that are not in ANY frame set (orphan lights, calibration frames, etc).
    let sql_unaffiliated = format!(
        "SELECT COUNT(*) FROM frames f
         WHERE f.id IN ({}) AND NOT EXISTS (
             SELECT 1 FROM session_members sm WHERE sm.frame_id = f.id
         )",
        placeholders
    );
    summary.frame_count_unaffiliated = conn.query_row(
        &sql_unaffiliated,
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;

    Ok(summary)
}

/// Re-evaluate the `override` flag for each given frame: if every editable
/// field on the frame now equals what the original FITS header said, drop
/// the flag back to 0. Otherwise leave it alone (`bulk_update_frame_metadata`
/// already sets it to 1 when it writes anything, so this is the inverse).
///
/// Used to make per-field revert "stick" — once a user has reverted every
/// field that was ever changed, the frame visually returns to its
/// "untouched" appearance (no pencil indicator in the file browser).
///
/// Only frames currently flagged `override = 1` are inspected; clean rows
/// are skipped so the cost is proportional to actual edits.
pub fn recompute_override_flag_for_frames(conn: &Connection, frame_ids: &[i64]) -> Result<usize> {
    use crate::fits_parser::stored_header::{parse_stored_header_keys, snapshot_from_keys};
    use crate::models::FileFormat;

    if frame_ids.is_empty() {
        return Ok(0);
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let sql = format!(
        "SELECT fr.id, fr.object, fr.filter, fr.imagetyp, fr.instrume, fr.telescop,
                fr.focallen, fr.gain, fr.offset, fr.binning, fr.exptime, fr.ccd_temp,
                fr.date_obs, fr.ra, fr.dec, files.format, fh.header
         FROM frames fr
         JOIN files ON files.id = fr.file_id
         LEFT JOIN fits_header fh ON fh.file_id = fr.file_id
         WHERE fr.id IN ({}) AND fr.override = 1",
        placeholders
    );

    type FrameRow = (
        i64,
        Option<String>, // object
        Option<String>, // filter
        Option<String>, // imagetyp
        Option<String>, // instrume
        Option<String>, // telescop
        Option<f64>,    // focallen
        Option<f64>,    // gain
        Option<f64>,    // offset
        Option<String>, // binning
        Option<f64>,    // exptime
        Option<f64>,    // ccd_temp
        Option<String>, // date_obs
        Option<f64>,    // ra
        Option<f64>,    // dec
        String,         // files.format
        Option<String>, // header
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<FrameRow> = stmt
        .query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut cleared = 0usize;
    for r in rows {
        let (
            frame_id,
            object,
            filter,
            imagetyp,
            instrume,
            telescop,
            focallen,
            gain,
            offset,
            binning,
            exptime,
            ccd_temp,
            date_obs,
            ra,
            dec,
            format_str,
            header,
        ) = r;

        let format = match format_str.as_str() {
            "FITS" => FileFormat::FITS,
            "XISF" => FileFormat::XISF,
            _ => continue,
        };
        let header_text = match header {
            Some(h) if !h.is_empty() => h,
            // No stored header → can't safely conclude "matches original",
            // so leave override = 1.
            _ => continue,
        };
        let keys = parse_stored_header_keys(format, &header_text);
        let snap = snapshot_from_keys(frame_id, &keys);

        let all_match = opt_str_eq(&object, &snap.object)
            && opt_str_eq(&filter, &snap.filter)
            && opt_str_eq(&imagetyp, &snap.imagetyp)
            && opt_str_eq(&instrume, &snap.instrume)
            && opt_str_eq(&telescop, &snap.telescop)
            && opt_f64_eq(&focallen, &snap.focallen)
            && opt_f64_eq(&gain, &snap.gain)
            && opt_f64_eq(&offset, &snap.offset)
            && opt_str_eq(&binning, &snap.binning)
            && opt_f64_eq(&exptime, &snap.exptime)
            && opt_f64_eq(&ccd_temp, &snap.ccd_temp)
            && opt_date_eq(&date_obs, &snap.date_obs)
            && opt_f64_eq(&ra, &snap.ra)
            && opt_f64_eq(&dec, &snap.dec);

        if all_match {
            conn.execute("UPDATE frames SET override = 0 WHERE id = ?1", [frame_id])?;
            cleared += 1;
        }
    }
    Ok(cleared)
}

fn opt_str_eq(a: &Option<String>, b: &Option<String>) -> bool {
    let an = a.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let bn = b.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
    match (an, bn) {
        (None, None) => true,
        (Some(av), Some(bv)) => av == bv,
        _ => false,
    }
}

fn opt_f64_eq(a: &Option<f64>, b: &Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(av), Some(bv)) => (!av.is_finite() && !bv.is_finite()) || (av - bv).abs() <= 1e-6,
        _ => false,
    }
}

/// Compare two ISO-ish datetime strings as instants. Handles the "FITS
/// header has no timezone marker, catalog stores RFC3339 with Z" mismatch
/// by treating naive timestamps as UTC (FITS DATE-OBS convention).
fn opt_date_eq(a: &Option<String>, b: &Option<String>) -> bool {
    use chrono::{DateTime, NaiveDateTime, Utc};
    fn parse_to_utc(s: &str) -> Option<DateTime<Utc>> {
        let s = s.trim();
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        for fmt in &["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
            if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
                return Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
            }
        }
        None
    }
    match (a, b) {
        (None, None) => true,
        (Some(av), Some(bv)) => match (parse_to_utc(av), parse_to_utc(bv)) {
            (Some(da), Some(db)) => da == db,
            _ => av.trim() == bv.trim(),
        },
        _ => false,
    }
}

/// Pull the original (last-scanned) header values for the given frames out
/// of `fits_header` and decode them into canonical fields. Used by the
/// metadata pane to show "what the file said before you edited it" and
/// to support per-field revert.
pub fn get_frame_metadata_originals(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Vec<crate::fits_parser::stored_header::FrameOriginalSnapshot>> {
    use crate::fits_parser::stored_header::{
        parse_stored_header_keys, snapshot_from_keys, FrameOriginalSnapshot,
    };
    use crate::models::FileFormat;

    if frame_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let sql = format!(
        "SELECT fr.id, files.format, fh.header
         FROM frames fr
         JOIN files ON files.id = fr.file_id
         LEFT JOIN fits_header fh ON fh.file_id = fr.file_id
         WHERE fr.id IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(id_params.iter()), |row| {
        let frame_id: i64 = row.get(0)?;
        let format_str: String = row.get(1)?;
        let header: Option<String> = row.get(2)?;
        Ok((frame_id, format_str, header))
    })?;

    let mut out: Vec<FrameOriginalSnapshot> = Vec::new();
    for r in rows {
        let (frame_id, format_str, header) = r?;
        let format = match format_str.as_str() {
            "FITS" => FileFormat::FITS,
            "XISF" => FileFormat::XISF,
            _ => continue, // Unknown format — skip rather than guess.
        };
        match header {
            Some(text) if !text.is_empty() => {
                let keys = parse_stored_header_keys(format, &text);
                out.push(snapshot_from_keys(frame_id, &keys));
            }
            // No stored header (rare — may pre-date the fits_header table or
            // a DB without re-scan after the migration). Emit an empty snapshot
            // so the UI can still show "no original recorded".
            _ => out.push(FrameOriginalSnapshot {
                frame_id,
                ..Default::default()
            }),
        }
    }
    Ok(out)
}

/// Counts of the relation rows that would be removed if `bulk_update_frame_metadata`
/// were called for the given frame_ids. Drives the pre-save unlink warning.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMetadataRelations {
    /// Frames that are *members* of a calibration set.
    pub calibration_set_member_count: i64,
    /// Frames that *consume* a calibration set (linked as a calibration consumer).
    pub calibration_set_consumer_count: i64,
    /// Frames that are in a session (and therefore in an imaging night / frame set).
    pub session_member_count: i64,
    /// Total distinct frames affected (deduped across the three buckets).
    pub affected_frame_count: i64,
}

/// Count how many of `frame_ids` have rows in `calibration_set_frames`,
/// `calibration_set_to_frames`, and `session_members`. Used by the dual-pane
/// metadata editor to warn the user before applying edits that will unlink
/// the affected frames.
pub fn count_frame_metadata_relations(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<FrameMetadataRelations> {
    if frame_ids.is_empty() {
        return Ok(FrameMetadataRelations::default());
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let id_params: Vec<rusqlite::types::Value> = frame_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();

    let csf_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT frame_id) FROM calibration_set_frames WHERE frame_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;
    let cstf_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT source_id) FROM calibration_set_to_frames
             WHERE source_type = 'frame' AND source_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;
    let sm_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT frame_id) FROM session_members WHERE frame_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(id_params.iter()),
        |r| r.get(0),
    )?;

    // Deduped affected-frame count across all three junctions.
    let affected: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM (
                SELECT frame_id AS fid FROM calibration_set_frames WHERE frame_id IN ({p})
                UNION
                SELECT source_id AS fid FROM calibration_set_to_frames
                  WHERE source_type = 'frame' AND source_id IN ({p})
                UNION
                SELECT frame_id AS fid FROM session_members WHERE frame_id IN ({p})
            )",
            p = placeholders
        ),
        rusqlite::params_from_iter(
            id_params
                .iter()
                .chain(id_params.iter())
                .chain(id_params.iter()),
        ),
        |r| r.get(0),
    )?;

    Ok(FrameMetadataRelations {
        calibration_set_member_count: csf_count,
        calibration_set_consumer_count: cstf_count,
        session_member_count: sm_count,
        affected_frame_count: affected,
    })
}

/// Rewire `files.path` for every catalog row whose path begins with
/// `old_prefix`, replacing only the leading prefix with `new_prefix`.
/// Returns the number of rows updated.
///
/// The naive approach `REPLACE(path, old, new)` is wrong: it substitutes
/// EVERY occurrence of the prefix in the path. If a path happens to repeat
/// the old segment elsewhere (e.g. user has `/data/2025/data/foo.fits` and
/// renames `/data/2025`), REPLACE mangles the inner segments. SUBSTR-based
/// prefix swap only touches the leading prefix and is safe regardless of
/// repetition.
///
/// Both `old_prefix` and `new_prefix` MUST end with a path separator so the
/// `[old_prefix, upper)` byte-range predicate only matches descendants and
/// not sibling folders sharing the same name root. The range predicate is
/// also exact-case and treats `old_prefix` literally — unlike `LIKE`, it
/// can't cross-match a differently-cased sibling or one containing `%`/`_`.
pub fn rename_files_path_prefix(
    conn: &Connection,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<usize> {
    let old_hi = path_prefix_upper(old_prefix);
    let n = conn.execute(
        "UPDATE files
         SET path = ?1 || SUBSTR(path, LENGTH(?2) + 1)
         WHERE path >= ?2 AND (?3 IS NULL OR path < ?3)",
        rusqlite::params![new_prefix, old_prefix, old_hi],
    )?;
    Ok(n)
}

/// Get all excluded frames with full file + frame metadata, the exclusion
/// reason, and the duplicate-group flag — the shape the Excluded Frames page
/// needs to drive its Missing-Metadata-style repair toolbar.
///
/// Reuses `MISSING_METADATA_SELECT` and `map_missing_metadata_row` so the
/// row shape stays in lock-step with the missing-metadata view; adds two
/// trailing columns for `reason` and `excluded_at`. Sorted newest-first by
/// `excluded_at`.
pub fn get_excluded_frames_with_metadata(
    conn: &Connection,
) -> Result<Vec<crate::models::ExcludedFrameRow>> {
    let query = format!(
        "{select}, ef.reason, ef.excluded_at
         FROM excluded_frames ef
         INNER JOIN files f ON ef.file_id = f.id
         INNER JOIN frames fr ON fr.file_id = f.id
         LEFT JOIN duplicate_group_files dgf ON dgf.file_id = f.id
         GROUP BY f.id
         ORDER BY ef.excluded_at DESC",
        select = MISSING_METADATA_SELECT,
    );

    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], |row| {
        let base = map_missing_metadata_row(row)?;
        // Trailing two columns appended after the MISSING_METADATA_SELECT
        // 50-column block (indices 0..=49).
        let reason: String = row.get(50)?;
        let excluded_at: String = row.get(51)?;
        Ok(crate::models::ExcludedFrameRow {
            file: base.file,
            frame: base.frame,
            has_duplicate: base.has_duplicate,
            reason,
            excluded_at,
        })
    })?;

    rows.collect()
}

#[cfg(test)]
mod bulk_metadata_tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::models::FrameMetadataEdits;
    use rusqlite::Connection;

    /// Insert a `files` + `frames` row pair and return the frame_id.
    fn insert_frame(conn: &Connection, path: &str, imagetyp: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, ?2)",
            rusqlite::params![file_id, imagetyp],
        )
        .unwrap();
        conn.query_row("SELECT id FROM frames WHERE file_id = ?1", [file_id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// Build a frames_set + imaging_night + session chain so a session row
    /// can satisfy its NOT NULL imaging_night_id FK. Returns the session_id.
    fn make_session(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (name, date_obs_start, date_obs_end)
             VALUES ('test', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        let frames_set_id: i64 = conn
            .query_row(
                "SELECT id FROM frames_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z')",
            [frames_set_id],
        )
        .unwrap();
        let imaging_night_id: i64 = conn
            .query_row(
                "SELECT id FROM imaging_nights ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'CamA')",
            [imaging_night_id],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM sessions ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn bulk_update_spares_superseded_source_set_when_last_member_reclassified() {
        // A user reclassifies the raw calibration frames whose set already fed
        // a master build (superseded + provenance-anchored). Unlinking empties
        // the raw set — but the bulk_update cascade's prune must SPARE it (its
        // NO-ACTION FKs would otherwise abort the whole edit with a FOREIGN KEY
        // failure), exactly like the trigger and startup sweep do.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dark_id = insert_frame(&conn, "/data/darks/d.fits", Some("Dark"));

        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (700, 'Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (701, 'MasterDark', '2026-01-01', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (700, ?1)",
            [dark_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = 701 WHERE id = 700",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
                (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (701, 700, '{}', '[]', 'h', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("LIGHT".into())),
            ..Default::default()
        };
        // Pre-fix this aborted with "FOREIGN KEY constraint failed".
        bulk_update_frame_metadata(&conn, &[dark_id], &edits)
            .expect("reclassifying a master-source frame must not FK-fail");

        // Membership cleared, but the lineage shell + provenance survive.
        let member_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = 700",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            member_after, 0,
            "the frame must be unlinked from the raw set"
        );
        let raw_set: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = 700",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            raw_set, 1,
            "the superseded raw set shell must survive the cascade prune"
        );
        let prov: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM master_provenance WHERE source_set_id = 700",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov, 1, "provenance must be preserved");
    }

    #[test]
    fn frame_ids_under_paths_matches_file_and_folder() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Build a temp catalog with files+frames at known paths (reusing the
        // module's `insert_frame` helper, which returns the generated frame_id).
        let l1 = insert_frame(
            &conn,
            "/data/astro/M31/2026-07-12/L_0001.fits",
            Some("Light"),
        );
        let l2 = insert_frame(
            &conn,
            "/data/astro/M31/2026-07-12/L_0002.fits",
            Some("Light"),
        );
        let f1 = insert_frame(&conn, "/data/astro/M31/flats/F_0001.fits", Some("Flat"));
        // A real subfolder directly under M31 — a folder select must still
        // sweep descendants at any depth after the LIKE→range-compare fix.
        let c1 = insert_frame(&conn, "/data/astro/M31/2026/c.fits", Some("Light"));
        let _other = insert_frame(&conn, "/data/other/x.fits", Some("Light"));

        // exact file → its frame
        assert_eq!(
            frame_ids_under_paths(&conn, &["/data/astro/M31/2026-07-12/L_0001.fits".into()])
                .unwrap(),
            vec![l1]
        );

        // folder → all frames under it (recursive), sorted/deduped
        let mut got = frame_ids_under_paths(&conn, &["/data/astro/M31".into()]).unwrap();
        got.sort();
        let mut expect = vec![l1, l2, f1, c1];
        expect.sort();
        assert_eq!(got, expect);

        // a file + an overlapping folder → deduped
        let mut both = frame_ids_under_paths(
            &conn,
            &[
                "/data/astro/M31/2026-07-12/L_0001.fits".into(),
                "/data/astro/M31".into(),
            ],
        )
        .unwrap();
        both.sort();
        assert_eq!(both, expect);

        // a sibling folder sharing a name prefix must NOT be swept in by the
        // LIKE (the trailing `/` in the prefix guards against `/M31extra/…`).
        let m31_extra = insert_frame(&conn, "/data/astro/M31extra/z.fits", Some("Light"));
        let mut only_m31 = frame_ids_under_paths(&conn, &["/data/astro/M31".into()]).unwrap();
        only_m31.sort();
        assert_eq!(
            only_m31, expect,
            "M31extra sibling must not match /data/astro/M31"
        );
        assert!(!only_m31.contains(&m31_extra));

        // OVER-MATCH GUARD 1 (wildcard chars): `_`/`%` in the selected folder's
        // own name must be matched LITERALLY, not as SQL `LIKE` wildcards.
        // Selecting `/data/astro/M31_L` must not sweep the sibling
        // `/data/astro/M31xL` (underscores are pervasive in astro paths).
        let wc_target = insert_frame(&conn, "/data/astro/M31_L/a.fits", Some("Light"));
        let wc_sibling = insert_frame(&conn, "/data/astro/M31xL/b.fits", Some("Light"));
        let got_wc = frame_ids_under_paths(&conn, &["/data/astro/M31_L".into()]).unwrap();
        assert_eq!(
            got_wc,
            vec![wc_target],
            "underscore in the folder name must be literal, not a LIKE wildcard"
        );
        assert!(!got_wc.contains(&wc_sibling));

        // OVER-MATCH GUARD 2 (case): the folder branch must be case-SENSITIVE,
        // agreeing with the exact-file branch. On a case-sensitive FS
        // (Linux desktop + Docker/web) selecting `/data/astro/M31` must not
        // sweep the sibling `/data/astro/m31`.
        let case_sibling = insert_frame(&conn, "/data/astro/m31/b.fits", Some("Light"));
        let mut got_case = frame_ids_under_paths(&conn, &["/data/astro/M31".into()]).unwrap();
        got_case.sort();
        assert_eq!(
            got_case, expect,
            "lowercase m31 sibling must not match /data/astro/M31 (case-sensitive)"
        );
        assert!(!got_case.contains(&case_sibling));

        // non-cataloged path → empty
        assert!(frame_ids_under_paths(&conn, &["/nope".into()])
            .unwrap()
            .is_empty());

        // empty input → empty
        assert!(frame_ids_under_paths(&conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn frame_ids_under_paths_windows_backslash_paths() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let a = insert_frame(&conn, r"C:\Astro\M31\L_0001.fits", Some("Light"));
        let b = insert_frame(&conn, r"C:\Astro\M31\sub\L_0002.fits", Some("Light"));
        let sib = insert_frame(&conn, r"C:\Astro\M31extra\L_0003.fits", Some("Light"));

        // Folder select sweeps descendants at any depth — the reported Windows
        // bug returned an empty list here (hardcoded '/' prefix).
        let mut got = frame_ids_under_paths(&conn, &[r"C:\Astro\M31".into()]).unwrap();
        got.sort();
        let mut expect = vec![a, b];
        expect.sort();
        assert_eq!(got, expect);

        // Trailing backslash tolerated (trim_end_matches('/') was a no-op).
        let mut got = frame_ids_under_paths(&conn, &[r"C:\Astro\M31\".into()]).unwrap();
        got.sort();
        assert_eq!(got, expect);

        // Sibling folder sharing a name prefix must not be swept in.
        assert!(!got.contains(&sib));

        // Exact-file branch unchanged.
        assert_eq!(
            frame_ids_under_paths(&conn, &[r"C:\Astro\M31extra\L_0003.fits".into()]).unwrap(),
            vec![sib]
        );
    }

    #[test]
    fn imagetyp_change_unlinks_calibration_set_membership_and_sessions() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dark_id = insert_frame(&conn, "/x/dark1.fits", Some("Dark"));

        // Create a calibration set and put the frame in it as a member.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        let set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, dark_id],
        )
        .unwrap();

        // Create a session (with parent imaging_night + frames_set) and put
        // the frame in it.
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, dark_id],
        )
        .unwrap();

        // Also link as a consumer to a calibration set.
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![dark_id, set_id],
        ).unwrap();

        // Sanity: links exist before edit.
        let csf_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1",
                [dark_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(csf_count, 1);

        // Reclassify the frame: Dark → Light.
        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("LIGHT".into())),
            ..Default::default()
        };
        let changed = bulk_update_frame_metadata(&conn, &[dark_id], &edits).unwrap();
        assert_eq!(changed, 1);

        // imagetyp updated.
        let new_type: String = conn
            .query_row(
                "SELECT imagetyp FROM frames WHERE id = ?1",
                [dark_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_type, "Light");

        // All three junction tables should no longer reference this frame.
        let csf_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1",
                [dark_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(csf_after, 0, "calibration_set_frames should be cleared");

        let cstf_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_to_frames WHERE source_type='frame' AND source_id = ?1",
            [dark_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(cstf_after, 0, "calibration_set_to_frames should be cleared");

        let sm_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
                [dark_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sm_after, 0, "session_members should be cleared");

        // calibration_set with no surviving members IS pruned (master sets
        // and last-frame regular sets — see
        // `changing_imagetyp_prunes_now_empty_master_calibration_set`).
        let set_still_there: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_still_there, 0,
            "now-empty calibration_set should be pruned"
        );
        // Sessions / imaging_nights / frames_set are still left intact —
        // their pruning policy is unchanged.
        let session_still_there: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(session_still_there, 1);
    }

    #[test]
    fn any_edit_unlinks_relations_not_just_imagetyp() {
        // Per design decision: any metadata edit invalidates the frame's
        // relations. Even editing OBJECT alone should unlink.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/light1.fits", Some("Light"));

        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        )
        .unwrap();

        let edits = FrameMetadataEdits {
            object: Some(Some("M31".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let sm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sm_count, 0, "OBJECT edit must unlink session membership");
    }

    #[test]
    fn bulk_update_frame_metadata_nests_inside_an_outer_transaction() {
        // The write section runs inside a SAVEPOINT, which nests. A raw
        // `BEGIN` would fail here with "cannot start a transaction within a
        // transaction", and an outer rollback must still take the edit with
        // it — callers that wrap this in their own transaction keep
        // all-or-nothing semantics.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/nested.fits", Some("Light"));

        let tx = conn.unchecked_transaction().unwrap();
        let edits = FrameMetadataEdits {
            object: Some(Some("M31".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
        drop(tx); // rollback — the edit must vanish with it

        let obj: Option<String> = conn
            .query_row("SELECT object FROM frames WHERE id = ?1", [frame_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(obj, None, "outer rollback must undo the savepointed edit");
    }

    #[test]
    fn bulk_update_frame_metadata_rolls_back_the_edit_when_a_cascade_fails() {
        // audit I1: the frames UPDATE (override stamp included), the three
        // cascade DELETEs and the empty-set prune are ONE unit. Before the
        // savepoint each was its own autocommit, so a failure partway left
        // the frame edited-and-flagged with only part of its now-stale links
        // removed — silently violating the "ANY edit unlinks" invariant.
        //
        // The abort is injected with a trigger on the LAST cascade table
        // (session_members): deterministic, and the same shape as the real
        // constraint failures this path can hit.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/atomic.fits", Some("Dark"));

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        let set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        )
        .unwrap();

        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        )
        .unwrap();

        conn.execute_batch(
            "CREATE TRIGGER injected_session_members_failure
             BEFORE DELETE ON session_members
             BEGIN SELECT RAISE(ABORT, 'injected cascade failure'); END",
        )
        .unwrap();

        let edits = FrameMetadataEdits {
            object: Some(Some("M31".into())),
            ..Default::default()
        };
        let err = bulk_update_frame_metadata(&conn, &[frame_id], &edits);
        assert!(
            err.is_err(),
            "injected cascade failure must surface as an error"
        );

        // Everything the call had already written is gone: the edit itself,
        // the override stamp, and the earlier cascade DELETE.
        let (obj, override_flag): (Option<String>, i64) = conn
            .query_row(
                "SELECT object, override FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(obj, None, "failed cascade must roll back the metadata edit");
        assert_eq!(
            override_flag, 0,
            "failed cascade must roll back the override stamp"
        );

        let csf_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            csf_count, 1,
            "the cascade DELETE that did run must be rolled back too"
        );

        let sm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sm_count, 1, "the aborted DELETE must have changed nothing");
    }

    #[test]
    fn empty_edits_do_not_cascade() {
        // If no fields are set in the edits payload, the function early-returns
        // 0 and must not touch junction tables.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        )
        .unwrap();

        let edits = FrameMetadataEdits::default();
        let n = bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
        assert_eq!(n, 0);

        let sm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_members WHERE frame_id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sm_count, 1, "empty-edit calls must not cascade");
    }

    #[test]
    fn binning_edit_updates_x_and_y_components() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));

        let edits = FrameMetadataEdits {
            binning: Some(Some("2x2".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (b, xb, yb): (String, i64, i64) = conn
            .query_row(
                "SELECT binning, xbinning, ybinning FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(b, "2x2");
        assert_eq!(xb, 2);
        assert_eq!(yb, 2);
    }

    #[test]
    fn binning_invalid_format_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        for bad in ["foo", "1", "x1", "0x1", "20x20", ""] {
            let edits = FrameMetadataEdits {
                binning: Some(Some(bad.into())),
                ..Default::default()
            };
            let r = bulk_update_frame_metadata(&conn, &[frame_id], &edits);
            assert!(r.is_err(), "binning '{}' should have been rejected", bad);
        }
    }

    #[test]
    fn exptime_zero_or_negative_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/a.fits", Some("Light"));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let edits = FrameMetadataEdits {
                exptime: Some(Some(bad)),
                ..Default::default()
            };
            let r = bulk_update_frame_metadata(&conn, &[frame_id], &edits);
            assert!(r.is_err(), "exptime {} should have been rejected", bad);
        }
    }

    #[test]
    fn rename_files_path_prefix_only_swaps_leading_segment() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert a few rows under /old/A/, including one whose path repeats
        // the old prefix in a non-leading segment — this is the case where
        // SQL REPLACE() would mangle paths.
        let rows = [
            "/old/A/foo.fits",
            "/old/A/sub/bar.fits",
            "/old/A/sub/old/A/baz.fits",     // ← repeated segment inside
            "/old/Acme/leave_me_alone.fits", // ← prefix-of-prefix, must not match
            "/other/quux.fits",              // ← unrelated
        ];
        for p in rows {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, 'x.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                [p],
            )
            .unwrap();
        }

        let n = rename_files_path_prefix(&conn, "/old/A/", "/new/A/").unwrap();
        assert_eq!(n, 3, "should update exactly the three rows under /old/A/");

        let collect = |sql: &str| -> Vec<String> {
            let mut stmt = conn.prepare(sql).unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        let paths = collect("SELECT path FROM files ORDER BY path");
        assert_eq!(
            paths,
            vec![
                "/new/A/foo.fits".to_string(),
                "/new/A/sub/bar.fits".to_string(),
                // The inner '/old/A/' segment is preserved — only the leading
                // prefix was swapped. (REPLACE would have produced /new/A/sub/new/A/baz.fits.)
                "/new/A/sub/old/A/baz.fits".to_string(),
                "/old/Acme/leave_me_alone.fits".to_string(),
                "/other/quux.fits".to_string(),
            ]
        );
    }

    #[test]
    fn rename_files_path_prefix_windows_backslash() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_frame(&conn, r"C:\data\Old\a.fits", Some("Light"));
        insert_frame(&conn, r"C:\data\Old\sub\b.fits", Some("Light"));
        insert_frame(&conn, r"C:\data\Oldextra\c.fits", Some("Light"));

        let n = rename_files_path_prefix(&conn, r"C:\data\Old\", r"C:\data\New\").unwrap();
        assert_eq!(
            n, 2,
            "both descendants rewired, sibling-prefix folder untouched"
        );

        let moved: i64 = conn
            .query_row(
                r"SELECT COUNT(*) FROM files WHERE path LIKE 'C:\data\New\%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(moved, 2);
    }

    /// Stash a believable FITS header text for a frame so the override
    /// recomputation has something to compare against.
    fn insert_fits_header_for_file(conn: &Connection, file_id: i64, header_text: &str) {
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            rusqlite::params![file_id, header_text],
        )
        .unwrap();
    }

    #[test]
    fn override_clears_after_revert_to_original_values() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert a file + frame with object="M42" (matches the FITS header
        // we'll attach below).
        let frame_id = insert_frame(&conn, "/x/m42.fits", Some("Light"));
        let file_id: i64 = conn
            .query_row(
                "SELECT file_id FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute("UPDATE frames SET object = 'M42' WHERE id = ?1", [frame_id])
            .unwrap();

        // Pretend the FITS header was scanned with OBJECT="M42" and
        // IMAGETYP="Light" — matching the frame's stored fields, so any
        // post-revert mismatch isolates to OBJECT.
        let cards = format!(
            "{}\n{}",
            format!("{:<80}", "OBJECT  = 'M42                 '"),
            format!("{:<80}", "IMAGETYP= 'Light               '"),
        );
        insert_fits_header_for_file(&conn, file_id, &cards);

        // Step 1: user edits OBJECT to "M 42" — bulk_update sets override = 1.
        let edits = FrameMetadataEdits {
            object: Some(Some("M 42".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();
        let override_flag: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(override_flag, 1, "edit should set override = 1");

        // Step 2: user reverts OBJECT to the FITS-header original ("M42").
        let revert = FrameMetadataEdits {
            object: Some(Some("M42".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &revert).unwrap();
        let override_flag_after: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            override_flag_after, 0,
            "reverting all changes back to the FITS header should clear override"
        );
    }

    #[test]
    fn override_persists_when_some_field_still_differs_from_original() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let frame_id = insert_frame(&conn, "/x/m42.fits", Some("Light"));
        let file_id: i64 = conn
            .query_row(
                "SELECT file_id FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE frames SET object = 'M42', filter = 'L' WHERE id = ?1",
            [frame_id],
        )
        .unwrap();

        let cards = format!(
            "{}\n{}\n{}",
            format!("{:<80}", "OBJECT  = 'M42                 '"),
            format!("{:<80}", "FILTER  = 'L                   '"),
            format!("{:<80}", "IMAGETYP= 'Light               '"),
        );
        insert_fits_header_for_file(&conn, file_id, &cards);

        // Edit OBJECT and FILTER both. Then revert OBJECT only.
        let edits1 = FrameMetadataEdits {
            object: Some(Some("OTHER".into())),
            filter: Some(Some("R".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits1).unwrap();

        let edits2 = FrameMetadataEdits {
            object: Some(Some("M42".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits2).unwrap();

        let override_flag: i64 = conn
            .query_row(
                "SELECT override FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            override_flag, 1,
            "FILTER still differs from original ('R' vs 'L'), override must stay"
        );
    }

    #[test]
    fn changing_imagetyp_prunes_now_empty_master_calibration_set() {
        // A MasterFlat is a calibration_set with exactly one member frame.
        // When the frame is reclassified to LIGHT, that set is meaningless
        // and must be deleted (along with any consumer references via FK
        // cascade), not left as a zombie.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let masterflat_id = insert_frame(&conn, "/x/master.fits", Some("MasterFlat"));

        // Build the master calibration set + member link + a consumer link.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, date)
             VALUES ('MasterFlat', 'CamA', 100.0, '1x1', '2026-01-01')",
            [],
        )
        .unwrap();
        let cal_set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![cal_set_id, masterflat_id],
        )
        .unwrap();

        // A LIGHT in another frame consumes this MasterFlat — the FK cascade
        // on calibration_set_to_frames.calibration_set_id should clean this
        // up automatically when the calibration_set itself is deleted.
        let consumer_light = insert_frame(&conn, "/x/light.fits", Some("Light"));
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Flat')",
            rusqlite::params![consumer_light, cal_set_id],
        ).unwrap();

        // Reclassify the MasterFlat → LIGHT.
        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("LIGHT".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[masterflat_id], &edits).unwrap();

        // Calibration set is gone.
        let set_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [cal_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_count, 0,
            "the empty MasterFlat calibration_set should have been pruned"
        );

        // Membership row gone.
        let member_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1",
                [cal_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(member_count, 0);

        // Consumer link gone too (via FK cascade).
        let consumer_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames WHERE calibration_set_id = ?1",
                [cal_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            consumer_count, 0,
            "consumer references should cascade-delete with the set"
        );
    }

    #[test]
    fn changing_one_frame_in_multi_member_set_keeps_set_alive() {
        // Non-master calibration set with two darks. Reclassifying ONE of
        // them should remove the membership row but leave the set (and its
        // other member) intact.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let dark_a = insert_frame(&conn, "/x/dark_a.fits", Some("Dark"));
        let dark_b = insert_frame(&conn, "/x/dark_b.fits", Some("Dark"));

        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, exptime, ccd_temp, date)
             VALUES ('Dark', 'CamA', 100.0, '1x1', 60.0, -10.0, '2026-01-01')",
            [],
        ).unwrap();
        let cal_set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        for fid in [dark_a, dark_b] {
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![cal_set_id, fid],
            )
            .unwrap();
        }

        // Reclassify dark_a → LIGHT.
        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("LIGHT".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[dark_a], &edits).unwrap();

        // Set still exists.
        let set_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = ?1",
                [cal_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_count, 1,
            "set with surviving members must NOT be pruned"
        );

        // dark_b membership intact.
        let surviving_member: i64 = conn
            .query_row(
                "SELECT frame_id FROM calibration_set_frames WHERE set_id = ?1",
                [cal_set_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(surviving_member, dark_b);
    }

    #[test]
    fn imagetyp_master_variant_sets_is_master_flag() {
        // Picking "MasterDark" (or any of its case-insensitive aliases) in
        // the dropdown must store imagetyp='MasterDark' AND is_master=1
        // without the caller having to set is_master separately.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("Dark"));

        for input in ["MASTERDARK", "Master Dark", "MasterDark"] {
            // Clear flag between attempts so each iteration has a meaningful
            // before/after.
            conn.execute("UPDATE frames SET is_master = 0 WHERE id = ?1", [frame_id])
                .unwrap();
            let edits = FrameMetadataEdits {
                imagetyp: Some(Some(input.into())),
                ..Default::default()
            };
            bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

            let (typ, is_master): (String, i64) = conn
                .query_row(
                    "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                    [frame_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                typ, "MasterDark",
                "input '{}' should normalize to MasterDark",
                input
            );
            assert_eq!(is_master, 1, "input '{}' should set is_master=1", input);
        }
    }

    #[test]
    fn imagetyp_base_variant_clears_is_master_flag() {
        // Picking "Dark" must store is_master=0 even if the row was a master
        // before (demotion via dropdown).
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("MasterDark"));
        conn.execute("UPDATE frames SET is_master = 1 WHERE id = ?1", [frame_id])
            .unwrap();

        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("DARK".into())),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (typ, is_master): (String, i64) = conn
            .query_row(
                "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(typ, "Dark");
        assert_eq!(is_master, 0, "is_master must be demoted to 0");
    }

    #[test]
    fn imagetyp_derived_master_flag_overrides_explicit_edit() {
        // If a caller passes both imagetyp=MASTERDARK and is_master=false,
        // the imagetyp-derived flag wins so the row stays consistent.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let frame_id = insert_frame(&conn, "/x/d.fits", Some("Dark"));

        let edits = FrameMetadataEdits {
            imagetyp: Some(Some("MasterDark".into())),
            is_master: Some(false),
            ..Default::default()
        };
        bulk_update_frame_metadata(&conn, &[frame_id], &edits).unwrap();

        let (typ, is_master): (String, i64) = conn
            .query_row(
                "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(typ, "MasterDark");
        assert_eq!(is_master, 1, "imagetyp=MasterDark must force is_master=1");
    }

    #[test]
    fn frame_memberships_summary_aggregates_all_three_relations() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let light = insert_frame(&conn, "/x/l.fits", Some("Light"));
        let dark = insert_frame(&conn, "/x/d.fits", Some("Dark"));
        let lonely = insert_frame(&conn, "/x/o.fits", Some("Light"));

        // light → frame_set "M42"
        let session_id = make_session(&conn);
        conn.execute(
            "UPDATE frames_set SET name = 'M42' WHERE id = (SELECT MIN(id) FROM frames_set)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, light],
        )
        .unwrap();

        // dark → calibration_set member
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, instrume, gain, binning, exptime, ccd_temp, date)
             VALUES ('Dark', 'CamA', 100.0, '1x1', 60.0, -10.0, '2026-01-01')",
            [],
        ).unwrap();
        let cal_set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![cal_set_id, dark],
        )
        .unwrap();

        // light → consumer of cal_set_id (as Dark)
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![light, cal_set_id],
        ).unwrap();

        let summary = get_frame_memberships_summary(&conn, &[light, dark, lonely]).unwrap();

        assert_eq!(summary.frame_count_total, 3);
        // 2 of 3 frames are not in any frame_set (dark, lonely).
        assert_eq!(summary.frame_count_unaffiliated, 2);

        assert_eq!(summary.frame_sets.len(), 1);
        assert_eq!(summary.frame_sets[0].name.as_deref(), Some("M42"));
        assert_eq!(summary.frame_sets[0].selected_count, 1);

        assert_eq!(summary.calibration_set_member.len(), 1);
        assert_eq!(summary.calibration_set_member[0].imagetyp, "Dark");
        assert_eq!(summary.calibration_set_member[0].selected_count, 1);

        assert_eq!(summary.calibration_set_consumer.len(), 1);
        assert_eq!(
            summary.calibration_set_consumer[0]
                .calibration_type
                .as_deref(),
            Some("Dark")
        );
        assert_eq!(summary.calibration_set_consumer[0].selected_count, 1);

        // The dark is a member of the cal set, the light consumes it, and
        // the light belongs to frame_set "M42". So selecting the dark
        // should report "Used in M42 (Dark)".
        assert_eq!(summary.used_in_frame_sets.len(), 1);
        assert_eq!(summary.used_in_frame_sets[0].name.as_deref(), Some("M42"));
        assert_eq!(summary.used_in_frame_sets[0].calibration_type, "Dark");
        assert_eq!(summary.used_in_frame_sets[0].used_for_frame_count, 1);
    }

    #[test]
    fn count_frame_metadata_relations_reports_each_bucket() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let f1 = insert_frame(&conn, "/x/f1.fits", Some("Light"));
        let f2 = insert_frame(&conn, "/x/f2.fits", Some("Dark"));

        // f1: in a session.
        let session_id = make_session(&conn);
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, f1],
        )
        .unwrap();

        // f2: member of a calibration set + consumer of one.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        let set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, f2],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type)
             VALUES (?1, 'frame', ?2, 'Dark')",
            rusqlite::params![f2, set_id],
        ).unwrap();

        let rel = count_frame_metadata_relations(&conn, &[f1, f2]).unwrap();
        assert_eq!(rel.calibration_set_member_count, 1);
        assert_eq!(rel.calibration_set_consumer_count, 1);
        assert_eq!(rel.session_member_count, 1);
        assert_eq!(
            rel.affected_frame_count, 2,
            "f1 and f2 are both linked somewhere"
        );
    }

    /// The metadata pane's revert button must be able to clear a user-typed
    /// XPIXSZ back to NULL on frames whose FITS header had no XPIXSZ. This
    /// only works because `FrameMetadataEdits.xpixsz` is `Option<Option<f64>>`
    /// with a custom serde deserializer — a plain `Option<f64>` would collapse
    /// "no edit" and "set to NULL" into the same `None` and silently drop the
    /// clear (no SET clause emitted).
    #[test]
    fn xpixsz_revert_to_null_clears_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let id = insert_frame(&conn, "/x/a.fits", Some("Light"));

        // Seed a user-typed value.
        let set_edits = FrameMetadataEdits {
            xpixsz: Some(Some(3.76)),
            ..Default::default()
        };
        let n = bulk_update_frame_metadata(&conn, &[id], &set_edits).unwrap();
        assert_eq!(n, 1, "set xpixsz should update 1 row");
        let stored: Option<f64> = conn
            .query_row("SELECT xpixsz FROM frames WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, Some(3.76), "xpixsz must be set after the edit");

        // Now revert to NULL via Some(None) — the wire JSON {xpixsz: null}.
        let clear_edits = FrameMetadataEdits {
            xpixsz: Some(None),
            ..Default::default()
        };
        let n = bulk_update_frame_metadata(&conn, &[id], &clear_edits).unwrap();
        assert_eq!(n, 1, "clear xpixsz should update 1 row");
        let stored: Option<f64> = conn
            .query_row("SELECT xpixsz FROM frames WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(stored.is_none(), "xpixsz must be NULL after the revert");

        // Counter-test: `xpixsz: None` (no edit, all other fields also None)
        // must be a no-op — distinguishable from the clear above.
        let conn2 = Connection::open_in_memory().unwrap();
        init_db(&conn2).unwrap();
        let id2 = insert_frame(&conn2, "/x/b.fits", Some("Light"));
        bulk_update_frame_metadata(
            &conn2,
            &[id2],
            &FrameMetadataEdits {
                xpixsz: Some(Some(2.5)),
                ..Default::default()
            },
        )
        .unwrap();
        let no_edit = FrameMetadataEdits::default(); // every field is the outer None
        let n = bulk_update_frame_metadata(&conn2, &[id2], &no_edit).unwrap();
        assert_eq!(n, 0, "default edits (all outer None) must be a no-op");
        let stored: Option<f64> = conn2
            .query_row("SELECT xpixsz FROM frames WHERE id = ?1", [id2], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            stored,
            Some(2.5),
            "no-edit must NOT clear the previously-set value"
        );
    }

    /// Wire-format check: confirm the custom serde deserializer distinguishes
    /// the three input shapes the metadata pane round-trips through JSON.
    #[test]
    fn xpixsz_serde_distinguishes_absent_null_value() {
        // Absent: outer None → "no edit".
        let edits: FrameMetadataEdits = serde_json::from_str(r#"{}"#).unwrap();
        assert!(edits.xpixsz.is_none(), "absent key → outer None");
        // null: Some(None) → "clear to NULL".
        let edits: FrameMetadataEdits = serde_json::from_str(r#"{"xpixsz": null}"#).unwrap();
        assert_eq!(edits.xpixsz, Some(None), "null → Some(None)");
        // value: Some(Some(v)) → "set to v".
        let edits: FrameMetadataEdits = serde_json::from_str(r#"{"xpixsz": 3.76}"#).unwrap();
        assert_eq!(edits.xpixsz, Some(Some(3.76)), "value → Some(Some(v))");
    }

    /// Regression test for the hand-maintained column indices in
    /// `MISSING_METADATA_SELECT` / `map_missing_metadata_row` /
    /// `get_excluded_frames_with_metadata`. Appending `uuid`/`updated_at` to
    /// the shared SELECT constant shifted `has_duplicate` (45→49) and the
    /// trailing `reason`/`excluded_at` columns (46,47→50,51) — a mismatch
    /// here would be a silent runtime bug, not a compile error.
    #[test]
    fn missing_metadata_and_excluded_frames_carry_uuid_and_shifted_indices() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // insert_frame leaves `object` NULL, which the "object" category treats
        // as missing metadata for LIGHT frames.
        let frame_id = insert_frame(&conn, "/x/missing_obj.fits", Some("Light"));
        let file_id: i64 = conn
            .query_row(
                "SELECT file_id FROM frames WHERE id = ?1",
                [frame_id],
                |r| r.get(0),
            )
            .unwrap();

        let rows = get_frames_with_missing_metadata(&conn, "object").unwrap();
        assert_eq!(rows.len(), 1, "expected exactly one missing-object row");
        let row = &rows[0];
        assert!(
            row.file.uuid.is_some(),
            "file.uuid must be populated by the identity trigger"
        );
        assert!(
            row.file.updated_at.is_some(),
            "file.updated_at must be populated by the identity trigger"
        );
        assert!(
            row.frame.uuid.is_some(),
            "frame.uuid must be populated by the identity trigger"
        );
        assert!(
            row.frame.updated_at.is_some(),
            "frame.updated_at must be populated by the identity trigger"
        );
        assert!(
            !row.has_duplicate,
            "no duplicate_group_files row exists for this file"
        );
        assert_eq!(row.file.id, Some(file_id));
        assert_eq!(row.frame.id, Some(frame_id));

        // Now exclude the frame and check get_excluded_frames_with_metadata's
        // trailing reason/excluded_at columns land correctly after the shift.
        insert_excluded_frames(&conn, &[(file_id, "test-reason".to_string())]).unwrap();

        let excluded = get_excluded_frames_with_metadata(&conn).unwrap();
        assert_eq!(excluded.len(), 1);
        let ex = &excluded[0];
        assert_eq!(
            ex.reason, "test-reason",
            "reason must read from its shifted index"
        );
        assert!(
            !ex.excluded_at.is_empty(),
            "excluded_at must read from its shifted index"
        );
        assert!(ex.file.uuid.is_some());
        assert!(ex.frame.uuid.is_some());
        assert!(!ex.has_duplicate);
    }
}

/// W2-T11: byte-range path-prefix matching (replaces `LIKE`). Covers the
/// `path_prefix_upper` helper directly, plus the case-exactness and
/// literal-wildcard semantics it's meant to fix at the query call sites.
#[cfg(test)]
mod path_prefix_range_tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    // -- path_prefix_upper -------------------------------------------------

    #[test]
    fn upper_bound_of_ascii_prefix_is_unchanged_behaviour() {
        // 'x' (U+0078) successor is 'y' (U+0079) — same result the old
        // byte-increment algorithm gave for plain ASCII.
        assert_eq!(path_prefix_upper("/data/x"), Some("/data/y".to_string()));
    }

    #[test]
    fn upper_bound_of_0xbf_terminated_prefix_is_some() {
        // 'ÿ' = U+00FF encodes as bytes [0xC3, 0xBF]. The OLD byte-increment
        // algorithm incremented the trailing 0xBF to 0xC0, a bare
        // continuation byte — invalid UTF-8 — and returned None, silently
        // degrading every one of these queries to an unbounded
        // lower-bound-only match. The char-successor algorithm instead
        // advances the whole scalar value: succ('\u{FF}') = '\u{100}'.
        assert_eq!(
            path_prefix_upper("/data/test\u{ff}"),
            Some("/data/test\u{100}".to_string()),
            "0xBF-class trailing byte must no longer fall back to None"
        );
    }

    #[test]
    fn upper_bound_of_other_0xbf_class_prefixes_is_some_and_byte_ordered() {
        // '¿' = U+00BF (bytes [0xC2, 0xBF]) and Cyrillic 'п' = U+043F (bytes
        // [0xD0, 0xBF]) both end in a 0xBF byte — the same failure class as
        // 'ÿ' above, exercised on two more codepoints named in the finding.
        for prefix in ["\u{bf}", "\u{43f}"] {
            let upper = path_prefix_upper(prefix)
                .unwrap_or_else(|| panic!("expected Some upper bound for {:?}", prefix));
            assert!(
                upper.as_bytes() > prefix.as_bytes(),
                "upper bound {:?} must sort after prefix {:?} under BINARY/memcmp collation",
                upper,
                prefix
            );
        }
    }

    #[test]
    fn upper_bound_skips_the_utf16_surrogate_gap() {
        // U+D7FF is the last scalar value before the surrogate gap
        // (D800..=DFFF, which are not valid `char`s). The successor must
        // jump straight to U+E000, the first scalar value after the gap.
        let prefix = "/data/x\u{d7ff}";
        assert_eq!(
            path_prefix_upper(prefix),
            Some("/data/x\u{e000}".to_string())
        );
    }

    #[test]
    fn upper_bound_carries_into_previous_char_when_last_is_char_max() {
        // char::MAX (U+10FFFF) has no successor scalar value, so the
        // algorithm must drop it and increment the char before it instead —
        // the same "carry" idiom the old byte-based version used for
        // trailing 0xFF bytes.
        let prefix = format!("/data/x{}", char::MAX);
        assert_eq!(
            path_prefix_upper(&prefix),
            Some("/data/y".to_string()),
            "trailing char::MAX must carry into the previous char"
        );
    }

    #[test]
    fn upper_bound_of_empty_prefix_is_none() {
        assert_eq!(path_prefix_upper(""), None);
    }

    #[test]
    fn upper_bound_of_all_char_max_prefix_is_none() {
        // Every char is char::MAX, so the carry runs off the front of the
        // string with nothing left to increment — the one remaining `None`
        // case for a non-empty prefix, unreachable for a real filesystem
        // path but handled defensively (and logged via tracing::warn! at the
        // call site, per the never-swallow-errors rule).
        let prefix: String = std::iter::repeat(char::MAX).take(2).collect();
        assert_eq!(path_prefix_upper(&prefix), None);
    }

    // -- query-level regression: the 0xBF fallback-gap scenario ------------

    #[test]
    fn rename_prefix_ending_in_0xbf_class_char_does_not_unbound_match() {
        // The reviewer's exact scenario. `/data/x\u{ff}` ('ÿ', bytes
        // [0xC3, 0xBF]) is the failure class from the finding: under the OLD
        // byte-increment algorithm, incrementing the trailing 0xBF produced
        // invalid UTF-8 → `path_prefix_upper` returned `None` → the caller
        // fell back to a lower-bound-only filter (`path >= prefix`), which
        // matches every path sorting after the prefix — including sibling
        // directory `/data/x\u{450}` ('ѐ', bytes [0xD1, 0x90], which sorts
        // after 'ÿ' byte-wise since 0xD1 > 0xC3). That would make an
        // unrelated sibling get swept into the rename/delete cascade.
        //
        // With the fixed char-successor algorithm, the upper bound is
        // `/data/x\u{100}` (bytes [..., 0xC4, 0x80]), which correctly
        // excludes the 'ѐ' sibling (0xD1 > 0xC4) while still including
        // everything actually nested under `/data/x\u{ff}`.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_file(&conn, "/data/x\u{ff}/f.fits");
        insert_file(&conn, "/data/x\u{450}/g.fits");

        let n = rename_files_path_prefix(&conn, "/data/x\u{ff}", "/data/RENAMED").unwrap();
        assert_eq!(
            n, 1,
            "only the file under the /data/x\\u{{ff}} prefix should be renamed"
        );

        let mut paths = all_paths(&conn);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/data/RENAMED/f.fits".to_string(),
                "/data/x\u{450}/g.fits".to_string(),
            ],
            "the \\u{{450}} sibling must be left untouched, not swept in by an unbounded fallback"
        );
    }

    // -- semantic scenarios (Step 3 of the brief) ---------------------------

    fn insert_file(conn: &Connection, path: &str) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'x.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
    }

    fn all_paths(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    fn dir_rename_prefix_is_case_exact() {
        // Old LIKE was ASCII-case-insensitive by default: `/data/Astro%`
        // would have also matched `/data/astro/y.fits`. The byte-range
        // predicate must not.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_file(&conn, "/data/Astro/x.fits");
        insert_file(&conn, "/data/astro/y.fits");

        let n = rename_files_path_prefix(&conn, "/data/Astro/", "/data/AstroRenamed/").unwrap();
        assert_eq!(n, 1, "only the exact-case row should be renamed");

        assert_eq!(
            all_paths(&conn),
            vec![
                "/data/AstroRenamed/x.fits".to_string(),
                "/data/astro/y.fits".to_string(),
            ]
        );
    }

    #[test]
    fn dir_rename_prefix_treats_underscore_literally() {
        // Old LIKE treats '_' as a single-char wildcard, so the pattern
        // `/data/M31_Ha%` would have also matched `/data/M31XHa/...`. The
        // byte-range predicate must treat '_' as a literal character.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_file(&conn, "/data/M31_Ha/a.fits");
        insert_file(&conn, "/data/M31XHa/b.fits");

        let n = rename_files_path_prefix(&conn, "/data/M31_Ha/", "/data/M31_Ha_renamed/").unwrap();
        assert_eq!(n, 1, "only the literal-underscore row should be renamed");

        assert_eq!(
            all_paths(&conn),
            vec![
                "/data/M31XHa/b.fits".to_string(),
                "/data/M31_Ha_renamed/a.fits".to_string(),
            ]
        );
    }

    #[test]
    fn delete_scan_root_cascade_does_not_cross_match_wildcard_siblings() {
        // Same literal-wildcard hazard as above, exercised through the
        // scan-root delete cascade (site 404) instead of the dir-rename.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/M31_Ha')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_file(&conn, "/data/M31_Ha/a.fits");
        insert_file(&conn, "/data/M31XHa/b.fits");

        delete_scan_root(&conn, root_id).unwrap();

        assert_eq!(
            all_paths(&conn),
            vec!["/data/M31XHa/b.fits".to_string()],
            "only the row under the literal /data/M31_Ha prefix should be deleted"
        );
    }

    // -- separator strictness on the three destructive byte-range sites -----
    // `add_scan_root`'s overlap guard is component-wise (`Path::starts_with`),
    // so two roots sharing a NAME prefix (`/data/M31` + `/data/M31_Ha`) are an
    // ordinary, legal configuration. A range built from the bare root path
    // spans them both, because every character that can follow the name sorts
    // above the separator.

    #[test]
    fn delete_scan_root_does_not_purge_name_prefix_sibling_root() {
        // Without a trailing separator the byte range for /data/M31 is
        // ["/data/M31", "/data/M32"), which CONTAINS the sibling root
        // /data/M31_Ha ('_' 0x5F sorts above '/' 0x2F). The predicate must
        // require the separator so only true descendants are swept.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/M31')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_file(&conn, "/data/M31/a.fits");
        insert_file(&conn, "/data/M31_Ha/b.fits");

        delete_scan_root(&conn, root_id).unwrap();

        assert_eq!(
            all_paths(&conn),
            vec!["/data/M31_Ha/b.fits".to_string()],
            "sibling root sharing the name prefix must survive the cascade"
        );
    }

    #[test]
    fn delete_scan_root_does_not_purge_name_prefix_sibling_root_windows() {
        // Windows arm: range for C:\Astro without separator is
        // ["C:\Astro", "C:\Astrp") — contains C:\Astro_backup. With the
        // separator it is ["C:\Astro\", "C:\Astro]") — excludes it.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('C:\\Astro')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_file(&conn, "C:\\Astro\\a.fits");
        insert_file(&conn, "C:\\Astro_backup\\b.fits");

        delete_scan_root(&conn, root_id).unwrap();

        assert_eq!(
            all_paths(&conn),
            vec!["C:\\Astro_backup\\b.fits".to_string()]
        );
    }

    #[test]
    fn reconcile_unique_camera_leaves_name_prefix_sibling_untouched() {
        // reconcile runs on EVERY scan — pre-fix it suffixes the SIBLING
        // root's frames.instrume and then deletes the sibling's calibration
        // sets via the same broken range.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO scan_roots (path, unique_camera) VALUES ('/data/M31', 1)",
            [],
        )
        .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_file(&conn, "/data/M31/a.fits");
        insert_file(&conn, "/data/M31_Ha/b.fits");
        conn.execute(
            "INSERT INTO frames (file_id, instrume) SELECT id, 'CAM' FROM files",
            [],
        )
        .unwrap();

        reconcile_unique_camera_instrume(&conn, root_id).unwrap();

        let own: String = conn
            .query_row(
                "SELECT fr.instrume FROM frames fr JOIN files f ON fr.file_id = f.id WHERE f.path = '/data/M31/a.fits'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(own, format!("CAM N{root_id}"));
        let sibling: String = conn
            .query_row(
                "SELECT fr.instrume FROM frames fr JOIN files f ON fr.file_id = f.id WHERE f.path = '/data/M31_Ha/b.fits'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sibling, "CAM",
            "sibling root's frames must not receive the suffix"
        );
    }

    #[test]
    fn delete_calibration_sets_for_root_spares_name_prefix_sibling_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/M31')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_file(&conn, "/data/M31_Ha/dark.fits");
        conn.execute(
            "INSERT INTO frames (file_id) SELECT id FROM files WHERE path = '/data/M31_Ha/dark.fits'",
            [],
        )
        .unwrap();
        let frame_id: i64 = conn
            .query_row("SELECT id FROM frames ORDER BY id DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Column set mirrors `delete_scan_root_preserves_master_source_lineage`:
        // `imagetyp` + `date` are the table's two NOT NULL columns.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES ('Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        let set_id: i64 = conn
            .query_row(
                "SELECT id FROM calibration_set ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();

        let deleted = delete_calibration_sets_for_root(&conn, root_id).unwrap();

        assert_eq!(deleted, 0, "no set lives under /data/M31");
        let sets: i64 = conn
            .query_row("SELECT COUNT(*) FROM calibration_set", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sets, 1, "sibling's calibration set must survive");
    }

    #[test]
    fn unique_camera_recluster_spares_master_source_lineage() {
        // Regression: `reconcile_unique_camera_instrume` runs on EVERY scan and
        // cascades into `delete_calibration_sets_for_root`. Pre-fix that delete
        // swept in every set holding a frame under the root, master lineage
        // included. Real-world shape: the raw darks sit under the imaging root
        // being toggled, their master sits in the Calibration Library root — so
        // deleting the raw set leaves `master_provenance.source_set_id`
        // (NO ACTION) dangling and the statement aborts with "FOREIGN KEY
        // constraint failed". The whole reconcile runs inside a SavepointGuard,
        // so the instrume rename rolled back with it and EVERY subsequent scan
        // of the root failed identically. An imported master whose own file
        // lives under the root (set 705) had no FK to abort on — it was silently
        // deleted, taking `archive_operations.calibration_set_id`
        // (ON DELETE CASCADE) audit rows with it.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (path, unique_camera) VALUES ('/data/lib', 1)",
            [],
        )
        .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Four files under the root (all get their instrume suffixed) plus the
        // master's own file and a bias member, both OUTSIDE the root.
        for (file_id, frame_id, path) in [
            (900i64, 901i64, "/data/lib/dark_raw.fits"),
            (920, 921, "/data/lib/bias_raw.fits"),
            (930, 931, "/data/lib/light.fits"),
            (950, 951, "/data/lib/imported_master.fits"),
            (910, 911, "/library/master_dark.fits"),
            (940, 941, "/other/bias.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
                 VALUES (?1, ?2, 'x.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                params![file_id, path],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, instrume) VALUES (?1, ?2, 'CAM')",
                params![frame_id, file_id],
            )
            .unwrap();
        }

        // 700 = raw dark set under the root, superseded by master 701 (outside);
        // 702 = plain raw bias set (no lineage, must still be deleted);
        // 703 = a bias set 705 links to as a sub-calibration, member outside;
        // 705 = an imported master whose own file lives under the root.
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (700, 'Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (701, 'MasterDark', '2026-01-01', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (702, 'Bias', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (703, 'Bias', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (705, 'MasterFlat', '2026-01-01', 1)",
            [],
        )
        .unwrap();
        for (set_id, frame_id) in [
            (700i64, 901i64),
            (701, 911),
            (702, 921),
            (703, 941),
            (705, 951),
        ] {
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = 701 WHERE id = 700",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
                (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (701, 700, '{}', '[]', 'h', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        // The under-the-root master's own sub-calibration link.
        conn.execute(
            "INSERT INTO calibration_set_to_frames
                (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (705, 'calibration_set', 703, 'Bias', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        // A light under the root consuming the master.
        conn.execute(
            "INSERT INTO calibration_set_to_frames
                (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (931, 'frame', 701, 'Dark', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        // Archive audit row for the under-the-root master (archive-of-originals).
        conn.execute(
            "INSERT INTO archive_operations
                (id, calibration_set_id, archive_root_path, compression, status, started_at)
             VALUES (800, 705, '/archive', 'stored', 'completed', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Pre-fix this returned Err("FOREIGN KEY constraint failed").
        let result = reconcile_unique_camera_instrume(&conn, root_id)
            .expect("reconcile must not trip over master provenance references");

        // The rename committed — the savepoint was not rolled back.
        assert_eq!(
            result.frames_renamed, 4,
            "every frame under the root is renamed"
        );
        let renamed: String = conn
            .query_row("SELECT instrume FROM frames WHERE id = 901", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(renamed, format!("CAM N{root_id}"));

        // Lineage survives; the plain raw set is the only casualty.
        assert_eq!(
            result.calibration_sets_deleted, 1,
            "only the lineage-free set is deleted"
        );
        let surviving: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id FROM calibration_set ORDER BY id")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            surviving,
            vec![700, 701, 703, 705],
            "superseded raw set, both masters and the sub-calibration target survive; 702 is deleted"
        );

        // Members of the spared sets are untouched (the cascade never saw them).
        let members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id IN (700, 705)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(members, 2, "spared sets keep their member frames");

        // Provenance + archive audit rows survive.
        let prov: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM master_provenance WHERE master_set_id = 701",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov, 1, "the provenance row must survive");
        let ops: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM archive_operations WHERE id = 800",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ops, 1, "the archive audit row must not be CASCADE-dropped");

        // The under-the-root master's sub-calibration link survives — it is only
        // deleted when the master's own id lands in the affected set, which it
        // no longer can.
        let sub_link: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames
                  WHERE source_type = 'calibration_set' AND source_id = 705",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sub_link, 1,
            "the master's sub-calibration link must survive"
        );

        // BY DESIGN, unchanged by this fix: the trailing per-frame link cleanup
        // is unconditional on the target set, so a light under the root loses
        // its link to the surviving master too. The reconcile rewrote every
        // frames.instrume under the root, which invalidates every parameter
        // match — the links are re-established by auto-find
        // (`calibration::processor::process_frame_set`), not by the scan's
        // set rebuild, which only re-creates sets.
        let light_link: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set_to_frames
                  WHERE source_type = 'frame' AND source_id = 931",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            light_link, 0,
            "per-frame links under the root are cleared wholesale"
        );
    }

    #[test]
    fn delete_scan_root_preserves_master_source_lineage() {
        // Regression: a scan root holding raw calibration frames that fed a
        // Phase-2 master build must delete cleanly. Once step 2/7 remove the
        // raw frames, the raw set is member-less but still referenced by
        // `master_provenance.source_set_id` (NO ACTION) and its own
        // `superseded_by_set_id` (NO ACTION). The step-9 prune must SPARE it
        // instead of aborting the whole delete with a FOREIGN KEY failure.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Scan root + one raw dark file/frame under it.
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/darks')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row(
                "SELECT id FROM scan_roots ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
             VALUES (900, '/data/darks/dark_0001.fits', 'dark_0001.fits', 0,
                     '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO frames (id, file_id) VALUES (901, 900)", [])
            .unwrap();

        // Raw set 600 holds that frame; master set 601 (is_master_library=1)
        // supersedes it and carries the provenance anchor back to 600. Note 601
        // is deliberately member-less: without the is_master_library exemption
        // the prune would also try to delete it, which the NO-ACTION
        // `600.superseded_by_set_id → 601` FK would reject too.
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (600, 'Dark', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (601, 'MasterDark', '2026-01-01', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (600, 901)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = 601 WHERE id = 600",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO master_provenance
                (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (601, 600, '{}', '[]', 'h', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Pre-fix this aborted at step 9 with "FOREIGN KEY constraint failed".
        delete_scan_root(&conn, root_id)
            .expect("scan-root delete must not trip over master provenance references");

        // The root's file + frame are gone.
        let files_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE id = 900", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(files_left, 0, "the root's file must be deleted");
        let frames_left: i64 = conn
            .query_row("SELECT COUNT(*) FROM frames WHERE id = 901", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(frames_left, 0, "the root's frame must be deleted");

        // Lineage survives as empty shells, provenance intact.
        let raw: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = 600",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw, 1, "the superseded raw set shell must survive");
        let master: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calibration_set WHERE id = 601",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(master, 1, "the master set must survive");
        let prov: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM master_provenance WHERE master_set_id = 601",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prov, 1, "the provenance row must survive");
    }

    /// The three scan-root-join sites (`find_duplicate_groups` and the two
    /// queries inside `rebuild_duplicate_groups_cache`) were restructured
    /// from a per-row SQL-side `LIKE sr.path || '%'` join into: fetch
    /// eligible roots in Rust, then OR together per-root `[lo, hi)` binds.
    /// Assert result parity against a hand-computed expectation: a duplicate
    /// pair under an eligible root is found; an equally-duplicated pair
    /// under a `find_duplicates = 0` root is not.
    #[test]
    fn scan_root_restructured_queries_match_only_eligible_roots() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/RootA', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/RootB', 0)",
            [],
        )
        .unwrap();

        // Duplicate pair under the eligible root.
        for (p, fname) in [
            ("/data/RootA/dup1.fits", "dup1.fits"),
            ("/data/RootA/dup2.fits", "dup2.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, 100, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'HASH1')",
                rusqlite::params![p, fname],
            ).unwrap();
        }
        // Equally-duplicated pair under the non-eligible root — must be excluded.
        for (p, fname) in [
            ("/data/RootB/dup1.fits", "dup1.fits"),
            ("/data/RootB/dup2.fits", "dup2.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, 100, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'HASH2')",
                rusqlite::params![p, fname],
            ).unwrap();
        }

        // Site 1427 (find_duplicate_groups).
        let groups = find_duplicate_groups(&conn, true).unwrap();
        assert_eq!(
            groups.len(),
            1,
            "only the RootA duplicate pair should be found"
        );
        assert_eq!(groups[0].content_hash, "HASH1");
        let mut found_paths = groups[0].file_paths.clone();
        found_paths.sort();
        assert_eq!(
            found_paths,
            vec![
                "/data/RootA/dup1.fits".to_string(),
                "/data/RootA/dup2.fits".to_string()
            ]
        );

        // Sites 1580 + 1614 (rebuild_duplicate_groups_cache, main query + per-group files query).
        let created = rebuild_duplicate_groups_cache(&conn, true).unwrap();
        assert_eq!(
            created, 1,
            "cache rebuild should also find exactly the RootA group"
        );
        let cached = get_cached_duplicates(&conn, true).unwrap();
        assert_eq!(cached.len(), 1);
        let mut cached_paths = cached[0].file_paths.clone();
        cached_paths.sort();
        assert_eq!(
            cached_paths,
            vec![
                "/data/RootA/dup1.fits".to_string(),
                "/data/RootA/dup2.fits".to_string()
            ]
        );
    }

    #[test]
    fn scan_root_prefix_predicate_with_no_roots_is_always_false() {
        // Pure-function check: zero roots must produce the always-false
        // fragment with no bind values, not an empty OR-chain (which would
        // be a SQL syntax error) or a fragment that accidentally matches
        // everything.
        let (predicate, values) = scan_root_prefix_predicate("f.path", &[]);
        assert_eq!(predicate, "0");
        assert!(values.is_empty());
    }

    // Step 5 decision (task-1-brief §5): nothing pinned the lax (no-separator)
    // behavior, so `scan_root_prefix_predicate` was made separator-strict —
    // same pattern as the three destructive sites above. Consequence of the
    // old behavior was over-inclusion, not destruction: a name-prefix sibling
    // root with `find_duplicates = 0` used to leak into duplicate detection
    // through the eligible root's byte range.
    #[test]
    fn find_duplicate_groups_excludes_name_prefix_sibling_root() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // /data/M31 is eligible; /data/M31_Ha is NOT (find_duplicates = 0)
        // but shares the name prefix ('_' 0x5F sorts above '/' 0x2F).
        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/M31', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/M31_Ha', 0)",
            [],
        )
        .unwrap();

        for (p, fname) in [
            ("/data/M31/dup1.fits", "dup1.fits"),
            ("/data/M31_Ha/dup2.fits", "dup2.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, 100, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'HASH1')",
                rusqlite::params![p, fname],
            ).unwrap();
        }

        let groups = find_duplicate_groups(&conn, true).unwrap();
        assert!(
            groups.is_empty(),
            "the M31_Ha file must not be pulled in through M31's range, so no pair forms"
        );

        let created = rebuild_duplicate_groups_cache(&conn, true).unwrap();
        assert_eq!(created, 0, "cache rebuild must likewise find no groups");
    }

    #[test]
    fn scan_root_restructured_queries_match_nothing_when_no_root_is_eligible() {
        // Same duplicate pair as above, but with zero scan_roots rows at
        // all (not even a `find_duplicates = 0` one) — mirrors the original
        // `EXISTS (SELECT 1 FROM scan_roots sr WHERE ...)` being
        // unconditionally false when the table has nothing to match.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for (p, fname) in [
            ("/data/Orphan/dup1.fits", "dup1.fits"),
            ("/data/Orphan/dup2.fits", "dup2.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, 100, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'HASH1')",
                rusqlite::params![p, fname],
            ).unwrap();
        }

        let groups = find_duplicate_groups(&conn, true).unwrap();
        assert!(
            groups.is_empty(),
            "no scan root is eligible, so nothing should match"
        );

        let created = rebuild_duplicate_groups_cache(&conn, true).unwrap();
        assert_eq!(created, 0, "cache rebuild should likewise find no groups");
    }

    // -- Finding 2: coverage for the two untested migrated sites -----------
    // (`get_files_by_directory` old site 659, `get_files_by_directory_for_camera`
    // old site 766) — same case-exactness / literal-wildcard hazards as the
    // `LIKE`-based sites above, exercised through the user-visible listing
    // queries directly.

    fn insert_file_with_instrume(conn: &Connection, path: &str, instrume: &str) {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, 'x.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, instrume) VALUES (?1, ?2)",
            rusqlite::params![file_id, instrume],
        )
        .unwrap();
    }

    #[test]
    fn get_files_by_directory_is_case_exact_and_wildcard_literal() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_file(&conn, "/data/Astro/a.fits");
        insert_file(&conn, "/data/astro/b.fits");
        insert_file(&conn, "/data/M31_Ha/c.fits");
        insert_file(&conn, "/data/M31XHa/d.fits");

        let astro = get_files_by_directory(&conn, "/data/Astro", None).unwrap();
        assert_eq!(
            astro
                .iter()
                .map(|(f, _)| f.path.clone())
                .collect::<Vec<_>>(),
            vec!["/data/Astro/a.fits".to_string()],
            "case-differing sibling /data/astro must not be swept in"
        );

        let m31 = get_files_by_directory(&conn, "/data/M31_Ha", None).unwrap();
        assert_eq!(
            m31.iter().map(|(f, _)| f.path.clone()).collect::<Vec<_>>(),
            vec!["/data/M31_Ha/c.fits".to_string()],
            "'_' must be treated literally, not as a single-char LIKE wildcard"
        );
    }

    #[test]
    fn get_files_by_directory_for_camera_is_case_exact_and_wildcard_literal() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        insert_file_with_instrume(&conn, "/data/Astro/a.fits", "CamA");
        insert_file_with_instrume(&conn, "/data/astro/b.fits", "CamA");
        insert_file_with_instrume(&conn, "/data/M31_Ha/c.fits", "CamA");
        insert_file_with_instrume(&conn, "/data/M31XHa/d.fits", "CamA");

        let astro = get_files_by_directory_for_camera(&conn, "/data/Astro", "CamA", None).unwrap();
        assert_eq!(
            astro
                .iter()
                .map(|(f, _)| f.path.clone())
                .collect::<Vec<_>>(),
            vec!["/data/Astro/a.fits".to_string()],
            "case-differing sibling /data/astro must not be swept in"
        );

        let m31 = get_files_by_directory_for_camera(&conn, "/data/M31_Ha", "CamA", None).unwrap();
        assert_eq!(
            m31.iter().map(|(f, _)| f.path.clone()).collect::<Vec<_>>(),
            vec!["/data/M31_Ha/c.fits".to_string()],
            "'_' must be treated literally, not as a single-char LIKE wildcard"
        );
    }

    /// Windows-shaped rows listed from a POSIX host: the separator must come
    /// from the DATA (`native_separator_of`), not from the build OS, and the
    /// directory must be trimmed so a drive root (`C:\`) still lists its
    /// direct children instead of building a `C:\\` prefix.
    #[test]
    fn get_files_by_directory_uses_data_separator_and_handles_drive_root() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        for (path, name) in [
            (r"C:\Astro\a.fits", "a.fits"),
            (r"C:\Astro\sub\b.fits", "b.fits"),
            (r"C:\root_level.fits", "root_level.fits"),
        ] {
            // `created_at` is supplied explicitly (like `insert_file_with_instrume`
            // above): the column's SQLite default is not RFC3339, and the row
            // mapper parses it strictly.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                rusqlite::params![path, name],
            )
            .unwrap();
        }
        // Direct children only, keyed off the DATA's separator (not the build OS's):
        let in_astro = get_files_by_directory(&conn, r"C:\Astro", None).unwrap();
        assert_eq!(in_astro.len(), 1, "only the direct child, not sub\\b.fits");
        assert_eq!(in_astro[0].0.filename, "a.fits");
        // Drive root with its trailing separator:
        let at_root = get_files_by_directory(&conn, r"C:\", None).unwrap();
        assert_eq!(at_root.len(), 1);
        assert_eq!(at_root[0].0.filename, "root_level.fits");
    }

    /// Duplicate-group scan-root attribution must respect the separator
    /// boundary. `enrich_duplicate_groups` walks the roots longest-first, so
    /// with roots `/data` and `/data/M31x` the bare `starts_with` it used to
    /// do matched `/data/M31x` against `/data/M31xyz/a.fits` and reported the
    /// wrong owning root in the duplicates UI. Driven through
    /// `enrich_duplicate_groups` directly rather than `find_duplicate_groups`
    /// — the latter needs real content hashes, and the helper's own boundary
    /// cases are already pinned in the scanner tests.
    #[test]
    fn duplicate_group_attribution_respects_separator_boundary() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for root in ["/data", "/data/M31x"] {
            conn.execute(
                "INSERT INTO scan_roots (path, enabled) VALUES (?1, 1)",
                [root],
            )
            .unwrap();
        }
        // The trap file (name-prefix sibling of the deeper root) plus a file
        // genuinely under `/data/M31x`, so the assert proves the boundary is
        // enforced without simply defeating the deeper root everywhere.
        let mut file_ids = Vec::new();
        for path in ["/data/M31xyz/a.fits", "/data/M31x/b.fits"] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at)
                 VALUES (?1, 'dup.fits', 1, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
                [path],
            )
            .unwrap();
            file_ids.push(conn.last_insert_rowid());
        }

        let mut groups = vec![DuplicateGroup {
            id: None,
            size: 1,
            content_hash: "deadbeef".to_string(),
            file_count: 2,
            file_paths: vec![
                "/data/M31xyz/a.fits".to_string(),
                "/data/M31x/b.fits".to_string(),
            ],
            file_ids: file_ids.clone(),
            files: Vec::new(),
        }];
        enrich_duplicate_groups(&conn, &mut groups).unwrap();

        let trap = groups[0]
            .files
            .iter()
            .find(|f| f.path == "/data/M31xyz/a.fits")
            .expect("trap file must be enriched");
        assert_eq!(
            trap.scan_root_path.as_deref(),
            Some("/data"),
            "name-prefix sibling root /data/M31x must not claim /data/M31xyz/a.fits"
        );

        let genuine = groups[0]
            .files
            .iter()
            .find(|f| f.path == "/data/M31x/b.fits")
            .expect("genuine child must be enriched");
        assert_eq!(
            genuine.scan_root_path.as_deref(),
            Some("/data/M31x"),
            "longest-prefix attribution must still win for a true descendant"
        );
    }
}

#[cfg(test)]
mod fingerprint_backfill_tests {
    use super::backfill_null_header_fingerprints;
    use crate::db::schema::init_db;
    use crate::fingerprint::compute_header_fingerprint;
    use rusqlite::Connection;

    #[test]
    fn backfill_fills_null_fingerprints_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Seed a files row + a legacy fits_header row with a NULL fingerprint.
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/x/a.fits', 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let header = r#"{"SIMPLE":true,"OBJECT":"M42"}"#;
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, NULL)",
            rusqlite::params![file_id, header],
        )
        .unwrap();

        // First pass heals the one NULL row with the canonical fingerprint.
        let n = backfill_null_header_fingerprints(&conn).unwrap();
        assert_eq!(n, 1, "one legacy row should be backfilled");
        let fp: Option<String> = conn
            .query_row(
                "SELECT header_fingerprint FROM fits_header WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fp.as_deref(),
            Some(compute_header_fingerprint(header).as_str()),
            "backfilled value must match compute_header_fingerprint",
        );

        // Second pass is a no-op (idempotent).
        let n2 = backfill_null_header_fingerprints(&conn).unwrap();
        assert_eq!(n2, 0, "idempotent: nothing left to backfill");
    }

    #[test]
    fn init_db_self_heals_null_fingerprints_on_relaunch() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Seed a legacy NULL-fingerprint header row (as a pre-fingerprint DB would have).
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/x/b.fits', 'b.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, NULL)",
            rusqlite::params![file_id, r#"{"SIMPLE":true}"#],
        )
        .unwrap();

        // A subsequent init_db (app relaunch) must self-heal the legacy NULL row.
        init_db(&conn).unwrap();
        let nulls: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fits_header WHERE header_fingerprint IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            nulls, 0,
            "init_db self-heal should leave no NULL fingerprints"
        );
    }
}

/// Coverage for W2-T7 (v0.2.2 hygiene): the three raw `BEGIN`/`COMMIT` sites
/// migrated to `SavepointGuard` — `reconcile_unique_camera_instrume`,
/// `delete_scan_root`, `rebuild_duplicate_groups_cache`. Raw `BEGIN` errors
/// with "cannot start a transaction within a transaction" whenever a
/// transaction is already open on the connection; `SAVEPOINT` doesn't.
#[cfg(test)]
mod savepoint_migration_tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    // -- category 1: each migrated function nests inside an outer transaction ---

    #[test]
    fn reconcile_unique_camera_instrume_nests_inside_outer_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (path, unique_camera) VALUES ('/data/Root', 1)",
            [],
        )
        .unwrap();
        let root_id: i64 = conn
            .query_row("SELECT id FROM scan_roots", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/data/Root/a.fits', 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row("SELECT id FROM files", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO frames (file_id, instrume) VALUES (?1, 'CamA')",
            [file_id],
        )
        .unwrap();

        // Mirrors the real call pattern: scan_directory_parallel (and friends)
        // hold an outer transaction open while calling into per-row helpers.
        let outer = conn.unchecked_transaction().unwrap();
        let result = reconcile_unique_camera_instrume(&conn, root_id);
        result
            .as_ref()
            .expect("SAVEPOINT must nest inside an already-open outer transaction, not error with 'cannot start a transaction within a transaction'");
        outer.commit().unwrap();

        let instrume: String = conn
            .query_row(
                "SELECT instrume FROM frames WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            instrume,
            format!("CamA N{}", root_id),
            "the unique-camera suffix must have landed"
        );
    }

    #[test]
    fn delete_scan_root_nests_inside_outer_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/Root')", [])
            .unwrap();
        let root_id: i64 = conn
            .query_row("SELECT id FROM scan_roots", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/data/Root/a.fits', 'a.fits', 0, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let outer = conn.unchecked_transaction().unwrap();
        let result = delete_scan_root(&conn, root_id);
        result
            .as_ref()
            .expect("SAVEPOINT must nest inside an already-open outer transaction, not error with 'cannot start a transaction within a transaction'");
        outer.commit().unwrap();

        let root_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scan_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(root_count, 0, "the scan root row must have been deleted");
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            file_count, 0,
            "files under the deleted root must have cascaded away"
        );
    }

    #[test]
    fn rebuild_duplicate_groups_cache_nests_inside_outer_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/Root', 1)",
            [],
        )
        .unwrap();
        for (p, fname) in [
            ("/data/Root/a.fits", "a.fits"),
            ("/data/Root/b.fits", "b.fits"),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, 100, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'HASH1')",
                rusqlite::params![p, fname],
            )
            .unwrap();
        }

        let outer = conn.unchecked_transaction().unwrap();
        let result = rebuild_duplicate_groups_cache(&conn, true);
        result
            .as_ref()
            .expect("SAVEPOINT must nest inside an already-open outer transaction, not error with 'cannot start a transaction within a transaction'");
        outer.commit().unwrap();

        assert_eq!(
            result.unwrap(),
            1,
            "the one duplicate pair should have produced one cached group"
        );
        let cached = get_cached_duplicates(&conn, true).unwrap();
        assert_eq!(cached.len(), 1);
    }

    // -- category 2: rollback-on-error leaves DB state unchanged ---------------

    #[test]
    fn rebuild_duplicate_groups_cache_rolls_back_partial_writes_on_constraint_violation() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (path, find_duplicates) VALUES ('/data/Root', 1)",
            [],
        )
        .unwrap();

        // Pre-existing cache row (same hash_type the call below will target)
        // so we can prove the guard's rollback restores it after the
        // function's own DELETE-then-INSERT sequence fails partway through.
        conn.execute(
            "INSERT INTO duplicate_groups (hash, hash_type, size, file_count, created_at, updated_at)
             VALUES ('PREEXISTING', 'content', 999, 2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Two duplicate pairs sharing the SAME content_hash but different
        // sizes. The dedup query groups by (hash, size), so both pairs are
        // found as separate duplicate groups — but duplicate_groups' unique
        // index is on (hash, hash_type) only (no size), so inserting the
        // second group's row deterministically violates it, failing the
        // function mid-loop, after the savepoint has already performed a
        // real DELETE plus one successful INSERT.
        for (p, fname, size) in [
            ("/data/Root/a.fits", "a.fits", 100),
            ("/data/Root/b.fits", "b.fits", 100),
            ("/data/Root/c.fits", "c.fits", 200),
            ("/data/Root/d.fits", "d.fits", 200),
        ] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'FITS', '2026-01-01T00:00:00Z', 'SAMEHASH')",
                rusqlite::params![p, fname, size],
            )
            .unwrap();
        }

        let result = rebuild_duplicate_groups_cache(&conn, true);
        assert!(
            result.is_err(),
            "the crafted (hash, hash_type) collision must surface as an Err, not silently succeed"
        );

        let groups: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT hash, size FROM duplicate_groups")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(
            groups,
            vec![("PREEXISTING".to_string(), 999)],
            "savepoint rollback must undo the DELETE + partial INSERTs and restore the prior cache state exactly"
        );

        let link_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM duplicate_group_files", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            link_count, 0,
            "no duplicate_group_files rows from the failed pass should remain"
        );
    }

    // -- category 3: guard unit test — drop-without-commit vs. commit ----------

    #[test]
    fn savepoint_guard_drop_without_commit_rolls_back() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/Root')", [])
            .unwrap();

        {
            let sp = SavepointGuard::new(&conn, "test_guard_drop").unwrap();
            conn.execute("DELETE FROM scan_roots", []).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM scan_roots", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                count, 0,
                "the delete should be visible inside the open savepoint"
            );
            drop(sp); // no commit() — Drop must ROLLBACK TO + RELEASE
        }

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scan_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "dropping the guard without commit() must roll back the delete"
        );
    }

    #[test]
    fn savepoint_guard_commit_persists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES ('/data/Root')", [])
            .unwrap();

        let sp = SavepointGuard::new(&conn, "test_guard_commit").unwrap();
        conn.execute("DELETE FROM scan_roots", []).unwrap();
        sp.commit().unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scan_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 0,
            "commit() must persist the delete past the savepoint"
        );
    }
}

/// Files whose `content_hash` (the SAMPLING xxh3 stored by the scanner) is one
/// of `hashes`, returned as `(content_hash, path)` pairs. Backs the P2P dedup
/// responder's membership probe: an offered sampling hash present here is a
/// *candidate* duplicate that still needs full-hash confirmation.
///
/// The `IN (…)` list is chunked at 900 bind params (SQLite's default limit is
/// 999) and the per-chunk results are unioned. An empty `hashes` slice runs no
/// query and returns an empty vec. `content_hash` and `path` are both indexed
/// (`idx_files_content_hash`, `idx_files_path`), so each chunk is an index scan.
pub fn find_files_by_content_hashes(
    conn: &Connection,
    hashes: &[String],
) -> Result<Vec<(String, String)>> {
    // SQLite's default SQLITE_MAX_VARIABLE_NUMBER is 999; stay well under it.
    const CHUNK: usize = 900;
    let mut out = Vec::new();
    if hashes.is_empty() {
        return Ok(out);
    }
    for chunk in hashes.chunks(CHUNK) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("SELECT content_hash, path FROM files WHERE content_hash IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        for r in rows {
            out.push(r?);
        }
    }
    Ok(out)
}

/// Task 9 (calibration_library scan-root kind) — DB-level coverage for
/// `upsert_scan_root`'s INSERT-only kind semantics and the
/// `count_scan_roots_of_kind` uniqueness-guard helper. The code-level
/// single-library-root enforcement lives in `api::scan_roots::add_scan_root`
/// (no `#[cfg(test)] mod tests` there to append to — see Task 9 brief).
#[cfg(test)]
mod scan_root_kind_tests {
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    #[test]
    fn upsert_scan_root_stores_kind() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let id =
            crate::db::upsert_scan_root(&conn, "/data/library", "calibration_library").unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "calibration_library");
        // Upserting an existing path must NOT silently flip its kind.
        let id2 = crate::db::upsert_scan_root(&conn, "/data/library", "normal").unwrap();
        assert_eq!(id, id2);
        let kind: String = conn
            .query_row("SELECT kind FROM scan_roots WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "calibration_library");
    }

    #[test]
    fn only_one_calibration_library_root() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        crate::db::upsert_scan_root(&conn, "/lib/a", "calibration_library").unwrap();
        // The uniqueness check is code-level in api::add_scan_root; expose the
        // count helper it uses and pin it here:
        let n = crate::db::count_scan_roots_of_kind(&conn, "calibration_library").unwrap();
        assert_eq!(n, 1);
    }
}

/// Task 10 (cross-platform path fixes) — `normalize_separators` boundary fold.
#[cfg(test)]
mod normalize_separators_tests {
    use super::normalize_separators;

    #[test]
    fn normalize_separators_is_identity_on_posix() {
        if !cfg!(windows) {
            assert_eq!(
                normalize_separators(r"/data/weird\name/x.fits"),
                r"/data/weird\name/x.fits"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn normalize_separators_folds_forward_slashes() {
        assert_eq!(normalize_separators("C:/Astro/Old"), r"C:\Astro\Old");
    }
}

/// OSC/CFA hardening Task 1 — the CFA phase offsets and row order survive a
/// write/read round-trip through the `frames` table.
#[cfg(test)]
mod bayer_column_tests {
    use crate::db::schema::init_db;
    use crate::models::Frame;
    use rusqlite::Connection;

    fn conn_with_file() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, format, modified_at)
             VALUES ('/astro/osc_001.fits', 'osc_001.fits', 100, 'FITS', '2026-08-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        (conn, file_id)
    }

    #[test]
    fn insert_frame_round_trips_bayer_offsets_and_roworder() {
        let (conn, file_id) = conn_with_file();

        let frame = Frame {
            file_id,
            bayerpat: Some("RGGB".to_string()),
            xbayroff: Some(1),
            ybayroff: Some(0),
            roworder: Some("TOP-DOWN".to_string()),
            ..Default::default()
        };
        let frame_id = super::insert_frame(&conn, &frame).unwrap();

        let (bayerpat, xbayroff, ybayroff, roworder): (
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT bayerpat, xbayroff, ybayroff, roworder FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        assert_eq!(bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(xbayroff, Some(1));
        assert_eq!(ybayroff, Some(0));
        assert_eq!(roworder.as_deref(), Some("TOP-DOWN"));
    }

    /// A frame whose header declared no offsets stores SQL NULL, not 0 — the
    /// "unknown phase" state has to survive into the catalog.
    #[test]
    fn insert_frame_keeps_absent_offsets_null() {
        let (conn, file_id) = conn_with_file();

        let frame = Frame {
            file_id,
            bayerpat: Some("RGGB".to_string()),
            ..Default::default()
        };
        let frame_id = super::insert_frame(&conn, &frame).unwrap();

        let (xbayroff, ybayroff, roworder): (Option<i64>, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT xbayroff, ybayroff, roworder FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(xbayroff, None, "absent XBAYROFF must store NULL, not 0");
        assert_eq!(ybayroff, None, "absent YBAYROFF must store NULL, not 0");
        assert_eq!(roworder, None);
    }
}

/// DB-hygiene Task 9 (audit I2) — `sync_missing_files` moved here out of the
/// two duplicated command bodies, where it ran on a raw `BEGIN`/`COMMIT` pair
/// that leaked an open transaction onto the pooled connection whenever a
/// statement mid-pass failed.
#[cfg(test)]
mod sync_missing_files_tests {
    use super::*;
    use rusqlite::Connection;

    fn conn_with_three_files() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (7, '/t')", [])
            .unwrap();
        for name in ["a", "b", "c"] {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 1, '2026-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![format!("/t/{name}.fits"), format!("{name}.fits")],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn sync_missing_files_reconciles_and_preserves_ignored() {
        let conn = conn_with_three_files();
        let ids: Vec<i64> = (1..=3).collect();

        // First sync: all three missing.
        sync_missing_files(&conn, 7, &ids).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM missing_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);

        // User ignores file 2; file 3 reappears on disk (drops out of the list).
        conn.execute(
            "UPDATE missing_files SET status = 'ignored' WHERE file_id = 2",
            [],
        )
        .unwrap();
        sync_missing_files(&conn, 7, &[1]).unwrap();

        let still: Vec<(i64, String)> = {
            let mut stmt = conn
                .prepare("SELECT file_id, status FROM missing_files ORDER BY file_id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        // 1 stays missing, 2 stays ignored, 3 is gone.
        assert_eq!(
            still,
            vec![(1, "missing".to_string()), (2, "ignored".to_string())]
        );
    }

    /// The point of the move: the reconcile now nests. A raw `BEGIN` (what
    /// both command bodies used to run) errors with "cannot start a
    /// transaction within a transaction" here.
    #[test]
    fn sync_missing_files_nests_inside_outer_transaction() {
        let conn = conn_with_three_files();

        let outer = conn.unchecked_transaction().unwrap();
        sync_missing_files(&conn, 7, &[1, 2])
            .expect("SAVEPOINT must nest inside an already-open outer transaction");
        outer.commit().unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM missing_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "the reconcile's writes must survive the outer commit");
    }
}

/// DB-hygiene Task 13 (2026-08-03 audit I6) — a malformed
/// `files.modified_at`/`files.created_at` string must degrade to the UNIX
/// epoch with a `warn!`, not panic every read of that row. `frames.date_obs`
/// was already parsed defensively; these two columns were not.
#[cfg(test)]
mod stored_timestamp_tests {
    use super::parse_stored_ts;
    use chrono::{DateTime, TimeZone, Utc};
    use rusqlite::Connection;

    fn conn_with_malformed_file() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES ('/t/x.fits','x.fits',1,'not-a-timestamp','FITS','also-bad')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn parse_stored_ts_round_trips_valid_and_falls_back_on_garbage() {
        assert_eq!(
            parse_stored_ts("files.modified_at", "2026-01-02T03:04:05Z"),
            Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
        );
        // A non-UTC offset still normalizes to UTC, as the old code did.
        assert_eq!(
            parse_stored_ts("files.modified_at", "2026-01-02T05:04:05+02:00"),
            Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
        );
        assert_eq!(
            parse_stored_ts("files.modified_at", "not-a-timestamp"),
            DateTime::<Utc>::UNIX_EPOCH
        );
        assert_eq!(
            parse_stored_ts("files.created_at", ""),
            DateTime::<Utc>::UNIX_EPOCH
        );
    }

    #[test]
    fn malformed_stored_timestamp_does_not_panic_reads() {
        let conn = conn_with_malformed_file();

        // Previously: thread panic on DateTime::parse_from_rfc3339(...).unwrap().
        let f = crate::db::get_file_by_path(&conn, "/t/x.fits").unwrap();
        assert_eq!(f.path, "/t/x.fits");
        assert_eq!(f.modified_at, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(f.created_at, DateTime::<Utc>::UNIX_EPOCH);
    }

    #[test]
    fn malformed_stored_timestamp_does_not_panic_listings() {
        let conn = conn_with_malformed_file();

        let listed = crate::db::get_files(&conn, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.modified_at, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(listed[0].0.created_at, DateTime::<Utc>::UNIX_EPOCH);
    }
}
