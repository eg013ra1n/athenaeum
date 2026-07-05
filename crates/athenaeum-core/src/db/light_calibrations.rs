// light_calibrations: tracking table for calibrated-light FITS artifacts
// (B5 in-app light calibration, Task 1).
//
// Calibrated lights live OUTSIDE the catalog — never registered in
// `files`/`frames` (design spec 2026-07-05-light-calibration-design.md §3) —
// so this table is their sole record: what was applied, whether it's still
// current relative to the frame's calibration links, and where the output
// file lives on disk so a re-run can find and overwrite it in place.
//
// Frame status (§5) is *derived*, never stored: [`derive_status`] compares a
// row against the frame's current calibration links and a few staleness
// signals (master rebuilt, engine bump, flat-norm toggle change).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::CalibrationLink;

use super::master_provenance;

/// Bump when the calibration math changes — every existing row becomes
/// [`LightCalStatus::Stale`] via the `engine_version` check in
/// [`derive_status`]. The single definition now lives with the calibration
/// engine ([`crate::calibration_library::light_cal`]); re-exported here so
/// this module's `derive_status` and callers keep referring to it by the same
/// path.
pub use crate::calibration_library::light_cal::LIGHT_CAL_ENGINE_VERSION;

/// Column list shared by every read query, so `row_from_sql`'s index-based
/// `row.get(N)` calls can't silently drift out of sync with the SELECT.
const SELECT_COLS: &str = "id, frame_id, source_uuid, source_filename, output_path, \
    dark_set_id, flat_set_id, bias_set_id, calstat, flat_norm_applied, output_hash, \
    engine_version, created_at";

/// One tracked calibrated-light output.
#[derive(Debug, Clone, PartialEq)]
pub struct LightCalRow {
    pub id: i64,
    /// NULL only for an "adopted" row whose source frame isn't cataloged yet
    /// (design §4.4) — backfilled once the source is scanned.
    pub frame_id: Option<i64>,
    pub source_uuid: Option<String>,
    pub source_filename: Option<String>,
    pub output_path: String,
    pub dark_set_id: Option<i64>,
    pub flat_set_id: Option<i64>,
    pub bias_set_id: Option<i64>,
    pub calstat: String,
    /// true = divided by `ATH_FNRM`, false = plain flat division.
    pub flat_norm_applied: bool,
    pub output_hash: String,
    pub engine_version: i64,
    pub created_at: String,
}

fn row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<LightCalRow> {
    Ok(LightCalRow {
        id: row.get(0)?,
        frame_id: row.get(1)?,
        source_uuid: row.get(2)?,
        source_filename: row.get(3)?,
        output_path: row.get(4)?,
        dark_set_id: row.get(5)?,
        flat_set_id: row.get(6)?,
        bias_set_id: row.get(7)?,
        calstat: row.get(8)?,
        flat_norm_applied: row.get::<_, i64>(9)? != 0,
        output_hash: row.get(10)?,
        engine_version: row.get(11)?,
        created_at: row.get(12)?,
    })
}

