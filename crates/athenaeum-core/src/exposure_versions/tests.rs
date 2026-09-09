use super::*;
use crate::{db, models::FileFormat, services::ServiceContext};

#[test]
fn external_headers_keep_unknown_and_separate_integration() {
    // Markers observed in local Siril headers; filenames intentionally absent.
    assert_eq!(
        classify(FileFormat::FITS, "HISTORY Calibrated with a master bias\n").stage,
        "calibrated"
    );
    assert_eq!(
        classify(
            FileFormat::FITS,
            "STACKCNT=                   10\nHISTORY mean stacking with winsorized sigma clipping"
        )
        .stage,
        "integrated"
    );
    assert_eq!(
        classify(
            FileFormat::FITS,
            "NCOMBINE=                   18\nHISTORY ImageCalibration: masterDark"
        )
        .stage,
        "integrated"
    );
    assert_eq!(
        classify(
            FileFormat::FITS,
            "SWCREATE= 'Siril'\nCALSTAT = 'NONE'\nOBJECT  = 'ImageIntegration'\n"
        )
        .stage,
        "unknown"
    );
    let xml = r#"<xisf><Image><FITSKeyword name="OBJECT" value="ImageIntegration"/><FITSKeyword name="HISTORY" value="ImageCalibration: completed"/><FITSKeyword name="HISTORY" value="StarAlignment: registered"/></Image></xisf>"#;
    let c = classify(FileFormat::XISF, xml);
    assert_eq!(c.stage, "registered");
    assert!(c.steps.contains(&"calibrated".into()));
    assert!(!c.steps.contains(&"integrated".into()));
    assert_eq!(classify(FileFormat::FITS, "").stage, "unknown");
}

fn fixture() -> (tempfile::TempDir, ServiceContext) {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ServiceContext::new_for_tests(tmp.path().join("test.db"));
    {
        let conn = ctx.db.get().unwrap().conn();
        conn.execute(
            "
        INSERT INTO frames_set(id,name) VALUES (1,'External versions')
        ",
            [],
        )
        .unwrap();
        let night =
            db::create_imaging_night(&conn, 1, "2026-05-21T11:31:25Z", "2026-05-21T12:00:00Z")
                .unwrap();
        let session = db::create_session(&conn, night, "QHYminiCam8M", 4, Some(1200.0)).unwrap();
        for (id, header) in [
            (1, "HISTORY Calibrated with a master dark\n"),
            (2, "HISTORY StarAlignment: registered\n"),
            (3, "STACKCNT=                   20\n"),
            (4, ""),
        ] {
            conn.execute(
                "
        INSERT INTO files(id,path,filename,size,modified_at,format) VALUES
        (?1,?2,?3,0, '2026-01-01' , 'FITS' )
        ",
                rusqlite::params![
                    id,
                    format!("/fixture/{id}.fits"),
                    format!("version_{id}.fits")
                ],
            )
            .unwrap();
            conn.execute(
                "
        INSERT INTO
        frames(id,file_id,imagetyp,date_obs,instrume,exptime,filter,naxis1,naxis2,ra,dec)
        VALUES (?1,?1, 'Light' , '2026-05-21T11:31:25Z' , 'QHYminiCam8M' ,300,
        'G' ,3856,2180,270,-20)
        ",
                [id],
            )
            .unwrap();
            db::insert_fits_header(&conn, id, header).unwrap();
            db::insert_session_members(&conn, session, &[id]).unwrap();
        }
        // Same camera/time/filter is not enough if duration differs.
        conn.execute(
            "
        UPDATE frames SET exptime=60 WHERE id=4
        ",
            [],
        )
        .unwrap();
    }
    (tmp, ctx)
}

