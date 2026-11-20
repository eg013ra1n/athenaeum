//! The Black Hole plan against a real catalog and real files (MCP plan Task 14). The fixtures are `pub(crate)`: the
//! tool tests (`mcp/tools/plans.rs`) build their catalogs with them. Every test names its files with letters, digits,
//! `_` and `-` only, and compares paths as the strings it built them from.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::Value;

use super::*;
use crate::mcp::plans::{GroupSelection, PlanAnswer, NOT_REACHABLE};
use crate::mcp::{mcp_server_name, HostKind, McpIdentity};
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::services::ServiceContext;
use crate::{db, events::ProgressEmitter};

// ---- the rig ----

#[derive(Default)]
pub(crate) struct Capture(Mutex<Vec<(String, Value)>>);

impl ProgressEmitter for Capture {
    fn emit_json(&self, event_name: &str, payload: Value) {
        self.0.lock().unwrap().push((event_name.to_string(), payload));
    }
}

impl Capture {
    fn named(&self, name: &str) -> Vec<Value> {
        self.0.lock().unwrap().iter().filter(|(n, _)| n == name).map(|(_, p)| p.clone()).collect()
    }
}

pub(crate) struct Rig {
    pub host: Arc<McpHost>,
    pub events: Arc<Capture>,
    pub ctx: Arc<ServiceContext>,
    pub base: PathBuf,
    // Dropped last: the host is shut down in `Drop` before the directory goes.
    _tmp: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A host over a fresh catalog with two scan roots: `nas` (duplicates are looked for) and `lib` (the calibration
/// library).
pub(crate) fn rig() -> Rig {
    let (tmp, base) = crate::test_support::canonical_tempdir();
    let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
    let events = Arc::new(Capture::default());
    let host = McpHost::new(
        Arc::clone(&ctx),
        McpIdentity { server_name: mcp_server_name(), version: "9.9.9-test", kind: HostKind::Desktop, web_api_key_set: false },
        Arc::clone(&events) as Arc<dyn ProgressEmitter>,
        Arc::new(|| {}),
        Arc::new(|| true),
    );
    {
        let database = ctx.db.get().expect("db");
        let conn = database.conn();
        std::fs::create_dir_all(base.join("nas")).expect("nas dir");
        std::fs::create_dir_all(base.join("lib")).expect("lib dir");
        db::upsert_scan_root(&conn, &text(&base.join("nas")), "normal").expect("nas root");
        db::upsert_scan_root(&conn, &text(&base.join("lib")), "calibration_library").expect("lib root");
    }
    host.on_catalog_open().expect("catalog open");
    Rig { host, events, ctx, base, _tmp: tmp }
}

/// One catalog file with a real file behind it.
#[derive(Clone)]
pub(crate) struct Fx {
    pub file_id: i64,
    pub frame_id: i64,
    pub path: PathBuf,
    pub name: String,
}

impl Fx {
    pub fn item(&self) -> String {
        format!("item:{}", self.file_id)
    }

    pub fn dir(&self) -> PathBuf {
        self.path.parent().expect("a parent").to_path_buf()
    }

    pub fn path_text(&self) -> String {
        text(&self.path)
    }
}

fn file_row(path: &str, name: &str, size: usize, zip: Option<&str>) -> File {
    File {
        id: None,
        path: path.to_string(),
        filename: name.to_string(),
        size: size as i64,
        modified_at: Utc::now(),
        format: FileFormat::FITS,
        created_at: Utc::now(),
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: zip.map(str::to_string),
        archive_path_in_zip: zip.map(|_| name.to_string()),
        uuid: None,
        updated_at: None,
    }
}

fn header_copy_at(rig: &Rig, path: PathBuf, zip: Option<&Path>, name: &str, bytes: &[u8]) -> Fx {
    if zip.is_none() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
        std::fs::write(&path, bytes).expect("write file");
    }
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let zip_text = zip.map(text);
    let file_id = db::insert_file(&conn, &file_row(&text(&path), name, bytes.len(), zip_text.as_deref())).expect("file row");
    // `insert_file` does not store the zip: a zip-resident row is marked the way the scanner marks it.
    if let Some(z) = &zip_text {
        conn.execute("UPDATE files SET archive_zip_path = ?1 WHERE id = ?2", rusqlite::params![z, file_id]).expect("zip");
    }
    let frame_id = db::insert_frame(
        &conn,
        &Frame {
            file_id,
            object: Some("M31".to_string()),
            instrume: Some("Cam".to_string()),
            filter: Some("L".to_string()),
            imagetyp: Some(ImageType::Light),
            exptime: Some(300.0),
            ..Default::default()
        },
    )
    .expect("frame row");
    let header = format!("HDR-{name}");
    conn.execute(
        "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
        rusqlite::params![file_id, header, crate::fingerprint::compute_header_fingerprint(&header)],
    )
    .expect("header row");
    Fx { file_id, frame_id, path, name: name.to_string() }
}

/// A raw light that is a header-key duplicate of every other file of this `name` and size: a real file of `bytes` in
/// `nas/<dir>`.
pub(crate) fn header_copy(rig: &Rig, dir: &str, name: &str, bytes: &[u8]) -> Fx {
    header_copy_at(rig, rig.base.join("nas").join(dir).join(name), None, name, bytes)
}

/// The handle `find_duplicates` gives the header group of files named `name`.
pub(crate) fn header_handle(name: &str) -> String {
    format!("dup:header:{}|{name}", crate::fingerprint::compute_header_fingerprint(&format!("HDR-{name}")))
}

/// A master of the calibration library. Masters of one `exptime` are copies of one master.
pub(crate) fn master_copy(rig: &Rig, name: &str, exptime: f64, bytes: &[u8]) -> Fx {
    let path = rig.base.join("lib").join(name);
    std::fs::write(&path, bytes).expect("write master");
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let file_id = db::insert_file(&conn, &file_row(&text(&path), name, bytes.len(), None)).expect("file row");
    let frame_id = db::insert_frame(
        &conn,
        &Frame {
            file_id,
            instrume: Some("Cam".to_string()),
            imagetyp: Some(ImageType::MasterDark),
            is_master: true,
            exptime: Some(exptime),
            ccd_temp: Some(-10.0),
            gain: Some(100.0),
            offset: Some(50.0),
            binning: Some("1x1".to_string()),
            naxis1: Some(6248),
            naxis2: Some(4176),
            date_obs: Some(
                chrono::DateTime::parse_from_rfc3339("2024-10-05T22:00:00Z").expect("date").with_timezone(&Utc),
            ),
            ..Default::default()
        },
    )
    .expect("frame row");
    Fx { file_id, frame_id, path, name: name.to_string() }
}

pub(crate) fn rebuilt_handle(first: &Fx) -> String {
    format!("rebuilt:{}", first.file_id)
}

/// The master set holding `fx`'s frame (made when there is none).
fn master_set(rig: &Rig, fx: &Fx) -> i64 {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let found: Option<i64> = conn
        .query_row("SELECT set_id FROM calibration_set_frames WHERE frame_id = ?1", [fx.frame_id], |r| r.get(0))
        .ok();
    if let Some(set) = found {
        return set;
    }
    conn.execute(
        "INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume) VALUES ('MasterDark', '2024-10-05', 1, 'Cam')",
        [],
    )
    .expect("set");
    let set = conn.last_insert_rowid();
    conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", [set, fx.frame_id])
        .expect("member");
    set
}

/// Points `lights` light frames' calibration links at `fx`.
pub(crate) fn link_master(rig: &Rig, fx: &Fx, lights: usize) {
    let set = master_set(rig, fx);
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    for n in 0..lights {
        let name = format!("light-for-{}-{n}.fits", fx.file_id);
        let file_id = db::insert_file(&conn, &file_row(&text(&rig.base.join("nas").join(&name)), &name, 1, None)).expect("light");
        let light = db::insert_frame(
            &conn,
            &Frame { file_id, imagetyp: Some(ImageType::Light), ..Default::default() },
        )
        .expect("light frame");
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
             VALUES (?1, 'frame', ?2, 'Dark', '2024-10-05T00:00:00Z')",
            [light, set],
        )
        .expect("link");
    }
}

