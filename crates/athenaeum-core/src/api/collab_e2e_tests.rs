//! Slice-5 capstone: the three-instance collaboration E2E (§11, Д5).
//!
//! One process, three tempdir-file-backed [`ServiceContext`]s — CONTRIB (a
//! send-only contributor), COORD (the coordinator, receives + moderates + serves),
//! and PROC (a send_receive processor) — wired over a single in-memory
//! [`LoopbackNetwork`] plus three wiremock account hubs. The scenario walks the
//! whole feature end to end:
//!
//! 1. CONTRIB publishes two gate-passing calibrated lights (require-approval ⇒
//!    `pending`), which push-seeds the review copy to the coordinator.
//! 2. COORD's receiver ingests the push-seed as contributions (never into
//!    `files`/`frames`) and the moderation queue lists it.
//! 3. COORD approves; the local package flips to `published`.
//! 4. PROC downloads the now-published package from COORD (the Task-6 serve
//!    circuit) and reports-have to its hub.
//! 5. PROC exports the project to a per-publisher WBPP tree, byte-identical to
//!    CONTRIB's stamped payloads, while PROC's catalog stays empty.
//!
//! Deflaked with `wait_until` on SQL state only — no fixed sleeps. `#[cfg(unix)]`
//! + a multi-thread runtime, because the loopback engines/receivers run their
//! event loops on background tasks.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::Connection;

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::collab::{
    decide_announcement, link_frame_set, list_moderation_queue, publish_collab_frames,
};
use crate::api::collab_exchange::{
    download_project_package, export_project_for_wbpp, handle_project_request,
    refresh_project_packages, CollabCleanupSink,
};
use crate::api::db;
use crate::db::collab::{upsert_project, CollabProjectRow};
use crate::db::collab_exchange::{contributions_for_package, get_package};
use crate::db::light_calibrations::{
    upsert_light_calibration, FlatNormMode, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
};
use crate::services::ServiceContext;
use crate::sharing::loopback::{LoopbackNetwork, LoopbackTransport};
use crate::sharing::types::NodeId;
use crate::sharing::SharingTransport;
use crate::sync::{
    allow_all_peers, node_id_hex, CatalogSyncStore, IncomingResolver, ProjectAnnounceGate,
    ProjectAnnouncementsRefresher, ProjectReceiveHooks, ProjectRequestHandler, StandaloneSyncStore,
    StartedSender, SyncConfig, SyncEngine, SyncEngineHandle, SyncReceiver, SyncRuntime,
    SyncSenderRuntime, SyncStore,
};

// ── Fixtures (reproduced-not-widened from the `api::collab` / `api::collab_exchange`
//    test mods; trimmed to this scenario) ──────────────────────────────────────

/// A minimal file-backed-`Database` [`ServiceContext`] (no keychain). A tempdir
/// FILE-backed `Database` (not `:memory:`) so the pool + each receiver's own
/// `CatalogSyncStore` see one catalog file. Copied from the `api::collab` /
/// `api::sync` test mods.
fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
    use crate::cache::MemoryImageCache;
    use crate::services::compute_queue::ComputeQueue;
    use crate::services::operation_queue::OperationQueue;
    use crate::settings::SettingsManager;
    use std::sync::{Mutex, OnceLock};
    #[cfg(all(feature = "render", feature = "solver"))]
    use std::sync::RwLock;

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
        active_registrations: Arc::new(Mutex::new(HashMap::new())),
        active_archives: Arc::new(Mutex::new(HashMap::new())),
        active_master_builds: Arc::new(Mutex::new(HashMap::new())),
        active_light_cal: Arc::new(Mutex::new(HashMap::new())),
        #[cfg(all(feature = "render", feature = "solver"))]
        dso_catalog: Arc::new(RwLock::new(None)),
        #[cfg(feature = "solver")]
        star_cache: Arc::new(RwLock::new(None)),
        #[cfg(feature = "solver")]
        bright_cache: Arc::new(RwLock::new(None)),
        image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
        operation_queue: OperationQueue::start(),
        compute_queue: ComputeQueue::new(),
    };
    (tmp, ctx)
}

