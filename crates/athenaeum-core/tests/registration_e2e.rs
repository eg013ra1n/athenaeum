//! End-to-end integration test for `athenaeum_core::registration`.
//!
//! Pipeline exercised: detect stars → precise-solve reference → align members → persist.
//!
//! Because this test requires real on-disk data (a FITS frame + the Gaia star
//! cache), it is gated `#[ignore]`. When the required paths are absent it
//! prints a SKIP line and returns cleanly — it never panics on missing data.
//!
//! Run:
//!   cargo test -p athenaeum-core --test registration_e2e -- --ignored --nocapture

use std::path::PathBuf;

use athenaeum_core::db::schema::init_db;
use athenaeum_core::events::NullEmitter;
use athenaeum_core::fits_parser::parse_fits_with_header;
use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::registration;
use athenaeum_core::registration::db::{get_light_frame_ids_for_frame_set, get_registration_for_frame_set};
use rusqlite::Connection;
use solvemyastro::StarCache;

// ── paths ─────────────────────────────────────────────────────────────────────

const FITS_PATH: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Astro/Platesolve Bench/2024-01-13_20-27-32_L_-10.00_120.00s_0081.fits";
const SMAC_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia";
const BRIGHT_DIR: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/smac_gaia_bright";

// Published reference truth from corpus_bench.rs for _0081 (M78 field).
// RA=86.682°, Dec=0.042°, scale≈0.96 arcsec/px.
const TRUTH_RA: f64 = 86.682;
const TRUTH_DEC: f64 = 0.042;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Insert one `files` row. Returns the `files.id` (last_insert_rowid).
fn insert_file(conn: &Connection, path: &str) -> i64 {
    let filename = PathBuf::from(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown.fits".to_string());
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format, created_at) \
         VALUES (?1, ?2, 0, '2024-01-13 20:27:32', 'FITS', '2024-01-13 20:27:32')",
        rusqlite::params![path, filename],
    )
    .expect("insert files row");
    conn.last_insert_rowid()
}

/// Insert one `frames` row populated from the parsed `Frame`.
///
/// Returns the `frames.id` (last_insert_rowid).
fn insert_frame(conn: &Connection, file_id: i64, frame: &athenaeum_core::models::Frame) -> i64 {
    let date_obs_str = frame.date_obs.map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.f").to_string());
    conn.execute(
        "INSERT INTO frames (
             file_id, object, date_obs, telescop, instrume,
             exptime, filter, imagetyp,
             gain, offset, binning, xbinning, ybinning,
             ccd_temp, set_temp, focallen, xpixsz, ypixsz,
             naxis1, naxis2, ra, dec,
             sitelat, lat_obs, sitelong, long_obs,
             objctra, objctdec, swcreate, bayerpat, rotation,
             override, is_master
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5,
             ?6, ?7, ?8,
             ?9, ?10, ?11, ?12, ?13,
             ?14, ?15, ?16, ?17, ?18,
             ?19, ?20, ?21, ?22,
             ?23, ?24, ?25, ?26,
             ?27, ?28, ?29, ?30, ?31,
             0, 0
         )",
        rusqlite::params![
            file_id,
            frame.object,
            date_obs_str,
            frame.telescop,
            frame.instrume,
            frame.exptime,
            frame.filter,
            // Force 'Light' so get_light_frame_ids_for_frame_set returns it.
            "Light",
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
            frame.swcreate,
            frame.bayerpat,
            frame.rotation,
        ],
    )
    .expect("insert frames row");
    conn.last_insert_rowid()
}