/// A raw set was superseded by `fx`'s master set.
fn supersede(rig: &Rig, fx: &Fx) {
    let set = master_set(rig, fx);
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    conn.execute(
        "INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume, superseded_by_set_id) \
         VALUES ('Dark', '2024-10-05', 0, 'Cam', ?1)",
        [set],
    )
    .expect("raw set");
}

// ---- driving a plan ----

fn args(source: BlackHoleSource, groups: &[String], folder: Option<&Path>, files: &[&Fx]) -> PlanBlackHoleArgs {
    PlanBlackHoleArgs {
        source,
        groups: groups.to_vec(),
        keep_in_folder: folder.map(text),
        keep_files: files.iter().map(|f| f.path_text()).collect(),
    }
}

/// Plans with the real byte comparison; the plan is waiting when this returns.
pub(crate) fn plan(rig: &Rig, source: BlackHoleSource, groups: &[String], folder: Option<&Path>, files: &[&Fx]) -> McpPlanView {
    plan_with(rig, source, groups, folder, files, real_verify())
}

fn plan_with(
    rig: &Rig,
    source: BlackHoleSource,
    groups: &[String],
    folder: Option<&Path>,
    files: &[&Fx],
    verify: VerifyFn,
) -> McpPlanView {
    let answer = prepare(&rig.host, args(source, groups, folder, files), verify).expect("the plan is prepared");
    let id = answer.data["planId"].as_str().expect("a plan id").to_string();
    let view = rig.host.plans.view(Some(&id)).expect("the plan");
    assert_eq!(view.state, PlanState::Waiting, "{}", answer.text);
    view
}

