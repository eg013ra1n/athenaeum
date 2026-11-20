//! The plan store and the apply framework (MCP plan Task 12): replace, expiry, the subset rule, the re-check at
//! apply, the report, the events and the log line. Time is paused (`start_paused`), so no test waits on a clock.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde_json::Value;

use super::*;
use crate::mcp::{mcp_server_name, HostKind, McpHost, McpIdentity};
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::services::ServiceContext;
use crate::{db, events::ProgressEmitter};

// ---- the rig ----

#[derive(Default)]
struct Capture(Mutex<Vec<(String, Value)>>);

impl ProgressEmitter for Capture {
    fn emit_json(&self, event_name: &str, payload: Value) {
        self.0.lock().unwrap().push((event_name.to_string(), payload));
    }
}

struct Rig {
    host: Arc<McpHost>,
    events: Arc<Capture>,
    fronts: Arc<AtomicUsize>,
    viewer: Arc<AtomicBool>,
    ctx: Arc<ServiceContext>,
    base: PathBuf,
    // Dropped last: the host is shut down in `Drop` before the directory goes.
    _tmp: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}

fn rig() -> Rig {
    let (tmp, base) = crate::test_support::canonical_tempdir();
    let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
    let events = Arc::new(Capture::default());
    let fronts = Arc::new(AtomicUsize::new(0));
    let viewer = Arc::new(AtomicBool::new(true));
    let host = McpHost::new(
        Arc::clone(&ctx),
        McpIdentity { server_name: mcp_server_name(), version: "9.9.9-test", kind: HostKind::Desktop, web_api_key_set: false },
        Arc::clone(&events) as Arc<dyn ProgressEmitter>,
        {
            let f = Arc::clone(&fronts);
            Arc::new(move || {
                f.fetch_add(1, Ordering::SeqCst);
            })
        },
        {
            let v = Arc::clone(&viewer);
            Arc::new(move || v.load(Ordering::SeqCst))
        },
    );
    host.on_catalog_open().expect("catalog open");
    Rig { host, events, fronts, viewer, ctx, base, _tmp: tmp }
}

impl Rig {
    fn names(&self) -> Vec<(String, String)> {
        self.events
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(n, p)| (n.clone(), p["planId"].as_str().unwrap_or_default().to_string()))
            .collect()
    }

    fn forget_events(&self) {
        self.events.0.lock().unwrap().clear();
    }
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// One catalog file with a frame and a real file of `len` bytes on disk.
#[derive(Clone)]
struct Made {
    file_id: i64,
    frame_id: i64,
    path: PathBuf,
}

impl Made {
    fn item(&self) -> String {
        format!("item:{}", self.file_id)
    }
}

