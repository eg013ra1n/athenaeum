//! Collab v3 wave 2 capstone (plan Task 13, P16, spec §15 subset): three
//! instances exchange per-frame calibrated lights through ONE stateful fake
//! hub and three REAL relay-disabled iroh nodes, and a disk ledger proves the
//! one-copy-per-machine rule on every side.
//!
//! - A — a `send` contributor: publishes six `FILTER = "Red"` lights.
//! - B — a `send_receive` processor: replicates, loses files, re-fetches,
//!   stops holding, takes a new version, exports.
//! - C — the coordinator (`send_receive`): moderates, then replicates from
//!   B alone once A is gone.
//!
//! Every numbered step of the plan is one helper, so a failure names its
//! step. `#[cfg(unix)]` and a multi-thread runtime, the way the package-era
//! e2e was gated; the nodes' accept loops run on background tasks.
//!
//! What is driven directly (the app runs these on its own loops): the
//! version poll (`poll_versions_once`), the fetch pass
//! (`replication_pass`), and the maintenance loop's per-tick function
//! (`run_maintenance`: disk truth, the loss guard, the full holder report,
//! the parked-frame recheck). No worker is spawned, so nothing runs behind
//! the test's back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::api::collab::tests::publish::{
    seed_real_light_set, set_mtime, write_dark, RealLightSet,
};
use crate::api::collab::tests::seed_plate_solve;
use crate::api::collab::{
    approve_collab_frame, auto_publish_collab_frames, link_frame_set, list_moderation_queue,
    publish_collab_frames, republish_collab_frames, PublishResult, PUBLISH_BUSY_MSG,
};
use crate::api::collab_exchange::{
    export_project_for_wbpp, poll_versions_once, replication_pass, resolve_collab_loss,
    run_maintenance, set_collab_policy, AutoSyncPassOutcome, LossAction, PassKind,
    ReplicationPolicy, COLLAB_REPLICATION_PAUSED_EVENT,
};
use crate::api::db;
use crate::collab::fake_hub::FakeHub;
use crate::db::collab_frames::{self as frames_db, LocalFrameRow};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::sharing::iroh::node::{project_frame_tag, test_gc, BlobHealth, Role, SharedIrohNode};

const PID: &str = "p-m31";
/// Frames A publishes.
const N: usize = 6;
/// The plan's ceiling for any file under B's working dir: the personal
/// store must never hold a frame's bytes.
const WORKING_DIR_FILE_CAP: u64 = 64 * 1024;

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A file-backed-`Database` [`ServiceContext`] (no keychain), the same shape
/// as the `api::collab` / `api::collab_exchange` test contexts.
fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
    use crate::cache::MemoryImageCache;
    use crate::services::compute_queue::ComputeQueue;
    use crate::services::operation_queue::OperationQueue;
    use crate::settings::SettingsManager;
    use std::sync::{Mutex, OnceLock, RwLock};

    let tmp = tempfile::tempdir().unwrap();
    let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
    let db_cell = OnceLock::new();
    let _ = db_cell.set(database);
    let ctx = ServiceContext {
        db: db_cell,
        settings: Arc::new(SettingsManager::new()),
        memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
        active_scans: Arc::new(Mutex::new(HashMap::new())),
        active_exports: Arc::new(Mutex::new(HashMap::new())),
        active_analyses: Arc::new(Mutex::new(HashMap::new())),
        active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
        active_archives: Arc::new(Mutex::new(HashMap::new())),
        active_master_builds: Arc::new(Mutex::new(HashMap::new())),
        active_stacks: Arc::new(Mutex::new(HashMap::new())),
        dso_catalog: Arc::new(RwLock::new(None)),
        image_pool: Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap(),
        ),
        operation_queue: OperationQueue::start(),
        compute_queue: ComputeQueue::new(),
        iroh_node: Arc::new(tokio::sync::Mutex::new(None)),
    };
    (tmp, ctx)
}

/// One app instance: its context, its node, its Collaboration root.
struct Member {
    tmp: tempfile::TempDir,
    ctx: Arc<ServiceContext>,
    node: Arc<SharedIrohNode>,
    collab: PathBuf,
    working_dir: PathBuf,
    account: &'static str,
    display: &'static str,
}

