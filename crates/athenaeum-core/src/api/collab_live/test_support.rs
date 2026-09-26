//! Fixtures shared by every wave-3 `api::collab_live` test module (Tasks
//! 7-18, per-task rulings): a signed-in [`ServiceContext`] wired to a
//! [`FakeHub`], with a real relay-disabled iroh node bound and the
//! Collaboration root mounted. `signed_in_rig` is the one entry point;
//! `collab_root`/`my_device`/`seed_replica_file` build on it.
//!
//! P1 headless rule: this module (like the rest of `api::collab_live`) is
//! gated on `render`+`solver` — a test elsewhere that imports it must carry
//! the same gate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::api::PathPolicy;
use crate::collab::fake_hub::FakeHub;
use crate::services::ServiceContext;
use crate::sharing::iroh::node::SharedIrohNode;

/// The one project every fixture built here seeds: `acc-me` is its sole,
/// coordinating, `send_receive` member.
pub(crate) const PID: &str = "p1";

/// A signed-in `ServiceContext` + `FakeHub` + a real relay-disabled iroh node
/// with the Collaboration root (`<tmp>/Collab`) mounted, and project [`PID`]
/// seeded both on the hub and in the local cache. The returned `TempDir`
/// must be kept alive for as long as `ctx`/`hub` are used.
pub(crate) async fn signed_in_rig() -> (tempfile::TempDir, ServiceContext, FakeHub) {
    let (tmp, ctx, hub) = signed_in_rig_no_root().await;
    let requested = tmp.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        &ctx,
        requested.to_string_lossy().to_string(),
        &PathPolicy::AllowAll,
    )
    .await
    .expect("designate the Collaboration root");
    (tmp, ctx, hub)
}

/// As [`signed_in_rig`], but WITHOUT designating a Collaboration root — for
/// a test that needs to set up its own folder (a pre-existing marker naming
/// another device, say) before anything is designated, e.g. the device
/// replace / reinstall tests.
pub(crate) async fn signed_in_rig_no_root() -> (tempfile::TempDir, ServiceContext, FakeHub) {
    let hub = FakeHub::start().await;
    let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
    crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), "tok");

    let dirs = crate::api::sync::sync_dirs(&ctx).unwrap();
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

    let my_pubkey = crate::api::account::own_device_id(&ctx).unwrap();
    hub.add_account("tok", "acc-me", "Me", &my_pubkey, None);
    hub.add_project(PID, "m31", &[("acc-me", "send_receive", true)], false);

    seed_local_project(&ctx, &my_pubkey);

    (tmp, ctx, hub)
}

/// The local `collab_projects` cache row [`signed_in_rig`] needs so
/// `db::collab_frames::project_ids` finds [`PID`] (the hub's own project
/// membership is a separate concern, seeded by `FakeHub::add_project`).
fn seed_local_project(ctx: &ServiceContext, my_pubkey: &str) {
    use crate::db::collab::{upsert_project, CollabProjectRow};

    let members_json = serde_json::json!([
        {
            "accountId": "acc-me",
            "displayName": "Me",
            "dataRole": "send_receive",
            "coordinator": true,
            "nodes": [my_pubkey],
        }
    ])
    .to_string();

    let db = crate::api::db(ctx).unwrap();
    let conn = db.conn();
    upsert_project(
        &conn,
        &CollabProjectRow {
            project_id: PID.to_string(),
            slug: "m31".to_string(),
            title: "M31".to_string(),
            data_role: "send_receive".to_string(),
            is_coordinator: true,
            require_approval: false,
            pending_frames: 0,
            project_status: "active".to_string(),
            target_name: "M31".to_string(),
            target_ra_deg: 10.68,
            target_dec_deg: 41.27,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "x".to_string(),
            snapshot_signature_b64: "x".to_string(),
            members_json,
            thresholds_version: None,
            thresholds_rules_json: None,
            gov_caps_json: "[]".into(),
            auto_replicate: true,
            synced_caps_json: "[]".into(),
            hub_version: 0,
            manifest_cursor: 0,
            dictionary_version: None,
            dictionary_json: None,
            policy_json: r#"{"mode":"all"}"#.into(),
            replication_paused: false,
            auto_publish: true,
            fetched_at: String::new(),
            feed_epoch: None,
            holder_seq: -1,
        },
    )
    .unwrap();
}

/// The designated Collaboration root — panics if [`signed_in_rig`] did not
/// set one up.
pub(crate) fn collab_root(ctx: &ServiceContext) -> PathBuf {
    PathBuf::from(
        crate::api::scan_roots::get_collaboration_dir(ctx)
            .unwrap()
            .expect("a Collaboration root was designated"),
    )
}

/// This context's own device id, as the hub encodes it (standard base64 of
/// the device pubkey — P3).
pub(crate) async fn my_device(ctx: &ServiceContext) -> String {
    crate::api::account::own_device_id(ctx).unwrap()
}

/// Publish a small, real, byte-exact frame on the hub as `acc-me` (with its
/// real blake3/xxh3/size, not the fake's synthetic placeholders), land it in
/// the local cache exactly as a manifest catch-up would
/// (`upsert_from_manifest`), and drop its bytes at
/// `<root>/m31/other/<uuid>.fits` — as if the OLD device (the one about to
/// be replaced) had already landed it there. Returns `(project_id,
/// frame_uuid, path)`.
pub(crate) async fn seed_replica_file(
    ctx: &ServiceContext,
    hub: &FakeHub,
    root: &Path,
) -> (String, String, PathBuf) {
    let uuid = "f-old-1";
    let bytes = b"a tiny replica frame, just enough bytes to hash".to_vec();
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));

    hub.seed_frames(PID, "acc-me", &[uuid], "published");
    hub.update_frame(PID, uuid, |f| {
        f.blake3 = blake3;
        f.byte_size = bytes.len() as i64;
        f.xxh3 = xxh3;
    });
    let view = hub.frame(PID, uuid).expect("frame just seeded");

    {
        let db = crate::api::db(ctx).unwrap();
        let conn = db.conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }

    let dest_dir = root.join("m31").join("other");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let path = dest_dir.join(format!("{uuid}.fits"));
    std::fs::write(&path, &bytes).unwrap();

    (PID.to_string(), uuid.to_string(), path)
}