fn make(rig: &Rig, name: &str, len: usize) -> Made {
    let path = rig.base.join(name);
    std::fs::write(&path, vec![7u8; len]).expect("write file");
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    let file_id = db::insert_file(
        &conn,
        &File {
            id: None,
            path: text(&path),
            filename: name.to_string(),
            size: len as i64,
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
    let frame_id = db::insert_frame(
        &conn,
        &Frame { file_id, object: Some("M31".to_string()), imagetyp: Some(ImageType::Light), ..Default::default() },
    )
    .expect("frame row");
    Made { file_id, frame_id, path }
}

fn snapshot(rig: &Rig, m: &Made) -> ItemSnapshot {
    let database = rig.ctx.db.get().expect("db");
    let conn = database.conn();
    ItemSnapshot {
        file_id: m.file_id,
        frame_id: Some(m.frame_id),
        stamp: stamp_for_plan(&m.path).expect("stamp"),
        catalog: catalog_values(&conn, m.frame_id, &["OBJECT"]).expect("catalog values"),
    }
}

fn item_of(m: &Made, candidate: bool, locked: Option<&str>) -> PlanItem {
    PlanItem {
        id: m.item(),
        file_name: m.path.file_name().unwrap().to_string_lossy().into_owned(),
        folder: text(m.path.parent().unwrap()),
        values: BTreeMap::new(),
        action: "move to the Black Hole".to_string(),
        evidence: None,
        selected: false,
        locked: locked.map(str::to_string),
        candidate,
        size_bytes: None,
    }
}

/// The applier of these tests: records what it was asked to act on and says it applied each.
#[derive(Clone, Default)]
struct TestApplier {
    seen: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

impl PlanApplier for TestApplier {
    fn validate(&self, _view: &McpPlanView, _sel: &PlanSelection) -> Result<(), String> {
        Ok(())
    }

    fn apply(&self, _host: &McpHost, _plan_id: &str, _sel: &PlanSelection, fresh: &[String]) -> PlanReport {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().extend(fresh.iter().cloned());
        PlanReport {
            applied: fresh
                .iter()
                .map(|id| ReportLine { item_id: id.clone(), file_name: id.clone(), folder: String::new(), reason: "done".into() })
                .collect(),
            ..Default::default()
        }
    }
}

/// The files of a one-group plan: a kept copy, two others, one that is no candidate, one locked.
struct Fx {
    keeper: Made,
    a: Made,
    b: Made,
    non_candidate: Made,
    locked: Made,
}

fn fixture(rig: &Rig) -> Fx {
    Fx {
        keeper: make(rig, "keeper_1", 10),
        a: make(rig, "copy_a", 11),
        b: make(rig, "copy_b", 12),
        non_candidate: make(rig, "other_c", 13),
        locked: make(rig, "linked_d", 14),
    }
}

fn content(rig: &Rig, fx: &Fx, applier: &TestApplier) -> PlanContent {
    let mut snapshots = HashMap::new();
    for m in [&fx.keeper, &fx.a, &fx.b, &fx.non_candidate, &fx.locked] {
        snapshots.insert(m.item(), snapshot(rig, m));
    }
    PlanContent {
        title: "Move duplicates".to_string(),
        columns: vec![PlanColumn { key: "object".into(), label: "OBJECT".into() }],
        groups: vec![PlanGroup {
            id: "group:1".to_string(),
            title: "M31 duplicates".to_string(),
            note: None,
            choose_one: Some("Copy that stays".to_string()),
            chosen: Some(fx.keeper.item()),
            items: vec![
                item_of(&fx.keeper, true, Some("stays")),
                item_of(&fx.a, true, None),
                item_of(&fx.b, true, None),
                item_of(&fx.non_candidate, false, None),
                item_of(&fx.locked, true, Some("calibration links point at it")),
            ],
        }],
        notes: vec![],
        snapshots,
        applier: Box::new(applier.clone()),
    }
}

fn selection(chosen: Option<&Made>, included: &[&Made]) -> PlanSelection {
    PlanSelection {
        groups: vec![GroupSelection {
            group_id: "group:1".to_string(),
            chosen: chosen.map(Made::item),
            included: included.iter().map(|m| m.item()).collect(),
        }],
    }
}

/// A waiting plan over the fixture.
fn waiting(rig: &Rig, fx: &Fx, applier: &TestApplier) -> String {
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "Move duplicates");
    rig.host.plans.publish(&id, content(rig, fx, applier)).expect("publish");
    id
}

/// Waits until the apply thread has ended the plan. Bounded: a plan stranded in `applying` fails the test with its
/// state instead of hanging it.
async fn settled(rig: &Rig, id: &str) -> McpPlanView {
    let started = std::time::Instant::now();
    loop {
        let v = rig.host.plans.view(Some(id)).expect("plan");
        if v.state != PlanState::Applying {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the plan is stranded: {:?}", v.state);
        tokio::task::yield_now().await;
    }
}

fn apply_of(sel: PlanSelection) -> PlanAnswer {
    PlanAnswer::Apply { selection: sel }
}

fn invalid_text(r: Result<McpPlanView, ApiError>) -> String {
    match r {
        Err(ApiError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ---- replace, read ----

#[tokio::test(start_paused = true)]
async fn new_plan_replaces_an_undecided_one() {
    let rig = rig();
    let (a, flag_a) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "first");
    let (b, flag_b) = rig.host.plans.begin(&rig.host, PlanKind::Metadata, "second");
    let old = rig.host.plans.view(Some(&a)).expect("old plan");
    assert_eq!(old.state, PlanState::Replaced);
    assert_eq!(old.replaced_by.as_deref(), Some(b.as_str()));
    assert!(flag_a.load(Ordering::SeqCst), "the replaced plan's planner is told to stop");
    assert!(!flag_b.load(Ordering::SeqCst));
    assert!(matches!(rig.host.plans.view(Some(&b)).unwrap().state, PlanState::Preparing { .. }));
}

#[tokio::test(start_paused = true)]
async fn replaced_plan_stays_readable_by_id() {
    let rig = rig();
    let (a, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "first");
    let (b, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "second");
    assert_eq!(rig.host.plans.view(Some(&a)).unwrap().state, PlanState::Replaced);
    assert_eq!(rig.host.plans.view(None).unwrap().plan_id, b);
    assert!(rig.host.plans.view(Some("plan-nope")).is_none());
    // A replaced plan cannot be applied or published into.
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    assert!(rig.host.plans.publish(&a, content(&rig, &fx, &applier)).is_err());
    let said = invalid_text(rig.host.plans.answer(&rig.host, &a, apply_of(selection(Some(&fx.keeper), &[&fx.a]))));
    assert!(said.contains("replaced"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn expired_plan_cannot_be_applied() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    let shown = rig.host.plans.view(Some(&id)).unwrap();
    assert_eq!(shown.state, PlanState::Waiting);
    let at = shown.expires_at.expect("expires_at");
    assert!(chrono::NaiveDateTime::parse_from_str(&at, "%Y-%m-%d %H:%M:%S").is_ok(), "{at}");

    tokio::time::advance(Duration::from_secs(61 * 60)).await;
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))));
    assert!(said.contains("expired"), "{said}");
    assert_eq!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Expired);
    assert_eq!(applier.calls.load(Ordering::SeqCst), 0);
}

// ---- the subset rule ----

#[tokio::test(start_paused = true)]
async fn selection_must_be_a_subset() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);

    let mut unknown_group = selection(Some(&fx.keeper), &[&fx.a]);
    unknown_group.groups[0].group_id = "group:9".to_string();
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(unknown_group)));
    assert!(said.contains("group:9"), "{said}");

    let mut unknown_item = selection(Some(&fx.keeper), &[&fx.a]);
    unknown_item.groups[0].included.push("item:999999".to_string());
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(unknown_item)));
    assert!(said.contains("item:999999"), "{said}");

    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.locked]))));
    assert!(said.contains("linked_d") && said.contains("calibration links point at it"), "{said}");

    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.non_candidate), &[&fx.a]))));
    assert!(said.contains("other_c") && said.contains("cannot be chosen"), "{said}");

    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[]))));
    assert!(said.contains("nothing is selected"), "{said}");

    assert_eq!(applier.calls.load(Ordering::SeqCst), 0, "nothing was applied");
    assert_eq!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Waiting, "a refused answer leaves the plan waiting");
}

