//! Quality, calibration and storage summaries and the details of one file, against real catalogs.

use super::*;
use crate::api::ApiError;
use crate::db;
use crate::models::{FileFormat, FrameAnalysis, FrameVerdict, ImageType};
use crate::scope::{resolve_scope, FrameState, Scope};
use crate::services::ServiceContext;
use rusqlite::{params, Connection};

// ---- fixtures ----

fn catalog() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    db::schema::init_db(&conn).expect("schema");
    conn
}

fn dir_text() -> &'static str {
    if cfg!(windows) {
        "C:\\data\\night"
    } else {
        "/data/night"
    }
}

fn file_in(conn: &Connection, dir: &str, name: &str, size: i64) -> i64 {
    let sep = std::path::MAIN_SEPARATOR;
    db::insert_file(
        conn,
        &crate::models::File {
            id: None,
            path: format!("{dir}{sep}{name}"),
            filename: name.to_string(),
            size,
            modified_at: chrono::Utc::now(),
            format: FileFormat::FITS,
            created_at: chrono::Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: None,
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        },
    )
    .expect("file")
}

/// One frame of `kind` on a file; returns (file id, frame id).
fn frame_on(conn: &Connection, file_id: i64, kind: ImageType, is_master: bool) -> i64 {
    db::insert_frame(
        conn,
        &crate::models::Frame {
            file_id,
            object: Some("IC 5146".to_string()),
            instrume: Some("Cam".to_string()),
            telescop: Some("Scope".to_string()),
            filter: Some("H".to_string()),
            imagetyp: Some(kind),
            is_master,
            exptime: Some(300.0),
            date_obs: Some(
                chrono::DateTime::parse_from_rfc3339("2024-10-05T22:00:00Z").expect("date").with_timezone(&chrono::Utc),
            ),
            ..Default::default()
        },
    )
    .expect("frame")
}

fn light(conn: &Connection, name: &str) -> (i64, i64) {
    let file = file_in(conn, dir_text(), name, 100);
    (file, frame_on(conn, file, ImageType::Light, false))
}

fn black_hole(conn: &Connection, file_id: i64) {
    conn.execute(
        "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
        [file_id],
    )
    .expect("black hole");
}

fn scoped(conn: &Connection, scope: &Scope) -> Vec<ScopedFrame> {
    let resolved = resolve_scope(conn, scope).expect("resolve");
    fetch_scoped_frames(conn, &resolved.filter.expect("a filter")).expect("fetch")
}

/// The analysis row of a frame: `version` is the measurement version, `fwhm` the value asked for.
fn analyse(conn: &Connection, frame: i64, file: i64, version: Option<u32>, fwhm: f64, verdict: Option<FrameVerdict>) {
    db::analysis::upsert_frame_analysis(
        conn,
        &FrameAnalysis {
            id: None,
            frame_id: frame,
            file_id: file,
            stars_detected: 200,
            median_fwhm: fwhm,
            median_eccentricity: 0.1,
            median_snr: 100.0,
            median_hfr: 2.0,
            frame_snr: 30.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 100.0,
            noise: 5.0,
            detection_threshold: 5.0,
            width: 4000,
            height: 3000,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: false,
            median_beta: None,
            quality_score: None,
            config_hash: Some("t".into()),
            analyzed_at: "2026-03-01T23:00:00Z".into(),
            measure_version: version,
            stars_refused: None,
            frame_verdict: verdict,
            zero_point: None,
            zp_stars: None,
            zp_sigma: None,
            zp_version: None,
            zp_catalog: None,
            zp_refusal: None,
            zp_list_stars: None,
            zp_window_bright: None,
            zp_window_slope: None,
        },
    )
    .expect("analysis");
}

#[cfg(feature = "render")]
fn current() -> u32 {
    crate::star_measure::MEASURE_VERSION
}

// ---- quality ----

