//! Frame query tests: grouping and paging over in-memory frames, the fetch against a real catalog, and the frame
//! set figures against `get_frames_sets_by_project` (the page's own numbers).

use super::*;
use crate::db;
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::scope::{resolve_scope, Scope};
use chrono::Utc;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- in-memory frames ----

/// One in-memory frame; `night` is given directly.
fn mem(id: i64, object: &str, filter: &str, night: &str, exptime: f64, kind: ImageType) -> ScopedFrame {
    let path = format!("{}{}", if cfg!(windows) { "C:\\data\\" } else { "/data/" }, format_args!("f{id}.fits"));
    ScopedFrame {
        file: FileWithFrame {
            file: File {
                id: Some(id),
                path: path.clone(),
                filename: format!("f{id}.fits"),
                size: 10,
                modified_at: Utc::now(),
                format: FileFormat::FITS,
                created_at: Utc::now(),
                content_hash: None,
                archived_in_operation: None,
                archive_zip_path: None,
                archive_path_in_zip: None,
                uuid: None,
                updated_at: None,
            },
            frame: Some(Frame {
                id: Some(id),
                file_id: id,
                object: Some(object.to_string()),
                filter: Some(filter.to_string()),
                imagetyp: Some(kind),
                exptime: Some(exptime),
                ..Default::default()
            }),
        },
        folder: super::parent_of(&path),
        night: Some(night.to_string()),
        state: FrameState::Catalog,
        frame_sets: Vec::new(),
        scan_root: None,
    }
}

fn totals(frames: usize) -> Totals {
    Totals { frames, lights: 0, integration_s: 0.0, bytes: 0, nights: 0, first_night: None, last_night: None }
}

#[test]
fn group_label_reads_like_the_spec() {
    let key = BTreeMap::from([
        (GroupKey::Object, "IC 5146".to_string()),
        (GroupKey::Night, "2024-10-05".to_string()),
        (GroupKey::Filter, "H".to_string()),
        (GroupKey::Exposure, "300".to_string()),
    ]);
    assert_eq!(group_label(&key, &totals(24)), "IC 5146, night 2024-10-05, filter H, 300 s, 24 frames");
    assert_eq!(group_label(&BTreeMap::new(), &totals(1)), "1 frame");
}

#[test]
fn forty_thousand_frames_come_back_as_a_summary() {
    let frames: Vec<ScopedFrame> = (0..40_000)
        .map(|i| mem(i, &format!("Object {:03}", i % 120), "L", "2024-10-05", 60.0, ImageType::Light))
        .collect();
    let r = query_frames(&frames, &[GroupKey::Object], None);
    assert_eq!(r.totals.frames, 40_000);
    assert_eq!(r.groups.len(), GROUPS_SHOWN);
    assert_eq!(r.groups_total, 120);
    assert!(r.files.is_none());
    let rest = r.rest.expect("groups beyond the shown ones are summed");
    let shown: usize = r.groups.iter().map(|g| g.totals.frames).sum();
    assert_eq!(rest.frames + shown, 40_000);
    assert!(r.groups.iter().all(|g| g.examples.len() <= EXAMPLES_PER_GROUP && !g.examples.is_empty()));
    assert!(r.groups.windows(2).all(|w| w[0].totals.frames >= w[1].totals.frames), "frames descending");

    let paged = query_frames(&frames, &[GroupKey::Object], Some(0));
    let page = paged.files.expect("a page was asked for");
    assert_eq!(page.items.len(), 50);
    assert_eq!(page.page, 0);
    assert_eq!(page.page_count, 800);
    let last = query_frames(&frames, &[], Some(799)).files.unwrap();
    assert_eq!(last.items.len(), 50);
    assert!(query_frames(&frames, &[], Some(800)).files.unwrap().items.is_empty());
}

#[test]
fn integration_counts_lights_only() {
    let frames = vec![
        mem(1, "M31", "L", "2024-10-05", 300.0, ImageType::Light),
        mem(2, "M31", "L", "2024-10-05", 300.0, ImageType::Light),
        mem(3, "M31", "L", "2024-10-05", 300.0, ImageType::Dark),
        mem(4, "M31", "L", "2024-10-05", 1.0, ImageType::Flat),
    ];
    let r = query_frames(&frames, &[], None);
    assert_eq!(r.totals.frames, 4);
    assert_eq!(r.totals.lights, 2);
    assert_eq!(r.totals.integration_s, 600.0);
    assert_eq!(r.totals.bytes, 40);
    assert!(r.groups.is_empty() && r.groups_total == 0 && r.rest.is_none());
}