#[tokio::test(start_paused = true)]
async fn the_applier_may_refuse_a_selection() {
    struct Refuses;
    impl PlanApplier for Refuses {
        fn validate(&self, _v: &McpPlanView, _s: &PlanSelection) -> Result<(), String> {
            Err("pick the copy that stays".to_string())
        }
        fn apply(&self, _h: &McpHost, _p: &str, _s: &PlanSelection, _f: &[String]) -> PlanReport {
            panic!("a refused selection is never applied");
        }
    }
    let rig = rig();
    let fx = fixture(&rig);
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &TestApplier::default());
    c.applier = Box::new(Refuses);
    rig.host.plans.publish(&id, c).expect("publish");
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(None, &[&fx.a]))));
    assert_eq!(said, "pick the copy that stays");
}

// ---- the re-check ----

#[tokio::test(start_paused = true)]
async fn changed_on_disk_is_skipped() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    std::fs::write(&fx.a.path, vec![7u8; 99]).expect("rewrite");

    let started = rig
        .host
        .plans
        .answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.b])))
        .expect("apply");
    assert_eq!(started.state, PlanState::Applying);
    let done = settled(&rig, &id).await;
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].item_id, fx.b.item());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].item_id, fx.a.item());
    assert_eq!(report.skipped[0].file_name, "copy_a");
    assert_eq!(report.skipped[0].reason, "changed on disk since the plan (size 11 → 99)");
    assert_eq!(*applier.seen.lock().unwrap(), vec![fx.b.item()], "only the unchanged item reaches the applier");
    // The report shows what the user chose.
    assert_eq!(done.groups[0].chosen.as_deref(), Some(fx.keeper.item().as_str()));
    assert!(done.groups[0].items.iter().find(|i| i.id == fx.a.item()).unwrap().selected);
}

