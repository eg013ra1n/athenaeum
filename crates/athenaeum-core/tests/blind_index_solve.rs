//! Integration test: end-to-end blind solve using real on-disk data via the
//! solvemyastro `StarCache` backend (the only solver). Requires the
//! `smac_gaia` star cache and a known-truth FITS frame on disk; all cases are
//! `#[ignore]` so CI without those assets stays green.
//!
//! Run: cargo test -p athenaeum-core --test blind_index_solve -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::plate_solve::dso_lookup::DsoCatalog;
use athenaeum_core::plate_solve::service;
use athenaeum_core::plate_solve::storage;

const SMAC_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia";

// Known-truth frame: C/2025 A6 Lemmon, frame 93094
const TEST_FITS: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Unsorted/edph/asiduo/C-2025 A6 (Lemmon)/2025-10-25/LIGHT/2025-10-25_19-28-45_-10.00_15.00s_0123.fits";
const TRUE_RA: f64 = 231.954;
const TRUE_DEC: f64 = 18.674;

fn fits_exists() -> bool {
    PathBuf::from(TEST_FITS).exists()
}

fn smac_exists() -> bool {
    PathBuf::from(SMAC_DIR).exists()
}

fn open_cache() -> solvemyastro::StarCache {
    solvemyastro::StarCache::open(PathBuf::from(SMAC_DIR).as_path()).expect("open smac_gaia cache")
}

fn make_test_frame() -> athenaeum_core::models::Frame {
    athenaeum_core::models::Frame {
        id: Some(1),
        file_id: 1,
        focallen: Some(414.0),
        xpixsz: Some(3.76),
        ypixsz: Some(3.76),
        naxis1: Some(6248),
        naxis2: Some(4176),
        ..Default::default()
    }
}