#[test]
fn a_master_is_a_master_everywhere() {
    let mut master = mem(1, "M31", "L", "2024-10-05", 300.0, ImageType::Dark);
    master.file.frame.as_mut().unwrap().is_master = true;
    let light = mem(2, "M31", "L", "2024-10-05", 300.0, ImageType::Light);
    let mut master_light = mem(3, "M31", "L", "2024-10-05", 300.0, ImageType::Light);
    master_light.file.frame.as_mut().unwrap().is_master = true;
    let frames = vec![master, light, master_light];

    assert_eq!(file_line(&frames[0]).imagetyp.as_deref(), Some("MasterDark"));
    assert_eq!(file_line(&frames[1]).imagetyp.as_deref(), Some("Light"));
    let r = query_frames(&frames, &[GroupKey::FrameType], None);
    let labels: Vec<&str> = r.groups.iter().map(|g| g.key[&GroupKey::FrameType].as_str()).collect();
    assert_eq!(labels.len(), 3);
    for want in ["MasterDark", "MasterLight", "Light"] {
        assert!(labels.contains(&want), "{want} missing from {labels:?}");
    }
    assert_eq!(r.totals.lights, 1, "a master light is not a raw light");
    assert_eq!(r.totals.integration_s, 300.0);
}

#[test]
fn month_and_year_follow_the_night() {
    // 2024-10-01 00:30 UTC belongs to the night of 2024-09-30: month 2024-09.
    let frames = vec![
        mem(1, "M31", "L", "2024-09-30", 60.0, ImageType::Light),
        mem(2, "M31", "L", "2024-10-01", 60.0, ImageType::Light),
        mem(3, "M31", "L", "2024-10-31", 60.0, ImageType::Light),
        mem(4, "M31", "L", "2025-01-01", 60.0, ImageType::Light),
    ];
    let by_month = query_frames(&frames, &[GroupKey::Month], None);
    let months: Vec<(&str, usize)> =
        by_month.groups.iter().map(|g| (g.key[&GroupKey::Month].as_str(), g.totals.frames)).collect();
    assert_eq!(months, vec![("2024-10", 2), ("2024-09", 1), ("2025-01", 1)]);
    let by_year = query_frames(&frames, &[GroupKey::Year], None);
    let years: Vec<(&str, usize)> =
        by_year.groups.iter().map(|g| (g.key[&GroupKey::Year].as_str(), g.totals.frames)).collect();
    assert_eq!(years, vec![("2024", 3), ("2025", 1)]);
    let g = &by_month.groups[0];
    assert_eq!((g.totals.first_night.as_deref(), g.totals.last_night.as_deref()), (Some("2024-10-01"), Some("2024-10-31")));
    assert_eq!(g.totals.nights, 2);
}

#[test]
fn a_group_key_serialises_with_snake_case_names() {
    let g = FrameGroup {
        key: BTreeMap::from([(GroupKey::FrameSet, "A".to_string()), (GroupKey::ScanRoot, "/r".to_string())]),
        label: "x".to_string(),
        totals: totals(1),
        examples: Vec::new(),
        frame_set_handles: Vec::new(),
    };
    let v = serde_json::to_value(&g).unwrap();
    assert_eq!(v["key"]["frame_set"], "A");
    assert_eq!(v["key"]["scan_root"], "/r");
    assert_eq!(v["totals"]["integrationS"], 0.0);
    let line = file_line(&mem(7, "M31", "L", "2024-10-05", 60.0, ImageType::Light));
    let v = serde_json::to_value(&line).unwrap();
    assert_eq!(v["handle"], "file:7");
    assert_eq!(v["fileName"], "f7.fits");
    assert!(v["folder"].is_string());
    assert_eq!(v["state"], "catalog");
}

