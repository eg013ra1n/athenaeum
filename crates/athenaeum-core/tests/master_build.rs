//! Integration test for the master-build orchestration (Task 12).
//!
//! Seeds a real 3-frame source Dark calibration set (real 8x8 FITS files on
//! disk), then runs the SAME steps `api::masters::run_build` performs inside
//! its dedicated thread — resolve combine -> integrate -> load header inputs
//! -> write -> register — synchronously, against a temp "library" directory.
//! This deliberately does not exercise the threaded path itself (queue
//! admission, cancel-while-queued, cancel-mid-integration): that plumbing is
//! already pinned by `services::compute_queue`'s own tests and by
//! `integration::engine`'s `cancel_mid_run_returns_cancelled` test. What this
//! test pins is that the pieces compose end-to-end into a master file whose
//! header matches what was actually built (source uuid, frame count,
//! IMAGETYP).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use athenaeum_core::api::masters::resolve_combine;
use athenaeum_core::calibration_library::headers::{build_master_cards, load_header_inputs};
use athenaeum_core::calibration_library::paths::{master_relative_path, resolve_collision, MasterPathParams};
use athenaeum_core::calibration_library::register::{member_hash, register_master};
use athenaeum_core::fits_parser::FitsHeader;
use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::integration::engine::{integrate_bias_like, EngineProgress};
use rusqlite::Connection;

/// Seeds a 3-frame raw Dark calibration set with real 8x8 FITS files on
/// disk and their DB rows (files/frames/calibration_set_frames). Mirrors
/// `calibration_library::register::tests::seed_source_set`, which is
/// module-private to that file's `#[cfg(test)]` block — this is a minimal
/// standalone replica for this external integration-test crate.
fn seed_source_set(conn: &Connection, dir: &std::path::Path) -> i64 {
    athenaeum_core::db::schema::init_db(conn).unwrap();
    conn.execute(
        "INSERT INTO calibration_set
         (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
          date_start, date_end, temp_min, temp_max, frame_count)
         VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', '2026-06-28',
          '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z', -10.5, -9.5, 3)",
        [],
    ).unwrap();
    let set_id = conn.last_insert_rowid();
    for i in 0..3 {
        let p = dir.join(format!("raw{i}.fits"));
        let cards = HeaderBuilder::new(FrameKind::Dark)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .build().unwrap();
        write_fits_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards).unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 100, '2026-06-28', 'FITS')",
            rusqlite::params![p.to_string_lossy(), format!("raw{i}.fits")],
        ).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset, binning, ccd_temp, date_obs)
             VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', -10.0, '2026-06-28T21:00:00Z')",
            rusqlite::params![file_id],
        ).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        ).unwrap();
    }
    set_id
}

#[test]
fn synchronous_build_produces_registered_master_with_correct_header() {
    let dir = tempfile::tempdir().unwrap();
    let library_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let conn = Connection::open_in_memory().unwrap();

    let set_id = seed_source_set(&conn, dir.path());
    let source_uuid: String = conn
        .query_row("SELECT uuid FROM calibration_set WHERE id = ?1", [set_id], |r| r.get(0))
        .unwrap();

    // Load member paths — same query `api::masters::run_build` uses.
    let mut stmt = conn.prepare(
        "SELECT fi.path FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE csf.set_id = ?1 ORDER BY fi.path",
    ).unwrap();
    let paths: Vec<PathBuf> = stmt
        .query_map([set_id], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    drop(stmt);
    assert_eq!(paths.len(), 3);

    // Resolve -> integrate (n=3 < 15 => plain Median for a non-flat type).
    let combine = resolve_combine(None, "Dark", 3);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let on_band = |_current: usize, _total: usize| {};
    let out = integrate_bias_like(
        &paths, combine, &pool, scratch.path(), &AtomicBool::new(false),
        EngineProgress { on_band: &on_band },
    ).unwrap();

    // Write: consolidated header + fixed v1 naming into the temp library dir.
    let inputs = load_header_inputs(&conn, set_id).unwrap();
    let (hash, uuids) = member_hash(&conn, set_id).unwrap();
    assert_eq!(uuids.len(), 3);
    let cards = build_master_cards(&inputs, "0.2.5-test", "median n=3", &hash, out.flat_norm).unwrap();

    let target_rel = master_relative_path(&MasterPathParams {
        instrume: inputs.instrume.as_deref(),
        master_kind: inputs.kind,
        filter: inputs.filter.as_deref(),
        exptime: inputs.exptime,
        ccd_temp: inputs.temp_mean,
        gain: inputs.gain,
        binning: Some("1x1"),
        date: "2026-06-28",
    });
    let target_abs = resolve_collision(&library_dir.path().join(&target_rel));
    std::fs::create_dir_all(target_abs.parent().unwrap()).unwrap();
    write_fits_f32(&target_abs, out.width, out.height, 1, &out.data, &cards).unwrap();

    // Register: same DB rows a scanner-ingested master would get, plus provenance.
    let reg = register_master(&conn, set_id, &target_abs, r#"{"combine":"median"}"#).unwrap();
    assert!(reg.master_set_id > 0);

    // The load-bearing assertions: parse the just-written master back and
    // confirm its header actually reflects what was built.
    let header = FitsHeader::from_path(&target_abs).unwrap();
    assert_eq!(header.get_str("ATH_SRC").as_deref(), Some(source_uuid.as_str()));
    assert_eq!(header.get_i32("ATH_N"), Some(3));
    assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("Master Dark"));
}
