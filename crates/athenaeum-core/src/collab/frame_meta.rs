//! The per-frame `meta` object a collab publish sends to the hub inside
//! `FrameIn.meta` (hub-contract: a JSON object of at most 8192 bytes) —
//! everything about a light beyond what has its own top-level `FrameIn`
//! field. Camel-case keys, each omitted (never `null`) when its source
//! column or row is absent — the "vouched for" rule already used for
//! anything FITS-header-shaped (`fits_writer::keywords::Bayer::parse`).
//!
//! `meta.wcs` mirrors `fits_writer::wcs::wcs_cards` (P4: gate header
//! fallbacks stay for scale/centre, the plate-solved precondition is wave
//! 3): present with a FITS 1-based `crpix` only when the frame has a
//! `plate_solves` row, omitted otherwise — never the header WCS.
//!
//! `meta.calibration` (spec §6, F8) is NOT built here — this module has no
//! `GenerationSpec` to read. `api::collab::run_publish_generation` stamps it
//! onto a NEW frame's `meta` after generation (`{dark, flat, bias}` from the
//! resolved spec's own master fields) or, for an attested light, in place of
//! generation (`external: true`, the other three `false`).

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::collab::gate::header_pixel_scale_arcsec;
use crate::db::analysis::get_frame_analyses_by_ids;
use crate::fits_writer::keywords::Bayer;
use crate::plate_solve::storage::{get_plate_solve, PlateSolveRecord};

/// The manifest-relevant facts about one LIGHT frame: the top-level
/// `FrameIn` fields this module can answer (`filterRaw`/`channel`/
/// `exptimeSec`/`dateObs`), plus the whole `meta` object.
pub struct FrameMeta {
    pub filter_raw: String,
    pub channel: String,
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    pub meta: Value,
}

/// Raw `frames` columns `build_frame_meta` needs.
struct FrameRow {
    filter: Option<String>,
    exptime: Option<f64>,
    date_obs: Option<String>,
    instrume: Option<String>,
    telescop: Option<String>,
    xbinning: Option<i64>,
    naxis1: Option<i64>,
    naxis2: Option<i64>,
    focallen: Option<f64>,
    xpixsz: Option<f64>,
    bayerpat: Option<String>,
}

fn load_frame_row(conn: &Connection, frame_id: i64) -> Result<FrameRow> {
    Ok(conn.query_row(
        "SELECT filter, exptime, date_obs, instrume, telescop, xbinning, naxis1, naxis2, \
                focallen, xpixsz, bayerpat \
         FROM frames WHERE id = ?1",
        [frame_id],
        |r| {
            Ok(FrameRow {
                filter: r.get(0)?,
                exptime: r.get(1)?,
                date_obs: r.get(2)?,
                instrume: r.get(3)?,
                telescop: r.get(4)?,
                xbinning: r.get(5)?,
                naxis1: r.get(6)?,
                naxis2: r.get(7)?,
                focallen: r.get(8)?,
                xpixsz: r.get(9)?,
                bayerpat: r.get(10)?,
            })
        },
    )?)
}

