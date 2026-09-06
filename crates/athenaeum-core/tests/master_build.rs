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

use athenaeum_core::api::masters::resolve_recipe;
use athenaeum_core::integration::band_budget::MIN_BUDGET_BYTES;
use athenaeum_core::integration::combine::{IntegrationRecipe, Rejection};
use athenaeum_core::calibration_library::headers::{build_master_cards, load_header_inputs};
use athenaeum_core::calibration_library::paths::{master_relative_path, resolve_collision, MasterPathParams};
use athenaeum_core::calibration_library::register::{member_hash, register_master};
use athenaeum_core::fits_parser::FitsHeader;
use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::integration::engine::{integrate_bias_like, EngineProgress};
use athenaeum_core::integration::io_policy::IoPolicy;
use athenaeum_core::integration::storage_class::StorageClass;
use rusqlite::Connection;

/// This test bypasses `api::masters::run_build`'s own I/O-policy resolution
/// (it exercises the orchestration steps directly, not the build thread), so
/// it forces the floor budget with a fixed policy rather than a real
/// machine/storage-resolved one — concurrency and storage class are not
/// under test here.
fn fixed_io_policy() -> IoPolicy {
    IoPolicy { band_budget_bytes: MIN_BUDGET_BYTES, read_concurrency: 1, storage: StorageClass::Local }
}

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
    let combine = resolve_recipe(None, "Dark", 3);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let on_band = |_current: usize, _total: usize, _bytes_read_so_far: u64, _bytes_total: u64| {};
    let out = integrate_bias_like(
        &paths, combine, &pool, scratch.path(), &AtomicBool::new(false),
        EngineProgress { on_band: &on_band },
        fixed_io_policy(),
    ).unwrap();

    // Write: consolidated header + fixed v1 naming into the temp library dir.
    let inputs = load_header_inputs(&conn, set_id).unwrap();
    let (hash, uuids) = member_hash(&conn, set_id).unwrap();
    assert_eq!(uuids.len(), 3);
    let cards = build_master_cards(&inputs, "0.2.5-test", "median n=3", &hash, out.flat_norm, None).unwrap();

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

