//! Autofind: fill `frame.object` from already-known RA/Dec coordinates.
//!
//! Given a list of frame IDs, this module looks up each frame's stored RA/Dec
//! in the bundled DSO catalog and writes the nearest named object back into
//! `frames.object` — prefixed with `"Autofind: "` so the auto-assignment is
//! visible wherever the value is displayed.
//!
//! Skipped cases (counted, not errored):
//!   - `frame.object` already set (non-null, non-empty)
//!   - `frame.ra` or `frame.dec` is NULL
//!   - no DSO within [`tolerance_deg`] of the position
//!
//! This is the counterpart to the "Missing Coordinates" plate-solve batch:
//! the plate solver fills RA/Dec for frames that have none, and this module
//! fills `object` for frames that have RA/Dec but no label yet.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::plate_solve::dso_lookup::{DsoCatalog, DsoMatchReason};
use crate::plate_solve::storage::update_frame_object_if_missing;

/// Default tolerance for the proximity fallback when the caller doesn't
/// pass one explicitly. Individual runs pick their own tolerance via
/// `PlateSolveConfig::autofind_tolerance_deg`.
pub const AUTOFIND_DEFAULT_MAX_DEG: f64 = 0.5;

/// Prefix applied to every auto-assigned object name. Makes the origin
/// visible in the database and anywhere the value is displayed.
pub const AUTOFIND_PREFIX: &str = "Autofind: ";

/// Per-frame progress event. Emitted twice per frame: once with
/// [`AutofindStatus::Processing`] before the lookup, and once with the
/// resolved outcome after.
#[derive(Clone, Debug, Serialize)]
pub struct AutofindProgress {
    pub frame_id: i64,
    pub current: usize,
    pub total: usize,
    pub status: AutofindStatus,
    pub designation: Option<String>,
    pub distance_deg: Option<f64>,
    /// `"contains"` or `"nearest"` when `status == Labeled`; `None` otherwise.
    pub reason: Option<String>,
    /// Frame's RA in degrees, if available. Included for all outcomes so
    /// the UI can show the position even for skipped frames.
    pub frame_ra: Option<f64>,
    /// Frame's Dec in degrees, if available.
    pub frame_dec: Option<f64>,
    /// On [`AutofindStatus::NoMatch`], the designation of the nearest DSO
    /// regardless of tolerance — lets the user see why the match failed
    /// (e.g. "closest was M 31 at 0.38°, outside 0.2° tolerance").
    pub closest_designation: Option<String>,
    /// Distance to `closest_designation`, in degrees.
    pub closest_distance_deg: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AutofindStatus {
    Processing,
    Labeled,
    NoMatch,
    AlreadyLabeled,
    MissingCoords,
    Error,
}

/// Aggregate outcome of an autofind run.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AutofindSummary {
    pub total: usize,
    pub labeled: usize,
    /// Frame had ra/dec but no DSO within [`tolerance_deg`].
    pub no_match: usize,
    /// Frame already had a non-empty `object` — skipped without query.
    pub already_labeled: usize,
    /// Frame was missing ra and/or dec (or was not found in the `frames` table).
    pub missing_coords: usize,
    /// Per-frame SQL or DSO lookup errors. The loop continues past these.
    pub errors: usize,
    /// True if the cancel flag was observed before the loop completed.
    pub cancelled: bool,
}

/// Callback type for per-frame progress. Invoked twice per frame — first with
/// `Processing` before the lookup, and again after with the resolved outcome.
pub type ProgressFn<'a> = &'a (dyn Fn(AutofindProgress) + Send + Sync);