impl Member {
    fn pubkey(&self) -> String {
        B64.encode(self.node.node_id())
    }

    fn rows(&self) -> Vec<LocalFrameRow> {
        frames_db::list_for_project(&db(&self.ctx).unwrap().conn(), PID).unwrap()
    }

    fn row(&self, uuid: &str) -> LocalFrameRow {
        frames_db::get(&db(&self.ctx).unwrap().conn(), PID, uuid)
            .unwrap()
            .unwrap_or_else(|| panic!("{}: no row for {uuid}", self.display))
    }

    fn landed(&self, uuid: &str) -> PathBuf {
        PathBuf::from(
            self.row(uuid)
                .landed_path
                .unwrap_or_else(|| panic!("{}: {uuid} has no landed path", self.display)),
        )
    }

    fn paused(&self) -> bool {
        crate::db::collab::get_project(&db(&self.ctx).unwrap().conn(), PID)
            .unwrap()
            .unwrap()
            .replication_paused
    }
}

/// Bind a relay-disabled node on `ctx` (where `ensure_iroh_node` leaves it in
/// production) — also the "restart" of an instance: same identity dir, same
/// node id.
async fn bind_node(ctx: &ServiceContext) -> Arc<SharedIrohNode> {
    let dirs = crate::api::sync::sync_dirs(ctx).unwrap();
    std::fs::create_dir_all(&dirs.identity_dir).unwrap();
    std::fs::create_dir_all(&dirs.working_dir).unwrap();
    let node = SharedIrohNode::bind_with(
        &dirs.identity_dir,
        &dirs.working_dir,
        iroh::RelayMode::Disabled,
        crate::sharing::iroh::node::NodeOptions::default(),
    )
    .await
    .expect("bind relay-disabled node");
    *ctx.iroh_node.lock().await = Some(Arc::clone(&node));
    node
}

/// A signed-in instance with a bound node and its Collaboration root
/// `<tmp>/Collab` designated (which mounts the collab store on the node).
async fn member(
    hub: &FakeHub,
    token: &'static str,
    account: &'static str,
    display: &'static str,
) -> Member {
    let (tmp, ctx) = test_ctx();
    {
        let conn = db(&ctx).unwrap().conn();
        crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, &hub.uri()).unwrap();
    }
    crate::api::account::store_token_for_test(&ctx, token).unwrap();
    let node = bind_node(&ctx).await;
    hub.add_account(token, account, display, &B64.encode(node.node_id()), None);
    let requested = tmp.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    let collab = PathBuf::from(
        crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            requested.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap(),
    );
    assert!(
        node.collab_store().is_some(),
        "{display}: collab store mounted"
    );
    let working_dir = crate::api::sync::sync_dirs(&ctx).unwrap().working_dir;
    Member {
        tmp,
        ctx: Arc::new(ctx),
        node,
        collab,
        working_dir,
        account,
        display,
    }
}

/// Relay-disabled nodes have no discovery: exchange addresses.
async fn pair(a: &Arc<SharedIrohNode>, b: &Arc<SharedIrohNode>) {
    for n in [a, b] {
        n.handle(Role::Out).start().await.unwrap();
    }
    a.add_peer(b.endpoint_addr());
    b.add_peer(a.endpoint_addr());
}

/// Records every emitted event.
#[derive(Default)]
struct RecordingEmitter {
    events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
}

impl ProgressEmitter for RecordingEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((event_name.to_string(), payload));
    }
}

impl RecordingEmitter {
    fn count(&self, name: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == name)
            .count()
    }
}

/// Every regular file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn dir_bytes(dir: &Path) -> u64 {
    files_under(dir)
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn xxh3_of(path: &Path) -> String {
    crate::package::xxh3_full_file(path).unwrap()
}

/// Total bytes a node's endpoint has sent since bind — the "who served
/// payload" oracle (iroh's own socket counters).
fn sent_bytes(node: &SharedIrohNode) -> u64 {
    let c = node.counters_snapshot_for_test();
    c.send_direct_bytes.saturating_add(c.send_relay_bytes)
}