/// Full end-to-end blind solve: open the smac_gaia StarCache, call
/// `service::solve_frame`, verify the result is within 1° of truth, and that
/// DSO labeling + `store_result` persistence still work.
#[test]
#[ignore = "requires smac_gaia star cache and FITS file"]
fn blind_solve_end_to_end() {
    if !fits_exists() || !smac_exists() {
        eprintln!("SKIP: need smac_gaia cache and FITS");
        return;
    }

    // Set up a temporary in-memory DB for the test so `store_result` / `hints`
    // calls don't panic.
    use athenaeum_core::db::schema::init_db;
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("memory DB");
    init_db(&conn).expect("init schema");

    let frame = make_test_frame();
    let cache = open_cache();
    let config = PlateSolveConfig::default();

    let start = Instant::now();
    let result = service::solve_frame(&frame, TEST_FITS, &conn, &cache, &config);

    match result {
        Ok(solve) => {
            let dist = ((solve.wcs.crval.0 - TRUE_RA).powi(2)
                + (solve.wcs.crval.1 - TRUE_DEC).powi(2))
            .sqrt();
            eprintln!(
                "BLIND SOLVE SUCCESS: RA={:.4} Dec={:.4} (truth {:.4}, {:.4}), dist={:.3}°, scale={:.3}\"/px, {}ms",
                solve.wcs.crval.0,
                solve.wcs.crval.1,
                TRUE_RA,
                TRUE_DEC,
                dist,
                solve.pixel_scale_arcsec,
                start.elapsed().as_millis()
            );
            assert!(dist < 1.0, "position off by {:.3}°", dist);
            assert!(
                (solve.pixel_scale_arcsec - 1.87).abs() / 1.87 < 0.10,
                "pixel scale way off: {}",
                solve.pixel_scale_arcsec
            );

            // DSO lookup at the solved position
            let dso_cat = DsoCatalog::load().expect("dso catalog");
            match dso_cat.find_best(solve.wcs.crval.0, solve.wcs.crval.1) {
                Some(m) => eprintln!(
                    "DSO LOOKUP: '{}' ({:?}, {:.3}° away)",
                    m.designation, m.reason, m.distance_deg
                ),
                None => eprintln!("DSO LOOKUP: no nearby named object"),
            }

            // Integration: store_result should set frame.object when DSO is passed
            let frame_id = frame.id.unwrap();
            let _ = conn.execute(
                "INSERT OR REPLACE INTO files (id, path, filename, size, modified_at, format, created_at) VALUES (?1, ?2, ?3, 0, '2025-01-01', 'FITS', '2025-01-01')",
                rusqlite::params![1, TEST_FITS, "test.fits"],
            );
            let _ = conn.execute(
                "INSERT OR REPLACE INTO frames (id, file_id) VALUES (?1, 1)",
                [frame_id],
            );
            service::store_result(&conn, frame_id, &solve, Some(&dso_cat), &config)
                .expect("store_result");
            let object: Option<String> = conn
                .query_row(
                    "SELECT object FROM frames WHERE id = ?1",
                    [frame_id],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            eprintln!("Stored frame.object = {:?}", object);
            let _ = storage::get_plate_solve(&conn, frame_id).unwrap();
        }
        Err(e) => {
            panic!("Blind solve FAILED: {e}");
        }
    }
}

/// End-to-end blind solve on the real Lemmon frame, checked for speed:
///   * position within 1° of truth
///   * pixel scale within 10% of 1.87 "/px
///   * total solve time under 1.5 s (the plan's end-to-end target)
///   * DSO labeling still works when `store_result` gets a DsoCatalog
#[test]
#[ignore = "requires smac_gaia star cache and FITS file"]
fn blind_solve_fast_path() {
    if !fits_exists() || !smac_exists() {
        eprintln!("SKIP: need smac_gaia cache and FITS");
        return;
    }

    use athenaeum_core::db::schema::init_db;
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("memory DB");
    init_db(&conn).expect("init schema");

    let frame = make_test_frame();
    let cache = open_cache();
    let config = PlateSolveConfig::default();

    let start = Instant::now();
    let solve = service::solve_frame(&frame, TEST_FITS, &conn, &cache, &config)
        .expect("blind solve must succeed on the Lemmon frame");
    let wall_ms = start.elapsed().as_millis() as u64;

    let dist =
        ((solve.wcs.crval.0 - TRUE_RA).powi(2) + (solve.wcs.crval.1 - TRUE_DEC).powi(2)).sqrt();
    eprintln!(
        "BLIND SOLVE: RA={:.4} Dec={:.4} (truth {:.4},{:.4}) dist={:.3}° scale={:.3}\"/px solve={}ms wall={}ms matched={}",
        solve.wcs.crval.0,
        solve.wcs.crval.1,
        TRUE_RA,
        TRUE_DEC,
        dist,
        solve.pixel_scale_arcsec,
        solve.solve_time_ms,
        wall_ms,
        solve.matched_stars,
    );

    assert!(dist < 1.0, "position off by {:.3}°", dist);
    assert!(
        (solve.pixel_scale_arcsec - 1.87).abs() / 1.87 < 0.10,
        "pixel scale off: {}",
        solve.pixel_scale_arcsec
    );
    assert!(
        solve.matched_stars >= 15,
        "too few matched stars: {}",
        solve.matched_stars
    );
    assert!(
        solve.solve_time_ms < 1500,
        "solve too slow: {}ms (target < 1500ms)",
        solve.solve_time_ms
    );

    // DSO labeling path still runs.
    let dso_cat = DsoCatalog::load().expect("dso catalog");
    let frame_id = frame.id.unwrap();
    let _ = conn.execute(
        "INSERT OR REPLACE INTO files (id, path, filename, size, modified_at, format, created_at) VALUES (?1, ?2, ?3, 0, '2025-01-01', 'FITS', '2025-01-01')",
        rusqlite::params![1, TEST_FITS, "test.fits"],
    );
    let _ = conn.execute(
        "INSERT OR REPLACE INTO frames (id, file_id) VALUES (?1, 1)",
        [frame_id],
    );
    service::store_result(&conn, frame_id, &solve, Some(&dso_cat), &config)
        .expect("store_result with DSO");
    let _ = storage::get_plate_solve(&conn, frame_id).expect("plate solve row written");
}

/// Two independent solves of the same frame must converge to the same sky
/// position and both stay fast (determinism + perf smoke test).
#[test]
#[ignore = "requires smac_gaia star cache and FITS file — slow (~10s total)"]
fn blind_solve_is_deterministic() {
    if !fits_exists() || !smac_exists() {
        eprintln!("SKIP: need smac_gaia cache and FITS");
        return;
    }

    use athenaeum_core::db::schema::init_db;
    use rusqlite::Connection;

    let cache = open_cache();
    let frame = make_test_frame();
    let config = PlateSolveConfig::default();

    let conn_a = Connection::open_in_memory().expect("db a");
    init_db(&conn_a).expect("schema a");
    let solve_a =
        service::solve_frame(&frame, TEST_FITS, &conn_a, &cache, &config).expect("solve a");

    let conn_b = Connection::open_in_memory().expect("db b");
    init_db(&conn_b).expect("schema b");
    let solve_b =
        service::solve_frame(&frame, TEST_FITS, &conn_b, &cache, &config).expect("solve b");

    let dra = solve_a.wcs.crval.0 - solve_b.wcs.crval.0;
    let ddec = solve_a.wcs.crval.1 - solve_b.wcs.crval.1;
    let cos_dec = (solve_b.wcs.crval.1.to_radians()).cos();
    let sep_arcsec = (((dra * cos_dec).powi(2) + ddec.powi(2)).sqrt()) * 3600.0;

    eprintln!(
        "DETERMINISM: a={}ms ({}★)  b={}ms ({}★)  separation={:.1}\"",
        solve_a.solve_time_ms,
        solve_a.matched_stars,
        solve_b.solve_time_ms,
        solve_b.matched_stars,
        sep_arcsec,
    );

    assert!(
        sep_arcsec < 60.0,
        "two solves disagree by {:.1}\" (target < 60\")",
        sep_arcsec
    );
    assert!(
        solve_a.solve_time_ms < 1500 && solve_b.solve_time_ms < 1500,
        "solve too slow: a={}ms b={}ms (target < 1500ms)",
        solve_a.solve_time_ms,
        solve_b.solve_time_ms
    );
    assert!(
        solve_a.matched_stars >= 15 && solve_b.matched_stars >= 15,
        "too few matched stars: a={} b={}",
        solve_a.matched_stars,
        solve_b.matched_stars
    );
}