/// Insert or update the tracking row for one calibrated-light output.
///
/// Keyed on `frame_id` when the row has one (the normal case — the light was
/// resolved to a cataloged frame). Falls back to `output_path` when
/// `frame_id` is `None` (an "adopted" row with no cataloged source yet, or a
/// re-run that hasn't re-resolved the frame). Returns the row's `id` either
/// way.
pub fn upsert_light_calibration(conn: &Connection, row: &LightCalRow) -> Result<i64> {
    let flat_norm_applied = row.flat_norm_applied as i64;
    let conflict_col = if row.frame_id.is_some() {
        "frame_id"
    } else {
        "output_path"
    };
    let sql = format!(
        "INSERT INTO light_calibrations
         (frame_id, source_uuid, source_filename, output_path, dark_set_id, flat_set_id,
          bias_set_id, calstat, flat_norm_applied, output_hash, engine_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT({conflict_col}) DO UPDATE SET
            frame_id = excluded.frame_id,
            source_uuid = excluded.source_uuid,
            source_filename = excluded.source_filename,
            output_path = excluded.output_path,
            dark_set_id = excluded.dark_set_id,
            flat_set_id = excluded.flat_set_id,
            bias_set_id = excluded.bias_set_id,
            calstat = excluded.calstat,
            flat_norm_applied = excluded.flat_norm_applied,
            output_hash = excluded.output_hash,
            engine_version = excluded.engine_version,
            created_at = excluded.created_at"
    );
    conn.execute(
        &sql,
        params![
            row.frame_id,
            row.source_uuid,
            row.source_filename,
            row.output_path,
            row.dark_set_id,
            row.flat_set_id,
            row.bias_set_id,
            row.calstat,
            flat_norm_applied,
            row.output_hash,
            row.engine_version,
            row.created_at,
        ],
    )?;

    // last_insert_rowid() is unreliable after an UPSERT that took the UPDATE
    // branch (SQLite leaves it pointing at the prior INSERT), so re-look-up
    // the id via whichever column is guaranteed unique for this row.
    let id: i64 = if let Some(frame_id) = row.frame_id {
        conn.query_row(
            "SELECT id FROM light_calibrations WHERE frame_id = ?1",
            params![frame_id],
            |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT id FROM light_calibrations WHERE output_path = ?1",
            params![row.output_path],
            |r| r.get(0),
        )?
    };
    Ok(id)
}

/// Look up the tracking row for a cataloged frame, if any.
pub fn get_light_calibration_for_frame(
    conn: &Connection,
    frame_id: i64,
) -> Result<Option<LightCalRow>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM light_calibrations WHERE frame_id = ?1"),
        params![frame_id],
        row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Resolve a tracking row by identity when there is no cataloged `frame_id`
/// yet (adoption / scanner-repair paths, design §4). Tries `source_uuid`
/// first, then falls back to `source_filename`.
pub fn find_by_identity(
    conn: &Connection,
    source_uuid: Option<&str>,
    source_filename: Option<&str>,
) -> Result<Option<LightCalRow>> {
    if let Some(uuid) = source_uuid {
        let by_uuid = conn
            .query_row(
                &format!("SELECT {SELECT_COLS} FROM light_calibrations WHERE source_uuid = ?1"),
                params![uuid],
                row_from_sql,
            )
            .optional()?;
        if by_uuid.is_some() {
            return Ok(by_uuid);
        }
    }
    if let Some(filename) = source_filename {
        return conn
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM light_calibrations WHERE source_filename = ?1"
                ),
                params![filename],
                row_from_sql,
            )
            .optional()
            .map_err(Into::into);
    }
    Ok(None)
}

/// Repoint a row's `output_path` after the underlying file moved on disk
/// (scanner "moved file" reconcile branch, design §4.2).
pub fn update_output_path(conn: &Connection, id: i64, new_path: &str) -> Result<()> {
    let changed = conn.execute(
        "UPDATE light_calibrations SET output_path = ?1 WHERE id = ?2",
        params![new_path, id],
    )?;
    if changed == 0 {
        anyhow::bail!("no light_calibrations row for id {id}");
    }
    Ok(())
}

/// Derived (never stored) calibration status for a light frame — design §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightCalStatus {
    NotCalibrated,
    Calibrated,
    Partial,
    Stale,
}