/// Bytes under a member's working dir plus its collab store — everything the
/// app keeps beside the frame files themselves.
fn side_bytes(m: &Member) -> u64 {
    dir_bytes(&m.working_dir) + dir_bytes(&m.collab.join(".athenaeum"))
}

/// A fetch pass (what a version kick runs), bounded.
async fn fetch_pass(m: &Member) -> AutoSyncPassOutcome {
    let sync = crate::sync::SyncRuntime::new();
    tokio::time::timeout(
        Duration::from_secs(120),
        replication_pass(&m.ctx, &sync, PassKind::Fetch, None, None),
    )
    .await
    .unwrap_or_else(|_| panic!("{}: a fetch pass took over two minutes", m.display))
}

/// The maintenance loop's tick (disk truth, loss guard, full holders,
/// parked-frame recheck), bounded.
async fn maintenance(m: &Member, emitter: Option<&dyn ProgressEmitter>) {
    tokio::time::timeout(
        Duration::from_secs(60),
        run_maintenance(&m.ctx, None, emitter),
    )
    .await
    .unwrap_or_else(|_| panic!("{}: maintenance took over a minute", m.display));
}

async fn poll(m: &Member) -> Vec<String> {
    poll_versions_once(&m.ctx, None)
        .await
        .unwrap_or_else(|e| panic!("{}: version poll failed: {e:?}", m.display))
}