/// A signed-in rig with `n` held replica frames (Task 9): real files ABOVE
/// the store's 16 KiB inline threshold (fix round 1 — like real FITS frames,
/// so they are imported by reference) under `<root>/m31/other/`,
/// published on the hub by a second account (`acc-o`) with their real
/// hashes, cached through `upsert_from_manifest`, seeded into the collab
/// store and moved to `held` through `set_local_state` (so each carries its
/// outbox `add`). Until Task 11's landing exists the files are placed
/// directly.
pub(crate) struct LandedRig {
    pub _tmp: tempfile::TempDir,
    pub ctx: Arc<ServiceContext>,
    pub hub: FakeHub,
    pub node: Arc<SharedIrohNode>,
    pub root: PathBuf,
    /// `(project_id, frame_uuid, landed path)`, in uuid order.
    pub frames: Vec<(String, String, PathBuf)>,
    /// The local checks the node's serve oracle queued (Task 10): the
    /// receiving half of the [`DbServeOracle`](crate::api::collab_live::serve_oracle::DbServeOracle)
    /// channel [`landed_rig`] installs.
    pub checks: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<(String, String)>>,
}

impl LandedRig {
    /// The blake3 of frame `i` as the catalog records it (its current version).
    pub(crate) fn hash_of(&self, i: usize) -> iroh_blobs::Hash {
        let (pid, uuid, _) = &self.frames[i];
        let row =
            crate::db::collab_frames::get(&crate::api::db(&self.ctx).unwrap().conn(), pid, uuid)
                .unwrap()
                .expect("a landed row");
        row.blake3.parse().expect("a blake3 hex")
    }

    /// The next local check the serve oracle queued, or `None` after 5 s.
    pub(crate) async fn next_local_check(&self) -> Option<(String, String)> {
        let mut rx = self.checks.lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .ok()
            .flatten()
    }

    /// A storage engine on this rig's root, with the marker this device
    /// recorded at designation.
    pub(crate) fn engine(&self) -> crate::api::collab_live::storage_task::StorageEngine {
        let me = crate::api::account::own_device_id(&self.ctx).unwrap();
        let recorded = crate::db::collab_live::recorded_store_marker(
            &crate::api::db(&self.ctx).unwrap().conn(),
        )
        .unwrap();
        assert!(recorded.is_some(), "designation recorded the store marker");
        let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
            self.root.clone(),
            me,
            recorded,
        ));
        crate::api::collab_live::storage_task::StorageEngine::start(
            Arc::clone(&self.ctx),
            Arc::clone(&self.node),
            guard,
        )
    }
}

pub(crate) async fn landed_rig(n: usize) -> LandedRig {
    landed_rig_with(n, |uuid| {
        format!("replica frame {uuid}: pixels ")
            .repeat(1024)
            .into_bytes()
    })
    .await
}

/// As [`landed_rig`], with frames of exactly `size` bytes (Task 12: a
/// frame big enough to cancel mid-transfer). The bytes depend only on the
/// frame's uuid, so two rigs hold the same frames under the same hashes.
pub(crate) async fn landed_rig_big(n: usize, size: usize) -> LandedRig {
    landed_rig_with(n, |uuid| {
        let pattern = format!("replica frame {uuid}: pixels ").into_bytes();
        let mut bytes = Vec::with_capacity(size + pattern.len());
        while bytes.len() < size {
            bytes.extend_from_slice(&pattern);
        }
        bytes.truncate(size);
        bytes
    })
    .await
}

async fn landed_rig_with(n: usize, bytes_of: impl Fn(&str) -> Vec<u8>) -> LandedRig {
    let (tmp, ctx, hub) = signed_in_rig().await;
    let root = collab_root(&ctx);
    let node = ctx.iroh_node.lock().await.clone().expect("a bound node");
    assert!(node.collab_store().is_some(), "the collab store is mounted");
    let dir = root.join("m31").join("other");
    std::fs::create_dir_all(&dir).unwrap();

    let uuids: Vec<String> = (0..n).map(|i| format!("f{i:02}")).collect();
    let mut frames = Vec::with_capacity(n);
    for uuid in &uuids {
        let bytes = bytes_of(uuid);
        debug_assert!(bytes.len() > 16 * 1024);
        let path = dir.join(format!("{uuid}.fits"));
        land_frame(&ctx, &hub, &node, uuid, &path, &bytes).await;
        frames.push((PID.to_string(), uuid.clone(), path));
    }
    let ctx = Arc::new(ctx);
    // The live session's serve oracle (Task 10), on a guard that checked the
    // marker designation wrote.
    let me = crate::api::account::own_device_id(&ctx).unwrap();
    let recorded =
        crate::db::collab_live::recorded_store_marker(&crate::api::db(&ctx).unwrap().conn())
            .unwrap();
    let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
        root.clone(),
        me,
        recorded,
    ));
    assert!(
        guard.check_now().serving(),
        "the designated root is available"
    );
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    node.set_collab_serve_oracle(Some(Arc::new(
        crate::api::collab_live::serve_oracle::DbServeOracle::new(Arc::clone(&ctx), guard, tx),
    )));
    LandedRig {
        _tmp: tmp,
        ctx,
        hub,
        node,
        root,
        frames,
        checks: tokio::sync::Mutex::new(rx),
    }
}

/// Two signed-in providers holding the same single frame (`f00`, same
/// bytes, same hash), each with its own serve oracle (Task 12).
pub(crate) async fn two_landed_providers() -> (LandedRig, LandedRig) {
    let a = landed_rig(1).await;
    let b = landed_rig(1).await;
    assert_eq!(a.hash_of(0), b.hash_of(0), "the same frame on both");
    (a, b)
}

// ----- the live assignment run (Task 12) -----------------------------------

use crate::sharing::iroh::assign::{
    run_live, AssignmentReport, Dialer, FetchItem, ItemOutcome, LiveItem, LiveRunOptions,
    LiveVerdict, ProviderSet,
};

/// The work-unit cap the live-run helpers pass (spec §7.2: 256 MiB).
const TEST_UNIT_CAP: u64 = 256 * 1024 * 1024;

/// A collab-pool dialer from `me` whose address book is `providers`.
pub(crate) fn live_dialer(me: &SharedIrohNode, providers: &[&SharedIrohNode]) -> Dialer {
    let book: std::collections::HashMap<iroh::EndpointId, iroh::EndpointAddr> = providers
        .iter()
        .map(|p| {
            let addr = p.endpoint_addr();
            (addr.id, addr)
        })
        .collect();
    // Pool events are the scheduler's business; the run itself does not
    // need them, so the receiver is simply dropped.
    let (events, _) = tokio::sync::mpsc::unbounded_channel();
    Dialer::Collab {
        pool: crate::sharing::iroh::collab_pool::CollabPool::new(me.endpoint(), events),
        addrs: Arc::new(move |id| book.get(id).cloned()),
    }
}

/// Live-run knobs for the tests: a 5 s stall ceiling, no hedging.
pub(crate) fn live_opts(
    max_in_flight: usize,
    telemetry: crate::sharing::ProviderTelemetrySink,
) -> LiveRunOptions {
    LiveRunOptions {
        stall_hard_limit: std::time::Duration::from_secs(5),
        hedging: false,
        telemetry,
        max_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(max_in_flight)),
        limit_changed: None,
        unit_cap_bytes: TEST_UNIT_CAP,
    }
}