/// The selection a user gets by pressing Apply on the proposal as it stands.
fn proposal(view: &McpPlanView) -> PlanSelection {
    PlanSelection {
        groups: view
            .groups
            .iter()
            .map(|g| GroupSelection {
                group_id: g.id.clone(),
                chosen: g.chosen.clone(),
                included: g.items.iter().filter(|i| i.selected).map(|i| i.id.clone()).collect(),
            })
            .collect(),
    }
}

/// Presses Apply and waits (bounded) until the apply thread has ended the plan.
fn apply(rig: &Rig, id: &str, selection: PlanSelection) -> McpPlanView {
    rig.host.plans.answer(&rig.host, id, PlanAnswer::Apply { selection }).expect("the apply starts");
    let started = Instant::now();
    loop {
        let v = rig.host.plans.view(Some(id)).expect("plan");
        if v.state != PlanState::Applying {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "the plan is stranded: {:?}", v.state);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn item<'a>(view: &'a McpPlanView, fx: &Fx) -> &'a PlanItem {
    view.groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find(|i| i.id == fx.item())
        .unwrap_or_else(|| panic!("{} is not in the plan", fx.name))
}

fn black_hole_rows(rig: &Rig) -> Vec<(i64, String)> {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let mut stmt = conn.prepare("SELECT file_id, from_where FROM black_hole ORDER BY file_id").expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("rows").collect::<Result<_, _>>().expect("row")
}

fn reasons(lines: &[ReportLine]) -> HashMap<String, String> {
    lines.iter().map(|l| (l.item_id.clone(), l.reason.clone())).collect()
}

/// A comparison that counts its calls and answers with the real one.
fn counting() -> (VerifyFn, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let real = real_verify();
    let seen = Arc::clone(&calls);
    (
        Arc::new(move |host: &McpHost, a: i64, b: i64| {
            seen.fetch_add(1, Ordering::SeqCst);
            real(host, a, b)
        }),
        calls,
    )
}

// ---- the keeper ----