#[cfg(feature = "render")]
#[test]
fn unmeasured_is_said_not_estimated() {
    let conn = catalog();
    for (i, fwhm) in [2.0, 2.1, 2.2].into_iter().enumerate() {
        let (file, frame) = light(&conn, &format!("m{i}.fits"));
        analyse(&conn, frame, file, Some(current()), fwhm, None);
    }
    light(&conn, "none.fits");
    let (file, frame) = light(&conn, "old.fits");
    analyse(&conn, frame, file, Some(current() - 1), 9.0, None);

    let frames = scoped(&conn, &Scope::default());
    let q = quality_summary(&conn, None, &frames, QualityMetric::Fwhm).expect("summary");
    assert_eq!((q.measured, q.not_measured, q.older_version), (3, 1, 1));
    assert_eq!(q.median, Some(2.1));
    assert_eq!((q.min, q.max), (Some(2.0), Some(2.2)));
    assert!(q.refused.is_empty());
    let reason = q.not_measured_reason.expect("something is missing");
    assert!(reason.contains("run Analysis on these frames in Athenaeum"), "{reason}");
    assert!(reason.contains("measured by an earlier version — run Analysis again"), "{reason}");
}

#[cfg(feature = "render")]
#[test]
fn a_refused_frame_is_counted_apart_and_a_master_is_not_a_light() {
    let conn = catalog();
    let (file, frame) = light(&conn, "ok.fits");
    analyse(&conn, frame, file, Some(current()), 2.0, None);
    let (file, frame) = light(&conn, "trailed.fits");
    analyse(&conn, frame, file, Some(current()), 0.0, Some(FrameVerdict::Trailed));
    let file = file_in(&conn, dir_text(), "master.fits", 100);
    let master = frame_on(&conn, file, ImageType::Light, true);
    analyse(&conn, master, file, Some(current()), 7.0, None);

    let frames = scoped(&conn, &Scope::default());
    assert_eq!(frames.len(), 3);
    let q = quality_summary(&conn, None, &frames, QualityMetric::Fwhm).expect("summary");
    assert_eq!((q.measured, q.not_measured, q.older_version), (1, 0, 0));
    assert_eq!(q.refused, vec![("stars trailed".to_string(), 1)]);
    assert_eq!(q.max, Some(2.0), "the master light's value is not a light's");
    assert!(q.not_measured_reason.is_none());
}

#[cfg(feature = "render")]
#[test]
fn quality_outliers_named_by_file() {
    let conn = catalog();
    for (i, fwhm) in [2.0, 2.1, 2.0, 2.2, 2.1, 5.0].into_iter().enumerate() {
        let (file, frame) = light(&conn, &format!("f{}.fits", i + 1));
        analyse(&conn, frame, file, Some(current()), fwhm, None);
    }
    let frames = scoped(&conn, &Scope::default());
    let q = quality_summary(&conn, None, &frames, QualityMetric::Fwhm).expect("summary");
    assert_eq!(q.high_outliers.len(), 1);
    assert_eq!(q.high_outliers[0].0.file_name, "f6.fits");
    assert_eq!(q.high_outliers[0].0.handle, format!("file:{}", frames.iter().find(|f| f.file.file.filename == "f6.fits").unwrap().file.file.id.unwrap()));
    assert_eq!(q.high_outliers[0].0.folder, dir_text());
    assert_eq!(q.high_outliers[0].1, 5.0);
    assert!(q.low_outliers.is_empty());
}

#[cfg(feature = "render")]
#[test]
fn quality_over_forty_thousand_frames_chunks_its_ids() {
    let conn = catalog();
    let tx = conn.unchecked_transaction().expect("tx");
    for i in 0..40_000 {
        let (file, frame) = light(&conn, &format!("l{i}.fits"));
        analyse(&conn, frame, file, Some(current()), 2.0 + (i % 7) as f64 * 0.01, None);
    }
    tx.commit().expect("commit");
    let frames = scoped(&conn, &Scope::default());
    assert_eq!(frames.len(), 40_000);
    let q = quality_summary(&conn, None, &frames, QualityMetric::Fwhm).expect("no variable-limit error");
    assert_eq!(q.measured, 40_000);
    assert_eq!(q.not_measured, 0);
}

