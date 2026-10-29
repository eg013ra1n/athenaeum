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
    conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
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

    // After restore, the frame set is back in active/wip view (is_archived=0).
    // To re-archive, the user must first move it back to the Archive section.
    conn.execute("UPDATE frames_set SET is_archived = 1 WHERE id = 1", []).unwrap();

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

fn card(s: &str) -> Vec<u8> {
    let mut c = vec![b' '; 80];
    c[..s.len()].copy_from_slice(s.as_bytes());
    c
}

/// A small valid FITS light frame whose header (and so fingerprint) depends on `seed`.
fn light_fits(seed: u32) -> Vec<u8> {
    let seq = format!("SEQNUM  = {seed:>20}");
    let mut out = Vec::new();
    for c in [
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "OBJECT  = 'M31     '",
        "TELESCOP= 'T       '",
        "INSTRUME= 'C       '",
        "IMAGETYP= 'Light   '",
        seq.as_str(),
        "END",
    ] {
        out.extend(card(c));
    }
    while out.len() % 2880 != 0 {
        out.push(b' ');
    }
    out
}

/// A new, different capture written at the original path of an archived frame is cataloged as a row
/// of its own, the archived row moves to its zip path, and Unarchive to another folder restores the
/// archived frame to that folder without touching the new file's row.
#[test]
fn a_new_capture_at_an_archived_path_coexists_with_the_archived_frame_through_unarchive() {
    use athenaeum_core::scanner::{scan_directory, scan_directory_parallel};

    for parallel in [false, true] {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let original = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        let first = light_fits(1);
        std::fs::write(&original, &first).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
        let run_scan = || {
            if parallel {
                scan_directory_parallel(scan.path(), 1, &conn, &NullEmitter, Arc::new(AtomicBool::new(false)), false)
            } else {
                scan_directory(scan.path(), &conn, None, false, 1)
            }
        };
        let result = run_scan();
        assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);
        let (file_id, frame_id): (i64, i64) = conn
            .query_row("SELECT f.id, fr.id FROM files f JOIN frames fr ON fr.file_id = f.id", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();

        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame_id]).unwrap();

        let dispositions = Dispositions { flats: None, darks: None, bias: None, darkflats: None };
        let plan = build_plan(&conn, 1, arch.path(), &dispositions, ArchiveCompression::Store).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(!original.exists(), "parallel={parallel}: the light moved into the zip");
        let (path, zip_path, entry): (String, String, String) = conn
            .query_row(
                "SELECT path, archive_zip_path, archive_path_in_zip FROM files WHERE id = ?1",
                params![file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, original.to_str().unwrap(), "parallel={parallel}: the archived row keeps its restore target");

        // Capture software writes the next night's frame under the same name.
        let second = light_fits(2);
        std::fs::write(&original, &second).unwrap();
        let result = run_scan();
        assert!(result.errors.is_empty(), "parallel={parallel}: {:?}", result.errors);

        let virtual_path = format!("{zip_path}{}{}", std::path::MAIN_SEPARATOR, entry.replace('/', std::path::MAIN_SEPARATOR_STR));
        let archived_path: String = conn.query_row("SELECT path FROM files WHERE id = ?1", params![file_id], |r| r.get(0)).unwrap();
        assert_eq!(archived_path, virtual_path, "parallel={parallel}: the archived row moved to its zip path");
        let (new_id, new_zip): (i64, Option<String>) = conn
            .query_row("SELECT id, archive_zip_path FROM files WHERE path = ?1", params![original.to_str().unwrap()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_ne!(new_id, file_id);
        assert_eq!(new_zip, None);
        let frames: i64 = conn.query_row("SELECT COUNT(*) FROM frames WHERE file_id = ?1", [new_id], |r| r.get(0)).unwrap();
        assert_eq!(frames, 1, "parallel={parallel}: the new capture is a cataloged frame");

        // Unarchive to another folder.
        let restore_target = TempDir::new().unwrap();
        let outcome = run_restore(&conn, op_id, restore_target.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
        assert!(outcome.conflicts.is_empty(), "parallel={parallel}: {:?}", outcome.conflicts);
        let (restored_path, zip_col): (String, Option<String>) = conn
            .query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", params![file_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!(restored_path.starts_with(restore_target.path().to_str().unwrap()), "parallel={parallel}: {restored_path}");
        assert_eq!(std::fs::read(&restored_path).unwrap(), first, "parallel={parallel}: the archived frame's bytes");
        assert_eq!(zip_col, None, "parallel={parallel}: the row is a disk row again");
        let (still_new_path, still_new_zip): (String, Option<String>) = conn
            .query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", params![new_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((still_new_path.as_str(), still_new_zip), (original.to_str().unwrap(), None));
        assert_eq!(std::fs::read(&original).unwrap(), second);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "parallel={parallel}: both rows coexist with distinct paths");
    }
}

/// The re-pointed archived row (a newer capture took its path) is restored to the original path
/// once the newer file is gone: `files.path` is written back from the zip path.
#[test]
fn unarchive_to_the_original_path_rewrites_a_repointed_row_path() {
    use athenaeum_core::scanner::scan_directory;

    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();
    let original = scan.path().join("M31/L_001.fits");
    std::fs::create_dir_all(original.parent().unwrap()).unwrap();
    let first = light_fits(1);
    std::fs::write(&original, &first).unwrap();

    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
    let result = scan_directory(scan.path(), &conn, None, false, 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let (file_id, frame_id): (i64, i64) = conn
        .query_row("SELECT f.id, fr.id FROM files f JOIN frames fr ON fr.file_id = f.id", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
    conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
    conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame_id]).unwrap();
    let dispositions = Dispositions { flats: None, darks: None, bias: None, darkflats: None };
    let plan = build_plan(&conn, 1, arch.path(), &dispositions, ArchiveCompression::Store).unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();

    std::fs::write(&original, light_fits(2)).unwrap();
    let result = scan_directory(scan.path(), &conn, None, false, 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let repointed: String = conn.query_row("SELECT path FROM files WHERE id = ?1", [file_id], |r| r.get(0)).unwrap();
    assert_ne!(repointed, original.to_str().unwrap(), "the archived row moved off the original path");

    // The newer capture is removed from the catalog and from disk.
    conn.execute("DELETE FROM files WHERE path = ?1", [original.to_str().unwrap()]).unwrap();
    std::fs::remove_file(&original).unwrap();

    let outcome = run_restore(&conn, op_id, scan.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
    assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
    let (path, zip_col): (String, Option<String>) = conn
        .query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", [file_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(path, original.to_str().unwrap(), "files.path points at the restored file again");
    assert_eq!(zip_col, None);
    assert_eq!(std::fs::read(&original).unwrap(), first);
}

#[test]
fn unarchive_after_deleting_two_restored_files_extracts_exactly_those_two() {
    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
    conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
    conn.execute(
        "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
    let mut lights = Vec::new();
    for i in 0..4i64 {
        let p = scan.path().join(format!("M31/L_{i:03}.fits"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("light-bytes-{i}")).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 14, '2025-10-12', 'FITS')",
            params![1000 + i, p.to_str().unwrap(), format!("L_{i:03}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp) VALUES (?1, ?2, 'M31', 'T', 'C', 'Light')",
            params![10000 + i, 1000 + i],
        )
        .unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [10000 + i]).unwrap();
        lights.push(p);
    }

    let plan = build_plan(
        &conn,
        1,
        arch.path(),
        &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
        ArchiveCompression::Store,
    )
    .unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();

    // Unarchive, keeping the zip.
    let first = run_restore(&conn, op_id, scan.path(), false, true, &cancel, &NullEmitter).unwrap();
    assert_eq!(first.extracted, 4);
    assert!(!first.has_conflicts());

    // Two restored files are deleted; Unarchive again extracts exactly those two.
    std::fs::remove_file(&lights[1]).unwrap();
    std::fs::remove_file(&lights[3]).unwrap();
    let second = run_restore(&conn, op_id, scan.path(), false, true, &cancel, &NullEmitter).unwrap();
    assert_eq!(second.extracted, 2);
    assert!(!second.has_conflicts());
    for (i, p) in lights.iter().enumerate() {
        assert_eq!(std::fs::read(p).unwrap(), format!("light-bytes-{i}").as_bytes());
    }
}

/// A small valid FITS calibration frame (`imagetyp`) whose header depends on `seed`.
fn calibration_fits(imagetyp: &str, seed: u32) -> Vec<u8> {
    let seq = format!("SEQNUM  = {seed:>20}");
    let typ = format!("IMAGETYP= '{imagetyp:<8}'");
    let mut out = Vec::new();
    for c in [
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "TELESCOP= 'T       '",
        "INSTRUME= 'C       '",
        typ.as_str(),
        seq.as_str(),
        "END",
    ] {
        out.extend(card(c));
    }
    while out.len() % 2880 != 0 {
        out.push(b' ');
    }
    out
}

/// A Move-and-ZIP zip that holds a moved raw dark and a copied master dark is renamed outside the app and
/// picked up by the next scan: the operation follows the zip (Unarchive still finds it) and the copied
/// master is not cataloged as a foreign row of the renamed zip.
#[test]
fn a_zip_renamed_outside_the_app_stays_restorable_and_its_copied_entries_stay_rowless() {
    use athenaeum_core::scanner::scan_directory;
    use athenaeum_core::zip_store::scan::scan_zip;

    let arch = TempDir::new().unwrap();
    let scan = TempDir::new().unwrap();
    let light = scan.path().join("M31/L_001.fits");
    let light2 = scan.path().join("M31/L_002.fits");
    let raw_dark = scan.path().join("Cal/Dark_001.fits");
    let master = scan.path().join("Cal/MasterDark.fits");
    for p in [&light, &raw_dark, &master] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    }
    std::fs::write(&light, light_fits(1)).unwrap();
    std::fs::write(&light2, light_fits(4)).unwrap();
    let raw_bytes = calibration_fits("Dark", 2);
    std::fs::write(&raw_dark, &raw_bytes).unwrap();
    std::fs::write(&master, calibration_fits("Dark", 3)).unwrap();

    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();
    conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [scan.path().to_str().unwrap()]).unwrap();
    let result = scan_directory(scan.path(), &conn, None, false, 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let ids = |name: &str| -> (i64, i64) {
        conn.query_row(
            "SELECT f.id, fr.id FROM files f JOIN frames fr ON fr.file_id = f.id WHERE f.filename = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    let (_, light_frame) = ids("L_001.fits");
    let (_, light2_frame) = ids("L_002.fits");
    let (raw_file, raw_frame) = ids("Dark_001.fits");
    let (_, master_frame) = ids("MasterDark.fits");
    conn.execute("UPDATE frames SET is_master = 1 WHERE id = ?1", [master_frame]).unwrap();

    conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
    conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
    conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
    for f in [light_frame, light2_frame] {
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [f]).unwrap();
    }
    // The raw dark belongs to the night's own set; the master to a library set (the archive copies those).
    conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
    conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library) VALUES (501, 'Dark', '2025-10-10', 1)", []).unwrap();
    conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, ?1)", [raw_frame]).unwrap();
    conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (501, ?1)", [master_frame]).unwrap();
    for (frame, set) in [(light_frame, 500), (light2_frame, 501)] {
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', ?2, 'Dark', '2025-10-12')",
            params![frame, set],
        )
        .unwrap();
    }

    let dispositions = Dispositions { flats: None, darks: Some(ArchiveDisposition::Move), bias: None, darkflats: None };
    let plan = build_plan(&conn, 1, arch.path(), &dispositions, ArchiveCompression::Store).unwrap();
    let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
    run_operation(&conn, op_id, &Arc::new(AtomicBool::new(false)), &NullEmitter).unwrap();
    assert_eq!(adb::get_operation(&conn, op_id).unwrap().status, "completed");
    assert!(!raw_dark.exists() && master.exists(), "the raw dark moved, the master was copied");
    let rows_before: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(rows_before, 4, "two lights, the raw dark, the master");

    // The user renames the dark zip in Finder; the next scan finds it under its new name.
    let old_zip: String = conn.query_row("SELECT archive_zip_path FROM files WHERE id = ?1", [raw_file], |r| r.get(0)).unwrap();
    let copied_in_old: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_operation_files WHERE target_zip_path = ?1 AND disposition = 'copy'", [&old_zip], |r| r.get(0))
        .unwrap();
    assert_eq!(copied_in_old, 1, "the master dark shares the zip of the moved raw dark");
    let renamed = Path::new(&old_zip).with_file_name("renamed_by_finder.zip");
    std::fs::rename(&old_zip, &renamed).unwrap();
    let outcome = scan_zip(&conn, &renamed, 1, false).unwrap();
    assert_eq!(outcome.adopted, 1, "the raw dark's row follows the zip");
    assert!(outcome.inserted.is_empty(), "the copied master is not a new frame: {:?}", outcome.inserted);
    let rows_after: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(rows_after, 4, "no foreign row for the copied master");
    let stale: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_operation_files WHERE target_zip_path = ?1", [&old_zip], |r| r.get(0))
        .unwrap();
    assert_eq!(stale, 0, "the operation's files follow the zip");

    // A second scan of the (unchanged) zip keeps skipping the operation's copy.
    conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
    let again = scan_zip(&conn, &renamed, 1, false).unwrap();
    assert!(again.inserted.is_empty(), "{:?}", again.inserted);
    assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap(), 4);

    // Unarchive finds the renamed zip.
    let restore_target = TempDir::new().unwrap();
    let restored = run_restore(&conn, op_id, restore_target.path(), false, false, &Arc::new(AtomicBool::new(false)), &NullEmitter)
        .expect("Unarchive must find the renamed zip");
    assert!(restored.conflicts.is_empty(), "{:?}", restored.conflicts);
    let path: String = conn.query_row("SELECT path FROM files WHERE id = ?1", [raw_file], |r| r.get(0)).unwrap();
    assert!(path.starts_with(restore_target.path().to_str().unwrap()), "{path}");
    assert_eq!(std::fs::read(&path).unwrap(), raw_bytes);
}
