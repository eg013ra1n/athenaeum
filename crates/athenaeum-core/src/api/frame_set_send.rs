//! Frame-set send (spec 2026-08-28): the export pipeline's file list as sync
//! payload entries. `PayloadEntry` is the currency between whoever decides
//! WHAT to send (a frame selection, or a frame set under an export mode) and
//! the one package builder in `api::sync` that writes it.
//!
//! [`PayloadEntry`] is UNGATED — the frame-selection send path
//! (`api::sync::selection_entries` / `build_selection_package`) is built by
//! headless consumers (Perseus links core with `default-features = false`).
//! Composing entries FROM A FRAME SET needs the export readiness gate in
//! `api::lights`, which is `render`-only, so [`frame_set_entries`] and its
//! helper carry that feature gate.
#[cfg(feature = "render")]
use std::collections::HashSet;
use std::path::PathBuf;

#[cfg(feature = "render")]
use crate::api::lights::{check_mode_ready, get_export_readiness, FlatNormMode, LightCalParams};
#[cfg(feature = "render")]
use crate::api::{db, ApiError};
#[cfg(feature = "render")]
use crate::export::models::{CalibrationSetInfo, ExportData, ExportMode};
use crate::package::PayloadKind;
#[cfg(feature = "render")]
use crate::services::ServiceContext;

/// One file to put in a package: the catalog frame it is (or derives from —
/// a calibrated artifact points at its source light), the file to copy, its
/// path inside the package (WBPP dir + filename, forward slashes) and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    pub frame_id: i64,
    pub source_path: PathBuf,
    pub rel_path: String,
    pub kind: PayloadKind,
}