/// Build the manifest `meta` for one frame — the DB-wiring counterpart of the
/// pure gate engine (`collab::gate`). Only ever called from the
/// render+solver-gated publish path (`api::collab`), which already maps
/// `anyhow::Error` at its own boundary — hence the plain `anyhow::Result`
/// here rather than `ApiError`.
pub fn build_frame_meta(conn: &Connection, frame_id: i64) -> Result<FrameMeta> {
    let frow = load_frame_row(conn, frame_id)?;
    let analysis = get_frame_analyses_by_ids(conn, &[frame_id])?
        .into_iter()
        .next();
    let solve = get_plate_solve(conn, frame_id)?;

    let bayer = frow.bayerpat.as_deref().and_then(Bayer::parse);
    let channel = if bayer.is_some() { "osc" } else { "mono" }.to_string();

    let pixel_scale_arcsec = solve
        .as_ref()
        .map(|s| s.pixel_scale_arcsec)
        .or_else(|| header_pixel_scale_arcsec(frow.xpixsz, frow.focallen));

    let mut meta = Map::new();
    if let Some(v) = &frow.instrume {
        meta.insert("instrume".into(), json!(v));
    }
    if let Some(v) = &frow.telescop {
        meta.insert("telescope".into(), json!(v));
    }
    if let Some(v) = frow.xbinning {
        meta.insert("xbinning".into(), json!(v));
    }
    if let Some(v) = frow.naxis1 {
        meta.insert("naxis1".into(), json!(v));
    }
    if let Some(v) = frow.naxis2 {
        meta.insert("naxis2".into(), json!(v));
    }
    if let Some(v) = pixel_scale_arcsec {
        meta.insert("pixelScaleArcsec".into(), json!(v));
    }
    if let Some(b) = bayer {
        meta.insert("bayerpat".into(), json!(b.as_str()));
    }
    if let Some(v) = frow.focallen {
        meta.insert("focalLen".into(), json!(v));
    }
    if let Some(a) = aperture_mm_of_frame(conn, frame_id)? {
        meta.insert("aperture".into(), json!(a));
    }
    if let Some(a) = &analysis {
        if let Some(scale) = pixel_scale_arcsec {
            meta.insert("fwhmArcsec".into(), json!(a.median_fwhm * scale));
        }
        meta.insert("eccentricity".into(), json!(a.median_eccentricity));
        meta.insert("starsDetected".into(), json!(a.stars_detected));
        meta.insert("medianSnr".into(), json!(a.median_snr));
        meta.insert("snrWeight".into(), json!(a.snr_weight));
        meta.insert("frameSnr".into(), json!(a.frame_snr));
        // Append-only (PC-13 / C6): present only when the row says which measurement wrote it.
        if let Some(v) = a.measure_version {
            meta.insert("measureVersion".into(), json!(v));
        }
    }
    if let Some(s) = &solve {
        let mut wcs = Map::new();
        wcs.insert("crval1".into(), json!(s.crval1));
        wcs.insert("crval2".into(), json!(s.crval2));
        // FITS 1-based reference pixel — the stored record is 0-based (see
        // `fits_writer::wcs::wcs_cards`, which applies the identical +1).
        wcs.insert("crpix1".into(), json!(s.crpix1 + 1.0));
        wcs.insert("crpix2".into(), json!(s.crpix2 + 1.0));
        wcs.insert("cd".into(), json!([s.cd1_1, s.cd1_2, s.cd2_1, s.cd2_2]));
        if let Some(order) = s.sip_order {
            if let (Some(a_json), Some(b_json)) = (&s.sip_a_coeffs, &s.sip_b_coeffs) {
                let mut sip = Map::new();
                sip.insert("order".into(), json!(order));
                sip.insert("a".into(), serde_json::from_str::<Value>(a_json)?);
                sip.insert("b".into(), serde_json::from_str::<Value>(b_json)?);
                if let Some(ap) = &s.sip_ap_coeffs {
                    sip.insert("ap".into(), serde_json::from_str::<Value>(ap)?);
                }
                if let Some(bp) = &s.sip_bp_coeffs {
                    sip.insert("bp".into(), serde_json::from_str::<Value>(bp)?);
                }
                wcs.insert("sip".into(), Value::Object(sip));
            }
        }
        meta.insert("wcs".into(), Value::Object(wcs));
    }

    Ok(FrameMeta {
        filter_raw: frow.filter.unwrap_or_default().trim().to_string(),
        channel,
        exptime_sec: frow.exptime.unwrap_or(0.0),
        date_obs: frow.date_obs,
        meta: Value::Object(meta),
    })
}