#[tokio::test(start_paused = true)]
async fn a_missing_file_and_a_changed_catalog_value_are_skipped() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    std::fs::remove_file(&fx.a.path).expect("remove");
    rig.ctx
        .db
        .get()
        .unwrap()
        .conn()
        .execute("UPDATE frames SET object = 'M33' WHERE id = ?1", [fx.b.frame_id])
        .expect("update");

    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.b]))).expect("apply");
    let report = settled(&rig, &id).await.report.expect("report");
    assert!(report.applied.is_empty());
    let reasons: HashMap<_, _> = report.skipped.iter().map(|l| (l.item_id.clone(), l.reason.clone())).collect();
    assert_eq!(reasons[&fx.a.item()], "missing");
    assert_eq!(reasons[&fx.b.item()], "catalog changed: OBJECT is now 'M33'");
    assert_eq!(applier.calls.load(Ordering::SeqCst), 0, "nothing fresh, nothing to apply");
}

#[tokio::test(start_paused = true)]
async fn changed_kept_copy_skips_its_group() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    std::fs::write(&fx.keeper.path, vec![7u8; 77]).expect("rewrite the kept copy");

    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.b]))).expect("apply");
    let done = settled(&rig, &id).await;
    assert_eq!(done.state, PlanState::Applied);
    let report = done.report.expect("report");
    assert!(report.applied.is_empty());
    assert_eq!(report.skipped.len(), 2);
    for line in &report.skipped {
        assert!(line.reason.starts_with("the copy that stays changed or is gone: changed on disk since the plan"), "{}", line.reason);
    }
    assert_eq!(applier.calls.load(Ordering::SeqCst), 0);

    // A kept copy that is gone says so.
    let id = waiting(&rig, &fx, &applier);
    std::fs::remove_file(&fx.keeper.path).expect("remove the kept copy");
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))).expect("apply");
    let report = settled(&rig, &id).await.report.expect("report");
    assert_eq!(report.skipped[0].reason, "the copy that stays changed or is gone: missing");
}

#[tokio::test(start_paused = true)]
async fn unreachable_file_is_locked_not_reachable() {
    let rig = rig();
    let gone = rig.base.join("not_mounted").join("frame_x");
    let reason = stamp_for_plan(&gone).expect_err("a file under a missing folder has no stamp");
    assert_eq!(reason, "not reachable (its volume may be unmounted)");

    // A planning tool locks the item with that reason; the framework then refuses to include it.
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &applier);
    c.groups[0].items[1].locked = Some(reason);
    rig.host.plans.publish(&id, c).expect("publish");
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))));
    assert!(said.contains("not reachable (its volume may be unmounted)"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn plan_item_cap_asks_for_a_narrower_scope() {
    let rig = rig();
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "big");
    let items = (0..=PLAN_ITEM_CAP)
        .map(|i| PlanItem {
            id: format!("item:{i}"),
            file_name: format!("f{i}"),
            folder: "d".to_string(),
            values: BTreeMap::new(),
            action: "a".to_string(),
            evidence: None,
            selected: false,
            locked: None,
            candidate: false,
            size_bytes: None,
        })
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 2_001);
    let c = PlanContent {
        title: "big".into(),
        columns: vec![],
        groups: vec![PlanGroup { id: "group:1".into(), title: "g".into(), note: None, choose_one: None, chosen: None, items }],
        notes: vec![],
        snapshots: HashMap::new(),
        applier: Box::new(TestApplier::default()),
    };
    let err = rig.host.plans.publish(&id, c).expect_err("too large");
    assert_eq!(err, "this plan would hold 2001 items; narrow the scope to at most 2 000");
    assert!(matches!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Failed { .. }));
}

