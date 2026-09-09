use super::{models::*, policy::*, progress::progress, storage::*};
use crate::db;
use rusqlite::Connection;

fn goal() -> ObservingGoal {
    ObservingGoal {
        frame_set_id: 1,
        filter: "G".into(),
        revision: 0,
        target_seconds: 3600.0,
        require_analysis: true,
        max_fwhm_px: Some(4.0),
        max_eccentricity: Some(0.6),
        reject_trailed: true,
    }
}
fn fixture() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    db::schema::init_db(&c).unwrap();
    c.execute(
        "
        INSERT INTO frames_set(id,name) VALUES(1,'Synthetic field')
        ",
        [],
    )
    .unwrap();
    let n = db::create_imaging_night(&c, 1, "2026-01-01", "2026-01-02").unwrap();
    let s = db::create_session(&c, n, "Synthetic", 7, Some(2100.0)).unwrap();
    for id in 1..=7 {
        c.execute(
            "
        INSERT INTO files(id,path,filename,size,modified_at,format)
        VALUES(?1,?2,?2,0, 'fixture' , 'FITS' )
        ",
            rusqlite::params![id, format!("/synthetic/{id}.fits")],
        )
        .unwrap();
        c.execute(
            "
        INSERT INTO frames(id,file_id,imagetyp,exptime,filter) VALUES(?1,?1,
        'Light' ,300, 'G' )
        ",
            [id],
        )
        .unwrap();
        db::insert_session_members(&c, s, &[id]).unwrap();
    }
    c.execute_batch(
        "
        INSERT INTO exposure_versions VALUES(1, 'fixture-acquisition' ),(2,
        'fixture-acquisition' );
        INSERT INTO file_processing VALUES(3,0,'integrated','{}',NULL);
        UPDATE frames SET exptime=NULL WHERE id=5;
        INSERT INTO black_hole(file_id,from_where,moved_at,original_path)
        VALUES(7, 'fixture' , 'fixture' , '/synthetic/7.fits' );
        ",
    )
    .unwrap();
    for id in [2, 4] {
        c.execute(
            "
        INSERT INTO
        frame_analysis(frame_id,file_id,stars_detected,median_fwhm,median_eccentricity,median_snr,median_hfr,frame_snr,snr_weight,psf_signal,background,noise,detection_threshold,width,height,source_channels)
            VALUES(?1,?1,20,?2,0.4,10,2,10,1,1,1,1,1,100,100,1)
        ",rusqlite::params![id,if id==4 {5.0}else{3.0}]).unwrap();
    }
    c
}
#[test]
fn representative_counts_exclude_integrations_and_blackhole_without_cherry_picking() {
    let c = fixture();
    let r = progress(&c, 1).unwrap().remove(0);
    assert_eq!(
        (r.accepted, r.rejected, r.unknown, r.accepted_seconds),
        (3, 0, 1, 900.0)
    );
    assert!(r.goal.is_none());
    save(&c, goal()).unwrap();
    let r = progress(&c, 1).unwrap().remove(0);
    // The analyzed alternate (#2) must not silently replace representative #1.
    assert_eq!(
        (r.accepted, r.rejected, r.unknown, r.accepted_seconds),
        (0, 1, 3, 0.0)
    );
    c.execute(
        "
        UPDATE frame_analysis SET frame_id=1,file_id=1 WHERE frame_id=2
        ",
        [],
    )
    .unwrap();
    let r = progress(&c, 1).unwrap().remove(0);
    assert_eq!(
        (r.accepted, r.rejected, r.unknown, r.accepted_seconds),
        (1, 1, 2, 300.0)
    );
}
#[test]
fn empty_filter_goal_revision_and_additive_schema() {
    let c = fixture();
    let mut g = goal();
    g.filter = "H".into();
    save(&c, g.clone()).unwrap();
    assert!(save(&c, g).is_err());
    db::schema::init_db(&c).unwrap();
    let rows = progress(&c, 1).unwrap();
    let r = rows.iter().find(|r| r.filter == "H").unwrap();
    assert_eq!((r.accepted, r.accepted_seconds), (0, 0.0));
    let g = r.goal.clone().unwrap();
    assert_eq!(g.revision, 1);
    save(&c, g.clone()).unwrap();
    assert!(remove(&c, 1, "H", 1).is_err());
    remove(&c, 1, "H", 2).unwrap();
    assert!(goals(&c, 1).unwrap().is_empty());
}
#[test]
fn unknown_and_boundary_metrics_are_explicit() {
    let g = goal();
    assert!(matches!(
        qualify(Some(&g), Some(300.0), Some((1, 4.0, 0.6, false))),
        Qualification::Accepted
    ));
    assert!(matches!(
        qualify(Some(&g), Some(300.0), Some((0, 0.0, 0.0, false))),
        Qualification::Unknown
    ));
    assert!(matches!(
        qualify(Some(&g), Some(300.0), Some((20, 3.0, 0.4, true))),
        Qualification::Rejected
    ));
    assert!(matches!(
        qualify(None, Some(f64::NAN), None),
        Qualification::Unknown
    ));
    let mut invalid = g;
    invalid.require_analysis = false;
    assert!(validate(&invalid).is_err());
    invalid.require_analysis = true;
    invalid.target_seconds = f64::INFINITY;
    assert!(validate(&invalid).is_err());
}