/// Build the join chain frames_set → imaging_nights → sessions → session_members
/// for all three frame IDs under one session (one instrument, one night, one
/// frame set). Returns the `frames_set.id`.
fn build_join_chain(conn: &Connection, frame_ids: &[i64], instrume: &str) -> i64 {
    // frames_set
    conn.execute(
        "INSERT INTO frames_set (name, is_custom) VALUES ('M78 E2E test', 0)",
        [],
    )
    .expect("insert frames_set");
    let fs_id = conn.last_insert_rowid();

    // One imaging_night for the whole set.
    conn.execute(
        "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
         VALUES (?1, '2024-01-13 20:00:00', '2024-01-13 23:59:59')",
        [fs_id],
    )
    .expect("insert imaging_nights");
    let night_id = conn.last_insert_rowid();

    // One session.
    conn.execute(
        "INSERT INTO sessions (imaging_night_id, instrume, frame_count) \
         VALUES (?1, ?2, ?3)",
        rusqlite::params![night_id, instrume, frame_ids.len() as i64],
    )
    .expect("insert sessions");
    let session_id = conn.last_insert_rowid();

    // Link each frame.
    for &fid in frame_ids {
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, fid],
        )
        .expect("insert session_members");
    }

    fs_id
}

// ── the test ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires local FITS frame + smac_gaia star cache"]
fn registration_e2e_self_pair() {
    // ── skip guard ────────────────────────────────────────────────────────────
    let fits_path = PathBuf::from(FITS_PATH);
    let smac_path = PathBuf::from(SMAC_DIR);

    if !fits_path.exists() {
        eprintln!("SKIP registration_e2e: FITS frame not found at {FITS_PATH}");
        return;
    }
    if !smac_path.exists() {
        eprintln!("SKIP registration_e2e: smac_gaia catalog not found at {SMAC_DIR}");
        return;
    }

    // ── open star caches ──────────────────────────────────────────────────────
    let cache = StarCache::open(&smac_path).expect("open smac_gaia cache");
    let bright_path = PathBuf::from(BRIGHT_DIR);
    let bright_cache: Option<StarCache> = if bright_path.is_dir() {
        match StarCache::open(&bright_path) {
            Ok(c) => {
                eprintln!("registration_e2e: bright sub-cache loaded");
                Some(c)
            }
            Err(e) => {
                eprintln!("registration_e2e: bright cache open failed ({e}), using deep-only");
                None
            }
        }
    } else {
        eprintln!("registration_e2e: bright cache dir absent, using deep-only");
        None
    };

    // ── parse real FITS header ────────────────────────────────────────────────
    let (parsed_frame, _header_text) =
        parse_fits_with_header(&fits_path, /*file_id placeholder*/ 1)
            .expect("parse_fits_with_header must succeed on the bench FITS");

    eprintln!(
        "registration_e2e: parsed frame — \
         ra={:?}° dec={:?}° focallen={:?}mm xpixsz={:?}μm naxis1={:?} naxis2={:?} \
         instrume={:?} date_obs={:?}",
        parsed_frame.ra,
        parsed_frame.dec,
        parsed_frame.focallen,
        parsed_frame.xpixsz,
        parsed_frame.naxis1,
        parsed_frame.naxis2,
        parsed_frame.instrume,
        parsed_frame.date_obs,
    );

    // Sanity: parser must have produced coordinate hints for a fast solve.
    assert!(
        parsed_frame.focallen.is_some(),
        "FITS header must supply FOCALLEN for a fast hint-driven solve"
    );
    assert!(
        parsed_frame.ra.is_some() || parsed_frame.objctra.is_some(),
        "FITS header must supply some RA hint"
    );

    // ── in-memory DB ──────────────────────────────────────────────────────────
    let conn = Connection::open_in_memory().expect("open in-memory DB");
    init_db(&conn).expect("init_db");

    // ── insert one file + three identical frames (self-pair test) ─────────────
    let file_id = insert_file(&conn, FITS_PATH);
    let instrume = parsed_frame
        .instrume
        .clone()
        .unwrap_or_else(|| "TestCamera".to_string());

    let fid1 = insert_frame(&conn, file_id, &parsed_frame);
    let fid2 = insert_frame(&conn, file_id, &parsed_frame);
    let fid3 = insert_frame(&conn, file_id, &parsed_frame);
    let frame_ids = [fid1, fid2, fid3];
    eprintln!("registration_e2e: frame IDs inserted: {fid1}, {fid2}, {fid3}");

    // ── build join chain ──────────────────────────────────────────────────────
    let fs_id = build_join_chain(&conn, &frame_ids, &instrume);
    eprintln!("registration_e2e: frames_set.id = {fs_id}");

    // Confirm the DB join resolves all 3 LIGHT frames.
    let light_ids = get_light_frame_ids_for_frame_set(&conn, fs_id)
        .expect("get_light_frame_ids_for_frame_set");
    assert_eq!(
        light_ids.len(),
        3,
        "join chain must resolve exactly 3 LIGHT frames, got: {light_ids:?}"
    );
    eprintln!("registration_e2e: confirmed 3 LIGHT frames via DB join");

    // ── run the registration service ──────────────────────────────────────────
    let ps_config = PlateSolveConfig::default();
    let emitter = NullEmitter;

    eprintln!("registration_e2e: calling register_frame_set …");
    let summary = registration::register_frame_set(
        &conn,
        fs_id,
        None, // auto-select reference
        &cache,
        bright_cache.as_ref(),
        &ps_config,
        &emitter,
        None, // no cancel signal
    )
    .expect("register_frame_set must succeed on the M78 bench frame");

    eprintln!(
        "registration_e2e: summary — total={} aligned={} failed={} ref_frame_id={}",
        summary.total, summary.aligned, summary.failed, summary.reference_frame_id
    );

    // ── high-level summary assertions ─────────────────────────────────────────
    assert_eq!(summary.total, 3, "total must be 3 (self-pair of 3 identical frames)");
    assert_eq!(summary.aligned, 2, "2 non-reference frames must align");
    assert_eq!(summary.failed, 0, "no frame must fail (same image, identity transform)");
    assert!(
        frame_ids.contains(&summary.reference_frame_id),
        "reference_frame_id {} must be one of the inserted frames {:?}",
        summary.reference_frame_id,
        frame_ids
    );

    // ── load and inspect persisted rows ───────────────────────────────────────
    let rows = get_registration_for_frame_set(&conn, fs_id)
        .expect("get_registration_for_frame_set");

    assert_eq!(rows.len(), 3, "must persist exactly 3 registration_results rows");

    let ref_rows: Vec<_> = rows.iter().filter(|r| r.is_reference).collect();
    let aligned_rows: Vec<_> = rows.iter().filter(|r| !r.is_reference).collect();
    assert_eq!(ref_rows.len(), 1, "exactly one row must be the reference");
    assert_eq!(aligned_rows.len(), 2, "exactly two rows must be aligned");

    // ── reference row assertions ──────────────────────────────────────────────
    let ref_row = ref_rows[0];
    assert_eq!(ref_row.status, "reference");
    assert_eq!(
        ref_row.frame_id, summary.reference_frame_id,
        "reference row's frame_id must match the summary"
    );

    // crval1/crval2 must be finite and near the known sky position.
    let crval1 = ref_row
        .crval1
        .expect("reference row must have crval1 (solved RA)");
    let crval2 = ref_row
        .crval2
        .expect("reference row must have crval2 (solved Dec)");

    eprintln!(
        "registration_e2e: reference WCS — crval1={crval1:.4}° crval2={crval2:.4}° \
         (truth RA={TRUTH_RA}° Dec={TRUTH_DEC}°)"
    );

    let sep_deg = ((crval1 - TRUTH_RA).powi(2) + (crval2 - TRUTH_DEC).powi(2)).sqrt();
    assert!(
        sep_deg < 1.0,
        "reference WCS sky position must be within 1° of truth M78 coordinates \
         (RA={TRUTH_RA}°, Dec={TRUTH_DEC}°); got RA={crval1:.4}°, Dec={crval2:.4}°, sep={sep_deg:.3}°"
    );

    eprintln!(
        "registration_e2e: reference sky position — {crval1:.4}° / {crval2:.4}° (sep from truth: {sep_deg:.3}°) PASS"
    );

    // Reference affine must be identity (service always stores it).
    let a1 = ref_row.affine_a1.expect("reference row must have affine_a1");
    let b2 = ref_row.affine_b2.expect("reference row must have affine_b2");
    let b1 = ref_row.affine_b1.expect("reference row must have affine_b1");
    let a2 = ref_row.affine_a2.expect("reference row must have affine_a2");
    let c1 = ref_row.affine_c1.expect("reference row must have affine_c1");
    let c2 = ref_row.affine_c2.expect("reference row must have affine_c2");
    assert!(
        (a1 - 1.0).abs() < 1e-9,
        "reference affine_a1 must be 1.0 (identity), got {a1}"
    );
    assert!(
        (b2 - 1.0).abs() < 1e-9,
        "reference affine_b2 must be 1.0 (identity), got {b2}"
    );
    assert!(
        b1.abs() < 1e-9 && a2.abs() < 1e-9 && c1.abs() < 1e-9 && c2.abs() < 1e-9,
        "reference off-diagonal/translation affine terms must be 0; \
         b1={b1} a2={a2} c1={c1} c2={c2}"
    );

    // ── aligned row assertions ────────────────────────────────────────────────
    for row in &aligned_rows {
        assert_eq!(row.status, "aligned", "non-reference row must have status='aligned'");
        assert_eq!(
            row.reference_frame_id, summary.reference_frame_id,
            "aligned row must reference the chosen reference frame"
        );

        let matched = row.matched_stars;
        let rms_px = row.rms_residual_px;

        eprintln!(
            "registration_e2e: aligned frame {} — matched_stars={matched} rms={rms_px:.4}px \
             affine=[a1={:.4} b1={:.4} c1={:.4} / a2={:.4} b2={:.4} c2={:.4}]",
            row.frame_id,
            row.affine_a1.unwrap_or(f64::NAN),
            row.affine_b1.unwrap_or(f64::NAN),
            row.affine_c1.unwrap_or(f64::NAN),
            row.affine_a2.unwrap_or(f64::NAN),
            row.affine_b2.unwrap_or(f64::NAN),
            row.affine_c2.unwrap_or(f64::NAN),
        );

        // Self-pair: same image → at least ~20 matched stars expected.
        assert!(
            matched >= 20,
            "self-pair must match at least 20 stars, got {matched} for frame {}",
            row.frame_id
        );

        // Self-pair: residual must be sub-pixel (identity transform → near-zero RMS).
        assert!(
            rms_px < 0.5,
            "self-pair RMS residual must be < 0.5 px (identity), got {rms_px:.4} for frame {}",
            row.frame_id
        );

        // Affine must be ~identity for a self-pair.
        let aa1 = row.affine_a1.expect("aligned row must have affine_a1");
        let ab1 = row.affine_b1.expect("aligned row must have affine_b1");
        let ac1 = row.affine_c1.expect("aligned row must have affine_c1");
        let aa2 = row.affine_a2.expect("aligned row must have affine_a2");
        let ab2 = row.affine_b2.expect("aligned row must have affine_b2");
        let ac2 = row.affine_c2.expect("aligned row must have affine_c2");

        assert!(
            (aa1 - 1.0).abs() < 1e-2,
            "aligned affine_a1 must be ≈1 (identity diagonal), got {aa1:.6} for frame {}",
            row.frame_id
        );
        assert!(
            (ab2 - 1.0).abs() < 1e-2,
            "aligned affine_b2 must be ≈1 (identity diagonal), got {ab2:.6} for frame {}",
            row.frame_id
        );
        assert!(
            ab1.abs() < 1e-2,
            "aligned affine_b1 must be ≈0 (identity off-diagonal), got {ab1:.6} for frame {}",
            row.frame_id
        );
        assert!(
            aa2.abs() < 1e-2,
            "aligned affine_a2 must be ≈0 (identity off-diagonal), got {aa2:.6} for frame {}",
            row.frame_id
        );
        // Translation within ~1 pixel for a numerically identical image.
        assert!(
            ac1.abs() < 1.5,
            "aligned affine_c1 (x translation) must be < 1.5 px, got {ac1:.4} for frame {}",
            row.frame_id
        );
        assert!(
            ac2.abs() < 1.5,
            "aligned affine_c2 (y translation) must be < 1.5 px, got {ac2:.4} for frame {}",
            row.frame_id
        );
    }

    eprintln!("registration_e2e: ALL ASSERTIONS PASSED");
}