/// The one publisher folder under `<collab>/m31/`.
fn publisher_dir(m: &Member) -> PathBuf {
    let project_dir = m.collab.join("m31");
    let dirs: Vec<PathBuf> = std::fs::read_dir(&project_dir)
        .unwrap_or_else(|e| panic!("{}: read {}: {e}", m.display, project_dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        dirs.len(),
        1,
        "{}: one publisher folder: {dirs:?}",
        m.display
    );
    dirs[0].clone()
}

/// The rig after step 1.
struct World {
    hub: FakeHub,
    a: Member,
    b: Member,
    c: Member,
    set: RealLightSet,
    /// B's store GC gate (P20: closed except when a test lets GC run).
    b_gc: Arc<AtomicBool>,
    /// Sum of A's six published file sizes after step 2.
    payload: u64,
    /// The ledger's zero: bytes under A's working dir + collab store, and
    /// under B's Collaboration root, with the stores open and empty. Each
    /// store's database is preallocated (1 MiB of redb, whatever it holds),
    /// so the ledger measures what the exchange ADDS on top.
    a_baseline: u64,
    b_root_baseline: u64,
    /// Every file under B's working dir at step 1, with its size.
    b_working_baseline: HashMap<PathBuf, u64>,
    /// uuid → A's published file (name) after step 2.
    a_files: HashMap<String, PathBuf>,
}

// ── Steps ────────────────────────────────────────────────────────────────────

/// Step 1: one fake hub; the project requires approval, carries the default
/// dictionary (`R` has the alias `Red`) and no thresholds (gate version 0).
/// A holds six publishable `FILTER = "Red"` lights linked to one real master
/// dark, three of them plate-solved. B's store GC is gated for step 6.
async fn step1_rig() -> World {
    let hub = FakeHub::start().await;
    hub.add_project(
        PID,
        "m31",
        &[
            ("acc-a", "send", false),
            ("acc-b", "send_receive", false),
            ("acc-c", "send_receive", true),
        ],
        true,
    );
    let a = member(&hub, "tok-a", "acc-a", "Alice").await;
    let c = member(&hub, "tok-c", "acc-c", "Carol").await;
    // B's stores run GC every 100 ms, but only while the gate is open (the
    // override is read on this thread when the store opens).
    let b_gc = Arc::new(AtomicBool::new(false));
    test_gc::arm(Some(Arc::clone(&b_gc)));
    let b = member(&hub, "tok-b", "acc-b", "Bob").await;
    test_gc::arm(None);
    pair(&a.node, &b.node).await;
    pair(&b.node, &c.node).await;

    for m in [&a, &b, &c] {
        let cards = crate::api::collab::refresh_projects(&m.ctx)
            .await
            .unwrap_or_else(|e| panic!("step 1: {} refresh: {e:?}", m.display));
        assert!(
            cards.iter().any(|p| p.project_id == PID),
            "step 1: {} sees the project",
            m.display
        );
    }

    let set = {
        let conn = db(&a.ctx).unwrap().conn();
        let set = seed_real_light_set(&conn, a.tmp.path(), N, "Red");
        for frame_id in &set.frame_ids[..3] {
            seed_plate_solve(&conn, *frame_id, 0.776, 10.68, 41.27);
        }
        set
    };
    link_frame_set(&a.ctx, PID, set.set_id).unwrap();
    let a_baseline = side_bytes(&a);
    let b_root_baseline = dir_bytes(&b.collab);
    let b_working_baseline = files_under(&b.working_dir)
        .into_iter()
        .map(|f| {
            let len = f.metadata().unwrap().len();
            (f, len)
        })
        .collect();
    World {
        hub,
        a,
        b,
        c,
        set,
        a_baseline,
        b_root_baseline,
        b_working_baseline,
        b_gc,
        payload: 0,
        a_files: HashMap::new(),
    }
}

/// Step 2: A publishes — six `pending` announcements, filter `Red` mapped to
/// `R`. Ledger on A: exactly six files in its own folder, and everything
/// else under its working dir and its collab store is under 1 % of them.
async fn step2_a_publishes(w: &mut World) {
    let res = publish_collab_frames(&w.a.ctx, PID, None)
        .await
        .expect("step 2: publish");
    assert_eq!(
        (res.announced, res.updated, res.unchanged),
        (N, 0, 0),
        "step 2: {res:?}"
    );
    assert_eq!(res.state.as_deref(), Some("pending"), "step 2: {res:?}");
    assert!(res.held_back.is_empty(), "step 2: {:?}", res.held_back);
    for uuid in &w.set.uuids {
        let f = w.hub.frame(PID, uuid).expect("step 2: announced");
        assert_eq!(f.state, "pending", "step 2: {uuid}");
        assert_eq!(
            (f.filter_raw.as_str(), f.filter_canonical.as_str()),
            ("Red", "R"),
            "step 2: {uuid} filter"
        );
        assert_eq!(f.gate_version, 0, "step 2: {uuid}");
    }
    let solved = w
        .set
        .uuids
        .iter()
        .filter(|u| w.hub.frame(PID, u).unwrap().meta.get("wcs").is_some())
        .count();
    assert_eq!(
        solved, 3,
        "step 2: the three plate-solved frames carry meta.wcs"
    );

    let own = publisher_dir(&w.a);
    let files = files_under(&own);
    assert_eq!(
        files.len(),
        N,
        "step 2: exactly six files in {}",
        own.display()
    );
    w.payload = files.iter().map(|p| p.metadata().unwrap().len()).sum();
    for uuid in &w.set.uuids {
        let landed = w.a.landed(uuid);
        assert_eq!(landed.parent(), Some(own.as_path()), "step 2: {uuid}");
        w.a_files.insert(uuid.clone(), landed);
    }
    let overhead = side_bytes(&w.a).saturating_sub(w.a_baseline);
    assert!(
        overhead * 100 < w.payload,
        "step 2: A's working dir + collab store grew {overhead} B beside {} B of frames",
        w.payload
    );
}

/// Step 3: C lists six frames for moderation and approves one with trust —
/// the hub publishes all six.
async fn step3_c_approves_with_trust(w: &World) {
    poll(&w.c).await;
    let queue = list_moderation_queue(&w.c.ctx, PID).expect("step 3: queue");
    assert_eq!(queue.len(), N, "step 3: {queue:?}");
    assert!(queue.iter().all(|f| f.publisher_account_id == w.a.account));
    approve_collab_frame(&w.c.ctx, PID, &queue[0].frame_uuid, true)
        .await
        .expect("step 3: approve with trust");
    for uuid in &w.set.uuids {
        assert_eq!(
            w.hub.frame(PID, uuid).unwrap().state,
            "published",
            "step 3: trust publishes {uuid}"
        );
    }
    assert!(list_moderation_queue(&w.c.ctx, PID).unwrap().is_empty());
}

/// Step 4: B polls and runs a pass — six byte-identical files land under
/// `<B collab>/m31/<A>/`. Ledger on B: its collab root is the payload plus
/// under 1 %, no file under its working dir exceeds 64 KiB, and it is a
/// fresh holder of all six.
async fn step4_b_replicates(w: &World) {
    let moved = poll(&w.b).await;
    assert!(
        moved.iter().any(|p| p == PID),
        "step 4: poll moved {moved:?}"
    );
    let out = fetch_pass(&w.b).await;
    assert_eq!((out.landed, out.failed), (N, 0), "step 4: {out:?}");

    let dir = publisher_dir(&w.b);
    assert_eq!(
        dir.file_name(),
        publisher_dir(&w.a).file_name(),
        "step 4: the publisher folder is A's name"
    );
    let landed = files_under(&dir);
    assert_eq!(landed.len(), N, "step 4: {landed:?}");
    for uuid in &w.set.uuids {
        let b_path = w.b.landed(uuid);
        assert_eq!(b_path.parent(), Some(dir.as_path()), "step 4: {uuid}");
        let a_path = &w.a_files[uuid];
        assert_eq!(
            b_path.file_name(),
            a_path.file_name(),
            "step 4: {uuid} name"
        );
        assert_eq!(xxh3_of(&b_path), xxh3_of(a_path), "step 4: {uuid} bytes");
        assert!(w.b.row(uuid).on_disk, "step 4: {uuid} on disk");
        assert!(
            w.hub.holders_of(PID, uuid).contains(&w.b.pubkey()),
            "step 4: B holds {uuid} in the hub"
        );
    }

    let grown = dir_bytes(&w.b.collab).saturating_sub(w.b_root_baseline);
    assert!(
        grown >= w.payload && (grown - w.payload) * 100 < w.payload,
        "step 4: B's collab root grew {grown} B for {} B of frames",
        w.payload
    );
    // No frame bytes in the personal store: every file is under the cap,
    // except the store's own preallocated database, untouched since step 1.
    let mut working_total = 0;
    for f in files_under(&w.b.working_dir) {
        let len = f.metadata().unwrap().len();
        working_total += len;
        assert!(
            len <= WORKING_DIR_FILE_CAP || w.b_working_baseline.get(&f) == Some(&len),
            "step 4: {} is {len} B under B's working dir",
            f.display()
        );
    }
    let working_base: u64 = w.b_working_baseline.values().sum();
    assert!(
        working_total.saturating_sub(working_base) <= WORKING_DIR_FILE_CAP,
        "step 4: B's working dir grew {} B",
        working_total.saturating_sub(working_base)
    );
}

/// Step 5: A goes offline. C narrows its policy to `R` and runs a pass: all
/// six come from B alone — received frames are seeds.
async fn step5_c_fetches_from_b_alone(w: &World) {
    w.a.node.shutdown().await;
    let preview = set_collab_policy(
        &w.c.ctx,
        PID,
        ReplicationPolicy {
            filters: vec!["R".into()],
            ..Default::default()
        },
    )
    .await
    .expect("step 5: policy");
    assert_eq!(
        (preview.frames, preview.to_fetch),
        (N, N),
        "step 5: {preview:?}"
    );

    let before = sent_bytes(&w.b.node);
    let out = fetch_pass(&w.c).await;
    assert_eq!((out.landed, out.failed), (N, 0), "step 5: {out:?}");
    let served = sent_bytes(&w.b.node).saturating_sub(before);
    assert!(
        served >= w.payload,
        "step 5: B served {served} B of {} B",
        w.payload
    );
    for uuid in &w.set.uuids {
        assert_eq!(
            xxh3_of(&w.c.landed(uuid)),
            xxh3_of(&w.a_files[uuid]),
            "step 5: C's {uuid}"
        );
    }
}

/// Step 6: B deletes one landed file. Maintenance drops B as its holder in
/// the hub (F5: no phantom holder). The dead store entry parks the frame
/// until GC (P20); after a forced GC the next cycle re-fetches it from C.
async fn step6_one_deleted_replica_comes_back_from_c(w: &World) {
    let uuid = &w.set.uuids[0];
    let path = w.b.landed(uuid);
    let bytes = std::fs::read(&path).unwrap();
    let hash: iroh_blobs::Hash = w.b.row(uuid).blake3.parse().expect("blake3 hex");
    std::fs::remove_file(&path).unwrap();

    maintenance(&w.b, None).await;
    assert!(
        !w.hub.holders_of(PID, uuid).contains(&w.b.pubkey()),
        "step 6: the hub no longer lists B as a holder of {uuid}"
    );
    assert!(!w.b.paused(), "step 6: one loss does not trip the guard");
    let row = w.b.row(uuid);
    assert!(!row.on_disk && row.awaiting_gc, "step 6: parked: {row:?}");
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        out.attempted, 0,
        "step 6: nothing fetched over a dead entry: {out:?}"
    );

    w.b_gc.store(true, Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while w.b.node.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
        assert!(
            tokio::time::Instant::now() < deadline,
            "step 6: GC never dropped the dead entry"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    w.b_gc.store(false, Ordering::SeqCst);

    let before = sent_bytes(&w.c.node);
    maintenance(&w.b, None).await;
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        (out.landed, out.failed),
        (1, 0),
        "step 6: re-fetch: {out:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "step 6: the frame is back"
    );
    let served = sent_bytes(&w.c.node).saturating_sub(before);
    assert!(
        served >= bytes.len() as u64,
        "step 6: C served {served} B of {} B",
        bytes.len()
    );
    assert!(
        w.hub.holders_of(PID, uuid).contains(&w.b.pubkey()),
        "step 6: B holds {uuid} again"
    );
}

/// Step 7: B deletes four of six at once. The loss guard pauses and emits
/// `collab-replication-paused`; `StopHolding` declines the four and
/// unpauses; the next cycle fetches nothing.
async fn step7_mass_loss_trips_the_guard(w: &World) -> HashSet<String> {
    let lost: HashSet<String> = w.set.uuids[1..5].iter().cloned().collect();
    for uuid in &lost {
        std::fs::remove_file(w.b.landed(uuid)).unwrap();
    }
    let em = RecordingEmitter::default();
    maintenance(&w.b, Some(&em)).await;
    assert!(w.b.paused(), "step 7: the guard paused replication");
    assert_eq!(
        em.count(COLLAB_REPLICATION_PAUSED_EVENT),
        1,
        "step 7: one pause event"
    );

    resolve_collab_loss(
        Arc::clone(&w.b.ctx),
        Arc::new(crate::sync::SyncRuntime::new()),
        PID,
        LossAction::StopHolding,
        None,
    )
    .await
    .expect("step 7: stop holding");
    assert!(!w.b.paused(), "step 7: unpaused");
    for row in w.b.rows() {
        assert_eq!(
            row.locally_declined,
            lost.contains(&row.frame_uuid),
            "step 7: declined only the lost: {row:?}"
        );
    }

    maintenance(&w.b, None).await;
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        (out.attempted, out.landed),
        (0, 0),
        "step 7: nothing fetched: {out:?}"
    );
    lost
}

