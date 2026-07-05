//! Shared `lights` command-layer handlers (B5 Task 4): light-calibration
//! readiness for a frame set — single business-logic source for the Tauri
//! (`commands/lights.rs`) and web (`routes/lights.rs`) wrappers, mirroring
//! `api/masters.rs`. See `docs/superpowers/specs/2026-07-05-light-calibration-design.md`
//! §5 (derived status) and §8 (readiness dialog).
//!
//! `get_light_calibration_readiness` answers, for every LIGHT frame of a
//! frame set: what Dark/Flat/Bias calibration is available and whether the
//! frame's existing calibrated output (if any) is still current. It drives
//! the Calibrate-Lights dialog summary and the per-frame status badge.
//!
//! Two independent axes are reported per frame:
//! - **link readiness** (`dark`/`flat`/`bias` = `master` | `rawSet` |
//!   `missing`): can we calibrate now, must masters be built first, or is a
//!   calibration type simply not linked?
//! - **output status** (`status`, via `db::light_calibrations::derive_status`):
//!   is the already-written calibrated file fresh, partial, or stale?
//!
//! The core logic lives in `compute_readiness(&Connection, …)` so it is
//! unit-testable against a seeded in-memory connection (the `api/calibration.rs`
//! inner-fn precedent); the public handler is a thin `ctx` → `conn` wrapper.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::db::calibration_links::get_links_for_frame;
use crate::db::light_calibrations::{derive_status, LightCalStatus};
use crate::models::CalibrationLink;
use crate::services::ServiceContext;

// ── DTOs (single-sourced; both wrapper crates import these) ─────────────────

/// Per-frame readiness row for the Calibrate-Lights dialog + frame-table badge.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightFrameReadiness {
    pub frame_id: i64,
    pub filename: String,
    /// `db::light_calibrations::derive_status` mapped to the frontend's
    /// verbatim strings: `"notCalibrated"` | `"calibrated"` | `"partial"` |
    /// `"stale"`.
    pub status: String,
    /// Dark-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub dark: String,
    /// Flat-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub flat: String,
    /// Bias-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub bias: String,
    /// Distinct raw (non-master, non-superseded) calibration-set ids this
    /// frame links to — the sets a preflight would have to build masters for.
    pub raw_set_ids: Vec<i64>,
}

/// Frame-set-level readiness summary for the Calibrate-Lights dialog.
///
/// `ready_count` + `raw_set_count` + `missing_count` partition `frames`:
/// - `raw_set_count`: frames with at least one raw-set link (masters get
///   built automatically first).
/// - `missing_count`: of the rest, frames missing a Dark or Flat link (Bias
///   is optional for lights under the raw-master-dark convention — a missing
///   Bias never blocks readiness, though it is still reported in `bias`).
/// - `ready_count`: the remainder — Dark and Flat both present as masters,
///   ready to calibrate now.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightCalReadiness {
    pub frames: Vec<LightFrameReadiness>,
    pub ready_count: i64,
    pub raw_set_count: i64,
    pub missing_count: i64,
    /// Distinct raw calibration-set ids across all frames that a preflight
    /// must build masters for, in first-seen order. `raw_set_ids_to_build.len()`
    /// is the number of master builds; `raw_set_count` is the number of
    /// affected frames (a single raw set can serve many frames).
    pub raw_set_ids_to_build: Vec<i64>,
}

// ── Classification ──────────────────────────────────────────────────────────

const MASTER: &str = "master";
const RAW_SET: &str = "rawSet";
const MISSING: &str = "missing";

/// Classify one calibration-type link for a light frame.
///
/// Returns the wire classification string plus, when the link points at a raw
/// non-superseded set, that set's id (the caller collects these into the
/// build list). Rules (Task 4 brief):
/// - no link of this type → `missing`.
/// - link → a master-library set (`is_master_library = 1`) → `master`.
/// - link → a raw set already superseded (`superseded_by_set_id IS NOT NULL`)
///   → resolves to its master, counts as `master`, nothing to build.
/// - link → a raw, non-superseded set → `rawSet`, id returned for the build
///   list.
///
/// A link that targets a set id with no `calibration_set` row (dangling FK —
/// should not happen, `no action` FK) is treated as `missing` and logged.
fn classify(
    conn: &Connection,
    links: &[CalibrationLink],
    cal_type: &str,
) -> Result<(&'static str, Option<i64>), ApiError> {
    let set_id = match links.iter().find(|l| l.calibration_type == cal_type) {
        Some(l) => l.calibration_set_id,
        None => return Ok((MISSING, None)),
    };

    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT is_master_library, superseded_by_set_id FROM calibration_set WHERE id = ?1",
            params![set_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;

    match row {
        None => {
            tracing::warn!(set_id, cal_type, "calibration link targets a missing set");
            Ok((MISSING, None))
        }
        Some((is_master, superseded_by)) => {
            if is_master == 1 || superseded_by.is_some() {
                Ok((MASTER, None))
            } else {
                Ok((RAW_SET, Some(set_id)))
            }
        }
    }
}

/// Map the derived status enum to the frontend's verbatim camelCase strings.
fn status_str(s: LightCalStatus) -> &'static str {
    match s {
        LightCalStatus::NotCalibrated => "notCalibrated",
        LightCalStatus::Calibrated => "calibrated",
        LightCalStatus::Partial => "partial",
        LightCalStatus::Stale => "stale",
    }
}