/// One raw-blob live item and its cancel switch.
pub(crate) fn live_item(
    key: &str,
    hash: iroh_blobs::Hash,
    size: u64,
    providers: ProviderSet,
) -> (LiveItem, tokio::sync::watch::Sender<bool>) {
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
    let item = FetchItem {
        key: key.to_string(),
        request: iroh_blobs::protocol::GetRequest::blob(hash),
        hash,
        size,
        providers,
    };
    (LiveItem { item, cancel }, cancel_tx)
}

/// Frame `i` of `rig` as a live item.
pub(crate) fn rig_item(
    rig: &LandedRig,
    i: usize,
    providers: ProviderSet,
) -> (LiveItem, tokio::sync::watch::Sender<bool>) {
    let (_, uuid, path) = &rig.frames[i];
    let size = std::fs::metadata(path).unwrap().len();
    live_item(uuid, rig.hash_of(i), size, providers)
}

/// What one live run produced.
pub(crate) struct LiveRun {
    pub outcomes: Vec<(String, ItemOutcome)>,
    pub verdicts: Vec<LiveVerdict>,
    pub report: AssignmentReport,
}

/// Run `items` through [`run_live`] (the item channel closed behind them)
/// beside `side`, and collect everything it reported. 120 s at most.
pub(crate) async fn drive_live(
    store: &iroh_blobs::api::Store,
    dialer: Dialer,
    items: Vec<LiveItem>,
    opts: LiveRunOptions,
    side: impl std::future::Future<Output = ()>,
) -> LiveRun {
    let (item_tx, item_rx) = tokio::sync::mpsc::channel(items.len().max(1));
    for item in items {
        item_tx.send(item).await.unwrap();
    }
    drop(item_tx);
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let (verdict_tx, mut verdict_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_yield_tx, yield_rx) = tokio::sync::watch::channel(false);
    let run = run_live(store, dialer, item_rx, opts, done_tx, verdict_tx, yield_rx);
    let (report, ()) = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        tokio::join!(run, side)
    })
    .await
    .expect("the live run returns once its items are done");
    let mut outcomes = Vec::new();
    while let Ok(o) = done_rx.try_recv() {
        outcomes.push(o);
    }
    let mut verdicts = Vec::new();
    while let Ok(v) = verdict_rx.try_recv() {
        verdicts.push(v);
    }
    LiveRun {
        outcomes,
        verdicts,
        report,
    }
}

/// The one outcome of a one-item run.
fn only_outcome(run: &mut LiveRun) -> ItemOutcome {
    assert_eq!(run.outcomes.len(), 1, "one item, one outcome");
    run.outcomes.pop().unwrap().1
}

/// Fetch frame `i` of `rig` into `store` over a live run with `providers`,
/// while `side` runs (e.g. a provider appearing later).
pub(crate) async fn run_one_live(
    me: &SharedIrohNode,
    store: &iroh_blobs::api::Store,
    rig: &LandedRig,
    i: usize,
    providers: ProviderSet,
    side: impl std::future::Future<Output = ()>,
) -> (ItemOutcome, AssignmentReport) {
    let (item, _cancel) = rig_item(rig, i, providers);
    let dialer = live_dialer(me, &[&rig.node]);
    let opts = live_opts(8, crate::sharing::noop_provider_telemetry());
    let mut run = drive_live(store, dialer, vec![item], opts, side).await;
    (only_outcome(&mut run), run.report)
}

/// Fetch `hash` (keyed `key`) from the fixed provider list `rigs`, in that
/// order; returns the outcome and every verdict the run sent.
pub(crate) async fn run_one_live_fixed(
    me: &SharedIrohNode,
    store: &iroh_blobs::api::Store,
    hash: iroh_blobs::Hash,
    key: String,
    rigs: &[&LandedRig],
) -> (ItemOutcome, Vec<LiveVerdict>) {
    let nodes: Vec<&SharedIrohNode> = rigs.iter().map(|r| &*r.node).collect();
    let providers = ProviderSet::Fixed(Arc::new(
        nodes.iter().map(|n| n.endpoint_addr().id).collect(),
    ));
    let (item, _cancel) = live_item(&key, hash, 0, providers);
    let dialer = live_dialer(me, &nodes);
    let opts = live_opts(8, crate::sharing::noop_provider_telemetry());
    let mut run = drive_live(store, dialer, vec![item], opts, async {}).await;
    (only_outcome(&mut run), run.verdicts)
}

/// Fetch frame `i` of `rig` and flip its cancel switch as soon as the store
/// holds some of its bytes (polled, 30 s at most) — mid-transfer by
/// construction rather than by a guessed delay.
pub(crate) async fn run_one_live_cancel_once_moving(
    me: &SharedIrohNode,
    store: &iroh_blobs::api::Store,
    rig: &LandedRig,
    i: usize,
) -> ItemOutcome {
    let providers = ProviderSet::Fixed(Arc::new(vec![rig.node.endpoint_addr().id]));
    let (item, cancel) = rig_item(rig, i, providers);
    let hash = rig.hash_of(i);
    let dialer = live_dialer(me, &[&rig.node]);
    let opts = live_opts(8, crate::sharing::noop_provider_telemetry());
    let blobs = store.blobs().clone();
    let mut run = drive_live(store, dialer, vec![item], opts, async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let moved = blobs
                .observe(hash)
                .await
                .map(|b| b.total_bytes())
                .unwrap_or(0);
            if moved > 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no byte of the frame arrived within 30 s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        cancel.send_replace(true);
    })
    .await;
    only_outcome(&mut run)
}

/// Queue `items` on a live run whose item channel stays OPEN, request a
/// yield after `yield_after`, and return the outcomes plus how long the run
/// took to return after the yield (fix round 1, I1).
pub(crate) async fn run_live_then_yield(
    store: &iroh_blobs::api::Store,
    dialer: Dialer,
    items: Vec<LiveItem>,
    yield_after: std::time::Duration,
) -> (Vec<(String, ItemOutcome)>, std::time::Duration) {
    let (item_tx, item_rx) = tokio::sync::mpsc::channel(items.len().max(1));
    for item in items {
        item_tx.send(item).await.unwrap();
    }
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let (verdict_tx, _verdict_rx) = tokio::sync::mpsc::unbounded_channel();
    let (yield_tx, yield_rx) = tokio::sync::watch::channel(false);
    let opts = live_opts(8, crate::sharing::noop_provider_telemetry());
    let run = run_live(store, dialer, item_rx, opts, done_tx, verdict_tx, yield_rx);
    let yielded_at = Arc::new(std::sync::Mutex::new(None));
    let side = {
        let yielded_at = Arc::clone(&yielded_at);
        async move {
            tokio::time::sleep(yield_after).await;
            *yielded_at.lock().unwrap() = Some(std::time::Instant::now());
            yield_tx.send_replace(true);
            // Keep the sender (and so the yield) alive until the run ends.
            std::future::pending::<()>().await;
        }
    };
    let report = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::select! {
            r = run => r,
            _ = side => unreachable!("the side future never ends"),
        }
    })
    .await
    .expect("a yield ends the live run");
    drop(report);
    let after_yield = yielded_at
        .lock()
        .unwrap()
        .expect("the yield was requested before the run returned")
        .elapsed();
    drop(item_tx);
    let mut outcomes = Vec::new();
    while let Ok(o) = done_rx.try_recv() {
        outcomes.push(o);
    }
    (outcomes, after_yield)
}