/// Step 8: A comes back and re-publishes after its master dark CHANGED
/// PIXELS (an mtime-only touch would regenerate identical bytes and rightly
/// send no version, P19). B's next pass lands version 2 over version 1 for
/// every frame it still holds: one file each, same path, new bytes, the old
/// tag gone. The declined frames stay declined.
async fn step8_new_version_lands_over_the_old(w: &mut World, declined: &HashSet<String>) {
    let node = bind_node(&w.a.ctx).await;
    node.set_collab_root(Some(&w.a.collab))
        .await
        .expect("step 8: remount A's collab store");
    pair(&node, &w.b.node).await;
    w.a.node = node;

    write_dark(&w.set.master, 310.0);
    set_mtime(&w.set.master, 120);
    let res = publish_collab_frames(&w.a.ctx, PID, None)
        .await
        .expect("step 8: re-publish");
    assert_eq!(
        (res.announced, res.updated, res.unchanged),
        (0, N, 0),
        "step 8: {res:?}"
    );

    let held: Vec<&String> = w
        .set
        .uuids
        .iter()
        .filter(|u| !declined.contains(*u))
        .collect();
    let before: HashMap<&String, (PathBuf, String)> = held
        .iter()
        .map(|u| {
            let p = w.b.landed(u);
            let x = xxh3_of(&p);
            (*u, (p, x))
        })
        .collect();

    let moved = poll(&w.b).await;
    assert!(
        moved.iter().any(|p| p == PID),
        "step 8: poll moved {moved:?}"
    );
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        (out.attempted, out.landed, out.failed),
        (held.len(), held.len(), 0),
        "step 8: {out:?}"
    );
    for uuid in &held {
        let (old_path, old_xxh3) = &before[uuid];
        let row = w.b.row(uuid);
        assert_eq!(row.content_version, 2, "step 8: {uuid}");
        assert!(row.on_disk, "step 8: {uuid}");
        assert_eq!(&w.b.landed(uuid), old_path, "step 8: {uuid} same path");
        let new_xxh3 = xxh3_of(old_path);
        assert_ne!(&new_xxh3, old_xxh3, "step 8: {uuid} new bytes");
        assert_eq!(
            new_xxh3,
            xxh3_of(&w.a.landed(uuid)),
            "step 8: {uuid} = A's v2"
        );
        let store = w.b.node.collab_store().unwrap();
        for (version, present) in [(1, false), (2, true)] {
            let tag = project_frame_tag(PID, uuid, version);
            assert_eq!(
                store.tags().get(tag.as_bytes()).await.unwrap().is_some(),
                present,
                "step 8: {tag}"
            );
        }
    }
    for uuid in declined {
        assert_eq!(
            w.b.row(uuid).content_version,
            2,
            "step 8: {uuid} row synced"
        );
        assert!(
            !w.b.row(uuid).on_disk,
            "step 8: declined {uuid} not fetched"
        );
    }
    let files = files_under(&publisher_dir(&w.b));
    assert_eq!(
        files.len(),
        held.len(),
        "step 8: one file per held frame, no `_2` sibling: {files:?}"
    );
}