/// What a stacking run reads from one project frame's manifest row (plan
/// 6b-2 C4, spec 4.3), parsed once from `project_frames_local.manifest_json`
/// (an own row's `FrameInWire` shape or a replica's `FrameViewWire`; the
/// keys read here are the same in both). `exptimeSec` and `dateObs` are
/// top-level fields; everything else lives under `meta`, which is frozen at
/// announce, so an older frame simply lacks the newer keys (`aperture`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectFrameMeta {
    pub exptime_sec: f64,
    pub date_obs: Option<String>,
    pub naxis1: Option<i64>,
    pub naxis2: Option<i64>,
    pub pixel_scale_arcsec: Option<f64>,
    pub bayerpat: Option<String>,
    pub focal_len: Option<f64>,
    pub aperture: Option<f64>,
    pub instrume: Option<String>,
    pub wcs: Option<ManifestWcs>,
}

/// `meta.wcs` as [`build_frame_meta`] writes it: TAN (no CTYPE), `crpix`
/// FITS 1-BASED, the CD matrix row-major, optional SIP tables.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestWcs {
    pub crval1: f64,
    pub crval2: f64,
    pub crpix1: f64,
    pub crpix2: f64,
    pub cd: [f64; 4],
    #[serde(default)]
    pub sip: Option<ManifestSip>,
}