// ---- deciding ----

#[tokio::test(start_paused = true)]
async fn cancel_ends_a_waiting_plan() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    let v = rig.host.plans.answer(&rig.host, &id, PlanAnswer::Cancel).expect("cancel");
    assert_eq!(v.state, PlanState::Cancelled);
    assert_eq!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Cancelled);
    let said = invalid_text(rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))));
    assert!(said.contains("cancelled"), "{said}");
    assert!(rig.host.plans.answer(&rig.host, &id, PlanAnswer::Cancel).is_err(), "a decided plan cannot be cancelled again");
    assert_eq!(applier.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn a_preparing_plan_can_be_cancelled_and_then_refuses_its_publish() {
    let rig = rig();
    let fx = fixture(&rig);
    let (id, flag) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    rig.host.plans.progress(&id, 3, 9);
    assert_eq!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Preparing { done: 3, total: 9 });
    assert_eq!(rig.host.plans.answer(&rig.host, &id, PlanAnswer::Cancel).unwrap().state, PlanState::Cancelled);
    assert!(flag.load(Ordering::SeqCst));
    assert!(rig.host.plans.publish(&id, content(&rig, &fx, &TestApplier::default())).is_err());
}

#[tokio::test(start_paused = true)]
async fn the_state_has_the_seam_s_wire_form() {
    let v = serde_json::to_value(PlanState::Preparing { done: 3, total: 9 }).unwrap();
    assert_eq!(v, serde_json::json!({"kind": "preparing", "done": 3, "total": 9}));
    assert_eq!(serde_json::to_value(PlanState::Waiting).unwrap(), serde_json::json!({"kind": "waiting"}));
    assert_eq!(
        serde_json::to_value(PlanState::Failed { error: "x".into() }).unwrap(),
        serde_json::json!({"kind": "failed", "error": "x"})
    );
    let a: PlanAnswer = serde_json::from_str(r#"{"action":"apply","selection":{"groups":[{"groupId":"group:1","chosen":null,"included":["item:1"]}]}}"#).unwrap();
    assert!(matches!(a, PlanAnswer::Apply { .. }));
    assert_eq!(serde_json::from_str::<PlanAnswer>(r#"{"action":"cancel"}"#).unwrap(), PlanAnswer::Cancel);
    assert_eq!(serde_json::to_value(PlanKind::BlackHole).unwrap(), serde_json::json!("black_hole"));
}

// ---- events, front, log ----

#[tokio::test(start_paused = true)]
async fn events_and_front() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();

    let (a, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "a");
    assert_eq!(rig.names(), vec![("mcp-plan-updated".to_string(), a.clone())], "begin");
    assert_eq!(rig.fronts.load(Ordering::SeqCst), 1);
    rig.forget_events();
    rig.host.plans.publish(&a, content(&rig, &fx, &applier)).expect("publish");
    assert_eq!(rig.names(), vec![("mcp-plan-updated".to_string(), a.clone())], "publish");

    rig.forget_events();
    let (b, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "b");
    assert_eq!(
        rig.names(),
        vec![("mcp-plan-updated".to_string(), a.clone()), ("mcp-plan-updated".to_string(), b.clone())],
        "a replacement tells the window about both plans"
    );
    assert_eq!(rig.fronts.load(Ordering::SeqCst), 2, "once per begin");

    rig.forget_events();
    rig.host.plans.publish(&b, content(&rig, &fx, &applier)).expect("publish");
    rig.forget_events();
    rig.host.plans.answer(&rig.host, &b, apply_of(selection(Some(&fx.keeper), &[&fx.a]))).expect("apply");
    settled(&rig, &b).await;
    assert_eq!(
        rig.names(),
        vec![("mcp-plan-updated".to_string(), b.clone()), ("mcp-plan-updated".to_string(), b.clone())],
        "apply start and applied"
    );

    let (c, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "c");
    rig.forget_events();
    rig.host.plans.answer(&rig.host, &c, PlanAnswer::Cancel).expect("cancel");
    assert_eq!(rig.names(), vec![("mcp-plan-updated".to_string(), c.clone())], "cancel");

    let (d, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "d");
    rig.host.plans.publish(&d, content(&rig, &fx, &applier)).expect("publish");
    rig.forget_events();
    tokio::time::advance(PLAN_TTL + Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(rig.names(), vec![("mcp-plan-updated".to_string(), d.clone())], "expiry");

    assert_eq!(rig.fronts.load(Ordering::SeqCst), 4, "four begins, four times to the front");
    assert!(rig.events.0.lock().unwrap().iter().all(|(n, _)| n == "mcp-plan-updated"));
}

/// tracing-core shortcuts interest to the registering thread's default while only ONE dispatcher exists: a parallel
/// test's thread (logging to none) could then reach a line first and mark it never logged, process-wide. Every test
/// that installs its own subscriber calls this before `set_default`.
fn second_dispatcher() {
    static SECOND_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND_DISPATCH.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}

/// A scoped log capture: the lines of everything logged on this thread (and the apply thread, which inherits the
/// caller's dispatcher).
fn capture() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<u8>>>) {
    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }
    second_dispatcher();
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Buf(Arc::clone(&buf)))
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    (tracing::subscriber::set_default(subscriber), buf)
}