/// Point `ctx`'s account hub at `uri` + store a device token (mirrors
/// `api::collab::wire_hub`, but with a distinct per-ctx token).
fn wire_hub(ctx: &ServiceContext, uri: &str, token: &str) {
    {
        let conn = db(ctx).unwrap().conn();
        crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
    }
    crate::api::account::store_token_for_test(ctx, token).unwrap();
}

/// This device's node id for `ctx`'s sync dir (the identity the download role
/// guard / publish own-name resolve against the cached snapshot).
fn own_node_for(ctx: &ServiceContext) -> NodeId {
    let (sync_dir, _) = crate::api::sync::sync_paths(ctx).unwrap();
    DeviceKey::load_or_create(&device_key_path(&sync_dir))
        .unwrap()
        .node_id()
}

/// One `SnapshotMember` entry (camelCase), listing every device pubkey in
/// `nodes` (a member may carry both its device node and a wire endpoint node).
fn member_json(display: &str, data_role: &str, coordinator: bool, nodes: &[NodeId]) -> serde_json::Value {
    let nodes_b64: Vec<String> = nodes.iter().map(|n| B64.encode(n)).collect();
    serde_json::json!({
        "accountId": format!("acc-{display}"),
        "displayName": display,
        "dataRole": data_role,
        "coordinator": coordinator,
        "nodes": nodes_b64,
    })
}

/// Cache the project snapshot (title "M 101", target M101 @ 210.8/54.35 r1.5°)
/// with the passed members + require-approval flag. Reproduced from
/// `api::collab::seed_publish_project`, parameterized on `require_approval`.
fn seed_project(conn: &Connection, project_id: &str, members_json: &str, require_approval: bool) {
    upsert_project(
        conn,
        &CollabProjectRow {
            project_id: project_id.into(),
            slug: "m101".into(),
            title: "M 101".into(),
            data_role: "send_receive".into(),
            is_coordinator: true,
            require_approval,
            pending_announcements: 0,
            project_status: "active".into(),
            target_name: "M101".into(),
            target_ra_deg: 210.8,
            target_dec_deg: 54.35,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "e30=".into(),
            snapshot_signature_b64: "e30=".into(),
            members_json: members_json.into(),
            thresholds_version: Some(4),
            thresholds_rules_json: None,
            fetched_at: String::new(),
        },
    )
    .unwrap();
}

/// A frame set of gate-passing, calibrated LIGHT frames: on-target, known pixel
/// scale, not trailed, each with a real tiny calibrated FITS artifact on disk +
/// a `Calibrated` `light_calibrations` row. Returns the set id. Reproduced from
/// `api::collab::seed_publishable_set`.
fn seed_publishable_set(
    conn: &Connection,
    out_dir: &std::path::Path,
    name: &str,
    uuids: &[&str],
) -> i64 {
    std::fs::create_dir_all(out_dir).unwrap();
    conn.execute(
        "INSERT INTO frames_set (name, objctra, objctdec) VALUES (?1, '14:03:12', '+54:21:00')",
        [name],
    )
    .unwrap();
    let set_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
         VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
        [set_id],
    )
    .unwrap();
    let night_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'ASI2600MM')",
        [night_id],
    )
    .unwrap();
    let session_id = conn.last_insert_rowid();

    for (i, uuid) in uuids.iter().enumerate() {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at) \
             VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS', '2026-07-01T21:00:00Z')",
            rusqlite::params![
                format!("/data/{name}/L_{i:04}.fits"),
                format!("L_{i:04}.fits")
            ],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, exptime, filter, uuid) \
             VALUES (?1, 'Light', 'M101', 'ASI2600MM', 210.8, 54.35, 3.76, 1000.0, 300.0, 'L', ?2)",
            rusqlite::params![file_id, uuid],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![session_id, frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frame_analysis \
             (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity, median_snr, \
              median_hfr, frame_snr, snr_weight, psf_signal, background, noise, \
              detection_threshold, width, height, source_channels, trail_r_squared, possibly_trailed) \
             VALUES (?1, ?2, 400, 2.0, 0.4, 10.0, 2.0, 10.0, 1.0, 100.0, 10.0, 1.0, 5.0, \
                     6248, 4176, 1, 0.0, 0)",
            rusqlite::params![frame_id, file_id],
        )
        .unwrap();

        let out_path = out_dir.join(format!("c_{uuid}.fits"));
        crate::fits_writer::write_fits_f32(&out_path, 4, 4, 1, &vec![0.5f32; 16], &[]).unwrap();
        upsert_light_calibration(
            conn,
            &LightCalRow {
                id: 0,
                frame_id: Some(frame_id),
                source_uuid: Some(uuid.to_string()),
                source_filename: Some(format!("L_{i:04}.fits")),
                output_path: out_path.to_string_lossy().to_string(),
                dark_set_id: None,
                flat_set_id: None,
                bias_set_id: None,
                calstat: "B".to_string(),
                flat_norm_applied: false,
                flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
                output_hash: "deadbeef".to_string(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-07-10T00:00:00Z".to_string(),
                cal_params: "{}".to_string(),
            },
        )
        .unwrap();
    }
    set_id
}