/// Derive a light frame's calibration status from its tracking row (if any)
/// against the frame's *current* calibration links.
///
/// Rules (design §5): no row → `NotCalibrated`. A type the row actually
/// applied that now points at a *different* calibration set, a referenced
/// master having been rebuilt since (`master_provenance.created_at` newer
/// than the row's `created_at`), an older `engine_version`, or a
/// `flat_norm_applied` that no longer matches what the caller wants → all
/// `Stale`. Otherwise, a type the row never applied but the frame now has a
/// link for → `Partial` (a narrower case: re-calibrating would only *add*
/// coverage, not redo work). Anything else → `Calibrated`.
///
/// `current_links` is filtered by `calibration_type` — only `"Dark"`,
/// `"Flat"`, `"Bias"` are considered; a `"DarkFlat"` link never applies to a
/// light frame directly (it's a Flat's own sub-calibration).
pub fn derive_status(
    conn: &Connection,
    frame_id: i64,
    current_links: &[CalibrationLink],
    flat_norm_wanted: bool,
) -> Result<LightCalStatus> {
    let row = match get_light_calibration_for_frame(conn, frame_id)? {
        Some(row) => row,
        None => return Ok(LightCalStatus::NotCalibrated),
    };

    let link_set_id = |cal_type: &str| -> Option<i64> {
        current_links
            .iter()
            .find(|l| l.calibration_type == cal_type)
            .map(|l| l.calibration_set_id)
    };
    let link_dark = link_set_id("Dark");
    let link_flat = link_set_id("Flat");
    let link_bias = link_set_id("Bias");

    // A type the row actually applied that now differs from (or is no
    // longer covered by) the frame's current link is a full mismatch.
    let changed = |applied: Option<i64>, wanted: Option<i64>| applied.is_some() && applied != wanted;
    let mismatch = changed(row.dark_set_id, link_dark)
        || changed(row.flat_set_id, link_flat)
        || changed(row.bias_set_id, link_bias);

    let mut master_rebuilt = false;
    for set_id in [row.dark_set_id, row.flat_set_id, row.bias_set_id]
        .into_iter()
        .flatten()
    {
        if let Some(prov) = master_provenance::get(conn, set_id)? {
            if prov.created_at.as_str() > row.created_at.as_str() {
                master_rebuilt = true;
                break;
            }
        }
    }

    if mismatch
        || master_rebuilt
        || row.engine_version < LIGHT_CAL_ENGINE_VERSION
        || row.flat_norm_applied != flat_norm_wanted
    {
        return Ok(LightCalStatus::Stale);
    }

    // A type the row never recorded (NULL) but the frame now has a link for
    // — new coverage available, not a redo of existing work.
    let newly_linked =
        |applied: Option<i64>, wanted: Option<i64>| applied.is_none() && wanted.is_some();
    if newly_linked(row.dark_set_id, link_dark)
        || newly_linked(row.flat_set_id, link_flat)
        || newly_linked(row.bias_set_id, link_bias)
    {
        return Ok(LightCalStatus::Partial);
    }

    Ok(LightCalStatus::Calibrated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// Minimal `files` + `frames` rows so `light_calibrations.frame_id`'s FK
    /// (ON DELETE CASCADE) is satisfiable — FK enforcement is on by default
    /// in this codebase.
    fn seed_frame(conn: &Connection, frame_id: i64) {
        let file_id = frame_id + 1_000_000;
        conn.execute(
            "INSERT OR IGNORE INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/test/light_{frame_id}.fits"),
                format!("light_{frame_id}.fits"),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id) VALUES (?1, ?2)",
            params![frame_id, file_id],
        )
        .unwrap();
    }

    fn seed_calibration_set(conn: &Connection, id: i64, imagetyp: &str) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (?1, ?2, '2025-01-01')",
            params![id, imagetyp],
        )
        .unwrap();
    }

    /// Mark `set_id` as an Athenaeum-built master with the given
    /// `master_provenance.created_at`, so `derive_status`'s rebuild check has
    /// something to compare against.
    fn seed_master_provenance(conn: &Connection, set_id: i64, created_at: &str) {
        conn.execute(
            "INSERT INTO master_provenance
             (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, NULL, '{}', '[]', 'hash', ?2)",
            params![set_id, created_at],
        )
        .unwrap();
    }

    fn link(calibration_type: &str, calibration_set_id: i64) -> CalibrationLink {
        CalibrationLink {
            id: None,
            source_id: 0,
            source_type: "frame".to_string(),
            calibration_set_id,
            calibration_type: calibration_type.to_string(),
            matched_at: "2026-01-01T00:00:00Z".to_string(),
            match_score: None,
            date_warning: false,
            temp_warning: false,
            is_manual_override: false,
        }
    }

    fn base_row(frame_id: Option<i64>, output_path: &str) -> LightCalRow {
        LightCalRow {
            id: 0,
            frame_id,
            source_uuid: None,
            source_filename: None,
            output_path: output_path.to_string(),
            dark_set_id: None,
            flat_set_id: None,
            bias_set_id: None,
            calstat: "BD".to_string(),
            flat_norm_applied: false,
            output_hash: "deadbeef".to_string(),
            engine_version: LIGHT_CAL_ENGINE_VERSION,
            created_at: "2026-07-05T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn upsert_then_get_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 1);
        seed_calibration_set(&conn, 10, "Dark");

        let mut row = base_row(Some(1), "/lib/obj/cam/2026-07-05/c_light_1.fits");
        row.dark_set_id = Some(10);
        row.source_uuid = Some("uuid-1".to_string());
        row.source_filename = Some("light_1.fits".to_string());

        let id = upsert_light_calibration(&conn, &row).unwrap();
        assert!(id > 0);

        let got = get_light_calibration_for_frame(&conn, 1).unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.frame_id, Some(1));
        assert_eq!(got.dark_set_id, Some(10));
        assert_eq!(got.output_path, row.output_path);
        assert_eq!(got.calstat, "BD");
        assert!(!got.flat_norm_applied);
        assert_eq!(got.engine_version, LIGHT_CAL_ENGINE_VERSION);

        // Re-running (same frame_id) upserts in place rather than erroring on
        // the output_path UNIQUE constraint — same id, refreshed fields.
        let mut updated = row.clone();
        updated.calstat = "BDF".to_string();
        updated.flat_set_id = Some(20);
        updated.output_path = "/lib/obj/cam/2026-07-05/c_light_1_2.fits".to_string();
        seed_calibration_set(&conn, 20, "Flat");
        let id2 = upsert_light_calibration(&conn, &updated).unwrap();
        assert_eq!(id2, id, "same frame_id must upsert in place, not insert a new row");

        let got2 = get_light_calibration_for_frame(&conn, 1).unwrap().unwrap();
        assert_eq!(got2.calstat, "BDF");
        assert_eq!(got2.flat_set_id, Some(20));
        assert_eq!(got2.output_path, updated.output_path);
    }

    #[test]
    fn upsert_keyed_on_output_path_when_frame_id_none() {
        // Adopted-row shape: no cataloged frame yet, identity carried by
        // source_uuid/source_filename, keyed for upsert on output_path.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut row = base_row(None, "/lib/obj/cam/2026-07-05/c_orphan.fits");
        row.source_uuid = Some("uuid-orphan".to_string());
        let id = upsert_light_calibration(&conn, &row).unwrap();

        let mut updated = row.clone();
        updated.calstat = "BF".to_string();
        let id2 = upsert_light_calibration(&conn, &updated).unwrap();
        assert_eq!(id2, id, "same output_path must upsert in place, not insert a new row");

        let got = find_by_identity(&conn, Some("uuid-orphan"), None)
            .unwrap()
            .unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.calstat, "BF");
        assert_eq!(got.frame_id, None);
    }

    #[test]
    fn find_by_identity_uuid_then_filename_fallback() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut by_uuid = base_row(None, "/lib/a/c_a.fits");
        by_uuid.source_uuid = Some("uuid-a".to_string());
        by_uuid.source_filename = Some("a.fits".to_string());
        upsert_light_calibration(&conn, &by_uuid).unwrap();

        let mut by_filename_only = base_row(None, "/lib/b/c_b.fits");
        by_filename_only.source_uuid = None;
        by_filename_only.source_filename = Some("b.fits".to_string());
        upsert_light_calibration(&conn, &by_filename_only).unwrap();

        // uuid hit takes priority even when a filename is also supplied.
        let found = find_by_identity(&conn, Some("uuid-a"), Some("does-not-matter.fits"))
            .unwrap()
            .unwrap();
        assert_eq!(found.output_path, "/lib/a/c_a.fits");

        // uuid miss (or absent) falls back to filename.
        let found = find_by_identity(&conn, Some("uuid-does-not-exist"), Some("b.fits"))
            .unwrap()
            .unwrap();
        assert_eq!(found.output_path, "/lib/b/c_b.fits");

        let found = find_by_identity(&conn, None, Some("b.fits")).unwrap().unwrap();
        assert_eq!(found.output_path, "/lib/b/c_b.fits");

        assert!(find_by_identity(&conn, None, None).unwrap().is_none());
        assert!(find_by_identity(&conn, Some("nope"), Some("nope.fits"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn update_output_path_changes_path_and_errors_on_missing_row() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        seed_frame(&conn, 2);
        let row = base_row(Some(2), "/lib/old/c_light_2.fits");
        let id = upsert_light_calibration(&conn, &row).unwrap();

        update_output_path(&conn, id, "/lib/new/c_light_2.fits").unwrap();
        let got = get_light_calibration_for_frame(&conn, 2).unwrap().unwrap();
        assert_eq!(got.output_path, "/lib/new/c_light_2.fits");

        assert!(update_output_path(&conn, 99999, "/nowhere.fits").is_err());
    }

    #[test]
    fn derive_status_matrix() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // ---- no row → NotCalibrated ----
        // frame_id 100 has no light_calibrations row (and isn't even seeded
        // in `frames` — derive_status never needs to insert against it).
        let status = derive_status(&conn, 100, &[], false).unwrap();
        assert_eq!(status, LightCalStatus::NotCalibrated);

        // ---- fresh, everything matches → Calibrated ----
        seed_frame(&conn, 1);
        seed_calibration_set(&conn, 10, "Dark");
        let mut fresh = base_row(Some(1), "/lib/1.fits");
        fresh.dark_set_id = Some(10);
        fresh.engine_version = LIGHT_CAL_ENGINE_VERSION;
        fresh.flat_norm_applied = false;
        upsert_light_calibration(&conn, &fresh).unwrap();
        let links = vec![link("Dark", 10)];
        assert_eq!(
            derive_status(&conn, 1, &links, false).unwrap(),
            LightCalStatus::Calibrated
        );

        // ---- link-changed: current Dark link now points at a different
        // set than the row applied → Stale ----
        seed_calibration_set(&conn, 11, "Dark");
        let changed_links = vec![link("Dark", 11)];
        assert_eq!(
            derive_status(&conn, 1, &changed_links, false).unwrap(),
            LightCalStatus::Stale
        );

        // ---- master rebuilt: row's dark_set_id references a master whose
        // provenance.created_at is newer than the row's created_at → Stale
        // (even though the link itself didn't change set id) ----
        seed_frame(&conn, 2);
        seed_calibration_set(&conn, 12, "MasterDark");
        seed_master_provenance(&conn, 12, "2026-07-04T00:00:00Z");
        let mut rebuilt_case = base_row(Some(2), "/lib/2.fits");
        rebuilt_case.dark_set_id = Some(12);
        rebuilt_case.created_at = "2026-07-01T00:00:00Z".to_string();
        upsert_light_calibration(&conn, &rebuilt_case).unwrap();
        let same_links = vec![link("Dark", 12)];
        assert_eq!(
            derive_status(&conn, 2, &same_links, false).unwrap(),
            LightCalStatus::Stale,
            "master rebuilt after the row was written must be stale"
        );

        // ---- engine bumped: row.engine_version older than
        // LIGHT_CAL_ENGINE_VERSION → Stale ----
        seed_frame(&conn, 3);
        let mut old_engine = base_row(Some(3), "/lib/3.fits");
        old_engine.dark_set_id = Some(10);
        old_engine.engine_version = LIGHT_CAL_ENGINE_VERSION - 1;
        upsert_light_calibration(&conn, &old_engine).unwrap();
        let links3 = vec![link("Dark", 10)];
        assert_eq!(
            derive_status(&conn, 3, &links3, false).unwrap(),
            LightCalStatus::Stale
        );

        // ---- flat-norm flipped: row applied with flat_norm_applied=true,
        // caller now wants false (or vice versa) → Stale ----
        seed_frame(&conn, 4);
        seed_calibration_set(&conn, 13, "Flat");
        let mut norm_row = base_row(Some(4), "/lib/4.fits");
        norm_row.flat_set_id = Some(13);
        norm_row.flat_norm_applied = true;
        upsert_light_calibration(&conn, &norm_row).unwrap();
        let links4 = vec![link("Flat", 13)];
        assert_eq!(
            derive_status(&conn, 4, &links4, false).unwrap(),
            LightCalStatus::Stale,
            "flat_norm_wanted=false must be stale vs a row built with true"
        );
        assert_eq!(
            derive_status(&conn, 4, &links4, true).unwrap(),
            LightCalStatus::Calibrated,
            "matching flat_norm_wanted must be calibrated"
        );

        // ---- partial: row only ever applied Dark; frame now also has a
        // Flat link it didn't have at build time → Partial, not Stale ----
        seed_frame(&conn, 5);
        seed_calibration_set(&conn, 14, "Flat");
        let mut partial_row = base_row(Some(5), "/lib/5.fits");
        partial_row.dark_set_id = Some(10);
        upsert_light_calibration(&conn, &partial_row).unwrap();
        let partial_links = vec![link("Dark", 10), link("Flat", 14)];
        assert_eq!(
            derive_status(&conn, 5, &partial_links, false).unwrap(),
            LightCalStatus::Partial
        );

        // ---- a DarkFlat link never applies to a light: a row with no
        // bias/flat set that only sees a DarkFlat link stays Calibrated,
        // not Partial ----
        seed_frame(&conn, 6);
        seed_calibration_set(&conn, 15, "DarkFlat");
        let mut darkflat_row = base_row(Some(6), "/lib/6.fits");
        darkflat_row.dark_set_id = Some(10);
        upsert_light_calibration(&conn, &darkflat_row).unwrap();
        let darkflat_links = vec![link("Dark", 10), link("DarkFlat", 15)];
        assert_eq!(
            derive_status(&conn, 6, &darkflat_links, false).unwrap(),
            LightCalStatus::Calibrated
        );
    }
}