#[test]
fn keep_in_folder_picks_the_keeper_and_selects_the_rest() {
    let rig = rig();
    let a = header_copy(&rig, "keep", "t1.fits", b"AAAA");
    let b = header_copy(&rig, "other-b", "t1.fits", b"AAAA");
    let c = header_copy(&rig, "other-c", "t1.fits", b"AAAA");
    // A folder whose name merely starts like the kept one is not under it.
    let d = header_copy(&rig, "keep2", "t1.fits", b"AAAA");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t1.fits")], Some(&a.dir()), &[]);

    assert_eq!(view.kind, PlanKind::BlackHole);
    assert_eq!(view.groups.len(), 1);
    let g = &view.groups[0];
    assert_eq!(g.choose_one.as_deref(), Some("Copy that stays"));
    assert_eq!(g.chosen.as_deref(), Some(a.item().as_str()));
    let keeper = item(&view, &a);
    assert_eq!(keeper.locked.as_deref(), Some("stays"));
    assert!(!keeper.selected && keeper.candidate);
    for other in [&b, &c, &d] {
        let i = item(&view, other);
        assert!(i.selected && i.locked.is_none(), "{}: {i:?}", other.name);
        assert_eq!(i.action, "Move to the Black Hole");
        assert!(i.evidence.as_deref().unwrap_or("").contains("byte-identical"), "{i:?}");
        assert_eq!(i.values["IMAGETYP"], "Light");
        assert_eq!(i.values["OBJECT"], "M31");
        assert_eq!(i.values["EXPTIME"], "300");
        assert_eq!(i.size_bytes, Some(4));
    }
    let keys: Vec<&str> = view.columns.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["IMAGETYP", "OBJECT", "FILTER", "EXPTIME", "DATE-OBS", "INSTRUME", "size", "modified"]);
    assert!(view.groups[0].note.is_none(), "{:?}", view.groups[0].note);
}

#[test]
fn no_instruction_selects_nothing() {
    let rig = rig();
    let a = header_copy(&rig, "n-a", "t2.fits", b"AAAA");
    let b = header_copy(&rig, "n-b", "t2.fits", b"AAAA");
    let c = header_copy(&rig, "n-c", "t2.fits", b"AAAA");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t2.fits")], None, &[]);
    let g = &view.groups[0];
    assert_eq!(g.chosen, None);
    assert!(g.items.iter().all(|i| !i.selected && i.locked.is_none()), "{:?}", g.items);
    assert!(g.note.as_deref().unwrap_or("").contains("no instruction picks the copy that stays here"), "{:?}", g.note);
    for fx in [&a, &b, &c] {
        assert!(item(&view, fx).candidate);
    }

    // Two copies under the named folder: no single copy is picked.
    let a = header_copy(&rig, "two/inside", "t3.fits", b"AAAA");
    header_copy(&rig, "two/inside/deeper", "t3.fits", b"AAAA");
    header_copy(&rig, "elsewhere", "t3.fits", b"AAAA");
    let folder = a.dir();
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t3.fits")], Some(&folder), &[]);
    let note = view.groups[0].note.clone().unwrap_or_default();
    assert!(
        note.contains(&format!("no instruction picks the copy that stays here (2 copies under {})", text(&folder))),
        "{note}"
    );
    assert_eq!(view.groups[0].chosen, None);
    assert!(view.groups[0].items.iter().all(|i| !i.selected));
}