/// Force frame `i` of `rig` into `state` with a raw UPDATE (no transition
/// checks, no outbox) — for a test that makes its serve oracle refuse
/// (`idle`) and serve again (`held`).
pub(crate) fn set_frame_state_raw(
    rig: &LandedRig,
    i: usize,
    state: crate::db::collab_frames::LocalState,
) {
    let (pid, uuid, _) = &rig.frames[i];
    let conn = crate::api::db(&rig.ctx).unwrap().conn();
    conn.execute(
        "UPDATE project_frames_local SET local_state = ?3 WHERE project_id = ?1 AND frame_uuid = ?2",
        rusqlite::params![pid, uuid, state.as_db_str()],
    )
    .unwrap();
}

/// What [`run_many_live`] saw.
pub(crate) struct LiveStats {
    /// The most items that were in flight at once.
    pub max_concurrent: usize,
    /// Items that ended `Done`.
    pub completed: usize,
    /// `run_live` returned while its item channel was still open.
    pub returned_early: bool,
}

/// Queue every frame of `rig` on a live run capped at `max_in_flight`.
/// With `yield_after_first`, a yield is requested the moment the first item
/// starts and the item channel is kept OPEN (so only the yield can end the
/// run); without it the channel is closed behind the items. Concurrency is counted from
/// the telemetry: `+1` on each `Trying` (one per item — a healthy provider,
/// no hedging), `-1` per outcome, the outcomes drained inside the callback
/// so an item's end is always counted before the next item's start.
pub(crate) async fn run_many_live(
    me: &SharedIrohNode,
    store: &iroh_blobs::api::Store,
    rig: &LandedRig,
    max_in_flight: usize,
    yield_after_first: bool,
) -> LiveStats {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let provider = rig.node.endpoint_addr().id;
    let (item_tx, item_rx) = tokio::sync::mpsc::channel(rig.frames.len().max(1));
    let mut cancels = Vec::new();
    for i in 0..rig.frames.len() {
        let (item, cancel) = rig_item(rig, i, ProviderSet::Fixed(Arc::new(vec![provider])));
        cancels.push(cancel);
        item_tx.send(item).await.unwrap();
    }
    let item_tx = yield_after_first.then_some(item_tx);
    let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel::<(String, ItemOutcome)>();
    let done_rx = Arc::new(std::sync::Mutex::new(done_rx));
    let (verdict_tx, _verdict_rx) = tokio::sync::mpsc::unbounded_channel();
    let (yield_tx, yield_rx) = tokio::sync::watch::channel(false);
    let yield_tx = Arc::new(yield_tx);

    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let drain = {
        let (done_rx, current, completed) = (
            Arc::clone(&done_rx),
            Arc::clone(&current),
            Arc::clone(&completed),
        );
        move || {
            let mut rx = done_rx.lock().unwrap();
            while let Ok((_, outcome)) = rx.try_recv() {
                current.fetch_sub(1, Ordering::SeqCst);
                if matches!(outcome, ItemOutcome::Done) {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    };
    let telemetry: crate::sharing::ProviderTelemetrySink = {
        let (drain, current, max, yield_tx) = (
            drain.clone(),
            Arc::clone(&current),
            Arc::clone(&max),
            Arc::clone(&yield_tx),
        );
        Arc::new(move |ev| {
            if let crate::sharing::ProviderEvent::Trying(_) = ev {
                drain();
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(now, Ordering::SeqCst);
                if yield_after_first {
                    yield_tx.send_replace(true);
                }
            }
        })
    };
    let dialer = live_dialer(me, &[&rig.node]);
    let opts = live_opts(max_in_flight, telemetry);
    let returned = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        run_live(store, dialer, item_rx, opts, done_tx, verdict_tx, yield_rx),
    )
    .await;
    drain();
    // `item_tx` is still held here: a return is "early" when it came while
    // the channel was open, i.e. while items were still queued behind it.
    let returned_early = returned.is_ok() && item_tx.is_some();
    drop(item_tx);
    LiveStats {
        max_concurrent: max.load(Ordering::SeqCst),
        completed: completed.load(Ordering::SeqCst),
        returned_early,
    }
}

/// A relay-disabled node with no catalog behind it (a fetcher in the
/// two-node tests), in its own temp dir.
pub(crate) struct BareNode {
    pub node: Arc<SharedIrohNode>,
    _tmp: tempfile::TempDir,
}

impl std::ops::Deref for BareNode {
    type Target = Arc<SharedIrohNode>;
    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

pub(crate) async fn bare_node() -> BareNode {
    let tmp = tempfile::tempdir().unwrap();
    let node = SharedIrohNode::bind_with(
        tmp.path(),
        tmp.path(),
        iroh::RelayMode::Disabled,
        crate::sharing::iroh::node::NodeOptions::default(),
    )
    .await
    .expect("bind relay-disabled node");
    BareNode { node, _tmp: tmp }
}

/// Relay-disabled nodes have no discovery: exchange addresses.
pub(crate) async fn pair(a: &Arc<SharedIrohNode>, b: &Arc<SharedIrohNode>) {
    for n in [a, b] {
        n.handle(crate::sharing::iroh::node::Role::Out)
            .start()
            .await
            .unwrap();
    }
    a.add_peer(b.endpoint_addr());
    b.add_peer(a.endpoint_addr());
}

/// An empty in-memory blob store to fetch into.
pub(crate) fn scratch_store() -> iroh_blobs::api::Store {
    iroh_blobs::store::mem::MemStore::new().into()
}

/// Publish `uuid` on the hub as `acc-o` with `bytes`' real hashes, write
/// the file at `path`, cache the row, seed it and move it to `held` — the
/// landing [`landed_rig`] fakes until Task 11's exists.
pub(crate) async fn land_frame(
    ctx: &ServiceContext,
    hub: &FakeHub,
    node: &SharedIrohNode,
    uuid: &str,
    path: &Path,
    bytes: &[u8],
) {
    hub.seed_frames(PID, "acc-o", &[uuid], "published");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let blake3 = blake3::hash(bytes).to_hex().to_string();
    let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes));
    let file_name = path.file_name().unwrap().to_string_lossy().to_string();
    hub.update_frame(PID, uuid, |f| {
        f.blake3 = blake3;
        f.byte_size = bytes.len() as i64;
        f.xxh3 = xxh3;
        f.file_name = file_name;
    });
    let view = hub.frame(PID, uuid).expect("frame just seeded");
    {
        let conn = crate::api::db(ctx).unwrap().conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }
    node.seed_project_frame(PID, uuid, view.content_version, path)
        .await
        .expect("seed the landed frame");
    let conn = crate::api::db(ctx).unwrap().conn();
    let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(path).unwrap());
    crate::db::collab_frames::update_landed_path(&conn, PID, uuid, &path.to_string_lossy())
        .unwrap();
    crate::db::collab_frames::set_size_mtime_seen(&conn, PID, uuid, &stamp.encode()).unwrap();
    crate::db::collab_frames::set_local_state(
        &conn,
        PID,
        uuid,
        crate::db::collab_frames::LocalState::Held,
    )
    .unwrap();
}