#[tokio::test(start_paused = true)]
async fn the_apply_is_logged_with_its_plan() {
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let (guard, buf) = capture();
    let id = waiting(&rig, &fx, &applier);
    std::fs::write(&fx.b.path, vec![7u8; 55]).expect("rewrite");
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.b]))).expect("apply");
    settled(&rig, &id).await;
    drop(guard);
    let log = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    let line = log.lines().find(|l| l.contains("mcp plan applied")).unwrap_or_else(|| panic!("no apply line in:\n{log}"));
    assert!(line.contains(r#"via="mcp""#), "{line}");
    assert!(line.contains(&id), "{line}");
    assert!(line.contains(r#"kind="black_hole""#), "{line}");
    assert!(line.contains("applied=1") && line.contains("skipped=1") && line.contains("failed=0"), "{line}");
    assert!(line.contains("mcp_plan_apply{"), "the line is inside the apply span: {line}");
}

#[tokio::test(start_paused = true)]
async fn plan_opened_text_follows_the_viewer() {
    let rig = rig();
    assert_eq!(plan_opened_text(&rig.host), "the plan is open in Athenaeum's window");
    rig.viewer.store(false, Ordering::SeqCst);
    let said = plan_opened_text(&rig.host);
    assert!(said.contains("open Athenaeum to decide"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn finished_plans_are_kept_a_day_and_then_forgotten() {
    let rig = rig();
    let (a, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "a");
    let (b, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "b");
    tokio::time::advance(Duration::from_secs(23 * 3600)).await;
    assert!(rig.host.plans.view(Some(&a)).is_some());
    tokio::time::advance(Duration::from_secs(2 * 3600)).await;
    assert!(rig.host.plans.view(Some(&a)).is_none(), "a replaced plan goes after 24 h");
    assert!(rig.host.plans.view(Some(&b)).is_some(), "an undecided plan is never dropped");
}

// ---- fix round 1 ----

/// The catalog's pool holds this many connections (`Database::new`).
const POOL_SIZE: u32 = 8;

#[tokio::test(start_paused = true)]
async fn the_applier_finds_the_whole_pool_free() {
    /// Checks out every pooled connection at once, as an applier that calls core's own commands may.
    struct TakesThePool(Arc<AtomicUsize>);
    impl PlanApplier for TakesThePool {
        fn validate(&self, _v: &McpPlanView, _s: &PlanSelection) -> Result<(), String> {
            Ok(())
        }
        fn apply(&self, host: &McpHost, _p: &str, _s: &PlanSelection, fresh: &[String]) -> PlanReport {
            let database = host.ctx().db.get().expect("catalog");
            let held: Vec<_> = (0..POOL_SIZE).filter_map(|_| database.try_conn().ok()).collect();
            self.0.store(held.len(), Ordering::SeqCst);
            PlanReport {
                applied: fresh
                    .iter()
                    .map(|id| ReportLine { item_id: id.clone(), file_name: id.clone(), folder: String::new(), reason: "done".into() })
                    .collect(),
                ..Default::default()
            }
        }
    }
    let rig = rig();
    let fx = fixture(&rig);
    let got = Arc::new(AtomicUsize::new(0));
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &TestApplier::default());
    c.applier = Box::new(TakesThePool(Arc::clone(&got)));
    rig.host.plans.publish(&id, c).expect("publish");
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))).expect("apply");
    let done = settled(&rig, &id).await;
    assert_eq!(done.state, PlanState::Applied);
    assert_eq!(got.load(Ordering::SeqCst), POOL_SIZE as usize, "the apply holds no pooled connection across the applier");
}

#[tokio::test(start_paused = true)]
async fn preparing_progress_emits_throttled_events() {
    let rig = rig();
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    rig.forget_events();
    for i in 0..1000 {
        rig.host.plans.progress(&id, i, 1001);
        tokio::time::advance(Duration::from_micros(100)).await; // 100 ms in all
    }
    assert!(rig.names().len() <= 2, "{} events", rig.names().len());
    assert!(!rig.names().is_empty(), "the first call tells the window");
    assert!(rig.names().iter().all(|(n, p)| n == "mcp-plan-updated" && *p == id));
    assert_eq!(rig.host.plans.view(Some(&id)).unwrap().state, PlanState::Preparing { done: 999, total: 1001 });

    let before = rig.names().len();
    rig.host.plans.progress(&id, 1001, 1001);
    assert_eq!(rig.names().len(), before + 1, "the final call always emits");

    // 250 ms later the next one is due again.
    tokio::time::advance(Duration::from_millis(250)).await;
    rig.host.plans.progress(&id, 1, 2);
    assert_eq!(rig.names().len(), before + 2);
}

/// A waiting plan whose applier panics, with one item changed on disk so the store has a skip line of its own.
struct Panics;
impl PlanApplier for Panics {
    fn validate(&self, _v: &McpPlanView, _s: &PlanSelection) -> Result<(), String> {
        Ok(())
    }
    fn apply(&self, _h: &McpHost, _p: &str, _s: &PlanSelection, _f: &[String]) -> PlanReport {
        panic!("the applier broke");
    }
}

#[tokio::test(start_paused = true)]
async fn a_panicking_applier_fails_the_plan_and_keeps_the_skip_lines() {
    let rig = rig();
    let fx = fixture(&rig);
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &TestApplier::default());
    c.applier = Box::new(Panics);
    rig.host.plans.publish(&id, c).expect("publish");
    std::fs::write(&fx.a.path, vec![7u8; 99]).expect("rewrite");
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a, &fx.b]))).expect("apply");
    let done = settled(&rig, &id).await;
    assert!(matches!(done.state, PlanState::Failed { .. }), "{:?}", done.state);
    let report = done.report.expect("the failed plan keeps the store's skip lines");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].item_id, fx.a.item());
    assert!(report.applied.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_panic_outside_the_applier_still_ends_the_plan() {
    // The re-check has no input a test can make panic; the closing log line of the run is a step after the applier's
    // own guard, so a log writer that panics on it stands for any panic there.
    // Once: the fmt layer keeps its half-written line after a panic, so every later line would carry the match too.
    static BROKEN: AtomicBool = AtomicBool::new(false);
    #[derive(Clone)]
    struct Boom;
    impl std::io::Write for Boom {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if String::from_utf8_lossy(b).contains("mcp plan applied") && !BROKEN.swap(true, Ordering::SeqCst) {
                panic!("the log writer broke");
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Boom {
        type Writer = Boom;
        fn make_writer(&'a self) -> Boom {
            self.clone()
        }
    }
    let rig = rig();
    let fx = fixture(&rig);
    let applier = TestApplier::default();
    let id = waiting(&rig, &fx, &applier);
    second_dispatcher();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt().with_writer(Boom).with_max_level(tracing::Level::DEBUG).with_ansi(false).finish(),
    );
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))).expect("apply");
    let done = settled(&rig, &id).await;
    drop(guard);
    assert!(matches!(done.state, PlanState::Failed { .. }), "{:?}", done.state);
    assert_eq!(applier.calls.load(Ordering::SeqCst), 1, "the applier had run");
}