#[test]
fn last_copy_always_stays() {
    let rig = rig();
    let a = header_copy(&rig, "l-a", "t4.fits", b"AAAA");
    let b = header_copy(&rig, "l-b", "t4.fits", b"AAAA");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t4.fits")], None, &[]);
    let gid = view.groups[0].id.clone();

    // No copy chosen, one included.
    let sel = PlanSelection {
        groups: vec![GroupSelection { group_id: gid.clone(), chosen: None, included: vec![b.item()] }],
    };
    match rig.host.plans.answer(&rig.host, &view.plan_id, PlanAnswer::Apply { selection: sel }) {
        Err(ApiError::Invalid(m)) => assert!(m.contains("pick the copy that stays"), "{m}"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    // The chosen copy cannot also be moved.
    let sel = PlanSelection {
        groups: vec![GroupSelection { group_id: gid, chosen: Some(b.item()), included: vec![a.item(), b.item()] }],
    };
    match rig.host.plans.answer(&rig.host, &view.plan_id, PlanAnswer::Apply { selection: sel }) {
        Err(ApiError::Invalid(m)) => assert!(m.contains("cannot also be moved"), "{m}"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert_eq!(rig.host.plans.view(Some(&view.plan_id)).expect("plan").state, PlanState::Waiting);
    assert!(black_hole_rows(&rig).is_empty());
}

// ---- locks ----

#[test]
fn linked_master_is_locked_with_reason() {
    let rig = rig();
    let m1 = master_copy(&rig, "MasterDark-300s.xisf", 300.0, b"M1");
    let m2 = master_copy(&rig, "MasterDark-300s-2.xisf", 300.0, b"M2");
    let m3 = master_copy(&rig, "MasterDark-300s-3.xisf", 300.0, b"M3");
    link_master(&rig, &m1, 2);
    let view = plan(&rig, BlackHoleSource::RebuiltMasters, &[rebuilt_handle(&m1)], None, &[&m2]);

    assert_eq!(view.groups[0].chosen.as_deref(), Some(m2.item().as_str()));
    let linked = item(&view, &m1);
    assert_eq!(linked.locked.as_deref(), Some("calibration links point at it; it stays in v1"));
    assert!(!linked.selected);
    assert!(item(&view, &m3).selected && item(&view, &m3).locked.is_none());
    assert_eq!(item(&view, &m2).locked.as_deref(), Some("stays"));
    let note = view.groups[0].note.clone().unwrap_or_default();
    assert!(note.contains("MasterDark-300s.xisf") && note.contains("2 frames linked"), "{note}");

    // A locked copy is refused by the window's apply, whatever the selection says.
    let sel = PlanSelection {
        groups: vec![GroupSelection {
            group_id: view.groups[0].id.clone(),
            chosen: Some(m2.item()),
            included: vec![m1.item()],
        }],
    };
    match rig.host.plans.answer(&rig.host, &view.plan_id, PlanAnswer::Apply { selection: sel }) {
        Err(ApiError::Invalid(m)) => assert!(m.contains("locked"), "{m}"),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn superseding_copy_is_locked() {
    let rig = rig();
    // Group one: a copy that superseded raw frames at planning.
    let m1 = master_copy(&rig, "MasterDark-sup-1.xisf", 300.0, b"M1");
    let m2 = master_copy(&rig, "MasterDark-sup-2.xisf", 300.0, b"M2");
    let m3 = master_copy(&rig, "MasterDark-sup-3.xisf", 300.0, b"M3");
    supersede(&rig, &m1);
    // Group two: a copy that becomes so after planning.
    let m4 = master_copy(&rig, "MasterDark-60-1.xisf", 60.0, b"M4");
    let m5 = master_copy(&rig, "MasterDark-60-2.xisf", 60.0, b"M5");
    let m6 = master_copy(&rig, "MasterDark-60-3.xisf", 60.0, b"M6");
    let view = plan(
        &rig,
        BlackHoleSource::RebuiltMasters,
        &[rebuilt_handle(&m1), rebuilt_handle(&m4)],
        None,
        &[&m2, &m4],
    );
    let reason = "raw frames were superseded by it; it stays in v1";
    assert_eq!(item(&view, &m1).locked.as_deref(), Some(reason));
    assert!(item(&view, &m3).selected);
    assert!(item(&view, &m5).selected && item(&view, &m6).selected);

    supersede(&rig, &m5);
    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert_eq!(reasons(&report.skipped)[&m5.item()], reason);
    let moved: Vec<i64> = black_hole_rows(&rig).into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, vec![m3.file_id, m6.file_id]);
}

#[test]
fn zip_copy_is_locked() {
    let rig = rig();
    let a = header_copy(&rig, "z-a", "t5.fits", b"AAAA");
    let zip = rig.base.join("nas").join("z-pack.zip");
    std::fs::write(&zip, b"not really a zip").expect("zip file");
    let in_zip = header_copy_at(&rig, zip.join("t5.fits"), Some(&zip), "t5.fits", b"AAAA");
    let c = header_copy(&rig, "z-c", "t5.fits", b"AAAA");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t5.fits")], Some(&a.dir()), &[]);
    let zipped = item(&view, &in_zip);
    assert_eq!(zipped.locked.as_deref(), Some("inside a zip; a zip moves to the Black Hole only as a whole"));
    assert!(!zipped.selected);
    assert!(item(&view, &c).selected);
}

#[test]
fn an_unreachable_copy_is_locked() {
    let rig = rig();
    let a = header_copy(&rig, "u-a", "t6.fits", b"AAAA");
    let b = header_copy(&rig, "u-b", "t6.fits", b"AAAA");
    let c = header_copy(&rig, "u-c", "t6.fits", b"AAAA");
    std::fs::remove_file(&c.path).expect("remove");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t6.fits")], Some(&a.dir()), &[]);
    let gone = item(&view, &c);
    assert_eq!(gone.locked.as_deref(), Some(NOT_REACHABLE));
    assert!(!gone.selected && !gone.candidate);
    assert!(item(&view, &b).selected);
}

// ---- the byte checks ----

#[test]
fn header_pair_that_differs_in_bytes_is_dropped() {
    let rig = rig();
    let a = header_copy(&rig, "d-a", "t7.fits", b"AAAA");
    let b = header_copy(&rig, "d-b", "t7.fits", b"AAAA");
    let c = header_copy(&rig, "d-c", "t7.fits", b"AABA");
    let view = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t7.fits")], Some(&a.dir()), &[]);
    let dropped = item(&view, &c);
    assert_eq!(dropped.locked.as_deref(), Some("differs in bytes from the copy that stays"));
    assert!(!dropped.selected);
    assert!(item(&view, &b).selected);
    let note = view.groups[0].note.clone().unwrap_or_default();
    assert!(note.contains("different bytes from the copy that stays"), "{note}");
}

#[test]
fn header_pair_rechecked_after_keeper_change() {
    let rig = rig();
    let a = header_copy(&rig, "r-a", "t8.fits", b"AAAA");
    let b = header_copy(&rig, "r-b", "t8.fits", b"AAAA");
    let c = header_copy(&rig, "r-c", "t8.fits", b"AABA");
    let d = header_copy(&rig, "r-d", "t8.fits", b"AAAA");
    let (verify, calls) = counting();
    // No instruction: nothing is byte-checked at planning.
    let view = plan_with(&rig, BlackHoleSource::Duplicates, &[header_handle("t8.fits")], None, &[], verify);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // The user picks `a` in the window and ticks the other three; `c` differs from it.
    let sel = PlanSelection {
        groups: vec![GroupSelection {
            group_id: view.groups[0].id.clone(),
            chosen: Some(a.item()),
            included: vec![b.item(), c.item(), d.item()],
        }],
    };
    let done = apply(&rig, &view.plan_id, sel);
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert_eq!(reasons(&report.skipped)[&c.item()], "differs in bytes from the copy that stays");
    assert_eq!(report.applied.len(), 2);
    let moved: Vec<i64> = black_hole_rows(&rig).into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, vec![b.file_id, d.file_id], "the pair that differs stays in place");
    assert!(c.path.exists(), "the file that differs is untouched");
}

#[test]
fn header_pair_not_rechecked_when_the_keeper_is_unchanged() {
    let rig = rig();
    let a = header_copy(&rig, "s-a", "t9.fits", b"AAAA");
    let b = header_copy(&rig, "s-b", "t9.fits", b"AAAA");
    let c = header_copy(&rig, "s-c", "t9.fits", b"AAAA");
    let (verify, calls) = counting();
    let view = plan_with(&rig, BlackHoleSource::Duplicates, &[header_handle("t9.fits")], Some(&a.dir()), &[], verify);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one comparison per copy that is not the keeper");
    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.report.expect("report").applied.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the keeper is the one the plan checked against: nothing is read again");
    let moved: Vec<i64> = black_hole_rows(&rig).into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, vec![b.file_id, c.file_id]);
}

#[test]
fn verification_is_cancellable() {
    let rig = rig();
    let a = header_copy(&rig, "c-a", "t10.fits", b"AAAA");
    for dir in ["c-b", "c-c", "c-d"] {
        header_copy(&rig, dir, "t10.fits", b"AAAA");
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let verify: VerifyFn = {
        let calls = Arc::clone(&calls);
        let host = Arc::clone(&rig.host);
        Arc::new(move |_h: &McpHost, _a: i64, _b: i64| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // The user cancels the plan in the window while it is being checked.
                let id = host.plans.view(None).expect("the plan being prepared").plan_id;
                host.plans.answer(&host, &id, PlanAnswer::Cancel).expect("cancel");
            }
            Ok(true)
        })
    };
    let answer = prepare(
        &rig.host,
        args(BlackHoleSource::Duplicates, &[header_handle("t10.fits")], Some(&a.dir()), &[]),
        verify,
    )
    .expect("an answer in words");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no comparison after the cancel: three were due");
    assert!(answer.text.contains("cancelled") && answer.text.contains("nothing changed"), "{}", answer.text);
    let view = rig.host.plans.view(None).expect("the plan");
    assert_eq!(view.state, PlanState::Cancelled);
    assert!(view.groups.is_empty(), "a cancelled plan publishes nothing");
}

// ---- the apply ----

#[test]
fn keeper_gone_skips_the_group() {
    let rig = rig();
    let keep = rig.base.join("nas").join("keep");
    let mut keepers = Vec::new();
    let mut others = Vec::new();
    for n in 1..=3 {
        let name = format!("g{n}.fits");
        keepers.push(header_copy(&rig, "keep", &name, b"AAAA"));
        others.push(header_copy(&rig, &format!("away-{n}"), &name, b"AAAA"));
    }
    let handles: Vec<String> = (1..=3).map(|n| header_handle(&format!("g{n}.fits"))).collect();
    let view = plan(&rig, BlackHoleSource::Duplicates, &handles, Some(&keep), &[]);
    assert_eq!(view.groups.len(), 3);
    for o in &others {
        assert!(item(&view, o).selected, "{}", o.name);
    }
    // After planning: the first keeper goes to the Black Hole, the second is deleted from disk.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute(
            "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
            [keepers[0].file_id],
        )
        .expect("black hole");
    }
    std::fs::remove_file(&keepers[1].path).expect("delete the keeper");

    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    let skipped = reasons(&report.skipped);
    assert!(skipped[&others[0].item()].contains("Black Hole"), "{skipped:?}");
    assert!(skipped[&others[1].item()].starts_with("the copy that stays changed or is gone"), "{skipped:?}");
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].item_id, others[2].item());
    assert_eq!(
        black_hole_rows(&rig),
        vec![(keepers[0].file_id, "test".to_string()), (others[2].file_id, "assistant".to_string())],
        "nothing of the first two groups moved"
    );
}