/// Integration test for Task 13's rebuild-in-place path.
///
/// Runs the SAME sequence `api::masters::run_build`'s `BuildTarget::Rebuild`
/// arm performs (re-integrate the source set's member frames -> write to
/// the master's EXISTING path -> `master_provenance::update_rebuild` +
/// `scanner::resync_catalog_rows_from_disk`) — minus the
/// `ServiceContext`-only plumbing (queue admission, thread spawn,
/// progress/completion events), which is exactly what
/// `synchronous_build_produces_registered_master_with_correct_header`
/// above already does for the New path. What this test pins, per Task 13's
/// self-review requirement: a rebuild (a) actually re-reads changed source
/// pixels rather than being a no-op, (b) leaves the master's identity — its
/// `frames.id`/`imagetyp`/`is_master`, its `calibration_set` row, and every
/// existing consumer link in `calibration_set_to_frames` — untouched, and (c)
/// refreshes `master_provenance` (recipe/hash) and the master's `files` row
/// (size/modified_at) while preserving `source_set_id`. The header-derived
/// frames columns and the stored header blob DO get refreshed from the
/// rewritten file — that part is pinned by
/// `api::masters::tests::rebuild_finalize_syncs_frames_and_stored_header_with_the_rewritten_file`.
#[test]
fn rebuild_in_place_updates_pixels_and_provenance_leaves_links_and_identity_intact() {
    let dir = tempfile::tempdir().unwrap();
    let library_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let conn = Connection::open_in_memory().unwrap();

    let set_id = seed_source_set(&conn, dir.path());

    // A light frame linked to the raw set — the relink subject
    // (`register_master` repoints this at the master; the rebuild below
    // must NOT touch it again).
    conn.execute(
        "INSERT INTO files (path, filename, size, modified_at, format)
         VALUES ('/l/light.fits', 'light.fits', 100, '2026-06-28', 'FITS')",
        [],
    ).unwrap();
    let light_file_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO frames (file_id, imagetyp, instrume, exptime) VALUES (?1, 'Light', 'TestCam', 300.0)",
        [light_file_id],
    ).unwrap();
    let light_frame_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
         VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
        rusqlite::params![light_frame_id, set_id],
    ).unwrap();

    let member_paths = |conn: &Connection| -> Vec<PathBuf> {
        let mut stmt = conn.prepare(
            "SELECT fi.path FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1 ORDER BY fi.path",
        ).unwrap();
        stmt.query_map([set_id], |r| r.get::<_, String>(0)).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap()
            .into_iter().map(PathBuf::from).collect()
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    // Mean (not Median): the mutated raw frame below changes only ONE of
    // the 3 member frames, and a median of {100, 100, 400} is still 100 —
    // mean is what actually shifts, which is what "the rebuild re-read the
    // changed source" needs to observe.
    let integrate = |paths: &[PathBuf]| {
        let on_band = |_c: usize, _t: usize, _bytes_read_so_far: u64, _bytes_total: u64| {};
        integrate_bias_like(
            paths,
            IntegrationRecipe::average(Rejection::None),
            &pool, scratch.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            fixed_io_policy(),
        ).unwrap()
    };

    // ── Build #1: same sequence as the New-path test above. ──
    let paths = member_paths(&conn);
    let out1 = integrate(&paths);
    let inputs = load_header_inputs(&conn, set_id).unwrap();
    let (hash1, _uuids) = member_hash(&conn, set_id).unwrap();
    let cards1 = build_master_cards(&inputs, "0.2.5-test", "mean n=3", &hash1, out1.flat_norm, None).unwrap();
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
    write_fits_f32(&target_abs, out1.width, out1.height, 1, &out1.data, &cards1).unwrap();
    let reg = register_master(&conn, set_id, &target_abs, r#"{"combine":"median"}"#).unwrap();

    // Capture "before" state for everything the rebuild must NOT touch.
    let link_set_before: i64 = conn.query_row(
        "SELECT calibration_set_id FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
        [light_frame_id], |r| r.get(0),
    ).unwrap();
    assert_eq!(link_set_before, reg.master_set_id, "sanity: registration relinked the light");
    let frame_row_before: (String, i64) = conn.query_row(
        "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
        [reg.master_frame_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    let set_row_before: (i64, i64) = conn.query_row(
        "SELECT frame_count, is_master_library FROM calibration_set WHERE id = ?1",
        [reg.master_set_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();

    // Mutate ONE raw member frame's pixel data — the seed frames are all a
    // flat 100.0, so re-integrating the unmodified files would produce a
    // byte-identical result and "the rebuild actually re-read the source"
    // wouldn't be pinned by anything below.
    let cards_raw = HeaderBuilder::new(FrameKind::Dark)
        .instrume("TestCam").exptime(300.0).gain(100).offset(50)
        .binning(1, 1).ccd_temp(-10.0)
        .build().unwrap();
    write_fits_f32(&dir.path().join("raw0.fits"), 8, 8, 1, &vec![400.0; 64], &cards_raw).unwrap();

    // ── Rebuild: same source set, SAME target path, no register_master. ──
    let paths2 = member_paths(&conn);
    let out2 = integrate(&paths2);
    assert_ne!(out1.data, out2.data, "rebuild must re-read the now-changed source frame");

    let (hash2, _uuids2) = member_hash(&conn, set_id).unwrap();
    let cards2 = build_master_cards(&inputs, "0.2.5-test", "mean n=3", &hash2, out2.flat_norm, None).unwrap();
    write_fits_f32(&target_abs, out2.width, out2.height, 1, &out2.data, &cards2).unwrap();

    let recipe_json_2 = r#"{"combine":"median","rebuilt":true}"#;
    athenaeum_core::db::master_provenance::update_rebuild(&conn, reg.master_set_id, recipe_json_2, &hash2).unwrap();
    athenaeum_core::scanner::resync_catalog_rows_from_disk(&conn, reg.master_file_id, &target_abs)
        .unwrap();

    // ── Links / frames identity / calibration_set: untouched. ──
    let link_set_after: i64 = conn.query_row(
        "SELECT calibration_set_id FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
        [light_frame_id], |r| r.get(0),
    ).unwrap();
    assert_eq!(link_set_after, link_set_before, "rebuild must not touch existing consumer links");

    let frame_row_after: (String, i64) = conn.query_row(
        "SELECT imagetyp, is_master FROM frames WHERE id = ?1",
        [reg.master_frame_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(
        frame_row_after, frame_row_before,
        "rebuild must not change what the master IS (imagetyp/is_master) — only refresh its header-derived columns",
    );

    // The refresh is id-preserving: the frames row every junction table points
    // at is the same row, UPDATEd in place, never re-inserted.
    let frame_id_after: i64 = conn.query_row(
        "SELECT id FROM frames WHERE file_id = ?1", [reg.master_file_id], |r| r.get(0),
    ).unwrap();
    assert_eq!(frame_id_after, reg.master_frame_id, "rebuild must preserve frames.id");

    let set_row_after: (i64, i64) = conn.query_row(
        "SELECT frame_count, is_master_library FROM calibration_set WHERE id = ?1",
        [reg.master_set_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(set_row_after, set_row_before, "rebuild must not touch the master's calibration_set row");

    // ── Provenance + files: refreshed, source_set_id preserved. ──
    let prov = athenaeum_core::db::master_provenance::get(&conn, reg.master_set_id).unwrap().unwrap();
    assert_eq!(prov.recipe_json, recipe_json_2);
    assert_eq!(prov.member_hash, hash2);
    assert_eq!(prov.source_set_id, Some(set_id), "rebuild must not relink to a different source");

    // ── The file on disk actually changed (same path, new pixels). ──
    let src = athenaeum_core::integration::banded::BandSource::open(
        std::slice::from_ref(&target_abs), scratch.path(), 1,
    ).unwrap();
    let (w, h) = (src.width(), src.height());
    let mut planes = athenaeum_core::integration::banded::BandPlanes::new(&src);
    src.read_band(0, h, &mut planes, 1).unwrap();
    let mut data = vec![0f32; w * h];
    planes.decode_frame_into(0, &mut data);
    let mean: f32 = data.iter().sum::<f32>() / data.len() as f32;
    assert!(
        (mean - 100.0).abs() > 1.0,
        "rebuilt master pixel data must reflect the changed source frame, mean={mean}"
    );
}