#[cfg(feature = "render")]
#[test]
fn not_measured_names_the_app_action() {
    let conn = catalog();
    light(&conn, "none.fits");
    let (file, frame) = light(&conn, "old.fits");
    analyse(&conn, frame, file, Some(current() - 1), 3.0, None);
    let frames = scoped(&conn, &Scope::default());
    for metric in [
        QualityMetric::Fwhm,
        QualityMetric::Hfr,
        QualityMetric::Eccentricity,
        QualityMetric::Stars,
        QualityMetric::Snr,
        QualityMetric::FrameSnr,
        QualityMetric::Background,
        QualityMetric::Noise,
        QualityMetric::QualityScore,
        QualityMetric::TransparencyLoss,
    ] {
        let q = quality_summary(&conn, None, &frames, metric).expect("summary");
        assert_eq!(q.measured, 0, "{metric:?}");
        let reason = q.not_measured_reason.unwrap_or_else(|| panic!("{metric:?} has no reason"));
        assert!(reason.contains("Analysis") || reason.contains("plate-solve"), "{metric:?}: {reason}");
        if metric == QualityMetric::TransparencyLoss {
            assert!(reason.contains("plate-solve these frames in Athenaeum, then run Analysis"), "{reason}");
        }
    }
}

#[test]
fn a_quality_metric_is_a_snake_case_tool_argument() {
    assert_eq!(serde_json::to_string(&QualityMetric::FrameSnr).unwrap(), "\"frame_snr\"");
    let m: QualityMetric = serde_json::from_str("\"transparency_loss\"").unwrap();
    assert_eq!(m, QualityMetric::TransparencyLoss);
}

// ---- storage ----

#[test]
fn storage_black_hole_apart() {
    let conn = catalog();
    conn.execute("INSERT INTO scan_roots (path, kind) VALUES (?1, 'wip')", [dir_text()]).expect("root");
    light(&conn, "a.fits");
    light(&conn, "b.fits");
    let hole = file_in(&conn, dir_text(), "gone.fits", 10);
    frame_on(&conn, hole, ImageType::Light, false);
    black_hole(&conn, hole);

    let scope = Scope { state: Some(vec![FrameState::Catalog, FrameState::BlackHole]), ..Scope::default() };
    let frames = scoped(&conn, &scope);
    assert_eq!(frames.len(), 3);
    let s = storage_summary(&conn, &frames).expect("summary");
    assert_eq!(s.black_hole, (1, 10));
    let total: (usize, i64) = s.by_folder.iter().fold((0, 0), |a, l| (a.0 + l.files, a.1 + l.bytes));
    assert_eq!(total, (2, 200), "the Black Hole file is in no folder line");
    assert_eq!(s.by_scan_root.len(), 1);
    assert_eq!((s.by_scan_root[0].files, s.by_scan_root[0].bytes), (2, 200));
    assert_eq!(s.by_scan_root[0].label, dir_text());
    assert_eq!(s.by_state, vec![(FrameState::Catalog, 2, 200)]);
    assert!(s.other_folders.is_none());
}

#[test]
fn storage_lists_the_biggest_folders_and_sums_the_rest() {
    let conn = catalog();
    for i in 0..(GROUPS_SHOWN + 3) {
        let file = file_in(&conn, &format!("{}{i:03}", dir_text()), "x.fits", 10 + i as i64);
        frame_on(&conn, file, ImageType::Light, false);
    }
    let frames = scoped(&conn, &Scope::default());
    let s = storage_summary(&conn, &frames).expect("summary");
    assert_eq!(s.by_folder.len(), GROUPS_SHOWN);
    assert!(s.by_folder.windows(2).all(|w| w[0].bytes >= w[1].bytes));
    let other = s.other_folders.expect("three folders are not listed");
    assert_eq!((other.label.as_str(), other.files), ("3 other folders", 3));
    assert_eq!(s.by_scan_root[0].label, "(no scan root)");
}

// ---- calibration (needs the export readiness, which a headless build does not have) ----

#[cfg(feature = "render")]
mod calibration_lines {
    use super::*;

    fn ctx() -> (tempfile::TempDir, ServiceContext) {
        let (tmp, base) = crate::test_support::canonical_tempdir();
        let ctx = ServiceContext::new_for_tests(base.join("catalog.db"));
        (tmp, ctx)
    }

    /// A frame set `name` holding `lights` (frame ids).
    fn frame_set(conn: &Connection, name: &str, lights: &[i64]) {
        let set = db::create_frames_set(conn, Some(name), false, None, None, None, None, None, None, None, None)
            .expect("set");
        let night = db::create_imaging_night(conn, set, "2024-10-05T00:00:00Z", "2024-10-05T12:00:00Z").expect("night");
        let session = db::create_session(conn, night, "Cam", lights.len() as i32, None).expect("session");
        db::insert_session_members(conn, session, lights).expect("members");
    }