/// The export pipeline's file list for one frame set under `mode`, as payload
/// entries (spec 2026-08-28 §3 steps 1–4). Gate FIRST: a not-ready mode is
/// [`ApiError::Invalid`] with the sentence the Export tab shows, and nothing has
/// been touched on disk.
///
/// Catalog-only — the entries name the files to copy; the copying is the package
/// builder's job (`api::sync::build_selection_package`).
#[cfg(feature = "render")]
pub fn frame_set_entries(
    ctx: &ServiceContext,
    frame_set_id: i64,
    mode: ExportMode,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<Vec<PayloadEntry>, ApiError> {
    let readiness = get_export_readiness(ctx, frame_set_id, flat_norm, flat_norm_mode, params)?;
    if let Err(msg) = check_mode_ready(&readiness, mode) {
        tracing::warn!(frame_set_id, ?mode, error = %msg, "frame-set send refused: mode not ready");
        return Err(ApiError::Invalid(msg));
    }
    let db = db(ctx)?;
    let conn = db.conn();
    let mut data = crate::export::collect_export_data(&conn, frame_set_id)
        .map_err(|e| ApiError::Internal(format!("collect export data: {e:#}")))?;
    crate::export::apply_export_mode(&conn, &mut data, mode)
        .map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let master_sets = crate::export::data_collector::master_set_ids(&conn, &data)
        .map_err(|e| ApiError::Internal(format!("master set ids: {e:#}")))?;
    let masters = master_frame_ids(&data, &master_sets);
    let entries = crate::export::file_organizer::compute_wbpp_placements(&data)
        .into_iter()
        .map(|p| PayloadEntry {
            frame_id: p.frame_id,
            source_path: PathBuf::from(&p.file_path),
            rel_path: if p.rel_dir.is_empty() {
                p.filename.clone()
            } else {
                format!("{}/{}", p.rel_dir, p.filename)
            },
            kind: match mode {
                ExportMode::CalibratedLights => PayloadKind::CalibratedLight,
                _ if masters.contains(&p.frame_id) => PayloadKind::Master,
                _ => PayloadKind::RawFrame,
            },
        })
        .collect::<Vec<_>>();
    tracing::info!(
        frame_set_id,
        ?mode,
        count = entries.len(),
        "frame-set send composed"
    );
    Ok(entries)
}

/// Frame ids of every master file in the tree — a master set's frames are its
/// single master file.
#[cfg(feature = "render")]
fn master_frame_ids(data: &ExportData, master_sets: &HashSet<i64>) -> HashSet<i64> {
    fn walk(info: &CalibrationSetInfo, master_sets: &HashSet<i64>, out: &mut HashSet<i64>) {
        if master_sets.contains(&info.set_id) {
            out.extend(info.frames.iter().map(|f| f.frame_id));
        }
        for node in [
            info.dark_flat.as_deref(),
            info.dark.as_deref(),
            info.bias.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            walk(node, master_sets, out);
        }
    }
    let mut out = HashSet::new();
    for group in &data.groups {
        for subgroup in &group.subgroups {
            for node in [
                subgroup.flat.as_ref(),
                subgroup.dark.as_ref(),
                subgroup.bias.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                walk(node, master_sets, &mut out);
            }
        }
    }
    out
}

// The fixture exercises `frame_set_entries`, so it rides the same gate as the
// function under test (mirrors `api/mod.rs`'s own `cfg(all(test, feature = …))`).
#[cfg(all(test, feature = "render"))]
mod tests {
    use super::*;
    use crate::api::lights::{FlatNormMode, LightCalParams};
    use crate::db::light_calibrations::{
        upsert_light_calibration, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };
    use crate::export::models::ExportMode;
    use crate::services::ServiceContext;
    use rusqlite::{params, Connection};

    /// A ServiceContext over a temp catalog (`services::ServiceContext::new_for_tests`,
    /// the constructor `api/calibration.rs` and `api/collab_exchange.rs` tests use).
    fn ctx_with(tmp: &std::path::Path) -> ServiceContext {
        ServiceContext::new_for_tests(tmp.join("catalog.db"))
    }

    fn seed(conn: &Connection) {
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", [])
            .unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (1, 1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (1, 1, 'TestCam')",
            [],
        )
        .unwrap();
        for f in [10i64, 11] {
            conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![f, format!("/test/L_{f}.fits"), format!("L_{f}.fits")]).unwrap();
            conn.execute("INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, filter, uuid) VALUES (?1, ?1, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', 'Ha', ?2)",
                params![f, format!("uuid-{f}")]).unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (1, ?1)",
                params![f],
            )
            .unwrap();
        }
        // raw dark set 100 with two frames; master flat set 200 with one file
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library) VALUES (100, 'Dark', '2026-07-05', 0)", []).unwrap();
        for i in [0i64, 1] {
            let id = 500 + i;
            conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![id, format!("/raw/D_{i}.fits"), format!("D_{i}.fits")]).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?1, 'Dark')",
                params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, ?1)",
                params![id],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library) VALUES (200, 'Flat', '2026-07-05', 1)", []).unwrap();
        conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (600, '/lib/master_flat.fits', 'master_flat.fits', 0, '2026-07-05T00:00:00Z', 'FITS')", []).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (600, 600, 'MasterFlat', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 600)",
            [],
        )
        .unwrap();
        for f in [10i64, 11] {
            conn.execute("INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 100, 'Dark', '2026-07-05T00:00:00Z')", params![f]).unwrap();
            conn.execute("INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 200, 'Flat', '2026-07-05T00:00:00Z')", params![f]).unwrap();
        }
    }

    fn prefs() -> (bool, FlatNormMode, LightCalParams) {
        (true, FlatNormMode::CentralThird, LightCalParams::default())
    }

    #[test]
    fn lights_only_and_raw_sets_compose_from_placements() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        {
            let db = ctx.db.get().unwrap();
            seed(&db.conn());
        }
        let (fn_, fm, p) = prefs();

        let lights = frame_set_entries(&ctx, 1, ExportMode::LightsOnly, fn_, fm, p).unwrap();
        assert_eq!(lights.len(), 2);
        assert!(lights
            .iter()
            .all(|e| e.kind == PayloadKind::RawFrame
                && e.rel_path.starts_with("camera_testcam/lights/")));

        let raw =
            frame_set_entries(&ctx, 1, ExportMode::RawWithCalibrationSets, fn_, fm, p).unwrap();
        assert_eq!(raw.len(), 2 + 2 + 1);
        assert_eq!(
            raw.iter().filter(|e| e.kind == PayloadKind::Master).count(),
            1
        );
        assert!(
            raw.iter()
                .any(|e| e.rel_path == "camera_testcam/DARKS_100/FLAT_200/master_flat.fits"),
            "{raw:?}"
        );
        assert!(
            raw.iter()
                .any(|e| e.rel_path == "camera_testcam/DARKS_100/D_0.fits"),
            "{raw:?}"
        );
    }

    #[test]
    fn raw_with_masters_is_refused_while_a_raw_set_is_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        {
            let db = ctx.db.get().unwrap();
            seed(&db.conn());
        }
        let (fn_, fm, p) = prefs();
        let err = frame_set_entries(&ctx, 1, ExportMode::RawWithMasters, fn_, fm, p).unwrap_err();
        assert!(
            err.to_string().contains("1 calibration set has no master"),
            "{err}"
        );
    }

    #[test]
    fn calibrated_lights_compose_artifacts_and_refuse_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        {
            let db = ctx.db.get().unwrap();
            seed(&db.conn());
        }
        let (fn_, fm, p) = prefs();
        let err = frame_set_entries(&ctx, 1, ExportMode::CalibratedLights, fn_, fm, p).unwrap_err();
        assert!(
            err.to_string()
                .contains("2 of 2 lights lack a fresh calibrated output"),
            "{err}"
        );

        // Track both lights as freshly calibrated against the master flat (set 200)
        // and the raw dark set 100 — derive_status reads the CURRENT links.
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for f in [10i64, 11] {
                upsert_light_calibration(
                    &conn,
                    &LightCalRow {
                        id: 0,
                        frame_id: Some(f),
                        source_uuid: Some(format!("uuid-{f}")),
                        source_filename: Some(format!("L_{f}.fits")),
                        output_path: format!("/lib/M31/TestCam/2026-07-05/c_L_{f}.fits"),
                        dark_set_id: Some(100),
                        flat_set_id: Some(200),
                        bias_set_id: None,
                        calstat: "BDF".into(),
                        flat_norm_applied: true,
                        flat_norm_mode: "centralThird".into(),
                        output_hash: "h".into(),
                        engine_version: LIGHT_CAL_ENGINE_VERSION,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        cal_params: "{}".into(),
                        cfa_scaling_applied: None,
                    },
                )
                .unwrap();
            }
        }
        let cal = frame_set_entries(&ctx, 1, ExportMode::CalibratedLights, fn_, fm, p).unwrap();
        assert_eq!(cal.len(), 2);
        assert!(cal.iter().all(|e| e.kind == PayloadKind::CalibratedLight));
        assert!(
            cal.iter()
                .any(|e| e.rel_path == "camera_testcam/lights/c_L_10.fits"
                    && e.source_path
                        == std::path::Path::new("/lib/M31/TestCam/2026-07-05/c_L_10.fits")),
            "{cal:?}"
        );
        assert!(
            cal.iter().all(|e| e.frame_id == 10 || e.frame_id == 11),
            "frame_id is the SOURCE light"
        );
    }
}