#[test]
fn master_linked_meanwhile_is_skipped() {
    let rig = rig();
    let m1 = master_copy(&rig, "MasterDark-lm-1.xisf", 300.0, b"M1");
    let m2 = master_copy(&rig, "MasterDark-lm-2.xisf", 300.0, b"M2");
    let m3 = master_copy(&rig, "MasterDark-lm-3.xisf", 300.0, b"M3");
    let view = plan(&rig, BlackHoleSource::RebuiltMasters, &[rebuilt_handle(&m1)], None, &[&m1]);
    assert!(item(&view, &m2).selected && item(&view, &m3).selected);
    link_master(&rig, &m2, 1);

    let done = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert_eq!(reasons(&report.skipped)[&m2.item()], "calibration links point at it; it stays in v1");
    assert_eq!(report.skipped[0].file_name, "MasterDark-lm-2.xisf");
    let moved: Vec<i64> = black_hole_rows(&rig).into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, vec![m3.file_id], "the rest of its group moved");
}

#[test]
fn apply_moves_exactly_the_ticked_files() {
    let rig = rig();
    let keep = rig.base.join("nas").join("keep");
    let mut ticked = Vec::new();
    let mut unticked = None;
    for n in 1..=3 {
        let name = format!("m{n}.fits");
        header_copy(&rig, "keep", &name, b"AAAA");
        let x = header_copy(&rig, &format!("x-{n}"), &name, b"AAAA");
        let y = header_copy(&rig, &format!("y-{n}"), &name, b"AAAA");
        if n == 3 {
            unticked = Some(x);
            ticked.push(y);
        } else {
            ticked.push(x);
            ticked.push(y);
        }
    }
    let handles: Vec<String> = (1..=3).map(|n| header_handle(&format!("m{n}.fits"))).collect();
    let view = plan(&rig, BlackHoleSource::Duplicates, &handles, Some(&keep), &[]);
    let unticked = unticked.expect("one unticked copy");
    let mut sel = proposal(&view);
    sel.groups[2].included.retain(|id| id != &unticked.item());

    let done = apply(&rig, &view.plan_id, sel);
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert_eq!(report.applied.len(), 5);
    assert!(report.skipped.is_empty() && report.failed.is_empty(), "{report:?}");
    let mut want: Vec<i64> = ticked.iter().map(|f| f.file_id).collect();
    want.sort_unstable();
    let rows = black_hole_rows(&rig);
    assert_eq!(rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(), want, "exactly the ticked files");
    assert!(rows.iter().all(|(_, from)| from == "assistant"));
    assert!(!rows.iter().any(|(id, _)| *id == unticked.file_id));
    // One event the pages refresh on, carrying the count moved; none of the per-file progress.
    let changed = rig.events.named("blackhole-changed");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0]["count"], 5);
    assert!(rig.events.named("bulk-move-to-black-hole-progress").is_empty());
    // What the user chose is what the plan shows afterwards.
    let shown = rig.host.plans.view(Some(&view.plan_id)).expect("plan");
    assert!(!item(&shown, &unticked).selected);
}

