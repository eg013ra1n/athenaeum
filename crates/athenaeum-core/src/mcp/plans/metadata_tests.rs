//! The metadata plan against a real catalog and real files (MCP plan Task 17). Every test names its files with letters,
//! digits, `_` and `-` only, and reads the plan and the catalog back as data.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;

use super::*;
use crate::mcp::plans::black_hole::tests::{rig, Fx, Rig};
use crate::mcp::plans::{GroupSelection, PlanAnswer, NOT_REACHABLE};
use crate::models::{File, FileFormat, Frame};
use crate::{db, events::NullEmitter};

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// ---- fixtures ----

/// A catalog file with a real file behind it, in `nas/<dir>`; `set` fills the frame.
fn put(rig: &Rig, dir: &str, name: &str, set: impl FnOnce(&mut Frame)) -> Fx {
    let path = rig.base.join("nas").join(dir).join(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    std::fs::write(&path, b"FITS").expect("write file");
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let file_id = db::insert_file(
        &conn,
        &File {
            id: None,
            path: text(&path),
            filename: name.to_string(),
            size: 4,
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
    )
    .expect("file row");
    let mut frame = Frame { file_id, ..Default::default() };
    set(&mut frame);
    let frame_id = db::insert_frame(&conn, &frame).expect("frame row");
    Fx { file_id, frame_id, path, name: name.to_string() }
}

fn light(rig: &Rig, dir: &str, name: &str, object: Option<&str>) -> Fx {
    put(rig, dir, name, |f| {
        f.imagetyp = Some(ImageType::Light);
        f.instrume = Some("Cam".to_string());
        f.object = object.map(str::to_string);
    })
}

fn dir_of(rig: &Rig, dir: &str) -> PathBuf {
    rig.base.join("nas").join(dir)
}

fn item_id_of(fx: &Fx, field: FillField) -> String {
    item_id(fx.frame_id, field)
}

fn args(rig: &Rig, dir: &str, fields: &[FillField], statement: Option<(FillField, &str)>) -> PlanMetadataArgs {
    PlanMetadataArgs {
        scope: Scope { folder: Some(text(&dir_of(rig, dir))), ..Default::default() },
        fields: fields.to_vec(),
        statement: statement.map(|(field, value)| UserStatement { field, value: value.to_string() }),
    }
}

/// Prepares the plan; it waits for the user when this returns.
fn plan_answer(rig: &Rig, a: PlanMetadataArgs) -> (McpPlanView, Answer) {
    let answer = prepare(&rig.host, a).expect("the plan is prepared");
    let id = answer.data["planId"].as_str().expect("a plan id").to_string();
    let view = rig.host.plans.view(Some(&id)).expect("the plan");
    assert_eq!(view.state, PlanState::Waiting, "{}", answer.text);
    (view, answer)
}

fn plan(rig: &Rig, a: PlanMetadataArgs) -> McpPlanView {
    plan_answer(rig, a).0
}

/// The selection a user gets by pressing Apply on the proposal as it stands.
fn proposal(view: &McpPlanView) -> PlanSelection {
    PlanSelection {
        groups: view
            .groups
            .iter()
            .map(|g| GroupSelection {
                group_id: g.id.clone(),
                chosen: None,
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

fn item<'a>(view: &'a McpPlanView, fx: &Fx, field: FillField) -> &'a PlanItem {
    let id = item_id_of(fx, field);
    view.groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("{} ({}) is not in the plan", fx.name, field.as_str()))
}

/// `object`, `instrume`, `date_obs`, `imagetyp`, `is_master`, `override` of a frame, as stored.
fn row(rig: &Rig, fx: &Fx) -> (Option<String>, Option<String>, Option<String>, Option<String>, i64, i64) {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    conn.query_row(
        "SELECT object, instrume, date_obs, imagetyp, is_master, override FROM frames WHERE id = ?1",
        [fx.frame_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )
    .expect("frame row")
}

// ---- tests ----

#[test]
fn fill_sets_override_and_survives_rescan() {
    let rig = rig();
    let name = "M33_2024-10-05_22-15-03_0001.fits";
    let path = dir_of(&rig, "rescan").join(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    crate::archive::restore::tests::write_minimal_fits(&path);

    // The scanner catalogs the file; its header holds no DATE-OBS.
    let root_id: i64 = {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.query_row("SELECT id FROM scan_roots WHERE path = ?1", [text(&rig.base.join("nas"))], |r| r.get(0)).expect("root")
    };
    let scan = |rig: &Rig| {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        let done = crate::scanner::scan_directory_parallel(
            &rig.base.join("nas"),
            root_id,
            &conn,
            &NullEmitter,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            false,
        );
        assert!(done.errors.is_empty(), "the scan must succeed: {:?}", done.errors);
    };
    scan(&rig);
    let frame_id: i64 = {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.query_row(
            "SELECT fr.id FROM frames fr JOIN files f ON f.id = fr.file_id WHERE f.path = ?1",
            [text(&path)],
            |r| r.get(0),
        )
        .expect("the scanned frame")
    };
    let fx = Fx { file_id: 0, frame_id, path: path.clone(), name: name.to_string() };
    let (_, _, date, _, _, flag) = row(&rig, &fx);
    assert_eq!((date, flag), (None, 0), "the scan leaves the date empty and no override");

    let view = plan(&rig, args(&rig, "rescan", &[FillField::Date], None));
    assert_eq!(view.groups.len(), 1);
    let title = &view.groups[0].title;
    assert!(title.starts_with("Date: ") && title.contains("rescan") && title.ends_with("(2024-10-05 22:15:03)"), "{title}");
    assert_eq!(view.groups[0].items[0].values["proposed"], "2024-10-05 22:15:03");
    assert_eq!(view.groups[0].items[0].action, "Set DATE-OBS to '2024-10-05 22:15:03'");
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    assert_eq!(ended.report.as_ref().map(|r| r.applied.len()), Some(1), "{:?}", ended.report);

    let (object, _, date, _, _, flag) = row(&rig, &fx);
    assert_eq!(date.as_deref(), Some("2024-10-05T22:15:03Z"));
    assert_eq!(flag, 1, "the write flags the frame as the user's edit");
    assert_eq!(object.as_deref(), Some("M33"), "nothing else changed");

    // The file is touched (same bytes, a later mtime) and the scanner reads it again: the value stays.
    let later = std::fs::metadata(&path).expect("meta").modified().expect("mtime") + Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).expect("open").set_modified(later).expect("touch");
    scan(&rig);
    let (_, _, date, _, _, flag) = row(&rig, &fx);
    assert_eq!(date.as_deref(), Some("2024-10-05T22:15:03Z"), "the re-parse keeps the filled value");
    assert_eq!(flag, 1, "the override survives the re-parse");
}

#[test]
fn apply_skips_a_frame_filled_meanwhile() {
    let rig = rig();
    let a = put(&rig, "meanwhile", "a.fits", |_| {});
    let b = put(&rig, "meanwhile", "b.fits", |_| {});
    let view = plan(&rig, args(&rig, "meanwhile", &[], Some((FillField::Camera, "Cam X"))));
    assert_eq!(view.groups.len(), 1);
    assert_eq!(view.groups[0].items.iter().filter(|i| i.selected).count(), 2);

    // Someone fills b's camera before the user presses Apply.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute("UPDATE frames SET instrume = 'Other' WHERE id = ?1", [b.frame_id]).expect("fill b");
    }
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    let report = ended.report.expect("a report");
    assert_eq!(report.applied.len(), 1, "{report:?}");
    assert_eq!(report.applied[0].item_id, item_id_of(&a, FillField::Camera));
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert_eq!(report.skipped[0].item_id, item_id_of(&b, FillField::Camera));
    assert_eq!(report.skipped[0].reason, "catalog changed: INSTRUME is now 'Other'");
    assert_eq!(report.skipped[0].file_name, "b.fits");
    assert_eq!(row(&rig, &a).1.as_deref(), Some("Cam X"));
    assert_eq!(row(&rig, &b).1.as_deref(), Some("Other"), "the value someone set is never overwritten");
}

#[test]
fn frames_metadata_changed_emitted() {
    let rig = rig();
    let a = put(&rig, "emit", "a.fits", |_| {});
    let b = put(&rig, "emit", "b.fits", |_| {});
    let view = plan(&rig, args(&rig, "emit", &[], Some((FillField::Camera, "Cam X"))));
    let before = rig.events.named(METADATA_CHANGED).len();
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    let events = rig.events.named(METADATA_CHANGED);
    assert_eq!(events.len(), before + 1, "{events:?}");
    assert_eq!(events.last().expect("an event"), &json!({ "count": 2 }));
    assert_eq!((row(&rig, &a).1.as_deref(), row(&rig, &b).1.as_deref()), (Some("Cam X"), Some("Cam X")));

    // A plan whose every frame was filled meanwhile changes nothing and says nothing.
    let c = put(&rig, "emit-two", "c.fits", |_| {});
    let view = plan(&rig, args(&rig, "emit-two", &[], Some((FillField::Camera, "Cam X"))));
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute("UPDATE frames SET instrume = 'Other' WHERE id = ?1", [c.frame_id]).expect("fill c");
    }
    let before = rig.events.named(METADATA_CHANGED).len();
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.report.as_ref().map(|r| (r.applied.len(), r.skipped.len())), Some((0, 1)), "{ended:?}");
    assert_eq!(rig.events.named(METADATA_CHANGED).len(), before, "nothing changed, so no event");
}

#[test]
fn the_plan_groups_by_value_locks_what_has_no_evidence_and_counts_the_links() {
    let rig = rig();
    // Three lights agree on the object; two miss it.
    for n in 1..=3 {
        light(&rig, "m31/Lights", &format!("n{n}.fits"), Some("M 31"));
    }
    let x1 = light(&rig, "m31/Lights", "x1.fits", None);
    let x2 = light(&rig, "m31/Lights", "x2.fits", None);
    // A folder that disagrees with itself.
    for n in 1..=3 {
        light(&rig, "mix", &format!("a{n}.fits"), Some("A"));
    }
    light(&rig, "mix", "b1.fits", Some("B"));
    let mx = light(&rig, "mix", "mx.fits", None);
    // A frame with no type, in a folder named for one.
    let f1 = put(&rig, "m31/Flats", "f1.fits", |f| f.instrume = Some("Cam".to_string()));
    // x1 sits in a calibration set: changing it takes it out.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute("INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume) VALUES ('Dark', '2024-10-05', 0, 'Cam')", []).expect("set");
        let set = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", [set, x1.frame_id]).expect("member");
    }
    let scope = Scope { folder: Some(text(&rig.base.join("nas"))), ..Default::default() };
    let a = PlanMetadataArgs { scope, fields: vec![FillField::Object, FillField::FrameType], statement: None };
    let (view, answer) = plan_answer(&rig, a);

    assert_eq!(view.kind, PlanKind::Metadata);
    let titles: Vec<&str> = view.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, vec!["Object: M 31", "Frame type: Flat", "Object: nothing proposed"]);
    let ids: Vec<&str> = view.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, vec!["group:1", "group:2", "group:3"]);

    for fx in [&x1, &x2] {
        let i = item(&view, fx, FillField::Object);
        assert!(i.selected && i.locked.is_none(), "{i:?}");
        assert_eq!(i.evidence.as_deref(), Some("3 frames in this folder have OBJECT = 'M 31'"));
        assert_eq!(i.values["current"], "(empty)");
        assert_eq!(i.values["proposed"], "M 31");
        assert_eq!(i.values["field"], "OBJECT");
        assert_eq!(i.action, "Set OBJECT to 'M 31'");
    }
    let flat = item(&view, &f1, FillField::FrameType);
    assert!(flat.selected, "{flat:?}");
    assert_eq!(flat.evidence.as_deref(), Some("folder 'Flats' in its path"));
    let none = item(&view, &mx, FillField::Object);
    assert!(!none.selected, "{none:?}");
    assert_eq!(none.locked.as_deref(), Some("frames in this folder disagree: 3 'A', 1 'B'"));
    assert_eq!(view.notes[0], "these changes take 0 frames out of their sessions and 1 out of calibration links");
    assert!(view.notes.iter().any(|n| n.starts_with("Only the catalog changes")), "{:?}", view.notes);

    // The answer is the plan: its words and its data say the same.
    assert!(answer.text.contains("Plan plan-") && answer.text.contains("is ready: 3 values proposed in 2 groups."), "{}", answer.text);
    assert!(answer.text.contains("- Object: M 31: 2 frames"), "{}", answer.text);
    assert!(answer.text.contains("x1.fits in ") && answer.text.contains(": 3 frames in this folder have OBJECT = 'M 31'"), "{}", answer.text);
    assert!(answer.text.contains("mx.fits in ") && answer.text.contains("locked: frames in this folder disagree: 3 'A', 1 'B'"), "{}", answer.text);
    assert!(answer.text.contains("these changes take 0 frames out of their sessions and 1 out of calibration links"), "{}", answer.text);
    assert!(answer.text.contains("Nothing changes until they press Apply there."), "{}", answer.text);
    assert!(!answer.text.to_lowercase().contains("applied"), "{}", answer.text);
    let groups = answer.data["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0]["field"], "OBJECT");
    assert_eq!(groups[0]["value"], "M 31");
    assert_eq!(groups[0]["proposed"], 2);
    let first = &groups[0]["items"][0];
    assert_eq!(first["selected"], true);
    assert!(first["locked"].is_null());
    assert_eq!(first["evidence"], "3 frames in this folder have OBJECT = 'M 31'");
    assert!(first["fileName"].as_str().is_some_and(|n| n.ends_with(".fits")) && first["folder"].is_string());
    assert_eq!(groups[2]["value"], json!(null));
    assert_eq!(groups[2]["items"][0]["locked"], "frames in this folder disagree: 3 'A', 1 'B'");
    assert_eq!(answer.data["proposedValues"], 3);
    assert_eq!(answer.data["lockedItems"], 1);

    // plan_status tells a waiting plan the same way.
    let status = crate::mcp::tools::plans::status_answer(&view);
    assert!(status.text.contains("waiting for you in Athenaeum's window"), "{}", status.text);
    assert!(status.text.contains("- Object: M 31: 2 frames"), "{}", status.text);
    assert!(status.text.contains("locked: frames in this folder disagree"), "{}", status.text);
    assert_eq!(status.data["groups"].as_array().map(Vec::len), Some(3));

    // Pressing Apply on the proposal writes what the plan says, and the frames carry the override.
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    assert_eq!(ended.report.as_ref().map(|r| r.applied.len()), Some(3), "{:?}", ended.report);
    for fx in [&x1, &x2] {
        let (object, _, _, _, _, flag) = row(&rig, fx);
        assert_eq!((object.as_deref(), flag), (Some("M 31"), 1));
    }
    let (_, _, _, imagetyp, is_master, flag) = row(&rig, &f1);
    assert_eq!((imagetyp.as_deref(), is_master, flag), (Some("Flat"), 0, 1));
    assert_eq!(row(&rig, &mx).0, None, "a frame with no proposal is untouched");
    // The calibration link x1 had is gone, as the page's confirm warned.
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let links: i64 =
        conn.query_row("SELECT COUNT(*) FROM calibration_set_frames WHERE frame_id = ?1", [x1.frame_id], |r| r.get(0)).expect("count");
    assert_eq!(links, 0);
}