#[test]
fn format_duration_reads_in_the_unit_that_fits() {
    assert_eq!(format_duration(45.0), "45 s");
    assert_eq!(format_duration(12.0 * 60.0), "12 min");
    assert_eq!(format_duration(2.0 * 3600.0 + 5.0 * 60.0), "2 h 05 min");
    assert_eq!(format_duration(3600.0), "1 h 00 min");
    assert_eq!(format_duration(0.0), "0 s");
}

#[test]
fn paths_split_on_their_own_separator_on_every_host() {
    assert_eq!(parent_of("C:\\data\\n\\a.fits"), "C:\\data\\n");
    assert_eq!(parent_of("C:\\a.fits"), "C:\\");
    assert_eq!(parent_of("/a.fits"), "/");
    assert_eq!(parent_of("\\\\server\\share\\n\\a.fits"), "\\\\server\\share\\n");
    let roots = vec![("C:\\data\\".to_string(), "C:\\data".to_string(), "main".to_string())];
    assert_eq!(scan_root_of(&roots, "C:\\data\\n\\a.fits"), Some(("C:\\data".to_string(), "main".to_string())));
    assert_eq!(scan_root_of(&roots, "C:\\database\\a.fits"), None, "a longer name is not inside the root");
}

#[test]
fn a_missing_filter_has_one_spelling() {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\n" } else { "/data/n" };
    let (_, f) = light(&conn, dir, "x.fits", "  ");
    let set = set_of(&conn, "IC 5146", false, None, &[f]);
    let t = frame_set_totals(&conn, set).unwrap();
    assert_eq!(t.by_filter, vec![("(no filter)".to_string(), 1, 300.0)]);
    assert_eq!(none_of("filter"), "(no filter)");
}

#[test]
fn every_state_name_round_trips() {
    for s in FrameState::ALL {
        assert_eq!(state_from_name(state_name(s)), Some(s));
    }
    assert_eq!(state_from_name("nope"), None);
}

// ---- a real catalog ----

fn catalog() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    db::schema::init_db(&conn).expect("schema");
    conn
}

struct Add<'a> {
    name: &'a str,
    filter: &'a str,
    kind: ImageType,
    exptime: f64,
    date_obs: &'a str,
    zip: Option<&'a str>,
}

fn add(conn: &Connection, dir: &str, a: &Add) -> (i64, i64) {
    let sep = std::path::MAIN_SEPARATOR;
    let file_id = db::insert_file(
        conn,
        &File {
            id: None,
            path: format!("{dir}{sep}{}", a.name),
            filename: a.name.to_string(),
            size: 100,
            modified_at: Utc::now(),
            format: FileFormat::FITS,
            created_at: Utc::now(),
            content_hash: None,
            archived_in_operation: None,
            archive_zip_path: a.zip.map(str::to_string),
            archive_path_in_zip: None,
            uuid: None,
            updated_at: None,
        },
    )
    .expect("file");
    if let Some(zip) = a.zip {
        // `insert_file` does not write the zip columns; a zip entry is a file row updated after.
        conn.execute("UPDATE files SET archive_zip_path = ?2 WHERE id = ?1", rusqlite::params![file_id, zip]).expect("zip");
    }
    let frame_id = db::insert_frame(
        conn,
        &Frame {
            file_id,
            object: Some("IC 5146".to_string()),
            instrume: Some("Cam".to_string()),
            telescop: Some("Scope".to_string()),
            filter: Some(a.filter.to_string()),
            imagetyp: Some(a.kind.clone()),
            exptime: Some(a.exptime),
            date_obs: Some(chrono::DateTime::parse_from_rfc3339(a.date_obs).expect("date").with_timezone(&Utc)),
            ..Default::default()
        },
    )
    .expect("frame");
    (file_id, frame_id)
}

fn light(conn: &Connection, dir: &str, name: &str, filter: &str) -> (i64, i64) {
    add(conn, dir, &Add { name, filter, kind: ImageType::Light, exptime: 300.0, date_obs: "2024-10-05T22:00:00Z", zip: None })
}

