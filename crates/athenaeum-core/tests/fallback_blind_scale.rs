//! Integration test: blind solve + focal-length correction on a frame with a
//! wrong header FOCALLEN. The solvemyastro backend re-derives scale blindly;
//! `solve_frame_with_hints` detects the mismatch and writes the corrected
//! focal length back.
//!
//! Run: cargo test -p athenaeum-core --test fallback_blind_scale -- --ignored --nocapture

use std::path::PathBuf;

use athenaeum_core::db::schema::init_db;
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::plate_solve::service;
use rusqlite::Connection;

const SMAC_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia";

// Real on-disk Heart-nebula mosaic pane, already plate-solved in the catalog.
const TEST_FITS: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Astro Pano/Heart/Pane 1/registered/Light_BIN-1_5496x3672_EXPOSURE-300.00s_FILTER-H_Mono/Light_Pane 1_300.0s_Bin1_H_gain111_20211007-235244_-10.0C_0029_c_lps_r.xisf";
const TRUE_RA: f64 = 37.2692;
const TRUE_DEC: f64 = 60.2273;
const TRUE_SCALE: f64 = 0.8788; // arcsec/px (from the stored solve)

// Correct header optics for this rig.
const HEADER_FOCALLEN: f64 = 560.0;
const XPIXSZ_UM: f64 = 2.4;
const BINNING: i32 = 1;
// A deliberately wrong focal length (e.g. a 0.62× reducer not accounted for):
// implies ~0.55"/px — ~60% off the true ~0.88"/px, far outside the ±5%
// candidate scale filter, so a hinted (stage-1) solve must fail.
const WRONG_FOCALLEN: f64 = 900.0;

fn assets_present() -> bool {
    PathBuf::from(SMAC_DIR).exists() && PathBuf::from(TEST_FITS).exists()
}

fn mem_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("memory DB");
    init_db(&conn).expect("init schema");
    conn
}

fn open_cache() -> solvemyastro::StarCache {
    solvemyastro::StarCache::open(PathBuf::from(SMAC_DIR).as_path()).expect("open smac_gaia cache")
}

fn frame_with_focallen(focallen: f64) -> Frame {
    Frame {
        id: Some(1),
        file_id: 1,
        focallen: Some(focallen),
        xpixsz: Some(XPIXSZ_UM),
        ypixsz: Some(XPIXSZ_UM),
        xbinning: Some(BINNING),
        naxis1: Some(5496),
        naxis2: Some(3672),
        // Position hint roughly correct (good mount sync) so the ±10°
        // positional prior is satisfied through stage 2.
        ra: Some(TRUE_RA),
        dec: Some(TRUE_DEC),
        ..Default::default()
    }
}

fn dist_deg(ra: f64, dec: f64) -> f64 {
    ((ra - TRUE_RA).powi(2) + (dec - TRUE_DEC).powi(2)).sqrt()
}

/// Stage 2: a wrong header FOCALLEN makes the hinted solve fail; the
/// scale-cleared retry succeeds and the corrected focal length is returned.
#[test]
#[ignore = "requires smac_gaia star cache and the Heart frame on disk"]
fn fallback_corrects_wrong_focallen() {
    if !assets_present() {
        eprintln!("SKIP: need smac_gaia cache + Heart frame");
        return;
    }
    let conn = mem_conn();
    let cache = open_cache();
    let config = PlateSolveConfig::default(); // fallback on by default

    let frame = frame_with_focallen(WRONG_FOCALLEN);
    let solve = service::solve_frame(&frame, TEST_FITS, &conn, &cache, &config)
        .expect("blind-scale fallback must rescue a wrong-FOCALLEN frame");

    let d = dist_deg(solve.wcs.crval.0, solve.wcs.crval.1);
    eprintln!(
        "SOLVED RA={:.4} Dec={:.4} dist={:.3}° scale={:.4}\"/px corrected={} derived_fl={:?}",
        solve.wcs.crval.0,
        solve.wcs.crval.1,
        d,
        solve.pixel_scale_arcsec,
        solve.focallen_corrected,
        solve.derived_focallen_mm
    );

    assert!(d < 1.0, "position off by {d:.3}°");
    assert!(
        (solve.pixel_scale_arcsec - TRUE_SCALE).abs() / TRUE_SCALE < 0.10,
        "pixel scale {:.4} not within 10% of {TRUE_SCALE}",
        solve.pixel_scale_arcsec
    );
    assert!(
        solve.focallen_corrected,
        "should be flagged as a correction"
    );

    let fl = solve.derived_focallen_mm.expect("corrected focal length");
    // Exact inverse of hints.rs's atan formula, binning-aware.
    let eff_px_mm = (XPIXSZ_UM / 1000.0) * BINNING as f64;
    let expected_fl = eff_px_mm / (solve.pixel_scale_arcsec / 3600.0).to_radians().tan();
    assert!(
        (fl - expected_fl).abs() < 1.0,
        "derived FL {fl:.2} != formula {expected_fl:.2}"
    );
    // The whole point: it must NOT echo the wrong header value.
    assert!(
        (fl - WRONG_FOCALLEN).abs() > 100.0 && (450.0..650.0).contains(&fl),
        "derived FL {fl:.2} should be the true ~560mm, not the wrong {WRONG_FOCALLEN}"
    );
}