#[test]
fn confirmed_versions_count_once_without_a_raw_file_and_unlink_restores_count() {
    let (_tmp, ctx) = fixture();
    let before = get_review(&ctx, vec![1]).unwrap();
    assert_eq!(before.exposure_count, 3);
    assert_eq!(before.exposure_seconds, 660.0);
    assert_eq!(before.suggestions.len(), 1);
    assert!(before
        .versions
        .iter()
        .all(|r| r.classification.stage != "raw"));
    assert!(confirm_link(&ctx, 1, 3).is_err());
    assert!(confirm_link(&ctx, 1, 4).is_err());
    confirm_link(&ctx, 1, 2).unwrap();
    let review = get_review(&ctx, vec![1]).unwrap();
    assert_eq!(review.exposure_count, 2);
    assert_eq!(review.exposure_seconds, 360.0);
    {
        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(
            db::get_frames_sets_by_project(&conn, 1).unwrap()[0]
                .0
                .total_exp_time,
            Some(360.0)
        );
        let locations = crate::api::spatial::get_imaging_locations(&conn).unwrap();
        assert_eq!(locations[0].total_exposure, 360.0);
        assert_eq!(locations[0].frame_count, 2);
        assert_eq!(effective_ids(&conn, &[2]).unwrap(), vec![2]);
        assert_eq!(
            crate::export::frame_set_queries::get_exportable_frame_sets(&conn).unwrap()[0]
                .total_exposure_seconds,
            360.0
        );
        assert_eq!(db::get_all_cameras(&conn).unwrap()[0].total_hours, 0.1);
        assert_eq!(
            conn.query_row(
                "
        SELECT COUNT(*) FROM frames
        ",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            4
        );
    }
    unlink_version(&ctx, 2).unwrap();
    assert_eq!(get_review(&ctx, vec![1]).unwrap().exposure_count, 3);
    set_stage(&ctx, 4, Some("integrated".into())).unwrap();
    assert_eq!(get_review(&ctx, vec![1]).unwrap().exposure_seconds, 600.0);
    assert!(set_stage(&ctx, 3, Some("raw".into())).is_err());
}

#[test]
fn rescanning_a_linked_version_as_a_stack_breaks_the_link() {
    let (_tmp, ctx) = fixture();
    confirm_link(&ctx, 1, 2).unwrap();
    {
        let conn = ctx.db.get().unwrap().conn();
        db::insert_fits_header(&conn, 2, "STACKCNT=                    5\n").unwrap();
    }
    let review = get_review(&ctx, vec![1]).unwrap();
    let stack = review.versions.iter().find(|r| r.frame_id == 2).unwrap();
    assert_eq!(stack.classification.stage, "integrated");
    assert!(stack.exposure_id.is_none());
    assert_eq!(review.exposure_seconds, 360.0);
}

#[test]
fn unchanged_rescan_preserves_links_but_metadata_edits_invalidate_them() {
    let (_tmp, ctx) = fixture();
    confirm_link(&ctx, 1, 2).unwrap();
    {
        let conn = ctx.db.get().unwrap().conn();
        db::insert_fits_header(&conn, 2, "HISTORY StarAlignment: registered\n").unwrap();
        init_schema(&conn).unwrap();
    }
    assert_eq!(get_review(&ctx, vec![1]).unwrap().exposure_count, 2);
    {
        let conn = ctx.db.get().unwrap().conn();
        conn.execute(
            "
        UPDATE frames SET exptime=120 WHERE id=2
        ",
            [],
        )
        .unwrap();
    }
    let review = get_review(&ctx, vec![1]).unwrap();
    assert_eq!(review.exposure_count, 3);
    assert_eq!(review.exposure_seconds, 480.0);
    assert!(review
        .versions
        .iter()
        .find(|r| r.frame_id == 2)
        .unwrap()
        .exposure_id
        .is_none());
    assert!(confirm_link(&ctx, 1, 2).is_err());
}

#[test]
fn stack_only_object_keeps_pointing_without_single_exposure_time() {
    let (_tmp, ctx) = fixture();
    let conn = ctx.db.get().unwrap().conn();
    let metadata =
        crate::frames_set_metadata::calculate_metadata_from_frame_ids(&[3], &conn).unwrap();
    assert!(metadata.objctra.is_some());
    assert!(metadata.objctdec.is_some());
    assert_eq!(metadata.total_exp_time, Some(0.0));
}

#[test]
fn statistics_use_exposure_identity_across_exports_sessions_and_overlapping_objects() {
    let (_tmp, ctx) = fixture();
    confirm_link(&ctx, 1, 2).unwrap();
    let conn = ctx.db.get().unwrap().conn();
    assert_eq!(exposure_seconds(&conn, &[1, 2, 3, 4, 1]).unwrap(), 360.0);
    assert_eq!(exposure_seconds(&conn, &[2, 3]).unwrap(), 300.0);
    // Debayering can put two versions in different export camera-type groups.
    conn.execute(
        "
        UPDATE frames SET bayerpat='RGGB' WHERE id=2
        ",
        [],
    )
    .unwrap();
    let data = crate::export::data_collector::collect_export_data(&conn, 1).unwrap();
    assert_eq!(data.total_light_frames, 4);
    assert_eq!(data.total_exposure_seconds, 360.0);
    assert_eq!(
        data.groups.iter().map(|g| g.total_exposure).sum::<f64>(),
        360.0
    );
    let night: i64 = conn
        .query_row(
            "
        SELECT id FROM imaging_nights WHERE frames_set_id=1
        ",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db::get_sessions_for_night(&conn, night).unwrap()[0].total_exp_time,
        Some(360.0)
    );
    let bounds = crate::models::SelectionBounds {
        ra_min: 269.0,
        ra_max: 271.0,
        dec_min: -21.0,
        dec_max: -19.0,
        crosses_meridian: Some(false),
    };
    let selection = crate::api::spatial::query_frames_in_bounds(&conn, bounds).unwrap();
    assert_eq!(selection.count, 4);
    assert_eq!(selection.total_exposure_seconds, 360.0);
    conn.execute(
        "
        INSERT INTO frames_set(id,name) VALUES (2,'Overlapping object')
        ",
        [],
    )
    .unwrap();
    let other =
        db::create_imaging_night(&conn, 2, "2026-05-21T11:31:25Z", "2026-05-21T12:00:00Z").unwrap();
    let session = db::create_session(&conn, other, "QHYminiCam8M", 2, Some(600.0)).unwrap();
    db::insert_session_members(&conn, session, &[1, 2]).unwrap();
    let calendar = crate::api::calendar::get_calendar_month_data(&conn, 2026, 5).unwrap();
    assert_eq!(calendar.total_frame_count, 2);
    assert_eq!(calendar.total_exposure_seconds, 360.0);
}