/// Run a manual Republish and an auto-publish of the project on A at the
/// same time (final review C1): exactly one runs, the other is refused with
/// the busy Conflict. Returns the run that went through.
async fn overlapping_publishes_on_a(w: &World, step: &str) -> PublishResult {
    let (manual, auto) = tokio::join!(
        republish_collab_frames(&w.a.ctx, PID, None),
        auto_publish_collab_frames(&w.a.ctx, PID, None)
    );
    let (ran, refused) = match (manual, auto) {
        (Ok(r), Err(e)) | (Err(e), Ok(r)) => (r, e),
        other => panic!("{step}: exactly one publish run must go through: {other:?}"),
    };
    match refused {
        crate::api::ApiError::Conflict(m) => assert_eq!(m, PUBLISH_BUSY_MSG, "{step}"),
        other => panic!("{step}: expected the busy Conflict, got {other:?}"),
    }
    assert!(ran.held_back.is_empty(), "{step}: {:?}", ran.held_back);
    ran
}

/// Step 8b (final review C1): overlapping Republish + auto-publish on A
/// never version the same bytes twice. With changed pixels exactly ONE new
/// version per frame reaches the hub (v3, never a v4 of identical bytes) and
/// B lands it once; with nothing changed, no version at all — and B's next
/// pass re-downloads nothing, its replicas intact (the pre-fix receiver
/// deleted its own replica on an identical-bytes bump).
async fn step8b_overlapping_publishes_never_double_version(w: &World, declined: &HashSet<String>) {
    let held: Vec<&String> = w
        .set
        .uuids
        .iter()
        .filter(|u| !declined.contains(*u))
        .collect();

    write_dark(&w.set.master, 320.0);
    set_mtime(&w.set.master, 240);
    let ran = overlapping_publishes_on_a(w, "step 8b (changed)").await;
    assert_eq!(ran.updated, N, "step 8b: {ran:?}");
    for uuid in &w.set.uuids {
        assert_eq!(
            w.hub.frame(PID, uuid).unwrap().content_version,
            3,
            "step 8b: one new version of {uuid}, not two"
        );
    }
    poll(&w.b).await;
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        (out.landed, out.failed),
        (held.len(), 0),
        "step 8b: B lands v3 once: {out:?}"
    );

    let ran = overlapping_publishes_on_a(w, "step 8b (unchanged)").await;
    assert_eq!((ran.announced, ran.updated), (0, 0), "step 8b: {ran:?}");
    let before: HashMap<&String, String> =
        held.iter().map(|u| (*u, xxh3_of(&w.b.landed(u)))).collect();
    let sent_before = sent_bytes(&w.a.node);
    poll(&w.b).await;
    maintenance(&w.b, None).await;
    let out = fetch_pass(&w.b).await;
    assert_eq!(
        (out.attempted, out.landed, out.failed),
        (0, 0, 0),
        "step 8b: nothing to fetch: {out:?}"
    );
    let sent = sent_bytes(&w.a.node).saturating_sub(sent_before);
    assert!(
        sent * 100 < w.payload,
        "step 8b: A served {sent} B — nothing may be re-downloaded"
    );
    for uuid in &held {
        let row = w.b.row(uuid);
        assert!(row.on_disk, "step 8b: {uuid} still held");
        assert_eq!(row.content_version, 3, "step 8b: {uuid}");
        let path = w.b.landed(uuid);
        assert_eq!(
            xxh3_of(&path),
            before[uuid],
            "step 8b: {uuid} replica intact"
        );
        assert_eq!(
            xxh3_of(&path),
            xxh3_of(&w.a.landed(uuid)),
            "step 8b: {uuid} = A's v3"
        );
        assert!(
            w.hub.holders_of(PID, uuid).contains(&w.b.pubkey()),
            "step 8b: B still holds {uuid}"
        );
    }
}