/// A set holding `frames`; `stored` is the `frames_set.total_exp_time` column.
fn set_of(conn: &Connection, name: &str, archived: bool, stored: Option<f64>, frames: &[i64]) -> i64 {
    let set = db::create_frames_set(conn, Some(name), false, None, None, None, None, stored, None, None, None).expect("set");
    if archived {
        conn.execute("UPDATE frames_set SET is_archived = 1 WHERE id = ?1", [set]).expect("archive");
    }
    let night = db::create_imaging_night(conn, set, "2024-10-05T00:00:00Z", "2024-10-05T12:00:00Z").expect("night");
    let session = db::create_session(conn, night, "Cam", frames.len() as i32, None).expect("session");
    db::insert_session_members(conn, session, frames).expect("members");
    set
}

fn black_hole(conn: &Connection, file_id: i64) {
    conn.execute(
        "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
        [file_id],
    )
    .expect("black hole");
}

fn all_frames(conn: &Connection, scope: &Scope) -> Vec<ScopedFrame> {
    let resolved = resolve_scope(conn, scope).expect("resolve");
    fetch_scoped_frames(conn, &resolved.filter.expect("a filter")).expect("fetch")
}

fn page_of(conn: &Connection, set: i64) -> (usize, f64) {
    let (s, n) = db::get_frames_sets_by_project(conn, 0)
        .unwrap()
        .into_iter()
        .find(|(s, _)| s.id == Some(set))
        .expect("set on the page");
    (n, s.total_exp_time.unwrap_or(0.0))
}

static STATEMENTS: AtomicUsize = AtomicUsize::new(0);
fn count_statement(_: rusqlite::trace::TraceEvent<'_>) {
    STATEMENTS.fetch_add(1, Ordering::SeqCst);
}

fn fetch_count(rows: usize) -> usize {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\night" } else { "/data/night" };
    let tx = conn.unchecked_transaction().unwrap();
    let mut ids = Vec::new();
    for i in 0..rows {
        ids.push(light(&conn, dir, &format!("l{i}.fits"), "H").1);
    }
    set_of(&conn, "IC 5146", false, None, &ids);
    tx.commit().unwrap();
    let resolved = resolve_scope(&conn, &Scope::default()).unwrap();
    let filter = resolved.filter.unwrap();
    conn.trace_v2(rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT, Some(count_statement));
    STATEMENTS.store(0, Ordering::SeqCst);
    let got = fetch_scoped_frames(&conn, &filter).unwrap();
    let n = STATEMENTS.load(Ordering::SeqCst);
    conn.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
    assert_eq!(got.len(), rows);
    assert!(got.iter().all(|f| f.frame_sets.len() == 1 && f.frame_sets[0].name.as_deref() == Some("IC 5146") && f.frame_sets[0].tab == FrameSetTab::Stage));
    n
}

#[test]
fn fetch_scoped_frames_is_three_statements() {
    let small = fetch_count(20);
    let large = fetch_count(2_000);
    assert!(small <= 3, "20 rows took {small} statements");
    assert_eq!(small, large, "the statement count does not grow with the rows");
}

#[test]
fn fetch_gives_folder_night_state_sets_and_root() {
    let conn = catalog();
    let sep = std::path::MAIN_SEPARATOR;
    let root = if cfg!(windows) { "C:\\data".to_string() } else { "/data".to_string() };
    conn.execute("INSERT INTO scan_roots (path, kind) VALUES (?1, 'normal')", [&root]).unwrap();
    let dir = format!("{root}{sep}night1");
    let (f_cat, fr_cat) = light(&conn, &dir, "a.fits", "H");
    let (f_arch, fr_arch) = light(&conn, &dir, "b.fits", "H");
    let (_f_zip, fr_zip) = add(
        &conn,
        &dir,
        &Add { name: "c.fits", filter: "H", kind: ImageType::Light, exptime: 300.0, date_obs: "2024-10-05T22:00:00Z", zip: Some("z.zip") },
    );
    let (f_bh, fr_bh) = light(&conn, &dir, "d.fits", "H");
    let _ = (f_cat, fr_arch, f_arch);
    set_of(&conn, "Set A", false, None, &[fr_cat]);
    set_of(&conn, "Set B", true, None, &[fr_arch]);
    black_hole(&conn, f_bh);
    let _ = fr_zip;

    let scope = Scope { state: Some(vec![FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped, FrameState::BlackHole]), ..Scope::default() };
    let got = all_frames(&conn, &scope);
    let by_name: BTreeMap<String, &ScopedFrame> = got.iter().map(|f| (f.file.file.filename.clone(), f)).collect();
    assert_eq!(by_name["a.fits"].state, FrameState::Catalog);
    assert_eq!(by_name["b.fits"].state, FrameState::SoftArchived);
    assert_eq!(by_name["c.fits"].state, FrameState::Zipped);
    assert_eq!(by_name["d.fits"].state, FrameState::BlackHole);
    let a_sets = &by_name["a.fits"].frame_sets;
    assert_eq!((a_sets.len(), a_sets[0].name.as_deref(), a_sets[0].tab), (1, Some("Set A"), FrameSetTab::Stage));
    assert!(by_name["d.fits"].frame_sets.is_empty());
    assert_eq!(by_name["a.fits"].folder, dir);
    assert_eq!(by_name["a.fits"].night.as_deref(), Some("2024-10-05"));
    assert_eq!(by_name["a.fits"].scan_root, Some((root.clone(), "normal".to_string())));
    let _ = fr_bh;
}

