//! End-to-end integration test: archive a frame set with a master dark,
//! then restore, then re-archive. Catches integration bugs across modules.

use athenaeum_core::archive::{
    db as adb,
    executor::run_operation,
    models::{ArchiveCompression, ArchiveDisposition, ConflictResolution, Dispositions},
    planner::{build_plan, commit_plan},
    restore::run_restore,
};
use athenaeum_core::db::schema::init_db;
use athenaeum_core::events::NullEmitter;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn archive_then_restore_then_archive_again() {
    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();

    // Filesystem fixture
    let l1 = scan.path().join("M31/L_001.fits");
    let l2 = scan.path().join("M31/L_002.fits");
    let d1 = scan.path().join("Cal/MasterDark.fits");
    for p in [&l1, &l2, &d1] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    }
    std::fs::write(&l1, b"l1-content-1").unwrap();
    std::fs::write(&l2, b"l2-content-2").unwrap();
    std::fs::write(&d1, b"dark-content-x").unwrap();

    // DB fixture
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
        [scan.path().to_str().unwrap()]).unwrap();
    conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
    conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
    for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 12, '2025-10-12', 'FITS')",
            params![file_id, path.to_str().unwrap(), path.file_name().unwrap().to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
            params![frame_id, file_id],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame_id]).unwrap();
    }
    // Dark (master)
    conn.execute(
        "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (2000, ?1, 'MasterDark.fits', 14, '2025-10-10', 'FITS')",
        [d1.to_str().unwrap()],
    ).unwrap();
    conn.execute(
        "INSERT INTO frames (id, file_id, instrume, imagetyp, is_master) VALUES (20000, 2000, 'C', 'Dark', 1)",
        [],
    ).unwrap();
    conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
    conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)", []).unwrap();
    for fid in [10000, 10001] {
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 500, 'Dark', '2025-10-12')",
            [fid],
        ).unwrap();
    }

    // Archive: lights move, dark copy
    let dispositions = Dispositions {
        flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
    };
    let plan = build_plan(&conn, 1, arch.path(), &dispositions, ArchiveCompression::Store).unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();

    // Lights deleted, darks stay (copy mode)
    assert!(!l1.exists() && !l2.exists());
    assert!(d1.exists());
    // Two zips produced
    let zips: Vec<_> = std::fs::read_dir(arch.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
        .collect();
    assert_eq!(zips.len(), 2);

    // Restore
    let restore_target = TempDir::new().unwrap();
    run_restore(
        &conn, op_id, restore_target.path(), true, false,
        &Arc::new(AtomicBool::new(false)), &NullEmitter,
    ).unwrap();

    // files.path rewritten for the lights
    let l1_new: String = conn.query_row(
        "SELECT path FROM files WHERE id = 1000", [], |r| r.get(0),
    ).unwrap();
    assert!(l1_new.starts_with(restore_target.path().to_str().unwrap()));
    assert!(Path::new(&l1_new).exists());

    // Frame set is no longer archived
    let archived_at: Option<String> = conn.query_row(
        "SELECT archived_at FROM frames_set WHERE id = 1", [], |r| r.get(0),
    ).unwrap();
    assert!(archived_at.is_none());

    // Re-archive (should work now that everything is back)
    let plan2 = build_plan(
        &conn, 1, arch.path(),
        &Dispositions { flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None },
        ArchiveCompression::Store,
    ).unwrap();
    let op_id2 = commit_plan(&conn, &plan2, ConflictResolution::AddSuffix).unwrap();
    run_operation(&conn, op_id2, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
    let op = adb::get_operation(&conn, op_id2).unwrap();
    assert_eq!(op.status, "completed");
}