#[test]
fn a_statement_covers_only_the_frames_in_scope() {
    let rig = rig();
    let a1 = light(&rig, "stmt-a", "a1.fits", None);
    let a2 = light(&rig, "stmt-a", "a2.fits", None);
    let b1 = light(&rig, "stmt-b", "b1.fits", None);
    let a_has = light(&rig, "stmt-a", "a3.fits", Some("Other"));
    let view = plan(&rig, args(&rig, "stmt-a", &[], Some((FillField::Object, "M 31"))));
    assert_eq!(view.groups.len(), 1);
    assert_eq!(view.groups[0].title, "Object: M 31");
    for fx in [&a1, &a2] {
        let i = item(&view, fx, FillField::Object);
        assert_eq!(i.evidence.as_deref(), Some("by your instruction"));
        assert!(i.selected);
    }
    let in_plan: Vec<&str> = view.groups.iter().flat_map(|g| g.items.iter()).map(|i| i.file_name.as_str()).collect();
    assert_eq!(in_plan.len(), 2, "{in_plan:?}");
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    assert_eq!(row(&rig, &a1).0.as_deref(), Some("M 31"));
    assert_eq!(row(&rig, &b1).0, None, "a frame outside the scope is not covered");
    assert_eq!(row(&rig, &a_has).0.as_deref(), Some("Other"), "a frame that has a value is not overwritten");

    // A statement of the frame type is a word the planner normalizes.
    let d1 = put(&rig, "stmt-c", "d1.fits", |_| {});
    let view = plan(&rig, args(&rig, "stmt-c", &[], Some((FillField::FrameType, "darks"))));
    assert_eq!(view.groups[0].title, "Frame type: Dark");
    assert_eq!(item(&view, &d1, FillField::FrameType).evidence.as_deref(), Some("by your instruction"));
}