#[test]
fn the_state_of_a_fetched_frame_agrees_with_the_scope_partition() {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\n" } else { "/data/n" };
    let (_, a) = light(&conn, dir, "a.fits", "H");
    let (fb, b) = light(&conn, dir, "b.fits", "H");
    let (_, c) = add(
        &conn,
        dir,
        &Add { name: "c.fits", filter: "H", kind: ImageType::Light, exptime: 1.0, date_obs: "2024-10-05T22:00:00Z", zip: Some("z.zip") },
    );
    light(&conn, dir, "d.fits", "H");
    set_of(&conn, "S", true, None, &[a]);
    black_hole(&conn, fb);
    let _ = (b, c);
    for state in [FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped, FrameState::BlackHole] {
        let scope = Scope { state: Some(vec![state]), ..Scope::default() };
        let got = all_frames(&conn, &scope);
        assert_eq!(got.len(), 1, "{state:?}");
        assert_eq!(got[0].state, state);
    }
}

fn six_lights(conn: &Connection, stored: Option<f64>) -> (i64, Vec<(i64, i64)>) {
    let dir = if cfg!(windows) { "C:\\data\\n" } else { "/data/n" };
    let mut rows = Vec::new();
    for i in 0..3 {
        rows.push(light(conn, dir, &format!("h{i}.fits"), "H"));
    }
    for i in 0..3 {
        rows.push(light(conn, dir, &format!("o{i}.fits"), "O"));
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.1).collect();
    (set_of(conn, "IC 5146", false, stored, &ids), rows)
}

#[test]
fn frame_set_totals_equal_the_page() {
    let conn = catalog();
    let (set, _) = six_lights(&conn, Some(1800.0));
    let t = frame_set_totals(&conn, set).unwrap();
    let (members, total) = page_of(&conn, set);
    assert_eq!(t.frames, members);
    assert_eq!(t.frames, 6);
    assert_eq!(t.integration_s, total);
    assert_eq!(t.integration_s, 1800.0);
    assert_eq!(t.black_hole_left_out, None);
    assert_eq!(t.by_filter, vec![("H".to_string(), 3, 900.0), ("O".to_string(), 3, 900.0)]);
}

#[test]
fn black_hole_member_left_out() {
    let conn = catalog();
    let (set, rows) = six_lights(&conn, Some(1800.0));
    black_hole(&conn, rows[0].0);
    let t = frame_set_totals(&conn, set).unwrap();
    let (members, total) = page_of(&conn, set);
    assert_eq!(members, 5);
    assert_eq!(t.frames, members, "frames drop by one as member_count does");
    assert_eq!(t.integration_s, total, "the figure is the page's, whatever the stored column still says");
    assert_eq!(t.integration_s, 1500.0);
    assert_eq!(t.by_filter, vec![("H".to_string(), 2, 600.0), ("O".to_string(), 3, 900.0)]);
    assert_eq!(t.black_hole_left_out, Some((1, 300.0)), "the member the page leaves out is named");
}

#[test]
fn no_black_hole_note_when_none_is_in_the_black_hole() {
    let conn = catalog();
    let (set, _) = six_lights(&conn, None);
    assert_eq!(frame_set_totals(&conn, set).unwrap().black_hole_left_out, None);
}