#[test]
fn a_group_the_catalog_no_longer_holds_or_of_the_other_source_is_refused() {
    let rig = rig();
    let a = header_copy(&rig, "q-a", "t11.fits", b"AAAA");
    header_copy(&rig, "q-b", "t11.fits", b"AAAA");
    let first = plan(&rig, BlackHoleSource::Duplicates, &[header_handle("t11.fits")], Some(&a.dir()), &[]);

    let refused = |groups: &[String], source| {
        match prepare(&rig.host, args(source, groups, None, &[]), real_verify()) {
            Err(ApiError::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    };
    let m = refused(&["dup:header:nope|none.fits".to_string()], BlackHoleSource::Duplicates);
    assert!(m.contains("find_duplicates"), "{m}");
    let m = refused(&[header_handle("t11.fits")], BlackHoleSource::RebuiltMasters);
    assert!(m.contains("find_rebuilt_masters"), "{m}");
    let m = refused(&[], BlackHoleSource::Duplicates);
    assert!(m.contains("groups"), "{m}");
    // A refusal never replaces the plan the user is looking at.
    assert_eq!(rig.host.plans.view(Some(&first.plan_id)).expect("plan").state, PlanState::Waiting);
}

#[test]
fn the_handle_is_the_one_the_duplicates_page_gives() {
    let rig = rig();
    header_copy(&rig, "h-a", "t12.fits", b"AAAA");
    header_copy(&rig, "h-b", "t12.fits", b"AAAA");
    let groups = crate::api::files::get_duplicates(&rig.ctx).expect("groups");
    let handles: Vec<String> = groups
        .iter()
        .filter_map(|g| g.key.map(|k| format!("dup:{}:{}", k.hash_type(), g.content_hash)))
        .collect();
    assert_eq!(handles, vec![header_handle("t12.fits")]);
}