#[test]
fn a_frame_whose_file_cannot_be_stamped_is_locked() {
    let rig = rig();
    let a = light(&rig, "gone", "a.fits", None);
    let b = light(&rig, "gone", "b.fits", None);
    std::fs::remove_file(&b.path).expect("remove");
    let view = plan(&rig, args(&rig, "gone", &[], Some((FillField::Object, "M 31"))));
    let ia = item(&view, &a, FillField::Object);
    assert!(ia.selected && ia.locked.is_none(), "{ia:?}");
    let ib = item(&view, &b, FillField::Object);
    assert!(!ib.selected, "{ib:?}");
    assert_eq!(ib.locked.as_deref(), Some(NOT_REACHABLE));
}

#[test]
fn a_request_with_nothing_to_fill_leaves_the_waiting_plan_alone() {
    let rig = rig();
    light(&rig, "wait", "w1.fits", None);
    let first = plan(&rig, args(&rig, "wait", &[], Some((FillField::Object, "M 31"))));
    // Frames that miss nothing.
    light(&rig, "full", "f1.fits", Some("M 31"));
    // Frames that miss a field with no evidence.
    light(&rig, "lonely", "l1.fits", None);

    let still_waiting = |what: &str| {
        let now = rig.host.plans.view(None).expect("a plan");
        assert_eq!((now.plan_id.as_str(), &now.state), (first.plan_id.as_str(), &PlanState::Waiting), "{what}");
        assert_eq!(rig.host.plans.view(Some(&first.plan_id)).expect("plan").groups.len(), 1, "{what}");
    };
    let refused = |a: PlanMetadataArgs| match prepare(&rig.host, a) {
        Err(ApiError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    };

    let said = refused(args(&rig, "full", &[FillField::Object, FillField::Camera], None));
    assert!(said.contains("misses OBJECT, INSTRUME") && said.contains("nothing was planned"), "{said}");
    still_waiting("nothing missing");

    let said = refused(args(&rig, "lonely", &[FillField::Object], None));
    assert!(said.contains("1 frame missing OBJECT") && said.contains("fewer than 3 frames in this folder have it"), "{said}");
    assert!(said.contains("statement") && said.contains("nothing was planned"), "{said}");
    still_waiting("no evidence");

    let said = refused(PlanMetadataArgs {
        scope: Scope { camera: Some("No Such Camera".to_string()), ..Default::default() },
        fields: Vec::new(),
        statement: None,
    });
    assert!(said.contains("No Such Camera") && said.contains("nothing was planned"), "{said}");
    still_waiting("a scope that names nothing");

    let said = refused(args(&rig, "wait", &[], Some((FillField::Date, "last night"))));
    assert!(said.contains("not a date to set"), "{said}");
    still_waiting("a bad statement");
}

#[test]
fn an_oversized_request_is_refused_before_any_plan_begins() {
    let rig = rig();
    light(&rig, "cap-wait", "w1.fits", None);
    let first = plan(&rig, args(&rig, "cap-wait", &[], Some((FillField::Object, "M 31"))));
    let over = crate::mcp::plans::PLAN_ITEM_CAP + 1;
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute_batch("BEGIN").expect("begin");
        for n in 0..over {
            let name = format!("big-{n}.fits");
            let path = dir_of(&rig, "cap-big").join(&name);
            let file_id = db::insert_file(
                &conn,
                &File {
                    id: None,
                    path: text(&path),
                    filename: name,
                    size: 1,
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
            )
            .expect("file row");
            db::insert_frame(&conn, &Frame { file_id, imagetyp: Some(ImageType::Light), ..Default::default() }).expect("frame");
        }
        conn.execute_batch("COMMIT").expect("commit");
    }
    let said = match prepare(&rig.host, args(&rig, "cap-big", &[], Some((FillField::Object, "M 31")))) {
        Err(ApiError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    };
    assert!(said.contains("2 000") && said.contains(&over.to_string()), "{said}");
    let now = rig.host.plans.view(None).expect("a plan");
    assert_eq!((now.plan_id.as_str(), &now.state), (first.plan_id.as_str(), &PlanState::Waiting));
}

#[test]
fn a_master_in_a_darks_folder_stays_a_master() {
    let rig = rig();
    let m = put(&rig, "cal/Darks", "master_dark_300s.fits", |f| f.is_master = true);
    let raw = put(&rig, "cal/Darks", "d1.fits", |_| {});
    assert_eq!(row(&rig, &m).4, 1, "the fixture is a master");
    let view = plan(&rig, args(&rig, "cal/Darks", &[FillField::FrameType], None));
    let i = item(&view, &m, FillField::FrameType);
    assert_eq!(i.values["proposed"], "MasterDark");
    assert_eq!(i.evidence.as_deref(), Some("folder 'Darks' in its path; the file is a master"));
    assert_eq!(item(&view, &raw, FillField::FrameType).values["proposed"], "Dark");
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    let (_, _, _, imagetyp, is_master, flag) = row(&rig, &m);
    assert_eq!((imagetyp.as_deref(), is_master, flag), (Some("MasterDark"), 1, 1));
    let (_, _, _, imagetyp, is_master, _) = row(&rig, &raw);
    assert_eq!((imagetyp.as_deref(), is_master), (Some("Dark"), 0));
}

#[test]
fn a_date_without_a_time_is_refused() {
    let rig = rig();
    light(&rig, "date-wait", "w1.fits", None);
    let first = plan(&rig, args(&rig, "date-wait", &[], Some((FillField::Object, "M 31"))));
    let a = light(&rig, "date-none", "a.fits", Some("M 31"));
    let said = match prepare(&rig.host, args(&rig, "date-none", &[], Some((FillField::Date, "2024-10-05")))) {
        Err(ApiError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    };
    assert!(
        said.contains("give the time too (YYYY-MM-DD HH:MM:SS): the night a frame belongs to is decided by its time"),
        "{said}"
    );
    let now = rig.host.plans.view(None).expect("a plan");
    assert_eq!((now.plan_id.as_str(), &now.state), (first.plan_id.as_str(), &PlanState::Waiting));
    assert_eq!(row(&rig, &a).2, None);
    // With the time it is planned and shown as a date.
    let view = plan(&rig, args(&rig, "date-none", &[], Some((FillField::Date, "2024-10-05 22:15:03"))));
    assert_eq!(item(&view, &a, FillField::Date).values["proposed"], "2024-10-05 22:15:03");
}

#[test]
fn dates_are_grouped_by_folder() {
    let rig = rig();
    let n = 6;
    let mut a = Vec::new();
    let mut b = Vec::new();
    for k in 0..n {
        a.push(put(&rig, "nightA", &format!("L_2024-10-05_22-1{k}-03.fits"), |f| f.imagetyp = Some(ImageType::Light)));
        b.push(put(&rig, "nightB", &format!("L_2024-10-06_23-0{k}-30.fits"), |f| f.imagetyp = Some(ImageType::Light)));
    }
    let scope = Scope { folder: Some(text(&rig.base.join("nas"))), ..Default::default() };
    let view = plan(&rig, PlanMetadataArgs { scope, fields: vec![FillField::Date], statement: None });
    assert_eq!(view.groups.len(), 2, "one group per folder, not per frame: {:?}", view.groups.iter().map(|g| &g.title).collect::<Vec<_>>());
    let titles: Vec<&str> = view.groups.iter().map(|g| g.title.as_str()).collect();
    assert!(titles[0].starts_with("Date: ") && titles[0].contains("nightA"), "{titles:?}");
    assert!(titles[0].ends_with("(2024-10-05 22:10:03 to 2024-10-05 22:15:03)"), "{titles:?}");
    assert!(titles[1].contains("nightB") && titles[1].ends_with("(2024-10-06 23:00:30 to 2024-10-06 23:05:30)"), "{titles:?}");
    assert_eq!(view.groups[0].items.len(), n);
    // Each item keeps its own value.
    assert_eq!(item(&view, &a[2], FillField::Date).values["proposed"], "2024-10-05 22:12:03");
    assert_eq!(item(&view, &b[5], FillField::Date).values["proposed"], "2024-10-06 23:05:30");
    assert_eq!(item(&view, &a[2], FillField::Date).action, "Set DATE-OBS to '2024-10-05 22:12:03'");
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    assert_eq!(row(&rig, &a[2]).2.as_deref(), Some("2024-10-05T22:12:03Z"));
    assert_eq!(row(&rig, &b[5]).2.as_deref(), Some("2024-10-06T23:05:30Z"));
}

#[test]
fn flats_never_name_a_lights_object() {
    let rig = rig();
    for n in 1..=3 {
        put(&rig, "flatobj", &format!("f{n}.fits"), |f| {
            f.imagetyp = Some(ImageType::Flat);
            f.object = Some("FlatField".to_string());
        });
    }
    light(&rig, "flatobj", "l1.fits", None);
    let said = match prepare(&rig.host, args(&rig, "flatobj", &[FillField::Object], None)) {
        Err(ApiError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    };
    assert!(said.contains("fewer than 3 frames in this folder have it") && !said.contains("FlatField"), "{said}");
}

#[test]
fn the_applier_rechecks_the_field_and_the_light() {
    let rig = rig();
    let filled = put(&rig, "recheck", "a.fits", |_| {});
    let lit = light(&rig, "recheck", "b.fits", None);
    let ok = put(&rig, "recheck", "c.fits", |_| {});
    let info = |fx: &Fx, field: FillField, value: &str| ItemInfo {
        frame_id: fx.frame_id,
        field,
        value: value.to_string(),
        file_name: fx.name.clone(),
        folder: text(&dir_of(&rig, "recheck")),
    };
    let ids = [
        item_id_of(&filled, FillField::Camera),
        item_id_of(&lit, FillField::Object),
        item_id_of(&ok, FillField::Camera),
    ];
    let items = HashMap::from([
        (ids[0].clone(), info(&filled, FillField::Camera, "Cam X")),
        (ids[1].clone(), info(&lit, FillField::Object, "M 31")),
        (ids[2].clone(), info(&ok, FillField::Camera, "Cam X")),
    ]);
    // After planning: a's camera is filled, b stops being a light.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute("UPDATE frames SET instrume = 'Other' WHERE id = ?1", [filled.frame_id]).expect("fill a");
        conn.execute("UPDATE frames SET imagetyp = 'Dark' WHERE id = ?1", [lit.frame_id]).expect("retype b");
    }
    let selection = PlanSelection {
        groups: vec![GroupSelection { group_id: "group:1".to_string(), chosen: None, included: ids.to_vec() }],
    };
    let report = MetadataApplier { items }.apply(&rig.host, "plan-recheck", &selection, &ids);
    let reason = |id: &str| report.skipped.iter().find(|l| l.item_id == id).map(|l| l.reason.clone());
    assert_eq!(reason(&ids[0]).as_deref(), Some("it has INSTRUME now: 'Other'"), "{report:?}");
    assert_eq!(reason(&ids[1]).as_deref(), Some("it is no longer a light frame"), "{report:?}");
    assert_eq!(report.applied.len(), 1, "{report:?}");
    assert_eq!(report.applied[0].item_id, ids[2]);
    assert_eq!(row(&rig, &filled).1.as_deref(), Some("Other"));
    assert_eq!(row(&rig, &lit).0, None);
    assert_eq!(row(&rig, &ok).1.as_deref(), Some("Cam X"));
}

#[test]
fn a_frame_deleted_before_the_write_is_failed() {
    let rig = rig();
    let a = put(&rig, "vanish", "a.fits", |_| {});
    let b = put(&rig, "vanish", "b.fits", |_| {});
    let view = plan(&rig, args(&rig, "vanish", &[], Some((FillField::Camera, "Cam X"))));
    assert_eq!(view.groups[0].items.iter().filter(|i| i.selected).count(), 2);
    // b leaves the catalog after the re-check and before the write reaches it: writing a's row deletes b's.
    {
        let database = rig.ctx.db.get().expect("db");
        let conn = database.conn();
        conn.execute_batch(&format!(
            "CREATE TRIGGER vanish_b BEFORE UPDATE ON frames WHEN OLD.id = {} BEGIN DELETE FROM frames WHERE id = {}; END;",
            a.frame_id, b.frame_id
        ))
        .expect("trigger");
    }
    let ended = apply(&rig, &view.plan_id, proposal(&view));
    assert_eq!(ended.state, PlanState::Applied, "{ended:?}");
    let report = ended.report.expect("a report");
    assert_eq!(report.applied.len(), 1, "{report:?}");
    assert_eq!(report.applied[0].item_id, item_id_of(&a, FillField::Camera));
    assert_eq!(report.failed.len(), 1, "a write that did not reach a frame is not reported applied: {report:?}");
    assert_eq!(report.failed[0].item_id, item_id_of(&b, FillField::Camera));
    assert_eq!(report.failed[0].reason, "not in the catalog any more");
    assert_eq!(row(&rig, &a).1.as_deref(), Some("Cam X"));
}