/// Move `path`'s mtime `secs_forward` seconds past now (beyond the 2 s
/// tolerance for anything ≥ 3).
pub(crate) fn set_mtime(path: &Path, secs_forward: u64) {
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(secs_forward);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Flip one byte, keep the size, bump the mtime — an in-place edit.
pub(crate) fn overwrite_same_size(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(path, &bytes).unwrap();
    set_mtime(path, 10);
}

/// Frames of [`fetch_rig`]: above the collab store's 16 KiB inline limit, so
/// a fetched blob is OWNED by the store and lands by the DIRECT export
/// (Task 11 ruling R1: the landing tests must run on that path).
pub(crate) const FETCH_FRAME_BYTES: usize = 64 * 1024;

/// Task 11's two-node landing rig: a signed-in receiver (`ctx`, `node`, the
/// Collaboration root `root`) whose manifest is synced from the fake hub
/// (rows `wanted`), and a relay-disabled provider with its own collab store
/// holding every published version's bytes. [`fetch_blob`](Self::fetch_blob)
/// is one `execute_get` from the provider over the collab ALPN into the
/// receiver's collab store, under the frame's in-flight tag; it remembers
/// the row as it stood then — the row a landing carries.
pub(crate) struct FetchRig {
    pub _tmp: tempfile::TempDir,
    pub ctx: Arc<ServiceContext>,
    pub hub: FakeHub,
    pub node: Arc<SharedIrohNode>,
    pub root: PathBuf,
    pub provider: BareNode,
    _provider_root: tempfile::TempDir,
    pub guard: crate::collab::storage::marker::StoreGuard,
    pub hooks: crate::sharing::iroh::blobs::ExportHooks,
    uuids: Vec<String>,
    /// Per frame, the bytes of every published version (index 0 = v1).
    versions: std::sync::Mutex<Vec<Vec<Vec<u8>>>>,
    /// Per frame, the row as it stood at its last [`fetch_blob`](Self::fetch_blob).
    in_flight: std::sync::Mutex<Vec<Option<crate::db::collab_frames::LocalFrameRow>>>,
}

/// `n` frames of [`FETCH_FRAME_BYTES`], published and fetched (v1).
pub(crate) async fn fetch_rig(n: usize) -> FetchRig {
    fetch_rig_sized(n, FETCH_FRAME_BYTES).await
}

/// [`fetch_rig`] with frames of `size` bytes (≤ 16 KiB: inline blobs).
pub(crate) async fn fetch_rig_sized(n: usize, size: usize) -> FetchRig {
    let (tmp, ctx, hub) = signed_in_rig().await;
    let root = collab_root(&ctx);
    let node = ctx.iroh_node.lock().await.clone().expect("a bound node");
    let provider = bare_node().await;
    let provider_root = tempfile::tempdir().unwrap();
    provider
        .set_collab_root(Some(provider_root.path()))
        .await
        .expect("mount the provider's collab store");
    pair(&node, &provider).await;
    let me = crate::api::account::own_device_id(&ctx).unwrap();
    let recorded =
        crate::db::collab_live::recorded_store_marker(&crate::api::db(&ctx).unwrap().conn())
            .unwrap();
    let guard = crate::collab::storage::marker::StoreGuard::new(root.clone(), me, recorded);
    assert!(
        guard.check_now().fetching(),
        "the designated root is available"
    );
    let uuids: Vec<String> = (0..n).map(|i| format!("f{i:02}")).collect();
    let rig = FetchRig {
        _tmp: tmp,
        ctx: Arc::new(ctx),
        hub,
        node,
        root,
        provider,
        _provider_root: provider_root,
        guard,
        hooks: Default::default(),
        versions: std::sync::Mutex::new(vec![Vec::new(); n]),
        in_flight: std::sync::Mutex::new(vec![None; n]),
        uuids,
    };
    for i in 0..n {
        let uuid = rig.uuids[i].clone();
        let bytes = FetchRig::pattern(&uuid, 1, size);
        rig.provide(&bytes).await;
        rig.hub.seed_frames(PID, "acc-o", &[&uuid], "published");
        rig.publish(i, bytes, false);
    }
    for i in 0..n {
        rig.fetch_blob(i).await;
    }
    rig
}

impl FetchRig {
    /// `size` bytes of frame `uuid`, version `version` — distinct per both.
    pub(crate) fn pattern(uuid: &str, version: i32, size: usize) -> Vec<u8> {
        format!("frame {uuid} v{version}: pixels ")
            .into_bytes()
            .into_iter()
            .cycle()
            .take(size)
            .collect()
    }

    pub(crate) fn frame(&self, i: usize) -> (String, String) {
        (PID.to_string(), self.uuids[i].clone())
    }

    pub(crate) fn receiver_store(&self) -> iroh_blobs::api::Store {
        self.node
            .collab_store()
            .expect("the receiver's collab store")
    }

    pub(crate) fn row(&self, i: usize) -> crate::db::collab_frames::LocalFrameRow {
        crate::db::collab_frames::get(
            &crate::api::db(&self.ctx).unwrap().conn(),
            PID,
            &self.uuids[i],
        )
        .unwrap()
        .expect("a cached row")
    }

    pub(crate) fn bytes(&self, i: usize, version: usize) -> Vec<u8> {
        self.versions.lock().unwrap()[i][version - 1].clone()
    }

    pub(crate) fn v1_bytes(&self, i: usize) -> Vec<u8> {
        self.bytes(i, 1)
    }

    pub(crate) fn v2_bytes(&self, i: usize) -> Vec<u8> {
        self.bytes(i, 2)
    }

    pub(crate) fn v2_hash(&self, i: usize) -> iroh_blobs::Hash {
        iroh_blobs::Hash::new(self.v2_bytes(i))
    }

    /// Put `bytes` into the provider's collab store (tagged, so they stay).
    async fn provide(&self, bytes: &[u8]) {
        let store = self.provider.collab_store().expect("provider store");
        let tt = store
            .blobs()
            .add_bytes(bytes.to_vec())
            .temp_tag()
            .await
            .unwrap();
        let hash = tt.hash();
        store
            .tags()
            .set(format!("test/{hash}"), iroh_blobs::HashAndFormat::raw(hash))
            .await
            .unwrap();
    }

    /// Publish `bytes` as frame `i`'s next version on the hub (or its first,
    /// `bump = false`) and sync the receiver's manifest row.
    fn publish(&self, i: usize, bytes: Vec<u8>, bump: bool) {
        let uuid = self.uuids[i].clone();
        let blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        let len = bytes.len() as i64;
        self.hub.update_frame(PID, &uuid, |f| {
            if bump {
                f.content_version += 1;
            }
            f.blake3 = blake3;
            f.byte_size = len;
            f.xxh3 = xxh3;
        });
        self.versions.lock().unwrap()[i].push(bytes);
        let view = self.hub.frame(PID, &uuid).expect("frame published");
        let conn = crate::api::db(&self.ctx).unwrap().conn();
        crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
    }

    /// The provider re-versions frame `i` with new bytes of the same size;
    /// the receiver's manifest follows (a held row goes back to `wanted`,
    /// its old file untouched).
    pub(crate) async fn publish_new_version(&self, i: usize) {
        let next = self.versions.lock().unwrap()[i].len() as i32 + 1;
        let size = self.v1_bytes(i).len();
        let bytes = Self::pattern(&self.uuids[i], next, size);
        self.publish_version_with(i, bytes).await;
    }

    /// [`publish_new_version`](Self::publish_new_version) with given bytes.
    pub(crate) async fn publish_version_with(&self, i: usize, bytes: Vec<u8>) {
        self.provide(&bytes).await;
        self.publish(i, bytes, true);
    }

    /// Fetch frame `i`'s current bytes from the provider into the receiver's
    /// collab store under the frame's in-flight tag, and remember the row.
    pub(crate) async fn fetch_blob(&self, i: usize) {
        let row = self.row(i);
        let hash: iroh_blobs::Hash = row.blake3.parse().unwrap();
        let conn = self
            .node
            .endpoint()
            .connect(
                self.provider.endpoint_addr(),
                crate::sharing::iroh::COLLAB_BLOBS_ALPN,
            )
            .await
            .expect("dial the provider on the collab ALPN");
        let store = self.receiver_store();
        store
            .remote()
            .execute_get(conn, iroh_blobs::protocol::GetRequest::blob(hash))
            .await
            .expect("fetched");
        store
            .tags()
            .set(
                crate::api::collab_live::landing::project_frame_in_flight_tag(
                    PID,
                    &row.frame_uuid,
                    row.content_version,
                ),
                iroh_blobs::HashAndFormat::raw(hash),
            )
            .await
            .unwrap();
        self.in_flight.lock().unwrap()[i] = Some(row);
    }

    /// The manifest moved while the bytes were in flight (a version bump
    /// written straight into the receiver's row).
    pub(crate) fn bump_manifest_version_locally(&self, i: usize) {
        crate::api::db(&self.ctx)
            .unwrap()
            .conn()
            .execute(
                "UPDATE project_frames_local SET content_version = content_version + 1
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![PID, self.uuids[i]],
            )
            .unwrap();
    }

    /// A storage engine on this rig's root (it registers as the engine the
    /// landing hands edited targets to).
    pub(crate) fn engine(&self) -> crate::api::collab_live::storage_task::StorageEngine {
        let me = crate::api::account::own_device_id(&self.ctx).unwrap();
        let recorded = crate::db::collab_live::recorded_store_marker(
            &crate::api::db(&self.ctx).unwrap().conn(),
        )
        .unwrap();
        let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
            self.root.clone(),
            me,
            recorded,
        ));
        crate::api::collab_live::storage_task::StorageEngine::start(
            Arc::clone(&self.ctx),
            Arc::clone(&self.node),
            guard,
        )
    }

    /// `link_identical` of frame `i` (its current row) from `src`.
    pub(crate) async fn link(
        &self,
        i: usize,
        src: &Path,
    ) -> Result<PathBuf, crate::api::collab_live::landing::Landed> {
        use crate::api::collab_live::landing::{link_identical, Landed, LandingEnv};
        let row = self.row(i);
        let project =
            crate::db::collab::get_project(&crate::api::db(&self.ctx).unwrap().conn(), PID)
                .unwrap()
                .expect("the project row");
        let store = self.receiver_store();
        let started_at = crate::sync::now_iso();
        let env = LandingEnv {
            ctx: &self.ctx,
            node: &self.node,
            store: &store,
            project: &project,
            collab_root: &self.root,
            guard: &self.guard,
            started_at: &started_at,
            hooks: &self.hooks,
        };
        match link_identical(&env, &row, src).await {
            Landed::Yes(p) => Ok(p),
            other => Err(other),
        }
    }

    /// Land frame `i` with the row its last fetch carried: `Ok(path)` for
    /// `Landed::Yes`, `Err(landed)` otherwise.
    pub(crate) async fn land(
        &self,
        i: usize,
    ) -> Result<PathBuf, crate::api::collab_live::landing::Landed> {
        use crate::api::collab_live::landing::{land_frame, Landed, LandingEnv};
        let row = self.in_flight.lock().unwrap()[i]
            .clone()
            .expect("fetch_blob first");
        let hash: iroh_blobs::Hash = row.blake3.parse().unwrap();
        let project =
            crate::db::collab::get_project(&crate::api::db(&self.ctx).unwrap().conn(), PID)
                .unwrap()
                .expect("the project row");
        let store = self.receiver_store();
        let started_at = crate::sync::now_iso();
        let env = LandingEnv {
            ctx: &self.ctx,
            node: &self.node,
            store: &store,
            project: &project,
            collab_root: &self.root,
            guard: &self.guard,
            started_at: &started_at,
            hooks: &self.hooks,
        };
        match land_frame(&env, &row, hash).await {
            Landed::Yes(p) => Ok(p),
            other => Err(other),
        }
    }
}