/// Step 9: after the coordinator excludes one held frame, B's WBPP project
/// export carries exactly the frames that are on disk AND accepted.
async fn step9_export_carries_on_disk_accepted_frames(w: &World, declined: &HashSet<String>) {
    let held: Vec<&String> = w
        .set
        .uuids
        .iter()
        .filter(|u| !declined.contains(*u))
        .collect();
    let excluded = held[1].clone();
    w.hub
        .set_accepted(PID, &excluded, false, Some("step 9 exclusion"));
    poll(&w.b).await;
    assert!(!w.b.row(&excluded).accepted, "step 9: B sees the exclusion");

    let expected: HashSet<String> =
        w.b.rows()
            .into_iter()
            .filter(|r| r.state == "published" && r.accepted && r.on_disk && !r.awaiting_gc)
            .map(|r| xxh3_of(Path::new(r.landed_path.as_deref().unwrap())))
            .collect();
    assert_eq!(expected.len(), 1, "step 9: one on-disk accepted frame");

    let out_dir = w.b.tmp.path().join("export");
    std::fs::create_dir_all(&out_dir).unwrap();
    let res = export_project_for_wbpp(&w.b.ctx, PID, &out_dir.to_string_lossy(), false, None)
        .await
        .expect("step 9: export");
    assert!(res.success, "step 9: {res:?}");
    assert_eq!(res.files_organized, 1, "step 9: {res:?}");
    let exported: HashSet<String> = files_under(&out_dir)
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "fits"))
        .map(|p| xxh3_of(p))
        .collect();
    assert_eq!(
        exported, expected,
        "step 9: exactly the on-disk accepted frames"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_instances_exchange_frames_with_one_copy_per_machine() {
    let mut w = step1_rig().await;
    step2_a_publishes(&mut w).await;
    step3_c_approves_with_trust(&w).await;
    step4_b_replicates(&w).await;
    step5_c_fetches_from_b_alone(&w).await;
    step6_one_deleted_replica_comes_back_from_c(&w).await;
    let declined = step7_mass_loss_trips_the_guard(&w).await;
    step8_new_version_lands_over_the_old(&mut w, &declined).await;
    step8b_overlapping_publishes_never_double_version(&w, &declined).await;
    step9_export_carries_on_disk_accepted_frames(&w, &declined).await;
    for m in [&w.a, &w.b, &w.c] {
        m.node.shutdown().await;
    }
}