    /// A built master of `kind` whose file exists on disk, linked to every frame in `lights`.
    fn master_for(conn: &Connection, dir: &std::path::Path, kind: ImageType, link: &str, lights: &[i64]) {
        let name = format!("master_{link}_{}.fits", lights[0]);
        let path = dir.join(&name);
        std::fs::write(&path, b"x").expect("master file");
        let file = file_in(conn, &dir.to_string_lossy(), &name, 1);
        let frame = frame_on(conn, file, kind.clone(), true);
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume, exptime) \
             VALUES (?1, '2024-10-05', 1, 'Cam', 300)",
            [format!("{kind:?}")],
        )
        .expect("set");
        let set = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", [set, frame]).expect("member");
        for light in lights {
            conn.execute(
                "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
                 VALUES (?1, 'frame', ?2, ?3, '2024-10-05T00:00:00Z')",
                params![light, set, link],
            )
            .expect("link");
        }
    }

    #[test]
    fn calibration_reason_only_where_the_app_states_one() {
        let (tmp, ctx) = ctx();
        let dir = tmp.path().join("masters");
        std::fs::create_dir_all(&dir).expect("dir");
        {
            let db = ctx.db.get().expect("db");
            let conn = db.conn();
            // A: two lights, nothing linked.
            let a: Vec<i64> = (0..2).map(|i| light(&conn, &format!("a{i}.fits")).1).collect();
            frame_set(&conn, "A bare", &a);
            // B: dark and flat masters, files on disk.
            let b: Vec<i64> = (0..2).map(|i| light(&conn, &format!("b{i}.fits")).1).collect();
            frame_set(&conn, "B calibrated", &b);
            master_for(&conn, &dir, ImageType::MasterDark, "Dark", &b);
            master_for(&conn, &dir, ImageType::MasterFlat, "Flat", &b);
            // C: a dark only: no flats, and the app states no reason against it.
            let c: Vec<i64> = (0..2).map(|i| light(&conn, &format!("c{i}.fits")).1).collect();
            frame_set(&conn, "C dark only", &c);
            master_for(&conn, &dir, ImageType::MasterDark, "Dark", &c);
        }
        let frames = {
            let db = ctx.db.get().expect("db");
            let conn = db.conn();
            scoped(&conn, &Scope::default())
        };

        let (lines, sets) = calibration_summary(&ctx, &frames, 0).expect("summary");
        assert_eq!(sets, 3);
        assert_eq!(lines.len(), 3, "{lines:#?}");
        let line = |name: &str| lines.iter().find(|l| l.frame_set == name).unwrap_or_else(|| panic!("{name} missing"));

        let a = line("A bare");
        assert_eq!((a.lights, a.darks, a.flats, a.bias), (2, false, false, false));
        assert_eq!(a.reason.as_deref(), Some("2 lights have no calibration links"));
        assert_eq!((a.camera.as_str(), a.filter.as_str(), a.night.as_str()), ("Cam", "H (300s)", "2024-10-05"));

        let b = line("B calibrated");
        assert_eq!((b.darks, b.flats, b.bias), (true, true, false));
        assert_eq!(b.reason, None, "{b:#?}");

        let c = line("C dark only");
        assert_eq!((c.darks, c.flats), (true, false));
        assert_eq!(c.reason, None, "no flats, but the app states no reason: none is invented");

        // A second page holds no set.
        let (page, total) = calibration_summary(&ctx, &frames, 1).expect("page");
        assert!(page.is_empty());
        assert_eq!(total, 3);
    }
}

// ---- file details ----

mod details {
    use super::*;

    fn ctx() -> (tempfile::TempDir, std::path::PathBuf, ServiceContext) {
        let (tmp, base) = crate::test_support::canonical_tempdir();
        let ctx = ServiceContext::new_for_tests(base.join("catalog.db"));
        (tmp, base, ctx)
    }