/// One announcement wire row (camelCase), as `GET /announcements` returns it.
fn announcement_json(
    id: &str,
    package_id: &str,
    state: &str,
    anchor: &str,
    frame_count: i64,
    holders: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "packageId": package_id,
        "publisherDisplayName": "Contrib",
        "own": false,
        "rootHash": "a".repeat(64),
        "byteSize": 4096,
        "frameCount": frame_count,
        "aggregateStats": { "manifestXxh3": anchor },
        "supersedes": [],
        "state": state,
        "rejectReason": null,
        "createdAt": "2026-07-13T00:00:00Z",
        "decidedAt": null,
        "holders": holders,
    })
}

async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Spawn a loopback-backed collab sender engine on a fresh endpoint of `net`,
/// targeting `peer`, and register it into `sender` keyed by `peer` (so
/// `ensure_collab_sender_engine` short-circuits to it). `config = None` ⇒ the
/// production default (30s ack timeout) + [`CollabCleanupSink`]; `Some(cfg)` ⇒ a
/// test-scaled config with NO sink (the push-seed side, whose first announce is
/// dropped until the coordinator's row appears, needs a fast retry). Returns the
/// engine's endpoint node id + the engine handle.
async fn start_collab_engine(
    sender: &SyncSenderRuntime,
    net: &LoopbackNetwork,
    store_path: PathBuf,
    peer: NodeId,
    config: Option<SyncConfig>,
) -> (NodeId, Arc<SyncEngineHandle>) {
    let ep = net.endpoint();
    let node = ep.node_id();
    let store = Arc::new(StandaloneSyncStore::open(store_path).unwrap());
    let engine = Arc::new(match config {
        None => SyncEngine::spawn_with_sink_and_emitter(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(ep) as Arc<dyn SharingTransport>,
            peer,
            Arc::new(CollabCleanupSink),
            None,
        ),
        Some(cfg) => SyncEngine::spawn_with_config_and_emitter(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(ep) as Arc<dyn SharingTransport>,
            peer,
            cfg,
            None,
        ),
    });
    sender.lock_inner().await.insert(
        peer,
        StartedSender {
            engine: Arc::clone(&engine),
            origin_device: node_id_hex(&node),
            peer,
        },
    );
    (node, engine)
}

// ── Step 1 smoke: the fixture layer compiles and publish returns `pending` ─────