/// No-regression: a correct FOCALLEN solves on stage 1 (hinted); no
/// correction, and the existing header value is left untouched.
#[test]
#[ignore = "requires smac_gaia star cache and the Heart frame on disk"]
fn correct_focallen_no_fallback_no_correction() {
    if !assets_present() {
        eprintln!("SKIP: need smac_gaia cache + Heart frame");
        return;
    }
    let conn = mem_conn();
    let cache = open_cache();
    let config = PlateSolveConfig::default();

    let frame = frame_with_focallen(HEADER_FOCALLEN);
    let solve = service::solve_frame(&frame, TEST_FITS, &conn, &cache, &config)
        .expect("correct-FOCALLEN frame must solve on the hinted path");

    let d = dist_deg(solve.wcs.crval.0, solve.wcs.crval.1);
    eprintln!(
        "SOLVED dist={:.3}° corrected={} derived_fl={:?}",
        d, solve.focallen_corrected, solve.derived_focallen_mm
    );
    assert!(d < 1.0, "position off by {d:.3}°");
    assert!(!solve.focallen_corrected, "must not flag a correction");
    assert!(
        solve.derived_focallen_mm.is_none(),
        "header FOCALLEN present and correct → no derived value written"
    );
}

/// Stage 3 (full blind): a wrong FOCALLEN *and* a bogus RA/Dec (mount sync
/// way off) — stage 1 fails (scale filter), stage 2 fails (positional prior
/// rejects the correct candidate against the bad hint), stage 3 drops the
/// prior too and still solves. The corrected focal length is written.
#[test]
#[ignore = "requires smac_gaia star cache and the Heart frame on disk"]
fn full_blind_stage3_wrong_focallen_and_bogus_position() {
    if !assets_present() {
        eprintln!("SKIP: need smac_gaia cache + Heart frame");
        return;
    }
    let conn = mem_conn();
    let cache = open_cache();
    let config = PlateSolveConfig::default();

    let mut frame = frame_with_focallen(WRONG_FOCALLEN);
    // Bogus pointing, ~far from truth and well outside the ±10° prior.
    frame.ra = Some(200.0);
    frame.dec = Some(-30.0);

    let solve = service::solve_frame(&frame, TEST_FITS, &conn, &cache, &config)
        .expect("full-blind stage 3 must rescue wrong FOCALLEN + bad pointing");

    let d = dist_deg(solve.wcs.crval.0, solve.wcs.crval.1);
    eprintln!(
        "SOLVED RA={:.4} Dec={:.4} dist={:.3}° corrected={} derived_fl={:?}",
        solve.wcs.crval.0,
        solve.wcs.crval.1,
        d,
        solve.focallen_corrected,
        solve.derived_focallen_mm
    );
    assert!(d < 1.0, "position off by {d:.3}°");
    assert!(
        solve.focallen_corrected,
        "should be flagged as a correction"
    );
    let fl = solve.derived_focallen_mm.expect("corrected focal length");
    assert!(
        (450.0..650.0).contains(&fl),
        "derived FL {fl:.2} should be the true ~560mm"
    );
}