// ── Handler ─────────────────────────────────────────────────────────────────

/// LIGHT members (frame_id, filename) of a frame set, mirroring the
/// membership join used elsewhere in the calibration layer
/// (`db::calibration_links::get_calibration_groups_for_frame_set`), plus a
/// `files` join for the filename.
fn load_light_members(conn: &Connection, set_id: i64) -> Result<Vec<(i64, String)>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id, fi.filename
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY sm.frame_id",
    )?;
    let rows = stmt
        .query_map(params![set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

/// Compute readiness for every LIGHT frame of `set_id`. Pure DB work — no
/// pixel I/O — so both transports' wrappers can run it inside `spawn_blocking`.
fn compute_readiness(
    conn: &Connection,
    set_id: i64,
    flat_norm: bool,
) -> Result<LightCalReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;

    let mut frames = Vec::with_capacity(members.len());
    let mut ready_count = 0i64;
    let mut raw_set_count = 0i64;
    let mut missing_count = 0i64;
    let mut raw_set_ids_to_build: Vec<i64> = Vec::new();

    for (frame_id, filename) in members {
        let links = get_links_for_frame(conn, frame_id)?;

        let (dark, dark_raw) = classify(conn, &links, "Dark")?;
        let (flat, flat_raw) = classify(conn, &links, "Flat")?;
        let (bias, bias_raw) = classify(conn, &links, "Bias")?;

        let mut raw_set_ids: Vec<i64> = Vec::new();
        for r in [dark_raw, flat_raw, bias_raw].into_iter().flatten() {
            if !raw_set_ids.contains(&r) {
                raw_set_ids.push(r);
            }
            if !raw_set_ids_to_build.contains(&r) {
                raw_set_ids_to_build.push(r);
            }
        }

        // Partition into ready / raw / missing. Raw sets take precedence: a
        // preflight builds their masters first, after which the frame is
        // re-evaluated. Among frames with no raw links, a missing Dark or
        // Flat makes the frame "missing"; Bias is optional for lights
        // (raw-master-dark convention) so its absence never blocks readiness.
        if !raw_set_ids.is_empty() {
            raw_set_count += 1;
        } else if dark == MISSING || flat == MISSING {
            missing_count += 1;
        } else {
            ready_count += 1;
        }

        let status = status_str(derive_status(conn, frame_id, &links, flat_norm)?);

        frames.push(LightFrameReadiness {
            frame_id,
            filename,
            status: status.to_string(),
            dark: dark.to_string(),
            flat: flat.to_string(),
            bias: bias.to_string(),
            raw_set_ids,
        });
    }

    tracing::debug!(
        set_id,
        total = frames.len() as i64,
        ready_count,
        raw_set_count,
        missing_count,
        to_build = raw_set_ids_to_build.len() as i64,
        "light calibration readiness computed"
    );

    Ok(LightCalReadiness {
        frames,
        ready_count,
        raw_set_count,
        missing_count,
        raw_set_ids_to_build,
    })
}

/// Readiness summary + per-frame status for the frame set's LIGHT members.
/// `flat_norm` is the dialog's "Normalize master flat" toggle — it feeds
/// `derive_status`'s flat-normalization staleness check (a frame calibrated
/// with a different normalization choice than the user now wants is stale).
pub fn get_light_calibration_readiness(
    ctx: &ServiceContext,
    set_id: i64,
    flat_norm: bool,
) -> Result<LightCalReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_readiness(&conn, set_id, flat_norm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::light_calibrations::{
        upsert_light_calibration, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };
    use crate::db::schema::init_db;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("fs_{fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// One LIGHT frame (files + frames rows) joined into `session_id`.
    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64) -> String {
        let file_id = frame_id + 1_000_000;
        let filename = format!("light_{frame_id}.fits");
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![file_id, format!("/test/{filename}"), filename],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume) VALUES (?1, ?2, 'Light', 'TestCam')",
            params![frame_id, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
        filename
    }

    fn seed_set(conn: &Connection, id: i64, imagetyp: &str, is_master: bool) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', ?3)",
            params![id, imagetyp, is_master as i64],
        )
        .unwrap();
    }

    fn supersede(conn: &Connection, raw_id: i64, master_id: i64) {
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            params![master_id, raw_id],
        )
        .unwrap();
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    /// Master sets used across the tests: Dark #100, Flat #101, Bias #102.
    fn seed_masters(conn: &Connection) {
        seed_set(conn, 100, "MasterDark", true);
        seed_set(conn, 101, "MasterFlat", true);
        seed_set(conn, 102, "MasterBias", true);
    }

    #[test]
    fn readiness_classifies_master_raw_missing() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_set(&conn, 200, "Dark", false); // raw dark set

        // Light 1 — fully mastered, with a fresh matching tracking row.
        seed_light(&conn, 1, session);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");
        add_link(&conn, 1, 102, "Bias");
        upsert_light_calibration(
            &conn,
            &LightCalRow {
                id: 0,
                frame_id: Some(1),
                source_uuid: None,
                source_filename: None,
                output_path: "/lib/c_light_1.fits".to_string(),
                dark_set_id: Some(100),
                flat_set_id: Some(101),
                bias_set_id: Some(102),
                calstat: "BDF".to_string(),
                flat_norm_applied: false,
                output_hash: "deadbeef".to_string(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-07-05T00:00:00Z".to_string(),
            },
        )
        .unwrap();

        // Light 2 — Dark links a raw set; Flat/Bias mastered.
        seed_light(&conn, 2, session);
        add_link(&conn, 2, 200, "Dark");
        add_link(&conn, 2, 101, "Flat");
        add_link(&conn, 2, 102, "Bias");

        // Light 3 — no Flat link; Dark/Bias mastered.
        seed_light(&conn, 3, session);
        add_link(&conn, 3, 100, "Dark");
        add_link(&conn, 3, 102, "Bias");

        let r = compute_readiness(&conn, 1, false).unwrap();
        assert_eq!(r.frames.len(), 3);

        let f1 = &r.frames[0];
        assert_eq!(f1.frame_id, 1);
        assert_eq!(f1.filename, "light_1.fits");
        assert_eq!((f1.dark.as_str(), f1.flat.as_str(), f1.bias.as_str()), (MASTER, MASTER, MASTER));
        assert!(f1.raw_set_ids.is_empty());
        assert_eq!(f1.status, "calibrated", "fresh tracking row that matches links is calibrated");

        let f2 = &r.frames[1];
        assert_eq!(f2.frame_id, 2);
        assert_eq!((f2.dark.as_str(), f2.flat.as_str(), f2.bias.as_str()), (RAW_SET, MASTER, MASTER));
        assert_eq!(f2.raw_set_ids, vec![200]);
        assert_eq!(f2.status, "notCalibrated", "no tracking row yet");

        let f3 = &r.frames[2];
        assert_eq!(f3.frame_id, 3);
        assert_eq!((f3.dark.as_str(), f3.flat.as_str(), f3.bias.as_str()), (MASTER, MISSING, MASTER));
        assert!(f3.raw_set_ids.is_empty());
    }

    #[test]
    fn readiness_counts_and_build_list() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_set(&conn, 200, "Dark", false); // raw dark set (needs building)
        seed_set(&conn, 300, "Dark", false); // raw dark set, already superseded
        supersede(&conn, 300, 100); // 300 → master 100

        // Light 1 — fully mastered → ready.
        seed_light(&conn, 1, session);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");

        // Light 2 — raw dark 200 → raw bucket, contributes 200 to build list.
        seed_light(&conn, 2, session);
        add_link(&conn, 2, 200, "Dark");
        add_link(&conn, 2, 101, "Flat");

        // Light 3 — same raw dark 200 → raw bucket; 200 must dedupe.
        seed_light(&conn, 3, session);
        add_link(&conn, 3, 200, "Dark");
        add_link(&conn, 3, 101, "Flat");

        // Light 4 — Dark links a superseded raw set (300 → master) → master,
        // NOT added to the build list. No Flat → missing bucket.
        seed_light(&conn, 4, session);
        add_link(&conn, 4, 300, "Dark");

        let r = compute_readiness(&conn, 1, false).unwrap();

        assert_eq!(r.frames.len(), 4);
        assert_eq!(r.ready_count, 1, "only light 1 is fully ready");
        assert_eq!(r.raw_set_count, 2, "lights 2 and 3 link raw sets");
        assert_eq!(r.missing_count, 1, "light 4 is missing its Flat link");
        assert_eq!(
            r.raw_set_ids_to_build,
            vec![200],
            "raw set 200 appears once; superseded 300 resolves to a master and is excluded"
        );

        // Light 4's Dark resolves through the supersede pointer to a master.
        let f4 = r.frames.iter().find(|f| f.frame_id == 4).unwrap();
        assert_eq!(f4.dark, MASTER);
        assert_eq!(f4.flat, MISSING);
        assert!(f4.raw_set_ids.is_empty());
    }
}