/// A require-approval publish over a wiremock hub + a loopback coordinator seed
/// target returns `state == "pending"` and seeds the COORDINATOR — the publish
/// half of the scenario, validating the fixture layer before the full E2E.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_seeds_pending_smoke() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROJECT: &str = "proj-smoke";

    let (tmp, ctx) = test_ctx();
    let contrib_device = own_node_for(&ctx);

    // A throwaway coordinator seed target (no receiver on the other side — the
    // seed only needs to enqueue cleanly).
    let coord_seed = [0x77u8; 32];
    let members = serde_json::json!([
        member_json("Coord", "send_receive", true, &[coord_seed]),
        member_json("Contrib", "send", false, &[contrib_device]),
    ])
    .to_string();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "ann-1", "state": "pending"
        })))
        .mount(&server)
        .await;
    wire_hub(&ctx, &server.uri(), "smoke-tok");

    let out_dir = tmp.path().join("cal_out");
    let set_id = {
        let conn = db(&ctx).unwrap().conn();
        seed_project(&conn, PROJECT, &members, true);
        seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-s-1", "uuid-s-2"])
    };
    link_frame_set(&ctx, PROJECT, set_id).unwrap();

    let sender = SyncSenderRuntime::new();
    let seed_net = LoopbackNetwork::new();
    let (_n, engine) =
        start_collab_engine(&sender, &seed_net, tmp.path().join("smoke_send.db"), coord_seed, None)
            .await;

    let result = publish_collab_frames(&ctx, &sender, PROJECT, None).await.unwrap();
    assert_eq!(result.state, "pending", "require-approval publish is pending");
    assert_eq!(result.frame_count, 2);
    assert_eq!(
        result.seed_target.as_deref(),
        Some("Coord"),
        "a pending package seeds the coordinator"
    );

    engine.shutdown().await;
}

// ── Step 2: the full three-instance scenario ──────────────────────────────────

