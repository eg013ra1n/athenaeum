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

/// Spins (no clock) until the apply thread has ended the plan.
async fn settled(rig: &Rig, id: &str) -> McpPlanView {
    loop {
        let v = rig.host.plans.view(Some(id)).expect("plan");
        if v.state != PlanState::Applying {
            return v;
        }
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
    // tracing-core shortcuts interest to the registering thread's default while only ONE dispatcher exists.
    static SECOND_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND_DISPATCH.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
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
