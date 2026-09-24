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

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::db::analysis::get_frame_analyses_by_ids;
use crate::fits_writer::keywords::Bayer;
use crate::plate_solve::storage::get_plate_solve;

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

/// Header-fallback pixel scale — the SAME formula as
/// `api::collab::frame_gate_inputs`'s own fallback, duplicated rather than
/// shared because the two live on opposite sides of the `solver` feature
/// gate (this module; that one is render-only). `None` for a placeholder
/// `xpixsz = 0.0`/`focallen = 0.0`, the same sentinel `plate_solve::hints`
/// treats as "not actually set".
fn header_pixel_scale_arcsec(xpixsz: Option<f64>, focallen: Option<f64>) -> Option<f64> {
    match (xpixsz, focallen) {
        (Some(xpixsz), Some(focallen)) if focallen > 0.0 && xpixsz > 0.0 => {
            Some(((xpixsz / 1000.0) / focallen).atan().to_degrees() * 3600.0)
        }
        _ => None,
    }
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
    if let Some(a) = &analysis {
        if let Some(scale) = pixel_scale_arcsec {
            meta.insert("fwhmArcsec".into(), json!(a.median_fwhm * scale));
        }
        meta.insert("eccentricity".into(), json!(a.median_eccentricity));
        meta.insert("starsDetected".into(), json!(a.stars_detected));
        meta.insert("medianSnr".into(), json!(a.median_snr));
        meta.insert("snrWeight".into(), json!(a.snr_weight));
        meta.insert("frameSnr".into(), json!(a.frame_snr));
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
}