#[tokio::test(start_paused = true)]
async fn a_group_the_window_left_out_shows_nothing_applied() {
    let rig = rig();
    let fx = fixture(&rig);
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &TestApplier::default());
    // The planner proposes a second group with ticks and a kept copy; the window answers for the first only.
    let mut second = c.groups[0].clone();
    second.id = "group:2".to_string();
    second.chosen = Some(fx.keeper.item());
    for item in &mut second.items {
        item.selected = true;
    }
    c.groups.push(second);
    rig.host.plans.publish(&id, c).expect("publish");
    rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a]))).expect("apply");
    let done = settled(&rig, &id).await;
    assert_eq!(done.state, PlanState::Applied);
    assert!(done.groups[0].items.iter().any(|i| i.selected));
    assert_eq!(done.groups[1].chosen, None);
    assert!(done.groups[1].items.iter().all(|i| !i.selected), "nothing of an omitted group was applied");
}

#[tokio::test(start_paused = true)]
async fn unreachable_items_are_logged_once_per_plan() {
    let rig = rig();
    let fx = fixture(&rig);
    let (guard, buf) = capture();
    let (id, _) = rig.host.plans.begin(&rig.host, PlanKind::BlackHole, "t");
    let mut c = content(&rig, &fx, &TestApplier::default());
    for (n, item) in c.groups[0].items.iter_mut().enumerate() {
        let gone = rig.base.join("not_mounted").join(format!("frame_{n}"));
        item.locked = stamp_for_plan(&gone).err();
    }
    rig.host.plans.publish(&id, c).expect("publish");
    drop(guard);
    let log = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    let warns: Vec<&str> = log.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warns.len(), 1, "{log}");
    assert!(warns[0].contains("count=5") && warns[0].contains("keeper_1"), "{}", warns[0]);
    assert!(warns[0].contains(&id), "{}", warns[0]);
}