/// CONTRIB publishes → COORD ingests the push-seed + moderates → COORD approves →
/// PROC downloads over the serve circuit → PROC exports a per-publisher WBPP tree
/// byte-identical to CONTRIB's payloads, with PROC's catalog untouched (§11).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_instance_project_flow_publish_moderate_deliver_export() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROJECT: &str = "proj-e2e";
    const ANN: &str = "ann-1";
    const UUID_A: &str = "aaaaaaa1-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const UUID_B: &str = "aaaaaaa2-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    // ── Three contexts. COORD is `Arc` (its receiver hooks capture it). ────────
    let (contrib_tmp, contrib_ctx) = test_ctx();
    let (coord_tmp, coord_ctx) = test_ctx();
    let coord_ctx = Arc::new(coord_ctx);
    let (proc_tmp, proc_ctx) = test_ctx();

    let (contrib_sync_dir, _) = crate::api::sync::sync_paths(&contrib_ctx).unwrap();
    let (coord_sync_dir, coord_db_path) = crate::api::sync::sync_paths(&coord_ctx).unwrap();
    let (proc_sync_dir, proc_db_path) = crate::api::sync::sync_paths(&proc_ctx).unwrap();

    let contrib_device = own_node_for(&contrib_ctx);
    let proc_device = own_node_for(&proc_ctx);

    // ── Loopback endpoints (nodes minted up front so members/gates can name
    //    them): CONTRIB push, COORD recv+send, PROC recv+request. ──────────────
    let net = LoopbackNetwork::new();
    let coord_recv_ep = net.endpoint();
    let coord_recv_node = coord_recv_ep.node_id();
    let proc_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
    let proc_node = proc_ep.node_id();

    // ── Shared membership snapshot (identical in all three caches). PROC lists
    //    BOTH its device node (own role guard / export own-name) AND its wire
    //    endpoint node (COORD's `may_serve_package` sees the requester). ────────
    let members = serde_json::json!([
        member_json("Coord", "send_receive", true, &[coord_recv_node]),
        member_json("Contrib", "send", false, &[contrib_device]),
        member_json("Processor", "send_receive", false, &[proc_device, proc_node]),
    ])
    .to_string();

    // ── Seed the project + a `collaboration` landing root in all three caches. ─
    let caches: [(&ServiceContext, PathBuf); 3] = [
        (&contrib_ctx, contrib_sync_dir.join("land")),
        (coord_ctx.as_ref(), coord_sync_dir.join("land")),
        (&proc_ctx, proc_sync_dir.join("land")),
    ];
    for (ctx, landing) in caches {
        let conn = db(ctx).unwrap().conn();
        seed_project(&conn, PROJECT, &members, true);
        conn.execute(
            "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
            [landing.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    // ── CONTRIB's publishable set. ─────────────────────────────────────────────
    let contrib_out = contrib_tmp.path().join("cal_out");
    let set_id = {
        let conn = db(&contrib_ctx).unwrap().conn();
        seed_publishable_set(&conn, &contrib_out, "M101 Set", &[UUID_A, UUID_B])
    };
    link_frame_set(&contrib_ctx, PROJECT, set_id).unwrap();

    // ── CONTRIB's push-seed engine → COORD's recv node. Short ack timeout: the
    //    first announce is dropped until COORD's row appears, so the retry must
    //    be fast (no sink — CONTRIB's payloads are captured before delivery). ──
    let contrib_sender = SyncSenderRuntime::new();
    let (contrib_send_node, contrib_push_engine) = start_collab_engine(
        &contrib_sender,
        &net,
        contrib_tmp.path().join("contrib_send.db"),
        coord_recv_node,
        Some(SyncConfig {
            ack_timeout: Duration::from_millis(400),
            max_attempts: 20,
        }),
    )
    .await;

    // ── COORD's serve engine → PROC's node (default config + sink). ───────────
    let coord_serve_sender = Arc::new(SyncSenderRuntime::new());
    let (coord_send_node, coord_serve_engine) = start_collab_engine(
        &coord_serve_sender,
        &net,
        coord_tmp.path().join("coord_send.db"),
        proc_node,
        None,
    )
    .await;

    // ── COORD's receiver: gate authorizes CONTRIB's push; refresher polls
    //    COORD's hub for the anchor row; request handler serves PROC. ──────────
    let contrib_gate: ProjectAnnounceGate =
        Arc::new(move |from: &NodeId, pid: &str| *from == contrib_send_node && pid == PROJECT);
    let coord_refresher: ProjectAnnouncementsRefresher = {
        let ctx = Arc::clone(&coord_ctx);
        Arc::new(move |pid: &str| {
            let ctx = Arc::clone(&ctx);
            let pid = pid.to_string();
            tokio::spawn(async move {
                let _ = refresh_project_packages(&ctx, &pid).await;
            });
        })
    };
    let coord_serve_handler: ProjectRequestHandler = {
        let ctx = Arc::clone(&coord_ctx);
        let sender = Arc::clone(&coord_serve_sender);
        Arc::new(move |from: NodeId, project: String, package: String| {
            let ctx = Arc::clone(&ctx);
            let sender = Arc::clone(&sender);
            tokio::spawn(async move {
                if let Err(e) =
                    handle_project_request(&ctx, &sender, from, project, package, None).await
                {
                    tracing::error!(error = %format!("{e:#}"), "e2e: coord serve failed");
                }
            });
        })
    };
    let coord_store = Arc::new(CatalogSyncStore::open(&coord_db_path).unwrap());
    let coord_incoming: IncomingResolver = {
        let p = coord_sync_dir.join("incoming");
        Arc::new(move || p.clone())
    };
    let (_coord_info, _coord_handle) = SyncReceiver::spawn(
        coord_store,
        coord_sync_dir.clone(),
        coord_incoming,
        allow_all_peers(),
        ProjectReceiveHooks {
            gate: Some(contrib_gate),
            announcements_refresher: Some(coord_refresher),
            request_handler: Some(coord_serve_handler),
            ..Default::default()
        },
        Arc::new(coord_recv_ep) as Arc<dyn SharingTransport>,
        Arc::new(crate::events::NullEmitter),
    )
    .await
    .unwrap();

    // ── PROC's receiver + a started SyncRuntime over the same endpoint. Its
    //    gate authorizes COORD's SEND node (the serve announce). ───────────────
    let proc_gate: ProjectAnnounceGate =
        Arc::new(move |from: &NodeId, pid: &str| *from == coord_send_node && pid == PROJECT);
    let proc_store = Arc::new(CatalogSyncStore::open(&proc_db_path).unwrap());
    let proc_incoming: IncomingResolver = {
        let p = proc_sync_dir.join("incoming");
        Arc::new(move || p.clone())
    };
    let (_proc_info, proc_handle) = SyncReceiver::spawn(
        proc_store,
        proc_sync_dir.clone(),
        proc_incoming,
        allow_all_peers(),
        ProjectReceiveHooks { gate: Some(proc_gate), ..Default::default() },
        Arc::clone(&proc_ep) as Arc<dyn SharingTransport>,
        Arc::new(crate::events::NullEmitter),
    )
    .await
    .unwrap();
    let proc_runtime = SyncRuntime::new();
    proc_runtime
        .set_started_for_test(
            Arc::clone(&proc_ep) as Arc<dyn SharingTransport>,
            proc_handle,
            "ticket".into(),
        )
        .await;

    // ── Three account hubs (one per ctx, distinct tokens). CONTRIB's announce
    //    is mounted up front; COORD/PROC list mocks carry the anchor and are
    //    mounted after publish. ────────────────────────────────────────────────
    let contrib_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": ANN, "state": "pending"
        })))
        .mount(&contrib_server)
        .await;
    wire_hub(&contrib_ctx, &contrib_server.uri(), "contrib-tok");

    let coord_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/announcements/{ANN}/approve")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": ANN, "state": "published"
        })))
        .mount(&coord_server)
        .await;
    wire_hub(&coord_ctx, &coord_server.uri(), "coord-tok");

    let proc_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/announcements/{ANN}/have")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&proc_server)
        .await;
    wire_hub(&proc_ctx, &proc_server.uri(), "proc-tok");

    // ═══ Step 3 (brief): CONTRIB publishes → pending, seeds the coordinator. ═══
    let result = publish_collab_frames(&contrib_ctx, &contrib_sender, PROJECT, None)
        .await
        .unwrap();
    assert_eq!(result.state, "pending", "require-approval publish is pending");
    assert_eq!(result.frame_count, 2, "both gate-passing lights published");
    assert_eq!(
        result.seed_target.as_deref(),
        Some("Coord"),
        "a pending package push-seeds the coordinator"
    );
    let pkg = result.package_id.clone();

    // Capture CONTRIB's stamped payloads NOW — before COORD can fetch/ingest (its
    // list mock isn't mounted yet), so the retained pub dir is intact. These are
    // the exact bytes CONTRIB serves; the whole chain is anchor-verified against
    // them.
    let pub_dir = contrib_sync_dir.join("collab_pub").join(&pkg);
    let mut stamped: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in std::fs::read_dir(&pub_dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("fits") {
            stamped.insert(
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read(&p).unwrap(),
            );
        }
    }
    assert_eq!(stamped.len(), 2, "two stamped payloads retained by CONTRIB");

    // The real manifest anchor CONTRIB computed — the coordinator/processor
    // receivers verify the served bytes against it.
    let anchor = {
        let conn = db(&contrib_ctx).unwrap().conn();
        get_package(&conn, &pkg).unwrap().unwrap().manifest_xxh3.unwrap()
    };

    // ═══ Step 4 (brief): COORD ingests the push-seed; moderation queue lists it. ═
    // Mount COORD's list mock (pending, with the anchor) and poll once so the row
    // exists; the push-seed's retry announce then lands the ingest.
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            announcement_json(ANN, &pkg, "pending", &anchor, 2, serde_json::json!([]))
        ])))
        .mount(&coord_server)
        .await;
    refresh_project_packages(&coord_ctx, PROJECT).await.unwrap();

    let pkg_for_coord = pkg.clone();
    let coord_for_wait = Arc::clone(&coord_ctx);
    wait_until(
        move || {
            let conn = db(&coord_for_wait).unwrap().conn();
            get_package(&conn, &pkg_for_coord)
                .ok()
                .flatten()
                .map(|r| r.local_status)
                == Some("complete".to_string())
        },
        Duration::from_secs(15),
    )
    .await;

    {
        // The review copy landed as two contributions — never into the catalog.
        let conn = db(&coord_ctx).unwrap().conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 0, "coord catalog untouched");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 0, "coord catalog untouched");
        assert_eq!(
            contributions_for_package(&conn, &pkg).unwrap().len(),
            2,
            "both review frames landed as contributions"
        );
    }

    let queue = list_moderation_queue(&coord_ctx, PROJECT).unwrap();
    assert_eq!(queue.len(), 1, "one pending package awaits moderation");
    let item = &queue[0];
    assert_eq!(item.announcement_id, ANN);
    assert_eq!(item.publisher, "Contrib");
    assert_eq!(item.frames.len(), 2, "two review frames with metrics");
    assert!(item.review_copy_complete, "the review copy is complete");
    assert!(
        item.frames.iter().all(|f| f.fwhm == Some(2.0)),
        "each frame's FWHM parsed from the stamped analysis"
    );

    // ═══ Step 5 (brief): COORD approves → local state published. ═══════════════
    decide_announcement(&coord_ctx, ANN, true, None).await.unwrap();
    {
        let conn = db(&coord_ctx).unwrap().conn();
        let row = get_package(&conn, &pkg).unwrap().unwrap();
        assert_eq!(row.state, "published", "approve flips COORD's local state");
        assert!(row.decided_at.is_some(), "decided_at stamped");
    }

    // ═══ Step 6 (brief): PROC downloads the published package from COORD. ══════
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            announcement_json(
                ANN,
                &pkg,
                "published",
                &anchor,
                2,
                serde_json::json!([
                    { "pubkey": B64.encode(coord_recv_node), "displayName": "Coord",
                      "lastSeenAt": chrono::Utc::now().to_rfc3339() }
                ])
            )
        ])))
        .mount(&proc_server)
        .await;

    download_project_package(&proc_ctx, &proc_runtime, PROJECT, &pkg)
        .await
        .unwrap();

    {
        let conn = db(&proc_ctx).unwrap().conn();
        // SOLE failure gate: download_project_package returns Ok(()) even when the
        // download fails internally (an exhausted-holder attempt is a normal Ok),
        // so this local_status assert is the only thing that fails the step on a
        // broken download. Do not delete it as redundant with the `.unwrap()` above.
        assert_eq!(
            get_package(&conn, &pkg).unwrap().unwrap().local_status,
            "complete",
            "PROC observed the serve ingest"
        );
        assert_eq!(
            contributions_for_package(&conn, &pkg).unwrap().len(),
            2,
            "PROC landed both served frames as contributions"
        );
    }
    // report_have hit PROC's hub exactly once (also `.expect(1)` above).
    let haves = proc_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == format!("/api/v1/announcements/{ANN}/have"))
        .count();
    assert_eq!(haves, 1, "PROC reported have once after ingest");

    // ═══ Step 7 (brief): PROC exports the project WBPP tree. ═══════════════════
    let out = proc_tmp.path().join("wbpp");
    let export = export_project_for_wbpp(&proc_ctx, PROJECT, &out.to_string_lossy(), false, None)
        .await
        .unwrap();
    assert!(export.success, "project export succeeded: {:?}", export.error);
    assert_eq!(export.files_organized, 2, "two received frames organized");

    // <out>/<title>/<publisher>/camera_<instrume>/lights/<name>, byte-identical
    // to CONTRIB's stamped payloads (the whole chain preserved them).
    use crate::export::models::{sanitize_display_folder_name, sanitize_folder_name};
    let lights_dir = out
        .join(sanitize_display_folder_name("M 101"))
        .join(sanitize_display_folder_name("Contrib"))
        .join(format!("camera_{}", sanitize_folder_name("ASI2600MM")))
        .join("lights");
    let mut exported: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in std::fs::read_dir(&lights_dir).unwrap_or_else(|e| panic!("read {lights_dir:?}: {e}")) {
        let p = entry.unwrap().path();
        exported.insert(
            p.file_name().unwrap().to_string_lossy().to_string(),
            std::fs::read(&p).unwrap(),
        );
    }
    assert_eq!(exported.len(), 2, "exactly two files under the lights dir");
    let stamped_names: HashSet<&String> = stamped.keys().collect();
    let exported_names: HashSet<&String> = exported.keys().collect();
    assert_eq!(
        exported_names, stamped_names,
        "exported files carry their original publisher-side names"
    );
    for (name, bytes) in &exported {
        assert_eq!(
            bytes,
            stamped.get(name).unwrap(),
            "exported {name} is byte-identical to CONTRIB's stamped payload"
        );
    }

    // §11 catalog isolation: contributions never enter PROC's catalog.
    {
        let conn = db(&proc_ctx).unwrap().conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 0, "PROC files stay empty");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 0, "PROC frames stay empty");
    }

    // ═══ Step 8 (brief): teardown — stop the engines (receivers stop on drop). ═
    contrib_push_engine.shutdown().await;
    coord_serve_engine.shutdown().await;
}