    #[test]
    fn file_details_by_path_and_handle() {
        let (_tmp, _base, ctx) = ctx();
        let (file_id, path) = {
            let db = ctx.db.get().expect("db");
            let conn = db.conn();
            let (file, _frame) = light(&conn, "x.fits");
            // The catalog's own spelling of the path: the directory is never created on disk.
            let path = format!("{}{}x.fits", dir_text(), std::path::MAIN_SEPARATOR);
            conn.execute("INSERT INTO fits_header (file_id, header) VALUES (?1, 'EXPTIME = 300 / s\nFILTER  = ''H''')", [file])
                .expect("header");
            (file, path)
        };
        assert!(!std::path::Path::new(&path).exists(), "the fixture path is not on disk");

        let by_handle = file_details(&ctx, &format!("file:{file_id}")).expect("by handle");
        let by_path = file_details(&ctx, &path).expect("by path");
        assert_eq!(by_handle.line, by_path.line);
        assert_eq!(by_handle.path, path);
        assert_eq!(by_handle.size, 100);
        assert_eq!(by_handle.line.handle, format!("file:{file_id}"));
        assert_eq!(by_handle.line.file_name, "x.fits");
        assert_eq!(by_handle.line.folder, dir_text());
        assert_eq!(by_handle.line.state, FrameState::Catalog);
        assert_eq!(by_handle.modified_at.len(), "2024-10-05 22:00:00".len());
        assert!(by_handle.analysis.is_none() && !by_handle.solved && by_handle.transparency_loss.is_none());
        assert!(by_handle.calibration.is_empty() && by_handle.copies.is_empty());
        let keys: Vec<&str> = by_handle.header.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"EXPTIME") && keys.contains(&"FILTER"), "{keys:?}");

        for unknown in [format!("{}{}nope.fits", dir_text(), std::path::MAIN_SEPARATOR), "file:999999".to_string(), "file:abc".to_string()] {
            match file_details(&ctx, &unknown) {
                Err(ApiError::NotFound(message)) => assert!(message.contains(&unknown), "{message}"),
                other => panic!("{unknown}: expected NotFound, got {other:?}"),
            }
        }
    }

    #[test]
    fn file_details_names_a_copy_and_a_master() {
        let (_tmp, _base, ctx) = ctx();
        let (a, b) = {
            let db = ctx.db.get().expect("db");
            let conn = db.conn();
            conn.execute("INSERT INTO scan_roots (path, find_duplicates) VALUES ('/vol', 1)", []).expect("root");
            let mut ids = Vec::new();
            for (id, path) in [(1i64, "/vol/a/f.fits"), (2, "/vol/b/f.fits")] {
                conn.execute(
                    "INSERT INTO files (id, path, filename, size, modified_at, format) \
                     VALUES (?1, ?2, 'f.fits', 100, '2024-10-05T04:21:46+00:00', 'FITS')",
                    params![id, path],
                )
                .expect("file");
                conn.execute("INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?1, 'Flat', 0)", [id])
                    .expect("frame");
                conn.execute(
                    "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'HDR', ?2)",
                    params![id, crate::fingerprint::compute_header_fingerprint("HDR")],
                )
                .expect("header");
                ids.push(id);
            }
            // A built master dark linked to the first frame.
            let master_file = file_in(&conn, "/vol/masters", "master_dark.fits", 5);
            let master_frame = frame_on(&conn, master_file, ImageType::MasterDark, true);
            conn.execute(
                "INSERT INTO calibration_set (imagetyp, date, is_master_library) VALUES ('MasterDark', '2024-10-05', 1)",
                [],
            )
            .expect("set");
            let set = conn.last_insert_rowid();
            conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", [set, master_frame]).expect("member");
            conn.execute(
                "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
                 VALUES (1, 'frame', ?1, 'Dark', '2024-10-05T00:00:00Z')",
                [set],
            )
            .expect("link");
            (ids[0], ids[1])
        };
        let d = file_details(&ctx, &format!("file:{a}")).expect("details");
        assert_eq!(d.copies.len(), 1, "{:#?}", d.copies);
        assert_eq!(d.copies[0].handle, format!("file:{b}"));
        assert_eq!(d.copies[0].file_name, "f.fits");
        assert_eq!(d.copies[0].folder, "/vol/b");
        assert_eq!(d.calibration, vec![("Dark".to_string(), "master_dark.fits".to_string(), "/vol/masters".to_string())]);
    }
}