#[tokio::test(start_paused = true)]
async fn two_applies_racing_one_plan_apply_once() {
    let rig = rig();
    let fx = fixture(&rig);
    for _ in 0..20 {
        let applier = TestApplier::default();
        let id = waiting(&rig, &fx, &applier);
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<Result<McpPlanView, ApiError>> = std::thread::scope(|s| {
            let racers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a])))
                    })
                })
                .collect();
            racers.into_iter().map(|r| r.join().expect("racer")).collect()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
        assert!(results.iter().any(|r| matches!(r, Err(ApiError::Invalid(_)))), "{results:?}");
        assert_eq!(settled(&rig, &id).await.state, PlanState::Applied);
        assert_eq!(applier.calls.load(Ordering::SeqCst), 1, "the applier ran once");
    }
}

#[tokio::test(start_paused = true)]
async fn a_cancel_racing_an_apply_has_one_winner() {
    let rig = rig();
    let fx = fixture(&rig);
    for _ in 0..20 {
        let applier = TestApplier::default();
        let id = waiting(&rig, &fx, &applier);
        let barrier = std::sync::Barrier::new(2);
        let (applied, cancelled) = std::thread::scope(|s| {
            let a = s.spawn(|| {
                barrier.wait();
                rig.host.plans.answer(&rig.host, &id, apply_of(selection(Some(&fx.keeper), &[&fx.a])))
            });
            let c = s.spawn(|| {
                barrier.wait();
                rig.host.plans.answer(&rig.host, &id, PlanAnswer::Cancel)
            });
            (a.join().expect("apply"), c.join().expect("cancel"))
        });
        let end = settled(&rig, &id).await.state;
        match (applied.is_ok(), cancelled.is_ok()) {
            (true, false) => {
                assert!(matches!(cancelled, Err(ApiError::Invalid(_))));
                assert_eq!(end, PlanState::Applied);
                assert_eq!(applier.calls.load(Ordering::SeqCst), 1);
            }
            (false, true) => {
                assert!(matches!(applied, Err(ApiError::Invalid(_))));
                assert_eq!(end, PlanState::Cancelled);
                assert_eq!(applier.calls.load(Ordering::SeqCst), 0, "a cancelled plan is never applied");
            }
            other => panic!("exactly one of apply and cancel wins, got {other:?}"),
        }
    }
}