/// `meta.wcs.sip`: the stored tables verbatim (square or triangular).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ManifestSip {
    pub order: i32,
    pub a: Vec<Vec<f64>>,
    pub b: Vec<Vec<f64>>,
    #[serde(default)]
    pub ap: Option<Vec<Vec<f64>>>,
    #[serde(default)]
    pub bp: Option<Vec<Vec<f64>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestRowWire {
    exptime_sec: f64,
    #[serde(default)]
    date_obs: Option<String>,
    #[serde(default)]
    meta: Option<MetaWire>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct MetaWire {
    naxis1: Option<i64>,
    naxis2: Option<i64>,
    pixel_scale_arcsec: Option<f64>,
    bayerpat: Option<String>,
    focal_len: Option<f64>,
    aperture: Option<f64>,
    instrume: Option<String>,
    wcs: Option<ManifestWcs>,
}

/// Parse one manifest row (plan 6b-2 C4). A missing exposure or a malformed
/// key is an error: the caller leaves the frame out with a reason, never a
/// guess. Unknown keys are ignored.
pub fn parse_project_frame_meta(manifest_json: &str) -> Result<ProjectFrameMeta> {
    let row: ManifestRowWire =
        serde_json::from_str(manifest_json).context("manifest row is not a frame row")?;
    let meta = row.meta.unwrap_or_default();
    Ok(ProjectFrameMeta {
        exptime_sec: row.exptime_sec,
        date_obs: row.date_obs,
        naxis1: meta.naxis1,
        naxis2: meta.naxis2,
        pixel_scale_arcsec: meta.pixel_scale_arcsec,
        bayerpat: meta.bayerpat,
        focal_len: meta.focal_len,
        aperture: meta.aperture,
        instrume: meta.instrume,
        wcs: meta.wcs,
    })
}

impl ManifestWcs {
    /// The `PlateSolveRecord` this WCS is (plan 6b-2 C4): `crpix` back to
    /// 0-based (the record's convention, `fits_writer::wcs` adds the 1),
    /// the SIP tables re-encoded as the JSON strings the catalog stores,
    /// `frame_id = 0` (a project frame has no catalog row), the scale from
    /// the manifest when it has one (else `WcsSolution::pixel_scale_arcsec`), the rotation as
    /// `WcsSolution::field_rotation_deg` reports it, counts zero.
    pub fn to_record(&self, pixel_scale_arcsec: Option<f64>) -> PlateSolveRecord {
        let [cd1_1, cd1_2, cd2_1, cd2_2] = self.cd;
        // Without a manifest scale the WCS's own: `WcsSolution` is the one
        // definition of a solve's pixel scale. The determinant form is only
        // the fallback for a SIP table that cannot be read.
        let det_scale = (cd1_1 * cd2_2 - cd1_2 * cd2_1).abs().sqrt() * 3600.0;
        let rotation = (-cd1_2).atan2(cd2_2).to_degrees().rem_euclid(360.0);
        let table =
            |t: &Vec<Vec<f64>>| serde_json::to_string(t).expect("a table of numbers serializes");
        let mut record = PlateSolveRecord {
            id: None,
            frame_id: 0,
            crpix1: self.crpix1 - 1.0,
            crpix2: self.crpix2 - 1.0,
            crval1: self.crval1,
            crval2: self.crval2,
            cd1_1,
            cd1_2,
            cd2_1,
            cd2_2,
            sip_order: self.sip.as_ref().map(|s| s.order),
            sip_a_coeffs: self.sip.as_ref().map(|s| table(&s.a)),
            sip_b_coeffs: self.sip.as_ref().map(|s| table(&s.b)),
            sip_ap_coeffs: self.sip.as_ref().and_then(|s| s.ap.as_ref().map(table)),
            sip_bp_coeffs: self.sip.as_ref().and_then(|s| s.bp.as_ref().map(table)),
            matched_stars: 0,
            total_detected: 0,
            rms_residual_px: 0.0,
            rms_residual_arcsec: 0.0,
            pixel_scale_arcsec: pixel_scale_arcsec.unwrap_or(det_scale),
            field_rotation_deg: rotation,
            solve_time_ms: 0,
            catalog_used: "manifest".to_string(),
            algorithm_used: "manifest".to_string(),
            solved_at: String::new(),
            expected_catalog_stars_in_fov: None,
            inlier_ratio: None,
        };
        if pixel_scale_arcsec.is_none() {
            if let Some(solution) = record.to_solution() {
                record.pixel_scale_arcsec = solution.pixel_scale_arcsec();
            }
        }
        record
    }
}

/// `APTDIA` (mm) from the frame's stored header blob (spec 8: `frames` has
/// no aperture column). The first `fits_header` row that carries one wins.
fn aperture_mm_of_frame(conn: &Connection, frame_id: i64) -> Result<Option<f64>> {
    let mut stmt = conn.prepare(
        "SELECT fi.format, fh.header FROM frames f
         JOIN files fi ON fi.id = f.file_id
         JOIN fits_header fh ON fh.file_id = f.file_id
         WHERE f.id = ?1 ORDER BY fh.id",
    )?;
    let rows = stmt.query_map([frame_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (format, header) = row?;
        if let Some(a) =
            crate::fits_parser::stored_header::aperture_mm_from_stored_header(&format, &header)
        {
            return Ok(Some(a));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::analysis::upsert_frame_analysis;
    use crate::db::schema::init_db;
    use crate::models::FrameAnalysis;
    use crate::plate_solve::storage::{insert_plate_solve, PlateSolveRecord};
    use rusqlite::params;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn seed_frame(conn: &Connection, filter: Option<&str>, bayerpat: Option<&str>) -> i64 {
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) \
             VALUES (?1, 'f.fits', 0, '2026-09-24T00:00:00Z', 'FITS')",
            params![format!("/t/f_{n}.fits")],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, filter, exptime, date_obs, instrume, \
                                  telescop, xbinning, naxis1, naxis2, focallen, xpixsz, bayerpat) \
             VALUES (?1, 'Light', ?2, 300.0, '2026-09-24T02:00:00Z', 'ASI2600MM', 'RASA 11', \
                     1, 6248, 4176, 620.0, 3.76, ?3)",
            params![file_id, filter, bayerpat],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_analysis(conn: &Connection, frame_id: i64) {
        seed_analysis_version(conn, frame_id, None);
    }

    fn seed_analysis_version(conn: &Connection, frame_id: i64, measure_version: Option<u32>) {
        upsert_frame_analysis(
            conn,
            &FrameAnalysis {
                id: None,
                frame_id,
                file_id: frame_id,
                stars_detected: 400,
                median_fwhm: 2.0,
                median_eccentricity: 0.4,
                median_snr: 10.0,
                median_hfr: 2.0,
                frame_snr: 10.0,
                snr_weight: 1.0,
                psf_signal: 100.0,
                background: 10.0,
                noise: 1.0,
                detection_threshold: 5.0,
                width: 6248,
                height: 4176,
                source_channels: 1,
                trail_r_squared: 0.0,
                possibly_trailed: false,
                median_beta: None,
                quality_score: None,
                config_hash: None,
                analyzed_at: "2026-09-24T02:05:00Z".to_string(),
                measure_version,
                stars_refused: None,
                frame_verdict: None,
            },
        )
        .unwrap();
    }

    /// `sip_order = None` seeds no SIP tables at all (plain TAN); `Some(order)`
    /// seeds a square `(order+1)x(order+1)` table for each of A/B/AP/BP —
    /// the worst case `build_frame_meta` produces.
    fn seed_solve(conn: &Connection, frame_id: i64, sip_order: Option<i32>) {
        let sip_table = |c: f64| -> Option<String> {
            sip_order.map(|order| {
                let n = (order as usize) + 1;
                let row: Vec<f64> = (0..n).map(|j| c + j as f64 * 1e-7).collect();
                let table: Vec<Vec<f64>> = (0..n).map(|_| row.clone()).collect();
                serde_json::to_string(&table).unwrap()
            })
        };
        insert_plate_solve(
            conn,
            &PlateSolveRecord {
                id: None,
                frame_id,
                crpix1: 3123.5,
                crpix2: 2087.5,
                crval1: 210.802,
                crval2: 54.349,
                cd1_1: -0.0002777,
                cd1_2: 0.0,
                cd2_1: 0.0,
                cd2_2: 0.0002777,
                sip_order,
                sip_a_coeffs: sip_table(1e-6),
                sip_b_coeffs: sip_table(2e-6),
                sip_ap_coeffs: sip_table(3e-6),
                sip_bp_coeffs: sip_table(4e-6),
                matched_stars: 300,
                total_detected: 400,
                rms_residual_px: 0.3,
                rms_residual_arcsec: 0.6,
                pixel_scale_arcsec: 2.0,
                field_rotation_deg: 0.0,
                solve_time_ms: 500,
                catalog_used: "gaia".into(),
                algorithm_used: "quad".into(),
                solved_at: "2026-09-24T02:10:00Z".into(),
                expected_catalog_stars_in_fov: Some(500),
                inlier_ratio: Some(0.9),
            },
        )
        .unwrap();
    }

    #[test]
    fn wcs_crpix_is_one_based_and_fwhm_is_converted() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some(" L "), None);
        seed_analysis(&conn, frame_id);
        seed_solve(&conn, frame_id, None);

        let m = build_frame_meta(&conn, frame_id).unwrap();
        assert_eq!(m.filter_raw, "L");
        assert_eq!(m.channel, "mono");
        assert_eq!(m.exptime_sec, 300.0);

        let wcs = m.meta.get("wcs").expect("wcs present with a solve");
        assert_eq!(wcs["crpix1"].as_f64().unwrap(), 3123.5 + 1.0);
        assert_eq!(wcs["crpix2"].as_f64().unwrap(), 2087.5 + 1.0);
        assert!(wcs.get("sip").is_none(), "no SIP order was seeded");

        // median_fwhm(px) * pixel_scale_arcsec(solve) = 2.0 * 2.0
        let fwhm = m.meta.get("fwhmArcsec").unwrap().as_f64().unwrap();
        assert_eq!(fwhm, 4.0);
    }

    #[test]
    fn channel_is_osc_only_with_a_vouched_for_bayer_pattern() {
        let conn = seed_db();
        let osc = seed_frame(&conn, Some("L"), Some("RGGB"));
        assert_eq!(build_frame_meta(&conn, osc).unwrap().channel, "osc");
        assert_eq!(
            build_frame_meta(&conn, osc).unwrap().meta["bayerpat"],
            "RGGB"
        );

        let mono = seed_frame(&conn, Some("L"), None);
        assert_eq!(build_frame_meta(&conn, mono).unwrap().channel, "mono");
        assert!(build_frame_meta(&conn, mono)
            .unwrap()
            .meta
            .get("bayerpat")
            .is_none());
    }

    /// C6: the meta says which measurement wrote the analysis, only when it is known (append-only on the wire).
    #[test]
    fn frame_meta_carries_measure_version_when_known() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("L"), None);
        seed_analysis_version(&conn, frame_id, Some(5));
        let m = build_frame_meta(&conn, frame_id).unwrap();
        assert_eq!(m.meta.get("measureVersion"), Some(&serde_json::json!(5)));
    }

    #[test]
    fn frame_meta_omits_it_for_a_legacy_row() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("L"), None);
        seed_analysis(&conn, frame_id);
        let m = build_frame_meta(&conn, frame_id).unwrap();
        assert!(m.meta.get("starsDetected").is_some(), "premise: the analysis is in the meta");
        assert!(m.meta.get("measureVersion").is_none());
    }

    #[test]
    fn no_solve_means_no_wcs_key() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("L"), None);
        let m = build_frame_meta(&conn, frame_id).unwrap();
        assert!(m.meta.get("wcs").is_none());
    }

    /// Hub limit (hub-contract): `meta` is a JSON object of at most 8192
    /// bytes. A full SIP order-5 solve (A/B/AP/BP, 6x6 tables each) is the
    /// worst case this module produces — pin it under the limit.
    #[test]
    fn full_sip_order_5_meta_stays_under_the_hub_byte_limit() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("Ha"), Some("RGGB"));
        seed_analysis(&conn, frame_id);
        seed_solve(&conn, frame_id, Some(5));

        let m = build_frame_meta(&conn, frame_id).unwrap();
        let bytes = serde_json::to_vec(&m.meta).unwrap();
        assert!(
            bytes.len() <= 8192,
            "meta is {} bytes, over the hub's 8192-byte limit",
            bytes.len()
        );
        assert!(m.meta.get("wcs").unwrap().get("sip").is_some());
    }

    /// Plan 6b-2 C4: the round trip publish -> manifest -> stacking returns the
    /// catalog's own solve: crpix back to 0-based, CD and SIP tables equal,
    /// and the same pixel -> sky mapping.
    #[test]
    fn a_published_wcs_reads_back_as_the_catalog_record() {
        let conn = seed_db();
        let frame_id = seed_frame(&conn, Some("L"), None);
        seed_solve(&conn, frame_id, Some(3));
        let original = crate::plate_solve::storage::get_plate_solve(&conn, frame_id)
            .unwrap()
            .unwrap();
        let m = build_frame_meta(&conn, frame_id).unwrap();
        let row = serde_json::json!({
            "exptimeSec": m.exptime_sec,
            "dateObs": m.date_obs,
            "meta": m.meta,
        });
        let parsed = parse_project_frame_meta(&row.to_string()).unwrap();
        assert_eq!(parsed.exptime_sec, 300.0);
        assert_eq!((parsed.naxis1, parsed.naxis2), (Some(6248), Some(4176)));
        assert_eq!(parsed.pixel_scale_arcsec, Some(2.0));
        let record = parsed
            .wcs
            .as_ref()
            .unwrap()
            .to_record(parsed.pixel_scale_arcsec);
        assert_eq!(record.frame_id, 0);
        assert_eq!(
            (record.crpix1, record.crpix2),
            (original.crpix1, original.crpix2)
        );
        assert_eq!(
            (record.cd1_1, record.cd1_2, record.cd2_1, record.cd2_2),
            (
                original.cd1_1,
                original.cd1_2,
                original.cd2_1,
                original.cd2_2
            )
        );
        assert_eq!(record.sip_order, original.sip_order);
        assert_eq!(record.sip_a_coeffs, original.sip_a_coeffs);
        assert_eq!(record.sip_bp_coeffs, original.sip_bp_coeffs);
        let (a, b) = (
            original.to_solution().unwrap(),
            record.to_solution().unwrap(),
        );
        for (x, y) in [(0.0, 0.0), (3123.5, 2087.5), (6000.0, 100.0)] {
            let (ra0, dec0) = a.pixel_to_sky(x, y);
            let (ra1, dec1) = b.pixel_to_sky(x, y);
            assert!((ra0 - ra1).abs() < 1e-12 && (dec0 - dec1).abs() < 1e-12);
        }
    }

    /// Plan 6b-2 C4: an own row (the `FrameInWire` shape, no `dateObs` when
    /// unknown), a replica row with every key, and a row with `meta: null`.
    #[test]
    fn the_manifest_parser_reads_own_replica_and_bare_rows() {
        let own = r#"{"frameUuid":"u1","fileName":"c.fits","blake3":"b","byteSize":1,"xxh3":"x",
            "filterRaw":"Red","filterCanonical":"R","channel":"osc","exptimeSec":120.0,"gateVersion":0,
            "meta":{"bayerpat":"RGGB","focalLen":400.0,"aperture":80.0,"instrume":"ASI2600MC"}}"#;
        let m = parse_project_frame_meta(own).unwrap();
        assert_eq!(m.exptime_sec, 120.0);
        assert_eq!(m.date_obs, None);
        assert_eq!(m.bayerpat.as_deref(), Some("RGGB"));
        assert_eq!((m.focal_len, m.aperture), (Some(400.0), Some(80.0)));
        assert_eq!(m.wcs, None);
        let bare = parse_project_frame_meta(r#"{"exptimeSec":60.0,"meta":null}"#).unwrap();
        assert_eq!(
            bare,
            ProjectFrameMeta {
                exptime_sec: 60.0,
                ..ProjectFrameMeta::default()
            }
        );
        assert!(
            parse_project_frame_meta(r#"{"meta":{}}"#).is_err(),
            "no exposure is an error"
        );
        assert!(
            parse_project_frame_meta(r#"{"exptimeSec":1.0,"meta":{"wcs":{"cd":[1,2]}}}"#).is_err(),
            "a malformed WCS is an error, never a guess"
        );
    }

    /// Plan 6b-2 C4 / spec section 8: publication carries the aperture from the
    /// stored header blob; without one the key is omitted.
    #[test]
    fn meta_carries_the_aperture_from_the_stored_header() {
        let conn = seed_db();
        let with = seed_frame(&conn, Some("L"), None);
        let without = seed_frame(&conn, Some("L"), None);
        let card_text = format!(
            "{:<80}{:<80}",
            "APTDIA  =                200.0 / aperture diameter (mm)", "END"
        );
        conn.execute(
            "INSERT INTO fits_header (file_id, header)
             VALUES ((SELECT file_id FROM frames WHERE id = ?1), ?2)",
            params![with, card_text],
        )
        .unwrap();
        assert_eq!(
            build_frame_meta(&conn, with).unwrap().meta["aperture"],
            200.0
        );
        assert!(build_frame_meta(&conn, without)
            .unwrap()
            .meta
            .get("aperture")
            .is_none());
    }

    /// Parked from Task 4: with no manifest scale the record's scale is the
    /// WCS's own (`WcsSolution::pixel_scale_arcsec`, the geometric mean of
    /// the column norms), not `sqrt|det CD|`; a manifest scale still wins.
    #[test]
    fn a_manifest_wcs_without_a_scale_uses_the_solutions_own_scale() {
        let s = 1.5 / 3600.0;
        // A sheared CD, where the two definitions differ.
        let wcs = ManifestWcs {
            crval1: 83.8,
            crval2: -5.4,
            crpix1: 32.5,
            crpix2: 24.5,
            cd: [s, s, 0.0, s],
            sip: None,
        };
        let record = wcs.to_record(None);
        let expected = record.to_solution().unwrap().pixel_scale_arcsec();
        assert_eq!(record.pixel_scale_arcsec, expected);
        let det = (s * s).abs().sqrt() * 3600.0;
        assert!(
            (record.pixel_scale_arcsec - det).abs() > 1e-6,
            "the determinant form would have given {det}"
        );
        assert_eq!(wcs.to_record(Some(2.0)).pixel_scale_arcsec, 2.0);
    }
}