#[test]
fn a_member_that_is_not_a_light_is_no_black_hole_note() {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\n" } else { "/data/n" };
    let (_, l) = light(&conn, dir, "l.fits", "H");
    let (_, d) = add(
        &conn,
        dir,
        &Add { name: "d.fits", filter: "H", kind: ImageType::Dark, exptime: 300.0, date_obs: "2024-10-05T22:00:00Z", zip: None },
    );
    let set = set_of(&conn, "IC 5146", false, None, &[l, d]);
    let t = frame_set_totals(&conn, set).unwrap();
    assert_eq!(t.frames, 2);
    assert_eq!(t.black_hole_left_out, None, "a dark with an exposure is not a Black Hole member");
}

#[test]
fn a_set_that_does_not_exist_is_said() {
    let conn = catalog();
    let e = frame_set_totals(&conn, 99).unwrap_err().to_string();
    assert!(e.contains("99"), "{e}");
}

#[test]
fn group_by_frame_set_splits_same_named_sets() {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\ldn" } else { "/data/ldn" };
    let wip_frames: Vec<i64> = (0..3).map(|i| light(&conn, dir, &format!("w{i}.fits"), "H").1).collect();
    let stage_frames: Vec<i64> = (0..2).map(|i| light(&conn, dir, &format!("s{i}.fits"), "H").1).collect();
    let wip = set_of(&conn, "LDN 1272", false, None, &wip_frames);
    conn.execute("UPDATE frames_set SET is_custom = 1 WHERE id = ?1", [wip]).unwrap();
    let stage = set_of(&conn, "LDN 1272", false, None, &stage_frames);

    let frames = all_frames(&conn, &Scope::default());
    let r = query_frames(&frames, &[GroupKey::FrameSet], None);
    assert_eq!(r.groups_total, 2, "two sets of one name are two groups");
    let wip_label = format!("'LDN 1272' (WIP, set:{wip})");
    let stage_label = format!("'LDN 1272' (Stage, set:{stage})");
    let by_label: BTreeMap<&str, &FrameGroup> = r.groups.iter().map(|g| (g.key[&GroupKey::FrameSet].as_str(), g)).collect();
    assert_eq!(by_label[wip_label.as_str()].totals.frames, 3);
    assert_eq!(by_label[wip_label.as_str()].frame_set_handles, vec![format!("set:{wip}")]);
    assert_eq!(by_label[stage_label.as_str()].totals.frames, 2);
    assert!(r.groups.iter().all(|g| g.label.contains("(WIP, set:") || g.label.contains("(Stage, set:")), "{:?}", r.groups);

    // Not grouped by set: no handles in the group's data.
    let plain = query_frames(&frames, &[GroupKey::Filter], None);
    assert!(plain.groups.iter().all(|g| g.frame_set_handles.is_empty()));
}

#[test]
fn frame_set_facts_name_tab_cameras_telescopes_and_centre() {
    let conn = catalog();
    let dir = if cfg!(windows) { "C:\\data\\f" } else { "/data/f" };
    let (_, a) = light(&conn, dir, "a.fits", "H");
    let (_, b) = light(&conn, dir, "b.fits", "H");
    conn.execute("UPDATE frames SET instrume = 'CamB', telescop = 'Tele' WHERE id = ?1", [a]).unwrap();
    conn.execute("UPDATE frames SET instrume = 'CamA', telescop = '' WHERE id = ?1", [b]).unwrap();
    let set = set_of(&conn, "S", true, None, &[a, b]);
    conn.execute("UPDATE frames_set SET objctra = '05 40 00', objctdec = '-02 27 00' WHERE id = ?1", [set]).unwrap();
    let facts = frame_set_facts(&conn, set).unwrap();
    assert_eq!(facts.tab, FrameSetTab::Archive);
    assert_eq!(facts.cameras, vec!["CamA".to_string(), "CamB".to_string()]);
    assert_eq!(facts.telescopes, vec!["Tele".to_string()]);
    assert_eq!(facts.centre, Some((Some("05 40 00".to_string()), Some("-02 27 00".to_string()))));
    let bare = set_of(&conn, "T", false, None, &[]);
    assert_eq!(frame_set_facts(&conn, bare).unwrap().centre, None);
}
