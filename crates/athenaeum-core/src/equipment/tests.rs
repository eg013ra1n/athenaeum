use super::{matching::*, models::*, storage::*};
use rusqlite::Connection;
fn profile() -> EquipmentProfile {
    EquipmentProfile {
        id: 0,
        revision: 0,
        name: "Fixture refractor".into(),
        telescope: "100/500".into(),
        camera: "Synthetic camera".into(),
        focal_length_mm: 500.0,
        optical_multiplier: 0.8,
        pixel_size_um: 3.76,
        binning: 1,
        tolerance_percent: 5.0,
    }
}
#[test]
fn physical_scale_binning_multiplier_and_ambiguous_candidates() {
    let p = profile();
    let s = expected_scale(&p);
    assert!((s - 1.938889178607).abs() < 1e-8);
    let mut b = p.clone();
    b.binning = 2;
    assert_eq!(expected_scale(&b), 2.0 * s);
    assert_eq!(
        candidates(&[p.clone(), p.clone()], &p.camera, Some(1), Some(1), s).len(),
        2
    );
    assert!(candidates(&[p.clone()], &p.camera, None, None, s).is_empty());
    assert!(candidates(&[p.clone()], &p.camera, Some(1), Some(2), s).is_empty());
    assert!(candidates(&[p.clone()], "Other camera", Some(1), Some(1), s).is_empty());
    assert!(candidates(&[p.clone()], &p.camera, Some(1), Some(1), s * 1.1).is_empty());
    for bad in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        let mut p = profile();
        p.pixel_size_um = bad;
        assert!(validate(&p).is_err());
    }
}
fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "PRAGMA foreign_keys=ON;
      CREATE TABLE files(id INTEGER PRIMARY KEY,filename TEXT,path TEXT);
      CREATE TABLE frames(id INTEGER PRIMARY KEY,file_id INTEGER,instrume TEXT,xbinning INTEGER,ybinning INTEGER);
      CREATE TABLE plate_solves(frame_id INTEGER PRIMARY KEY,pixel_scale_arcsec REAL,solved_at TEXT);
      INSERT INTO files VALUES(7,'synthetic.fits','/synthetic/only.fits');
      INSERT INTO frames VALUES(9,7,'Synthetic camera',1,1);").unwrap();
    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap();
    conn.execute(
        "
        INSERT INTO plate_solves VALUES(9,?1,'fixture-time')
        ",
        [expected_scale(&profile())],
    )
    .unwrap();
    save(&conn, profile()).unwrap();
    conn
}
#[test]
fn reviewed_matches_detect_changed_solve_and_profile_without_touching_frames() {
    let conn = fixture();
    let p = profiles(&conn).unwrap().remove(0);
    let e = evidence(&conn, &p.camera, 0).unwrap().remove(0);
    assert_eq!(e.confirmed_profile_id, None);
    assert!(confirm(&conn, 9, p.id, p.revision, "old", e.solved_scale).is_err());
    confirm(&conn, 9, p.id, p.revision, &e.solved_at, e.solved_scale).unwrap();
    assert!(!evidence(&conn, &p.camera, 0).unwrap()[0].confirmation_stale);
    conn.execute(
        "
        UPDATE plate_solves SET solved_at='new-time' WHERE frame_id=9
        ",
        [],
    )
    .unwrap();
    assert!(evidence(&conn, &p.camera, 0).unwrap()[0].confirmation_stale);
    let mut changed = p.clone();
    changed.name = "Changed definition".into();
    save(&conn, changed).unwrap();
    assert!(save(&conn, p.clone()).is_err());
    assert!(confirm(&conn, 9, p.id, p.revision, "new-time", e.solved_scale).is_err());
    let ids: (i64, i64) = conn
        .query_row(
            "
        SELECT id,file_id FROM frames
        ",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ids, (9, 7));
    conn.execute(
        "
        DELETE FROM equipment_profiles WHERE id=?1
        ",
        [p.id],
    )
    .unwrap();
    assert_eq!(
        conn.query_row(
            "
        SELECT COUNT(*) FROM equipment_frame_matches
        ",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn pagination_is_by_frame_identity_not_file_id_and_never_confirms_implicitly() {
    let conn = fixture();
    assert!(evidence(&conn, "Synthetic camera", 9).unwrap().is_empty());
    assert_eq!(
        evidence(&conn, "Synthetic camera", 8).unwrap()[0].frame_id,
        9
    );
    assert!(evidence(&conn, "Wrong camera", 0).unwrap().is_empty());
    assert_eq!(
        conn.query_row(
            "
        SELECT COUNT(*) FROM equipment_frame_matches
        ",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn schema_initialization_is_additive_and_repeatable() {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    save(&conn, profile()).unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    assert_eq!(profiles(&conn).unwrap().len(), 1);
    assert_eq!(
        conn.query_row(
            "
        SELECT COUNT(*) FROM equipment_frame_matches
        ",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