// ----- two live instances (Task 15) ---------------------------------------

use crate::db::collab_frames::LocalState;
use crate::sync::receiver::InboundControl;

/// The fake hub's shortened presence timings for the live tests, and the
/// beat that keeps a session inside them.
pub(crate) const LIVE_TIMINGS: crate::collab::fake_hub::FakeTimings =
    crate::collab::fake_hub::FakeTimings {
        keepalive: std::time::Duration::from_millis(200),
        grace: std::time::Duration::from_millis(300),
        silence: std::time::Duration::from_secs(1),
    };
pub(crate) const LIVE_BEAT: std::time::Duration = std::time::Duration::from_millis(250);

/// One app instance with a live exchange: its own catalog, a bound
/// relay-disabled node, its Collaboration root, and a receive gate it owns
/// (the sync receiver is never started here).
pub(crate) struct Instance {
    _tmp: tempfile::TempDir,
    pub ctx: Arc<ServiceContext>,
    pub node: Arc<SharedIrohNode>,
    pub root: PathBuf,
    pub control: Arc<InboundControl>,
}

async fn live_instance(
    hub: &FakeHub,
    token: &'static str,
    account: &'static str,
    display: &'static str,
) -> Instance {
    let (tmp, ctx) = crate::api::collab_exchange::test_support::test_ctx();
    crate::api::collab_exchange::test_support::wire_hub(&ctx, &hub.uri(), token);
    let dirs = crate::api::sync::sync_dirs(&ctx).unwrap();
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
    crate::api::collab_live::serve_oracle::install_catalog_oracle(&ctx, &node);
    *ctx.iroh_node.lock().await = Some(Arc::clone(&node));
    let device = crate::api::account::own_device_id(&ctx).unwrap();
    hub.add_account(token, account, display, &device, None);
    let requested = tmp.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        &ctx,
        requested.to_string_lossy().to_string(),
        &PathPolicy::AllowAll,
    )
    .await
    .expect("designate the Collaboration root");
    let root = collab_root(&ctx);
    Instance {
        _tmp: tmp,
        ctx: Arc::new(ctx),
        node,
        root,
        control: Arc::new(InboundControl::new()),
    }
}