/// Iterate `frame_ids`, labelling each frame's `object` column from the
/// nearest DSO if — and only if — the frame has RA/Dec and no existing
/// object value.
pub fn autofind_objects_from_coordinates(
    conn: &Connection,
    catalog: &DsoCatalog,
    frame_ids: &[i64],
    tolerance_deg: f64,
    cancel: Arc<AtomicBool>,
    progress: ProgressFn<'_>,
) -> Result<AutofindSummary> {
    let tolerance_deg = if tolerance_deg.is_finite() && tolerance_deg > 0.0 {
        tolerance_deg
    } else {
        AUTOFIND_DEFAULT_MAX_DEG
    };

    let mut s = AutofindSummary {
        total: frame_ids.len(),
        ..Default::default()
    };

    // Default-everything-None progress event for the given frame_id. Each
    // call site then sets only the fields that are actually meaningful.
    let base = |frame_id: i64, current: usize, total: usize, status: AutofindStatus| {
        AutofindProgress {
            frame_id,
            current,
            total,
            status,
            designation: None,
            distance_deg: None,
            reason: None,
            frame_ra: None,
            frame_dec: None,
            closest_designation: None,
            closest_distance_deg: None,
        }
    };

    tracing::info!(total = s.total, tolerance_deg, "autofind batch starting");

    for (i, frame_id) in frame_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            tracing::info!(current = i, total = s.total, "autofind cancelled");
            s.cancelled = true;
            break;
        }

        progress(base(*frame_id, i, s.total, AutofindStatus::Processing));

        // Single cheap SELECT avoids pulling the full Frame struct per iteration.
        let row: rusqlite::Result<(Option<f64>, Option<f64>, Option<String>)> = conn
            .query_row(
                "SELECT ra, dec, object FROM frames WHERE id = ?1",
                [frame_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            );

        let (ra, dec, existing) = match row {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(frame_id, error = %e, "autofind: SQL load failed");
                s.errors += 1;
                progress(base(*frame_id, i + 1, s.total, AutofindStatus::Error));
                continue;
            }
        };

        if existing.as_deref().map(|v| !v.is_empty()).unwrap_or(false) {
            tracing::debug!(frame_id, object = ?existing, "autofind: skip, already labeled");
            s.already_labeled += 1;
            let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::AlreadyLabeled);
            p.frame_ra = ra;
            p.frame_dec = dec;
            progress(p);
            continue;
        }

        let (Some(ra), Some(dec)) = (ra, dec) else {
            tracing::debug!(frame_id, ra = ?ra, dec = ?dec, "autofind: skip, missing coordinates");
            s.missing_coords += 1;
            let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::MissingCoords);
            p.frame_ra = ra;
            p.frame_dec = dec;
            progress(p);
            continue;
        };

        // Scanner sentinel: some FITS pipelines write (ra, dec) = (0, 0) when
        // no coordinates are known instead of leaving the fields NULL. Treat
        // the literal origin as "missing coordinates" — nobody actually
        // images at (0h, 0°), and this prevents a cluster of bogus no_match
        // events all pointing at the same closest-to-origin DSO.
        if ra == 0.0 && dec == 0.0 {
            tracing::debug!(frame_id, "autofind: skip, missing coordinates (sentinel 0,0)");
            s.missing_coords += 1;
            let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::MissingCoords);
            p.frame_ra = Some(ra);
            p.frame_dec = Some(dec);
            progress(p);
            continue;
        }

        match catalog.find_best_within(ra, dec, tolerance_deg) {
            Some(m) => {
                let label = format!("{AUTOFIND_PREFIX}{}", m.designation);
                match update_frame_object_if_missing(conn, *frame_id, &label) {
                    Ok(true) => {
                        s.labeled += 1;
                        let reason = match m.reason {
                            DsoMatchReason::Contains => "contains",
                            DsoMatchReason::Nearest => "nearest",
                        };
                        tracing::debug!(
                            frame_id,
                            designation = %m.designation,
                            reason,
                            distance_deg = m.distance_deg,
                            ra,
                            dec,
                            "autofind: frame labeled"
                        );
                        let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::Labeled);
                        p.designation = Some(m.designation);
                        p.distance_deg = Some(m.distance_deg);
                        p.reason = Some(reason.into());
                        p.frame_ra = Some(ra);
                        p.frame_dec = Some(dec);
                        progress(p);
                    }
                    Ok(false) => {
                        // Race: some other writer labelled the frame between
                        // our SELECT and UPDATE. Count as already-labelled.
                        tracing::debug!(frame_id, "autofind: skip, already labeled (race after SELECT)");
                        s.already_labeled += 1;
                        let mut p = base(
                            *frame_id,
                            i + 1,
                            s.total,
                            AutofindStatus::AlreadyLabeled,
                        );
                        p.frame_ra = Some(ra);
                        p.frame_dec = Some(dec);
                        progress(p);
                    }
                    Err(e) => {
                        tracing::warn!(frame_id, error = %e, "autofind: UPDATE failed");
                        s.errors += 1;
                        let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::Error);
                        p.frame_ra = Some(ra);
                        p.frame_dec = Some(dec);
                        progress(p);
                    }
                }
            }
            None => {
                // No DSO within tolerance. Run an unbounded nearest lookup
                // to tell the user which DSO was closest and how far — lets
                // them see that e.g. the closest was at 0.38° (just outside
                // the 0.2° cap) rather than guessing.
                let closest = catalog.nearest_dso(ra, dec);
                match &closest {
                    Some((name, dist)) => tracing::debug!(
                        frame_id,
                        ra,
                        dec,
                        closest_designation = %name,
                        closest_distance_deg = dist,
                        tolerance_deg,
                        "autofind: no match within tolerance"
                    ),
                    None => tracing::debug!(frame_id, ra, dec, "autofind: no match, catalog empty"),
                }
                s.no_match += 1;
                let mut p = base(*frame_id, i + 1, s.total, AutofindStatus::NoMatch);
                p.frame_ra = Some(ra);
                p.frame_dec = Some(dec);
                if let Some((name, dist)) = closest {
                    p.closest_designation = Some(name);
                    p.closest_distance_deg = Some(dist);
                }
                progress(p);
            }
        }
    }

    tracing::info!(
        labeled = s.labeled,
        no_match = s.no_match,
        already_labeled = s.already_labeled,
        missing_coords = s.missing_coords,
        errors = s.errors,
        cancelled = s.cancelled,
        "autofind batch done"
    );

    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    fn insert_minimal_file(conn: &Connection, file_id: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
             VALUES (?1, ?2, ?3, 0, '2026-01-01', 'FITS', '2026-01-01')",
            rusqlite::params![file_id, path, format!("{file_id}.fits")],
        ).unwrap();
    }

    fn insert_frame(
        conn: &Connection,
        id: i64,
        file_id: i64,
        ra: Option<f64>,
        dec: Option<f64>,
        object: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO frames (id, file_id, ra, dec, object) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, file_id, ra, dec, object],
        ).unwrap();
    }

    #[test]
    fn autofind_labels_m42_skips_others() {
        let conn = Connection::open_in_memory().expect("memory db");
        init_db(&conn).expect("init schema");

        // File rows for the FK constraint on frames.file_id
        insert_minimal_file(&conn, 1, "/tmp/a.fits");
        insert_minimal_file(&conn, 2, "/tmp/b.fits");
        insert_minimal_file(&conn, 3, "/tmp/c.fits");

        // Frame 1: ra/dec near M 42, object NULL → should be labelled
        insert_frame(&conn, 1, 1, Some(83.82), Some(-5.39), None);
        // Frame 2: ra NULL → missing_coords
        insert_frame(&conn, 2, 2, None, Some(-5.39), None);
        // Frame 3: object already set → already_labeled (no lookup)
        insert_frame(&conn, 3, 3, Some(83.82), Some(-5.39), Some("MyField"));

        let cat = DsoCatalog::load().expect("catalog");
        let cancel = Arc::new(AtomicBool::new(false));
        let events_seen = std::sync::Mutex::new(Vec::<AutofindStatus>::new());
        let progress: &(dyn Fn(AutofindProgress) + Send + Sync) = &|p| {
            // Only collect terminal statuses (skip the Processing pre-event).
            if p.status != AutofindStatus::Processing {
                events_seen.lock().unwrap().push(p.status);
            }
        };

        let summary = autofind_objects_from_coordinates(
            &conn,
            &cat,
            &[1, 2, 3],
            0.5,
            cancel,
            &progress,
        ).expect("autofind");

        assert_eq!(summary.total, 3);
        assert_eq!(summary.labeled, 1, "summary: {:?}", summary);
        assert_eq!(summary.missing_coords, 1, "summary: {:?}", summary);
        assert_eq!(summary.already_labeled, 1, "summary: {:?}", summary);
        assert_eq!(summary.no_match, 0);
        assert_eq!(summary.errors, 0);
        assert!(!summary.cancelled);

        // Frame 1 was labelled with "Autofind: <M 42 designation>"
        let obj: Option<String> = conn
            .query_row("SELECT object FROM frames WHERE id = 1", [], |r| r.get(0))
            .expect("read frame 1");
        let obj = obj.expect("frame 1 object should be set");
        assert!(
            obj.starts_with(AUTOFIND_PREFIX),
            "expected Autofind prefix, got {obj}"
        );
        assert!(
            obj.to_lowercase().contains("m") || obj.contains("42"),
            "expected M 42 designation in {obj}"
        );

        // Frame 1 should now carry the override marker so the dual-pane
        // editor's revert UI lights up and the file-row marker renders.
        let override_flag: i64 = conn
            .query_row("SELECT override FROM frames WHERE id = 1", [], |r| r.get(0))
            .expect("read frame 1 override");
        assert_eq!(override_flag, 1, "auto-fill must set override = 1");

        // Frame 2 untouched (no coords → no auto-fill, no override change).
        let obj2: Option<String> = conn
            .query_row("SELECT object FROM frames WHERE id = 2", [], |r| r.get(0))
            .expect("read frame 2");
        assert!(obj2.is_none());
        let override_flag_2: i64 = conn
            .query_row("SELECT override FROM frames WHERE id = 2", [], |r| r.get(0))
            .expect("read frame 2 override");
        assert_eq!(override_flag_2, 0, "untouched frames must keep override = 0");

        // Frame 3 untouched.
        let obj3: Option<String> = conn
            .query_row("SELECT object FROM frames WHERE id = 3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(obj3.as_deref(), Some("MyField"));
        let override_flag_3: i64 = conn
            .query_row("SELECT override FROM frames WHERE id = 3", [], |r| r.get(0))
            .expect("read frame 3 override");
        assert_eq!(override_flag_3, 0, "already-labelled frames must keep override = 0");

        // Terminal events count matches summary
        let terminals = events_seen.lock().unwrap();
        assert_eq!(terminals.len(), 3);
    }

    #[test]
    fn autofind_cancel_stops_mid_run() {
        let conn = Connection::open_in_memory().expect("memory db");
        init_db(&conn).expect("init schema");

        insert_minimal_file(&conn, 1, "/tmp/a.fits");
        insert_minimal_file(&conn, 2, "/tmp/b.fits");
        insert_frame(&conn, 1, 1, Some(83.82), Some(-5.39), None);
        insert_frame(&conn, 2, 2, Some(83.82), Some(-5.39), None);

        let cat = DsoCatalog::load().expect("catalog");
        let cancel = Arc::new(AtomicBool::new(true)); // pre-set: cancel before any work
        let progress: &(dyn Fn(AutofindProgress) + Send + Sync) = &|_| {};

        let summary = autofind_objects_from_coordinates(
            &conn, &cat, &[1, 2], 0.5, cancel, &progress,
        ).expect("autofind");

        assert!(summary.cancelled);
        assert_eq!(summary.labeled, 0);
    }
}
