//! Integration: the stricter blind gate must NOT reject a true full-blind
//! solve. Ported from the legacy quad-index harness to the Phase-3
//! solvemyastro StarCache backend.
use std::path::PathBuf;
use athenaeum_core::db::schema::init_db;
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::plate_solve::service;
use rusqlite::Connection;

const SMAC_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia";
const FITS: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Astro Pano/Heart/Pane 1/registered/Light_BIN-1_5496x3672_EXPOSURE-300.00s_FILTER-H_Mono/Light_Pane 1_300.0s_Bin1_H_gain111_20211007-235244_-10.0C_0029_c_lps_r.xisf";
const TRUE_RA: f64 = 37.2692;
const TRUE_DEC: f64 = 60.2273;

#[test]
#[ignore = "requires smac_gaia star cache and the Heart frame on disk"]
fn blind_gate_keeps_true_full_blind_solve() {
    if !(PathBuf::from(SMAC_DIR).exists() && PathBuf::from(FITS).exists()) {
        eprintln!("SKIP: need smac_gaia cache + Heart frame");
        return;
    }
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    let cache = solvemyastro::StarCache::open(PathBuf::from(SMAC_DIR).as_path())
        .expect("open smac_gaia cache");
    let cfg = PlateSolveConfig::default(); // blind gate ON by default
    // Wrong FOCALLEN + bogus pointing forces stage 3 (full blind).
    let frame = Frame {
        id: Some(1), file_id: 1, focallen: Some(900.0), xpixsz: Some(2.4),
        ypixsz: Some(2.4), xbinning: Some(1), naxis1: Some(5496),
        naxis2: Some(3672), ra: Some(200.0), dec: Some(-30.0),
        ..Default::default()
    };
    let s = service::solve_frame(&frame, FITS, &conn, &cache, &cfg)
        .expect("a TRUE full-blind solve must still pass the stricter gate");
    let d = ((s.wcs.crval.0 - TRUE_RA).powi(2)
        + (s.wcs.crval.1 - TRUE_DEC).powi(2)).sqrt();
    assert!(d < 1.0, "off by {d:.3} deg");
}