async fn wait_until(what: &str, within: std::time::Duration, mut pred: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + within;
    loop {
        if pred() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: not within {within:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

impl Instance {
    /// This device's id as the hub names it.
    pub(crate) fn device(&self) -> String {
        crate::api::account::own_device_id(&self.ctx).unwrap()
    }

    /// Arm the live exchange (the test config: a beat inside the fake
    /// hub's shortened silence rule).
    pub(crate) fn start_live(&self) {
        let cfg = crate::api::collab_live::LiveConfig {
            beat: LIVE_BEAT,
            ready_poll: std::time::Duration::from_millis(100),
            ..Default::default()
        };
        crate::api::collab_live::spawn_with(
            Arc::clone(&self.ctx),
            crate::api::collab_live::GateSource::Fixed(Arc::clone(&self.control)),
            None,
            cfg,
        )
        .expect("armed");
    }

    /// Stop (clean exit) and arm it again.
    pub(crate) async fn restart_live(&self) {
        crate::api::collab_live::shutdown(&self.ctx).await;
        self.start_live();
    }

    pub(crate) fn row(&self, uuid: &str) -> Option<crate::db::collab_frames::LocalFrameRow> {
        crate::db::collab_frames::get(&crate::api::db(&self.ctx).unwrap().conn(), PID, uuid)
            .unwrap()
    }

    pub(crate) async fn wait_state(
        &self,
        uuid: &str,
        state: LocalState,
        within: std::time::Duration,
    ) {
        wait_until(&format!("{uuid} {state:?}"), within, || {
            self.row(uuid).is_some_and(|r| r.local_state == state)
        })
        .await;
    }

    pub(crate) async fn wait_state_version(
        &self,
        uuid: &str,
        state: LocalState,
        version: i32,
        within: std::time::Duration,
    ) {
        wait_until(&format!("{uuid} {state:?} v{version}"), within, || {
            self.row(uuid)
                .is_some_and(|r| r.local_state == state && r.content_version == version)
        })
        .await;
    }

    /// Every cached replica of the project held.
    pub(crate) async fn wait_all_held(&self, within: std::time::Duration) {
        wait_until("every replica held", within, || {
            let rows = crate::db::collab_frames::list_for_project(
                &crate::api::db(&self.ctx).unwrap().conn(),
                PID,
            )
            .unwrap();
            !rows.is_empty()
                && rows
                    .iter()
                    .filter(|r| r.origin == crate::db::collab_frames::FrameOrigin::Replica)
                    .all(|r| r.local_state == LocalState::Held)
        })
        .await;
    }

    /// Bytes of the frame's current version are arriving in the store.
    async fn fetching(&self, uuid: &str) -> bool {
        let Some(row) = self.row(uuid) else {
            return false;
        };
        let Some(store) = self.node.collab_store() else {
            return false;
        };
        let Ok(hash) = row.blake3.parse::<iroh_blobs::Hash>() else {
            return false;
        };
        match store.blobs().observe(hash).await {
            Ok(bitfield) => bitfield.total_bytes() > 0 && !bitfield.is_complete(),
            Err(_) => false,
        }
    }

    pub(crate) async fn wait_fetch_started(&self, uuid: &str, within: std::time::Duration) {
        let deadline = std::time::Instant::now() + within;
        while !self.fetching(uuid).await {
            assert!(
                std::time::Instant::now() < deadline,
                "{uuid}: no fetch started within {within:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    pub(crate) async fn wait_any_fetch_started(&self, within: std::time::Duration) {
        let deadline = std::time::Instant::now() + within;
        loop {
            let rows = crate::db::collab_frames::list_for_project(
                &crate::api::db(&self.ctx).unwrap().conn(),
                PID,
            )
            .unwrap();
            for r in rows {
                if self.fetching(&r.frame_uuid).await {
                    return;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no fetch started within {within:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The frame's file bytes as this instance holds them.
    pub(crate) fn file_bytes(&self, uuid: &str) -> Vec<u8> {
        let row = self.row(uuid).expect("a cached row");
        std::fs::read(row.landed_path.expect("a landed file")).unwrap()
    }

    /// One receive lane in all (the personal-priority test).
    pub(crate) async fn set_receive_limit(&self, n: usize) {
        self.control.receive_gate.set_limit(n);
    }

    /// How long a personal transfer waits for its receive permit.
    pub(crate) async fn personal_acquire_timed(&self) -> std::time::Duration {
        let t0 = std::time::Instant::now();
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            self.control.receive_gate.acquire(),
        )
        .await
        .expect("a personal permit within a minute");
        let waited = t0.elapsed();
        drop(permit);
        waited
    }
}

/// A (`send`, publishing) and B (`send_receive`, replicating), both live on
/// one fake hub, paired, project [`PID`] refreshed on both.
pub(crate) struct World {
    pub hub: FakeHub,
    pub a: Instance,
    pub b: Instance,
    next: std::sync::atomic::AtomicUsize,
}

pub(crate) async fn two_instances() -> World {
    two_instances_with(None).await
}

/// As [`two_instances`], A's uploads capped at `bps` bytes per second.
pub(crate) async fn two_instances_throttled(bps: u64) -> World {
    two_instances_with(Some(bps)).await
}

async fn two_instances_with(throttle: Option<u64>) -> World {
    let hub = FakeHub::start().await;
    hub.set_timings(LIVE_TIMINGS);
    hub.add_project(
        PID,
        "m31",
        &[("acc-a", "send", false), ("acc-b", "send_receive", false)],
        false,
    );
    let a = live_instance(&hub, "tok-a", "acc-a", "Alice").await;
    let b = live_instance(&hub, "tok-b", "acc-b", "Bob").await;
    pair(&a.node, &b.node).await;
    for i in [&a, &b] {
        let cards = crate::api::collab::refresh_projects(&i.ctx).await.unwrap();
        assert!(cards.iter().any(|p| p.project_id == PID));
    }
    if let Some(bps) = throttle {
        a.node.set_upload_limit(bps);
    }
    a.start_live();
    b.start_live();
    let window = std::time::Duration::from_secs(10);
    hub.wait_connected(PID, &a.device(), window).await;
    hub.wait_connected(PID, &b.device(), window).await;
    World {
        hub,
        a,
        b,
        next: std::sync::atomic::AtomicUsize::new(0),
    }
}

impl World {
    fn fresh_uuids(&self, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| {
                let i = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("f{i:03}")
            })
            .collect()
    }

    /// A publishes `n` frames of 64 KiB.
    pub(crate) async fn a_publishes(&self, n: usize) -> Vec<String> {
        self.a_publishes_big(n, FETCH_FRAME_BYTES).await
    }

    /// A publishes `n` frames of `size` bytes, in the order its publish run
    /// does it: its own file under its root, its own row `own_held`, seeded
    /// and claimed — THEN the hub's row with the real hashes and A's
    /// implicit claim (so no peer ever asks before A can serve).
    pub(crate) async fn a_publishes_big(&self, n: usize, size: usize) -> Vec<String> {
        let uuids = self.fresh_uuids(n);
        for uuid in &uuids {
            self.a_publishes_as(uuid, FetchRig::pattern(uuid, 1, size))
                .await;
        }
        uuids
    }

    /// A publishes one frame of exactly `bytes` (identical content in two
    /// frames: C10).
    pub(crate) async fn a_publishes_bytes(&self, bytes: &[u8]) -> String {
        let uuid = self.fresh_uuids(1).remove(0);
        self.a_publishes_as(&uuid, bytes.to_vec()).await;
        uuid
    }

    async fn a_publishes_as(&self, uuid: &String, bytes: Vec<u8>) {
        let dir = self.a.root.join("m31").join("Alice");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let path = dir.join(format!("{uuid}.fits"));
            std::fs::write(&path, &bytes).unwrap();
            let blake3 = blake3::hash(&bytes).to_hex().to_string();
            let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
            let len = bytes.len() as i64;
            let file_name = format!("{uuid}.fits");
            let view = crate::collab::hub_client::FrameViewWire {
                frame_uuid: uuid.clone(),
                frame_seq: 0,
                publisher_account_id: "acc-a".into(),
                publisher_display_name: "Alice".into(),
                own: true,
                file_name: file_name.clone(),
                content_version: 1,
                blake3: blake3.clone(),
                byte_size: len,
                xxh3: xxh3.clone(),
                filter_raw: "L".into(),
                filter_canonical: "L".into(),
                channel: "mono".into(),
                exptime_sec: 300.0,
                date_obs: None,
                meta: serde_json::json!({}),
                gate_version: 0,
                accepted: true,
                accepted_reason: None,
                state: "published".into(),
                reject_reason: None,
                manifest_version: 0,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            {
                let conn = crate::api::db(&self.a.ctx).unwrap().conn();
                crate::db::collab_frames::upsert_from_manifest(&conn, PID, &view).unwrap();
            }
            self.a
                .node
                .seed_project_frame(PID, uuid, 1, &path)
                .await
                .expect("seed A's own frame");
            let stamp =
                crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&path).unwrap());
            {
                let conn = crate::api::db(&self.a.ctx).unwrap().conn();
                crate::db::collab_frames::update_landed_path(
                    &conn,
                    PID,
                    uuid,
                    &path.to_string_lossy(),
                )
                .unwrap();
                crate::db::collab_frames::set_size_mtime_seen(&conn, PID, uuid, &stamp.encode())
                    .unwrap();
                crate::db::collab_live::add_implicit_claim(&conn, PID, uuid, 1).unwrap();
                conn.execute(
                    "UPDATE project_frames_local SET local_state = 'own_held', on_disk = 1
                     WHERE project_id = ?1 AND frame_uuid = ?2",
                    rusqlite::params![PID, uuid],
                )
                .unwrap();
            }
            self.hub
                .seed_frames_with(PID, "acc-a", &[uuid.as_str()], "published", |f| {
                    f.blake3 = blake3.clone();
                    f.xxh3 = xxh3.clone();
                    f.byte_size = len;
                    f.file_name = file_name.clone();
                });
        }
    }

    /// A publishes a new version of `uuid` with different bytes of the same
    /// size, as its publish run does: the new file, its own row at the new
    /// version (own_held, seeded, its implicit claim), then the hub's
    /// versions call as A (the hub writes A's implicit claim and bumps).
    pub(crate) async fn a_republishes_changed(&self, uuid: &str) {
        let row = self.a.row(uuid).expect("A's own row");
        let next = row.content_version + 1;
        let bytes = FetchRig::pattern(uuid, next, row.byte_size as usize);
        let path = self
            .a
            .root
            .join("m31")
            .join("Alice")
            .join(format!("{uuid}.v{next}.fits"));
        std::fs::write(&path, &bytes).unwrap();
        let blake3 = blake3::hash(&bytes).to_hex().to_string();
        let xxh3 = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes));
        self.a
            .node
            .seed_project_frame(PID, uuid, next, &path)
            .await
            .expect("seed A's new version");
        let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&path).unwrap());
        {
            let conn = crate::api::db(&self.a.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET content_version = ?3, blake3 = ?4, xxh3 = ?5,
                     landed_path = ?6, size_mtime_seen = ?7, local_state = 'own_held', on_disk = 1
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![
                    PID,
                    uuid,
                    next,
                    blake3,
                    xxh3,
                    path.to_string_lossy(),
                    stamp.encode()
                ],
            )
            .unwrap();
            crate::db::collab_live::add_implicit_claim(&conn, PID, uuid, next).unwrap();
        }
        let client = crate::collab::hub_client::CollabClient::new(self.hub.uri()).unwrap();
        let reply = client
            .frame_versions(
                "tok-a",
                PID,
                &[crate::collab::live::wire::VersionInWire {
                    uuid: uuid.to_string(),
                    expected_version: row.content_version,
                    blake3,
                    byte_size: row.byte_size,
                    xxh3,
                }],
            )
            .await
            .expect("the hub takes the new version");
        assert_eq!(reply.results[0].content_version, next);
    }
}
