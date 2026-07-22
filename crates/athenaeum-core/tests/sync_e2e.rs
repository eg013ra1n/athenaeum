//! Two-instance personal-sync end-to-end harness (Stage I, task M5, CI variant).
//!
//! This is the loopback, single-machine counterpart of the manual M-Sync1 proof
//! (`scripts/sync_e2e_manual.md`, which runs the same shape over real iroh + a
//! relay + hub pairing). It stands up TWO full [`ServiceContext`]s — a *capture*
//! node and a *primary* node — each with its own temp catalog DB and data dir,
//! and wires the capture node's real app sender (the M2a
//! [`enqueue_sync_selection`] path) to the primary node's real receiver
//! ([`SyncReceiver`]) over the in-process
//! [`LoopbackNetwork`](athenaeum_core::sharing::loopback::LoopbackNetwork). No
//! iroh, no hub, no keychain — the loopback transport is the observable oracle
//! for the exact engine/ingest/retention code that ships.
//!
//! It asserts, entirely via SQL against both catalog DBs, the four invariants
//! Stage I must hold before M-Sync1 sign-off:
//!
//! 1. **Full metadata delivery.** 50 fixture frames enqueued on the capture node
//!    land as 50 `files` + 50 `frames` rows on the primary, each carrying its
//!    catalog `uuid`, and a sampled `object` / `exptime` survive end to end.
//! 2. **Dedupe safety.** Re-running the identical selection creates NO new
//!    primary rows; every second-delivery ack is `Duplicate` (proven from the
//!    sender's own confirm history), so a lost ack / resend never double-ingests.
//! 3. **Retention safety.** Live mode (`on_confirm`, `dry_run=false` + the
//!    explicit opt-in) deletes ONLY confirmed synced sources — a never-synced
//!    "keeper" file in the same catalog is untouched — and records both the
//!    transfer event and the `retention_deleted` audit for each deleted frame.
//! 4. **History completeness on both ends** — 50 sender confirms, 50 receiver
//!    ingests, all searchable.
//!
//! The harness reaches production code through one intentionally minimal
//! test-support surface: [`ServiceContext::new_for_tests`] (a `#[doc(hidden)]`
//! constructor; see its doc comment). Everything else — `enqueue_sync_selection`,
//! `SyncReceiver`, the retention `evaluate` seam, the loopback transport, the
//! catalog store — is the same public API the desktop/web hosts use.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use athenaeum_core::api::sync::{
    cancel_incoming_package, cancel_sync_package, delete_transfer_history, enqueue_sync_selection,
    get_status, get_transfer_storage, list_terminal_transfers, list_transfer_files,
    resurrect_pending_senders_with, retry_sync_package, ResolvedDest,
};
use athenaeum_core::api::ApiError;
use athenaeum_core::api::retention::{evaluate, AppRetentionConfig};
use athenaeum_core::db::{insert_file, insert_frame, Database};
use athenaeum_core::events::{NullEmitter, ProgressEmitter};
use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::models::{File, FileFormat, Frame, ImageType};
use athenaeum_core::services::ServiceContext;
use athenaeum_core::sharing::iroh::proto::{FullHashEntry, OfferEntry};
use athenaeum_core::sharing::loopback::{FaultPlan, LoopbackNetwork};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::{
    allow_all_peers, node_id_hex, CatalogDedupResponder, CatalogSyncStore, DedupResponder,
    Direction, InboundControl, RetentionPolicy, StartedSender, SyncConfig, SyncEngine,
    SyncEngineHandle, SyncReceiver, SyncRuntime, SyncSenderRuntime, SyncStore,
};
use chrono::Utc;

/// 50 frames per the brief; small fixtures keep the whole run well under a minute
/// on loopback (the two-minute budget is never close).
const N: usize = 50;

/// Poll budget for a loopback confirm/ingest to land — generous vs the actual
/// sub-second latency so the test is not timing-fragile under CI load.
const WAIT: Duration = Duration::from_secs(30);

/// A little variety so the metadata assertions check *real* distinct values, not
/// one repeated constant that a broken pipeline could accidentally satisfy.
const OBJECTS: [&str; 5] = ["M42", "M31", "NGC7000", "M45", "IC1805"];

/// Count rows for `sql` against a catalog DB (fresh pooled connection each call
/// so a WAL reader always sees the latest committed writes from the engine /
/// receiver connections).
fn count(db: &Database, sql: &str) -> i64 {
    let conn = db.conn();
    let n: i64 = conn.query_row(sql, [], |r| r.get(0)).unwrap();
    n
}

/// One-off scalar helper for a `frame_uuid`-parameterised metadata probe.
fn scalar_opt_str(db: &Database, sql: &str, uuid: &str) -> Option<String> {
    let conn = db.conn();
    conn.query_row(sql, [uuid], |r| r.get(0)).unwrap()
}

fn scalar_opt_f64(db: &Database, sql: &str, uuid: &str) -> Option<f64> {
    let conn = db.conn();
    conn.query_row(sql, [uuid], |r| r.get(0)).unwrap()
}

/// Write a real single-frame FITS on disk and insert its `files` + `frames`
/// rows into `ctx`'s catalog. Returns `(frame_id, uuid, object, exptime)`; the
/// uuid is the trigger-assigned `frames.uuid` (identity anchor the receiver
/// dedups on, and what a re-run reuses to stay dedupe-safe).
fn insert_capture_frame(
    ctx: &ServiceContext,
    files_dir: &Path,
    idx: usize,
) -> (i64, String, String, f64) {
    let object = OBJECTS[idx % OBJECTS.len()];
    let exptime = 60.0 + (idx % 5) as f64 * 30.0;
    let filename = format!("light_{idx:04}.fits");
    let path = files_dir.join(&filename);

    let cards = HeaderBuilder::new(FrameKind::Light)
        .object(object)
        .exptime(exptime)
        .filter("Ha")
        .instrume("TestCam")
        .build()
        .expect("build fixture header");
    // Distinct pixel value per frame → distinct full-content xxh3 (defensive:
    // uuid dedup is the real anchor, but distinct bytes keep the package honest).
    let data = vec![idx as f32; 8 * 8];
    write_fits_f32(&path, 8, 8, 1, &data, &cards).expect("write fixture fits");
    let size = std::fs::metadata(&path).unwrap().len() as i64;

    let db = ctx.db.get().expect("ctx db");
    let conn = db.conn();
    let file = File {
        id: None,
        path: path.to_string_lossy().to_string(),
        filename: filename.clone(),
        size,
        modified_at: Utc::now(),
        format: FileFormat::FITS,
        created_at: Utc::now(),
        metadata_hash: None,
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    };
    let file_id = insert_file(&conn, &file).expect("insert file");
    let frame = Frame {
        file_id,
        object: Some(object.to_string()),
        exptime: Some(exptime),
        imagetyp: Some(ImageType::Light),
        ..Default::default()
    };
    let frame_id = insert_frame(&conn, &frame).expect("insert frame");
    // The `frames_identity` AFTER INSERT trigger fills uuid + updated_at.
    let uuid: String = conn
        .query_row("SELECT uuid FROM frames WHERE id = ?1", [frame_id], |r| r.get(0))
        .expect("read trigger-assigned uuid");
    (frame_id, uuid, object.to_string(), exptime)
}

/// Poll `pred` every 25ms until true, panicking after `timeout`. Async so
/// spawned background tasks (receiver loop, engine worker) make progress between
/// polls.
async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("wait_until timed out after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Records the sender engine's emitted events so the dedup e2e can read the Sync
/// Phase 3 `{new, duplicate}` outcome off each send-side `sync-finished`. Those
/// counts are the *only* place the split surfaces — `enqueue_sync_selection`'s
/// [`EnqueueSelectionResult`] reports the app-layer enqueue size, not what the
/// negotiate handshake actually transferred vs. dropped — so the emitter is the
/// observable oracle for "the re-send moved only the new frames".
#[derive(Default)]
struct RecordingEmitter {
    /// Each event is stamped with the monotonic `Instant` it was emitted, so the
    /// bidirectional e2e can compare stage timings across two emitters (all from
    /// the same process clock) to prove concurrent fetches overlap.
    events: Mutex<Vec<(Instant, String, serde_json::Value)>>,
}

impl ProgressEmitter for RecordingEmitter {
    fn emit_json(&self, name: &str, payload: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((Instant::now(), name.to_string(), payload));
    }
}

impl RecordingEmitter {
    /// The send-side `sync-finished` events seen so far, in emit order, as
    /// `(new_count, duplicate_count)` pairs — one per confirmed package.
    fn sent_finished(&self) -> Vec<(u64, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, p)| name == "sync-finished" && p["direction"] == "sent")
            .map(|(_, _, p)| {
                (
                    p["newCount"].as_u64().expect("newCount is a number"),
                    p["duplicateCount"].as_u64().expect("duplicateCount is a number"),
                )
            })
            .collect()
    }

    /// The receive-side `sync-progress` stage labels in emit order — the observable
    /// trail proving the inbound row walked `received → fetching → ingesting`.
    fn received_stages(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, p)| name == "sync-progress" && p["direction"] == "received")
            .map(|(_, _, p)| p["stage"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The `Instant` of the FIRST received-direction `sync-progress` tick whose
    /// coarse `stage` matches. `None` if no such tick was recorded. The bidirectional
    /// e2e reads `received` / `fetching` / `ingesting` off this to bound the
    /// announce→fetching gap and to intersect the two directions' fetch windows.
    fn first_received_stage_at(&self, stage: &str) -> Option<Instant> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .find(|(_, name, p)| {
                name == "sync-progress" && p["direction"] == "received" && p["stage"] == stage
            })
            .map(|(t, _, _)| *t)
    }

    /// Every `sync-file-progress` tick as `(file, bytes_done, bytes_total)` in emit
    /// order — the per-file byte trail the Transfers UI renders as live bars.
    fn file_progress(&self) -> Vec<(String, u64, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, _)| name == "sync-file-progress")
            .map(|(_, _, p)| {
                (
                    p["file"].as_str().unwrap_or_default().to_string(),
                    p["bytesDone"].as_u64().unwrap_or_default(),
                    p["bytesTotal"].as_u64().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The byte-carrying `fetching`-stage `sync-progress` ticks as
    /// `(bytes_done, bytes_total)` — the cumulative package-fetch progress
    /// (the coarse stage ticks that carry no byte figure are skipped).
    fn fetching_byte_ticks(&self) -> Vec<(u64, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, p)| {
                name == "sync-progress"
                    && p["stage"] == "fetching"
                    && p.get("bytesDone").is_some_and(|v| !v.is_null())
            })
            .map(|(_, _, p)| {
                (
                    p["bytesDone"].as_u64().unwrap_or_default(),
                    p["bytesTotal"].as_u64().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The receive-side `sync-finished` events as `(outcome, ok_count)` pairs, in
    /// emit order — the terminal signal the receiver's Transfers widget dismisses a
    /// row on. B8 scenario 1 reads the revoke path's single terminal `sync-finished`
    /// (outcome == the mapped row state) off this.
    fn received_finished(&self) -> Vec<(String, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, p)| name == "sync-finished" && p["direction"] == "received")
            .map(|(_, _, p)| {
                (
                    p["outcome"].as_str().unwrap_or_default().to_string(),
                    p["okCount"].as_u64().unwrap_or_default(),
                )
            })
            .collect()
    }
}

// ── e2e helpers shared by the acceptance tests (Task 16) ─────────────────────

/// Designate `dir` as the primary's `sync_incoming` scan root (received files
/// land under it, not the internal staging default).
fn designate_incoming(primary_ctx: &ServiceContext, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    athenaeum_core::api::scan_roots::add_scan_root(
        primary_ctx,
        dir.to_string_lossy().into_owned(),
        &athenaeum_core::api::PathPolicy::AllowAll,
        Some("sync_incoming".to_string()),
    )
    .expect("designate sync_incoming root");
}

/// The live per-package landing resolver, built exactly as the host builds it:
/// re-reads the designated `sync_incoming` root from the primary catalog on every
/// package, falling back to `fallback` when none is set.
fn incoming_resolver_for(
    primary_ctx: &ServiceContext,
    fallback: PathBuf,
) -> athenaeum_core::sync::receiver::IncomingResolver {
    let resolver_db = primary_ctx.db.get().unwrap().clone();
    Arc::new(move || {
        let conn = resolver_db.conn();
        match athenaeum_core::db::scan_root_path_of_kind(&conn, "sync_incoming") {
            Ok(Some(p)) => PathBuf::from(p),
            _ => fallback.clone(),
        }
    })
}

/// Build a loopback sender engine targeting `peer` with a short `ack_timeout`
/// (so the retry backoff schedule is ms-scale, per the engine_tests pattern),
/// then inject it into a fresh [`SyncSenderRuntime`] exactly as
/// `ensure_sender_engine` would — so the real `enqueue_sync_selection` /
/// `retry_sync_package` paths run unchanged, minus the iroh/hub build. Returns
/// the pieces those api fns consume.
async fn inject_sender_engine(
    net: &LoopbackNetwork,
    capture_db: &Path,
    peer: NodeId,
    ack_timeout: Duration,
) -> (
    Arc<SyncEngineHandle>,
    Arc<SyncSenderRuntime>,
    Arc<SyncSenderRuntime>,
    SyncRuntime,
) {
    let sender_ep = net.endpoint();
    let sender_node = sender_ep.node_id();
    let engine_store = Arc::new(CatalogSyncStore::open(capture_db).unwrap());
    let engine = Arc::new(SyncEngine::spawn_with_config(
        engine_store as Arc<dyn SyncStore>,
        Arc::new(sender_ep) as Arc<dyn SharingTransport>,
        peer,
        SyncConfig { ack_timeout },
    ));
    let sender = Arc::new(SyncSenderRuntime::new());
    let collab_sender = Arc::new(SyncSenderRuntime::new());
    let sync = SyncRuntime::new();
    {
        let mut guard = sender.lock_inner().await;
        guard.insert(
            peer,
            StartedSender {
                engine: Arc::clone(&engine),
                origin_device: node_id_hex(&sender_node),
                peer,
            },
        );
    }
    (engine, sender, collab_sender, sync)
}

/// Terminal states that crash-resume must not re-drive (mirrors
/// `OutboundState::is_terminal` at the text level).
fn is_terminal_state(s: &str) -> bool {
    matches!(s, "confirmed" | "failed" | "cancelled")
}

/// The newest `sync_outbound` row id (there is exactly one until a retry mints a
/// second). Panics if no row exists — every caller reads it after an enqueue.
fn latest_outbound_id(db: &Database) -> i64 {
    db.conn()
        .query_row("SELECT id FROM sync_outbound ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .expect("an outbound row exists")
}

fn outbound_state(db: &Database, id: i64) -> String {
    db.conn()
        .query_row("SELECT state FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

fn outbound_attempts(db: &Database, id: i64) -> i64 {
    db.conn()
        .query_row("SELECT attempts FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

fn outbound_next_retry(db: &Database, id: i64) -> Option<String> {
    db.conn()
        .query_row("SELECT next_retry_at FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

fn outbound_last_error(db: &Database, id: i64) -> Option<String> {
    db.conn()
        .query_row("SELECT last_error FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

/// The wire `package_id` of the single most-recent `sync_inbound` row (the wire
/// uuid the receiver keys its row on), or `None` before any announce lands.
fn latest_inbound_package_id(db: &Database) -> Option<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT package_id FROM sync_inbound ORDER BY id DESC LIMIT 1")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    rows.next().unwrap().map(|r| r.get(0).unwrap())
}

/// `(state, byte_size, bytes_done)` of the `sync_inbound` row for `package_id`, or
/// `None` if the row does not exist yet.
fn inbound_row(db: &Database, package_id: &str) -> Option<(String, u64, u64)> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT state, byte_size, bytes_done FROM sync_inbound WHERE package_id = ?1")
        .unwrap();
    let mut rows = stmt.query([package_id]).unwrap();
    rows.next().unwrap().map(|r| {
        (
            r.get::<_, String>(0).unwrap(),
            r.get::<_, i64>(1).unwrap().max(0) as u64,
            r.get::<_, i64>(2).unwrap().max(0) as u64,
        )
    })
}

/// Count regular files under `dir` (the designated landing root) — the "files
/// landed" oracle both acceptance directions assert against.
fn landed_count(dir: &Path) -> usize {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
}

/// Count *live* (`deleted_at IS NULL`) `sync_sources` retention rows keyed on
/// `package_ref` — the oracle for "retention follows the payload" across a
/// declined-resend re-key. Parameterised so a tempdir path with odd chars is safe.
fn sync_sources_live_count(db: &Database, package_ref: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_sources WHERE package_ref = ?1 AND deleted_at IS NULL",
            [package_ref],
            |r| r.get(0),
        )
        .expect("count sync_sources")
}

#[tokio::test]
async fn two_instance_sync_e2e() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    // Two full ServiceContexts, each with its own DB + data dir.
    // Owned `Arc` so `enqueue_sync_selection` (which now builds a `'static` retry
    // address-refresher, T8) accepts `&capture_ctx`; `&Arc` still coerces to
    // `&ServiceContext` for every other helper here.
    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // ── Primary receiver over loopback ───────────────────────────────────────
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    // Designate a `sync_incoming` scan root on the primary — received files must
    // land under it, not under the internal `<sync_dir>/incoming` staging default.
    let designated = tmp.path().join("designated_incoming");
    std::fs::create_dir_all(&designated).unwrap();
    athenaeum_core::api::scan_roots::add_scan_root(
        &primary_ctx,
        designated.to_string_lossy().into_owned(),
        &athenaeum_core::api::PathPolicy::AllowAll,
        Some("sync_incoming".to_string()),
    )
    .expect("designate sync_incoming root");

    // The live per-package landing resolver, built exactly as the host builds it:
    // it re-reads the designated `sync_incoming` root from the primary catalog on
    // every package (falling back to `<sync_dir>/incoming` when none is set).
    let resolver_db = primary_ctx.db.get().unwrap().clone();
    let fallback_incoming = primary_dir.join("incoming");
    let incoming: athenaeum_core::sync::receiver::IncomingResolver = Arc::new(move || {
        let conn = resolver_db.conn();
        match athenaeum_core::db::scan_root_path_of_kind(&conn, "sync_incoming") {
            Ok(Some(p)) => std::path::PathBuf::from(p),
            _ => fallback_incoming.clone(),
        }
    });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(athenaeum_core::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // ── Capture sender engine over loopback, injected into a SyncSenderRuntime
    // exactly as `ensure_sender_engine` would populate it — so the real
    // `enqueue_sync_selection` path runs unchanged, minus the iroh/hub build. ──
    let sender_ep = net.endpoint();
    let sender_node = sender_ep.node_id();
    let engine_store = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
    let engine = Arc::new(SyncEngine::spawn(
        engine_store as Arc<dyn SyncStore>,
        Arc::new(sender_ep) as Arc<dyn SharingTransport>,
        receiver_node,
    ));
    let sender = Arc::new(SyncSenderRuntime::new());
    // T6 wake-hook plumbing: the collab sender map is threaded alongside the
    // personal one; unused by this personal-sync e2e (no collab engines started).
    let collab_sender = Arc::new(SyncSenderRuntime::new());
    // T7: `enqueue_sync_selection` threads a SyncRuntime (only touched when
    // `ensure_sender_engine` has to build an engine; here the engine is pre-injected
    // above, so the peers-refresh-timer install is short-circuited and `sync` is
    // unused — a fresh unstarted runtime satisfies the signature).
    let sync = SyncRuntime::new();
    {
        let mut guard = sender.lock_inner().await;
        guard.insert(
            receiver_node,
            StartedSender {
                engine: Arc::clone(&engine),
                origin_device: node_id_hex(&sender_node),
                peer: receiver_node,
            },
        );
    }

    // Name the incoming sender's landing folder by its CURRENT friendly device
    // name: seed the receiver's cached hex → name map (the source the ingest
    // resolver reads with no hub round-trip). Received files must then land under
    // `<designated>/Studio_iMac/...` (sanitized), not the sender's hex slug.
    {
        let mut m = std::collections::HashMap::new();
        m.insert(node_id_hex(&sender_node), "Studio iMac".to_string());
        let conn = pdb.conn();
        athenaeum_core::db::set_setting(
            &conn,
            athenaeum_core::settings::keys::SYNC_DEVICE_NAMES,
            &serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
    }

    // ── Seed 50 fixture frames into the capture catalog + one never-synced
    // "keeper" file that retention must never touch. ──
    let mut frame_ids: Vec<i64> = Vec::with_capacity(N);
    let mut expected: Vec<(String, String, f64)> = Vec::with_capacity(N); // (uuid, object, exptime)
    for idx in 0..N {
        let (fid, uuid, object, exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
        expected.push((uuid, object, exptime));
    }
    let _keeper = insert_capture_frame(&capture_ctx, &capture_files, 9999);
    let keeper_path = capture_files.join("light_9999.fits");
    assert!(keeper_path.exists(), "keeper file written");

    // ── (1) First enqueue → primary ingests ALL 50 with metadata ─────────────
    let r1 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, frame_ids.clone(), None, None, None)
        .await
        .expect("first enqueue");
    assert_eq!(r1.enqueued_count, N as u32);
    assert_eq!(r1.eligible_count, N as u32);
    assert_eq!(r1.total_count, N as u32);
    assert!(r1.ineligible.is_empty(), "all 50 fixture frames are eligible");

    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    wait_until(
        || count(cdb, "SELECT COUNT(*) FROM sync_outbound WHERE state = 'confirmed'") == 1,
        WAIT,
    )
    .await;

    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), N as i64, "50 files ingested");
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM frames WHERE uuid IS NOT NULL AND uuid != ''"),
        N as i64,
        "every ingested frame carries its catalog uuid"
    );
    // Sampled metadata survived end to end (object + exposure), joined by uuid.
    for (uuid, object, exptime) in expected.iter().step_by(7) {
        assert_eq!(
            scalar_opt_str(pdb, "SELECT object FROM frames WHERE uuid = ?1", uuid).as_deref(),
            Some(object.as_str()),
            "object metadata survived for {uuid}"
        );
        assert_eq!(
            scalar_opt_f64(pdb, "SELECT exptime FROM frames WHERE uuid = ?1", uuid),
            Some(*exptime),
            "exptime metadata survived for {uuid}"
        );
    }
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'ingested'"),
        N as i64,
        "receiver logged 50 ingests"
    );
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'sent' AND outcome = 'ingested'"),
        N as i64,
        "sender logged 50 confirmed sends"
    );

    // All 50 landed files live under the designated `sync_incoming` root (the
    // live resolver, not the `<sync_dir>/incoming` fallback).
    let landed: Vec<_> = walkdir::WalkDir::new(&designated)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .collect();
    assert_eq!(landed.len(), N, "all frames land under the designated sync_incoming root");

    // …and every one lives under the sender's FRIENDLY-name folder (2C), not a
    // hex slug: the ingest resolver read the cached device-names map above.
    let named_dir = designated.join("Studio_iMac");
    assert!(named_dir.is_dir(), "landing folder is named by the sender's current device name");
    for e in &landed {
        assert!(
            e.path().starts_with(&named_dir),
            "landed file is under the friendly-name dir: {}",
            e.path().display()
        );
    }

    // ── (2) Re-run the identical enqueue → dedupe-safe ───────────────────────
    let r2 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, frame_ids.clone(), None, None, None)
        .await
        .expect("second enqueue");
    assert_eq!(r2.enqueued_count, N as u32, "the same 50 frames re-enqueue");

    wait_until(
        || count(cdb, "SELECT COUNT(*) FROM sync_outbound WHERE state = 'confirmed'") == 2,
        WAIT,
    )
    .await;
    wait_until(
        || {
            count(pdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'duplicate'")
                == N as i64
        },
        WAIT,
    )
    .await;

    // Row counts stable on the primary: no new files/frames from the redelivery.
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), N as i64, "redelivery created no new files");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), N as i64, "redelivery created no new frames");
    // The second ack's receipts are ALL Duplicate — proven from the sender's own
    // confirm history for the second package.
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'sent' AND outcome = 'duplicate'"),
        N as i64,
        "every second-delivery ack receipt is Duplicate"
    );

    // ── (3) Retention: live mode, on_confirm, opt-in set — deletes confirmed
    // sources ONLY. ──
    for idx in 0..N {
        assert!(
            capture_files.join(format!("light_{idx:04}.fits")).exists(),
            "source {idx} present before retention"
        );
    }
    let ret_store = CatalogSyncStore::open(&capture_db).unwrap();
    let cfg = AppRetentionConfig {
        policy: RetentionPolicy::OnConfirm,
        raw_dry_run: false,
        live_confirmed: true,
    };
    let outcome = evaluate(&ret_store, &cfg, Utc::now(), &|| 0u8).expect("retention evaluate");
    assert!(!outcome.dry_run, "dry_run=false + opt-in goes live");
    assert_eq!(outcome.eligible.len(), 2, "both confirmed packages are retention candidates");
    // The two packages link the SAME 50 files; whichever is processed first
    // deletes them, the second finds them gone and is a no-op → exactly one
    // package reports a real removal.
    assert_eq!(outcome.deleted.len(), 1, "one package did the real deletion; its twin was a no-op");

    // Every confirmed source is gone from disk AND its catalog rows are removed…
    for idx in 0..N {
        assert!(
            !capture_files.join(format!("light_{idx:04}.fits")).exists(),
            "confirmed source {idx} deleted at source"
        );
    }
    // …while the never-synced keeper survives, on disk and in the catalog.
    assert!(keeper_path.exists(), "the never-synced keeper file is untouched by retention");
    assert_eq!(count(cdb, "SELECT COUNT(*) FROM files"), 1, "only the never-synced keeper survives");
    assert_eq!(count(cdb, "SELECT COUNT(*) FROM frames"), 1, "keeper's frame survives (CASCADE removed the rest)");

    // Both history events are searchable for the deleted frames: the transfer
    // ('ingested') AND the retention audit ('retention_deleted').
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE outcome = 'retention_deleted'"),
        N as i64,
        "one retention_deleted audit per confirmed source"
    );
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'sent' AND outcome = 'ingested'"),
        N as i64,
        "the transfer events survive retention (both events searchable)"
    );
    for (uuid, _, _) in expected.iter().step_by(11) {
        assert!(
            count(
                cdb,
                &format!(
                    "SELECT COUNT(*) FROM sync_history WHERE frame_uuid = '{uuid}' AND outcome = 'ingested'"
                ),
            ) >= 1,
            "transfer event present for {uuid}"
        );
        assert!(
            count(
                cdb,
                &format!(
                    "SELECT COUNT(*) FROM sync_history WHERE frame_uuid = '{uuid}' AND outcome = 'retention_deleted'"
                ),
            ) >= 1,
            "retention_deleted event present for the same frame {uuid}"
        );
    }

    // Clean shutdown of the background tasks (tidy; tempdir drop handles the rest).
    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Sync Phase 3 (dedup handshake) end-to-end: a re-send of an OVERLAPPING batch
/// transfers only the genuinely-new frames.
///
/// This pins the whole offer/want chain — proto (task 1), pure split (task 2),
/// catalog responder (task 3), transport negotiate (task 4), want-subset serve
/// (task 5), engine splice (task 6) — over two real instances on the loopback
/// transport, the same observable oracle [`two_instance_sync_e2e`] uses.
///
/// The dedup only runs because instance B's loopback endpoint is given a real
/// [`CatalogDedupResponder`] over B's catalog (`endpoint_with_responder`): B
/// answers A's pre-announce `Offer` from `files.content_hash` (the sampling hash
/// that B's ingest populates via `compute_xxhash(&landed)`), sampling-matches the
/// frames it already holds, then full-hash-confirms them as true duplicates. A
/// responder-less endpoint would answer want-all and the assertions below would
/// see `{new:4, duplicate:0}` instead — so the responder wiring is exactly what
/// this test drives out.
///
/// The `{new, duplicate}` split surfaces only on the send-side `sync-finished`
/// event (not on `enqueue_sync_selection`'s result), so A's engine is spawned
/// with a [`RecordingEmitter`] that captures each package's outcome.
#[tokio::test]
async fn resend_transfers_only_new_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    // Owned `Arc` so `enqueue_sync_selection` (which now builds a `'static` retry
    // address-refresher, T8) accepts `&capture_ctx`; `&Arc` still coerces to
    // `&ServiceContext` for every other helper here.
    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let pdb = primary_ctx.db.get().unwrap();

    // ── Primary receiver over loopback, WITH a catalog dedup responder ──────────
    // The responder is the in-process stand-in for a running receiver's
    // `CatalogDedupResponder`: it answers A's `negotiate_want` from B's real
    // catalog, so a re-sent frame B already holds is recognized as a duplicate and
    // never transferred. Built from B's catalog store BEFORE the endpoint is
    // minted, then registered into the endpoint's inbox on `start`.
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let responder: Arc<dyn DedupResponder> =
        Arc::new(CatalogDedupResponder::new(Arc::clone(&primary_store)));
    let receiver_ep = Arc::new(net.endpoint_with_responder(responder));
    let receiver_node = receiver_ep.node_id();

    // Designate a `sync_incoming` scan root so received files land under it (same
    // as the metadata-delivery e2e); its live resolver re-reads it per package.
    let designated = tmp.path().join("designated_incoming");
    std::fs::create_dir_all(&designated).unwrap();
    athenaeum_core::api::scan_roots::add_scan_root(
        &primary_ctx,
        designated.to_string_lossy().into_owned(),
        &athenaeum_core::api::PathPolicy::AllowAll,
        Some("sync_incoming".to_string()),
    )
    .expect("designate sync_incoming root");
    let resolver_db = primary_ctx.db.get().unwrap().clone();
    let fallback_incoming = primary_dir.join("incoming");
    let incoming: athenaeum_core::sync::receiver::IncomingResolver = Arc::new(move || {
        let conn = resolver_db.conn();
        match athenaeum_core::db::scan_root_path_of_kind(&conn, "sync_incoming") {
            Ok(Some(p)) => std::path::PathBuf::from(p),
            _ => fallback_incoming.clone(),
        }
    });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(), // no project announce gate in this test
        Arc::new(athenaeum_core::sync::InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // ── Capture sender engine over loopback, WITH a recording emitter so the test
    // can read each package's `{new, duplicate}` dedup outcome. ──
    let sender_ep = net.endpoint();
    let sender_node = sender_ep.node_id();
    let engine_store = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
    let emitter = Arc::new(RecordingEmitter::default());
    let engine = Arc::new(SyncEngine::spawn_with_emitter(
        engine_store as Arc<dyn SyncStore>,
        Arc::new(sender_ep) as Arc<dyn SharingTransport>,
        receiver_node,
        Some(Arc::clone(&emitter) as Arc<dyn ProgressEmitter>),
    ));
    let sender = Arc::new(SyncSenderRuntime::new());
    // T6 wake-hook plumbing: the collab sender map is threaded alongside the
    // personal one; unused by this personal-sync e2e (no collab engines started).
    let collab_sender = Arc::new(SyncSenderRuntime::new());
    // T7: `enqueue_sync_selection` threads a SyncRuntime (only touched when
    // `ensure_sender_engine` has to build an engine; here the engine is pre-injected
    // above, so the peers-refresh-timer install is short-circuited and `sync` is
    // unused — a fresh unstarted runtime satisfies the signature).
    let sync = SyncRuntime::new();
    {
        let mut guard = sender.lock_inner().await;
        guard.insert(
            receiver_node,
            StartedSender {
                engine: Arc::clone(&engine),
                origin_device: node_id_hex(&sender_node),
                peer: receiver_node,
            },
        );
    }

    // Seed 4 fixture frames on A: idx 0,1,2 are the first batch; idx 3 is the one
    // new frame the overlapping re-send adds. Each idx writes a distinct pixel
    // value → a distinct sampling + full xxh3. The 3 overlapping payloads are
    // byte-identical across sends (same unchanged source files), so B's responder
    // sampling-matches then full-confirms them as true duplicates; idx 3's bytes
    // are genuinely different, so it stays wanted.
    let mut frame_ids: Vec<i64> = Vec::with_capacity(4);
    for idx in 0..4 {
        let (fid, _uuid, _object, _exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
    }

    // ── (1) First batch: 3 frames → B ingests all 3, all reported new ──────────
    let batch1: Vec<i64> = frame_ids[0..3].to_vec();
    let r1 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch1, None, None, None)
        .await
        .expect("first enqueue");
    assert_eq!(r1.enqueued_count, 3, "the first 3 frames enqueue");

    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 3, WAIT).await;
    wait_until(|| emitter.sent_finished().len() == 1, WAIT).await;
    assert_eq!(
        emitter.sent_finished()[0],
        (3, 0),
        "the first batch is all new: {{new:3, duplicate:0}}"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), 3, "B holds the 3 first-batch files");
    // B's ingest populated `files.content_hash` for all 3 — the sampling hash the
    // responder diffs the next offer against.
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL"),
        3,
        "every ingested file carries its sampling content_hash"
    );

    // ── (2) Overlapping batch: the same 3 + 1 new (4 total) ────────────────────
    // Only the 1 new frame is transferred; the 3 B already holds are dropped by
    // the negotiate handshake before any announce/serve of them.
    let batch2: Vec<i64> = frame_ids[0..4].to_vec();
    let r2 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch2, None, None, None)
        .await
        .expect("second enqueue");
    assert_eq!(r2.enqueued_count, 4, "all 4 frames re-enqueue at the app layer");

    wait_until(|| emitter.sent_finished().len() == 2, WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 4, WAIT).await;

    assert_eq!(
        emitter.sent_finished()[1],
        (1, 3),
        "the overlapping re-send moves only the 1 new frame; the 3 dupes are dropped"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), 4, "B holds exactly 4 files after the re-send");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 4, "B holds exactly 4 frames after the re-send");
    assert_eq!(
        count(pdb, "SELECT COUNT(DISTINCT uuid) FROM frames"),
        4,
        "B's 4 frames all carry distinct uuids — no frame was double-ingested"
    );

    // ── (3) Fully-overlapping re-send: all 4 again → all-duplicate terminal ─────
    // Every offered frame is a known duplicate, so the handshake returns an empty
    // want and the package terminalizes confirmed WITHOUT announcing or serving —
    // B never even sees an announce, and its catalog is untouched.
    let batch3: Vec<i64> = frame_ids[0..4].to_vec();
    enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch3, None, None, None)
        .await
        .expect("third enqueue");

    wait_until(|| emitter.sent_finished().len() == 3, WAIT).await;
    assert_eq!(
        emitter.sent_finished()[2],
        (0, 4),
        "a fully-overlapping re-send transfers nothing: {{new:0, duplicate:4}}"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), 4, "B unchanged by the all-duplicate re-send");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 4, "B unchanged by the all-duplicate re-send");

    // Clean shutdown of the background tasks (tidy; tempdir drop handles the rest).
    engine.shutdown().await;
    receiver.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 16: loopback e2e acceptance suite — the spec's acceptance evidence (§6,
// §12). Each test composes the WHOLE delivery-forever stack (Tasks 1–15) at the
// two-`ServiceContext` level, over the same loopback transport the unit tests use,
// and asserts an acceptance-shaped property end to end.
// ─────────────────────────────────────────────────────────────────────────────

/// Acceptance §6 — **deliver-forever**: a package enqueued while its destination
/// is offline delivers, unattended, once the peer comes back — with NO user action
/// and NO explicit kick. The backoff schedule alone must re-drive it.
///
/// Drive: mint the receiver endpoint (its node id is known before it starts), point
/// a short-`ack_timeout` (50ms → ms-scale backoff rungs) sender engine at it, and
/// enqueue 6 frames while the receiver's endpoint is NOT yet started — every
/// announce fails ("peer not started") and the package sits pending, retrying.
///
/// Assert: the sender tries ≥ 2 times, stays non-terminal (never `failed`/
/// `confirmed`/`cancelled`), and carries a persisted `next_retry_at` (Task 2) —
/// i.e. it is genuinely waiting out a backoff window, not stuck. Then the receiver
/// is brought online and — WITHOUT `kick`/`kick_all` — the schedule redelivers:
/// the package reaches `Confirmed`, all 6 files land under the designated
/// `sync_incoming` root, and both history halves record 6 `ingested` rows each,
/// every one carrying its `package_id` batch key (Task 14).
#[tokio::test]
async fn offline_peer_delivers_after_reconnect_without_user_action() {
    const N: usize = 6;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // Mint the receiver endpoint — its node id is available BEFORE it starts, so we
    // can address (and enqueue toward) a peer that is not yet online.
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Short-ack-timeout sender engine (ms-scale rungs), injected as the host would.
    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    // Seed 6 fixture frames on the capture node.
    let mut frame_ids: Vec<i64> = Vec::with_capacity(N);
    for idx in 0..N {
        let (fid, _uuid, _object, _exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
    }

    // Enqueue toward the still-offline receiver: the app-layer enqueue succeeds
    // (it hands off to the engine), but no announce can be delivered yet.
    let r1 = enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("enqueue toward offline peer");
    assert_eq!(r1.enqueued_count, N as u32);
    let id = latest_outbound_id(cdb);

    // The package retries against the offline peer: ≥ 2 attempts, still non-terminal,
    // and a persisted next_retry_at proves it is waiting out a real backoff window.
    wait_until(
        || {
            outbound_attempts(cdb, id) >= 2
                && !is_terminal_state(&outbound_state(cdb, id))
                && outbound_next_retry(cdb, id).is_some()
        },
        WAIT,
    )
    .await;
    // Snapshot the pending state (never terminal while offline — delivery-forever).
    let pending_state = outbound_state(cdb, id);
    assert!(
        !is_terminal_state(&pending_state),
        "an offline peer must never terminalize the package (delivery-forever); saw {pending_state}"
    );
    assert!(
        outbound_next_retry(cdb, id).is_some(),
        "a pending package waiting out a backoff window persists next_retry_at"
    );
    assert_eq!(
        landed_count(&designated),
        0,
        "nothing has been delivered while the peer is offline"
    );

    // Bring the receiver online — and DO NOT kick. The backoff schedule alone must
    // re-announce and deliver.
    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // Unattended delivery: Confirmed on the sender, 6 frames ingested on the primary.
    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;

    assert_eq!(landed_count(&designated), N, "all frames land under the designated root");
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_outbound WHERE state = 'confirmed'"),
        1,
        "exactly one package, and it is confirmed"
    );

    // Both history halves recorded 6 ingests, each carrying its package_id batch key.
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'ingested'"),
        N as i64,
        "receiver logged 6 ingests"
    );
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'sent' AND outcome = 'ingested'"),
        N as i64,
        "sender logged 6 confirmed sends"
    );
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND (package_id IS NULL OR package_id = '')"),
        0,
        "every received history row carries its package_id"
    );
    assert_eq!(
        count(cdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'sent' AND (package_id IS NULL OR package_id = '')"),
        0,
        "every sent history row carries its package_id"
    );

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Acceptance (Decline Finality Axis §6.1 / smoke №8 Task D) — **receiver decline
/// is final per its `batch_uuid`, but a deliberate sender Resend mints a NEW
/// transfer that delivers**. First half (unchanged from the finality proof): a
/// receiver declining an inbound package drives the sender's row terminal
/// (`cancelled`, reason "cancelled by receiver"), lands no files, and leaves the
/// receiver row `cancelled` + `declined_at` with a full set of `cancelled`
/// receipts. Second half (the new behaviour): the sender's Resend of that declined
/// row does NOT bounce — it renames the payload dir to a fresh basename (⇒ new wire
/// `batch_uuid`) and enqueues a brand-NEW `sync_outbound` row that confirms; the
/// receiver keys a SECOND inbound row on the new batch and ingests, while the old
/// declined row stays declined and untouched. Retention (`sync_sources`) follows
/// the renamed dir.
///
/// Deterministic mid-transfer cancel: the receiver endpoint's `abort_after_bytes`
/// fault is held armed so no payload fetch can ever COMPLETE (hence no ingest can
/// race the cancel), which also surfaces the wire `package_id` in `sync_inbound`.
/// Cancellation is then requested through the receiver's own
/// [`InboundControl::request_cancel`] — the exact call
/// [`cancel_incoming_package`] makes internally on a live transport — plus a real
/// [`cancel_incoming_package`] command invocation to exercise the command boundary
/// at the two-context level.
#[tokio::test]
async fn receiver_decline_then_resend_mints_new_transfer_and_delivers() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // The receiver's own cancel signal (the object a started SyncRuntime would hand
    // `cancel_incoming_package`). A fresh SyncRuntime for the command surface — its
    // in-process `inbound_control()` is None, so the command exercises its
    // persisted-row leg while the live-signal leg is driven through `control` below.
    let control = Arc::new(InboundControl::new());
    let primary_sync = SyncRuntime::new();

    // Hold the payload fetch aborting so no ingest can complete before we cancel.
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::clone(&control),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    let mut frame_ids: Vec<i64> = Vec::with_capacity(N);
    for idx in 0..N {
        let (fid, _uuid, _object, _exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
    }

    let _r = enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("enqueue selection");
    let old_id = latest_outbound_id(cdb);
    // The declined transfer's payload dir + its N retention rows — the re-key oracle.
    let old_pkg_ref = outbound_package_ref(cdb, old_id);
    assert_eq!(
        sync_sources_live_count(cdb, &old_pkg_ref),
        N as i64,
        "the send records one sync_sources retention row per frame"
    );

    // Learn the wire package_id from the inbound row the announce created, re-arming
    // the one-shot abort fault each poll so every retry's payload fetch keeps
    // aborting — ingest is impossible until we have registered the cancel.
    let mut wire_pkg = String::new();
    wait_until(
        || {
            receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
            match latest_inbound_package_id(pdb) {
                Some(p) => {
                    wire_pkg = p;
                    true
                }
                None => false,
            }
        },
        WAIT,
    )
    .await;
    assert!(!wire_pkg.is_empty(), "an inbound row surfaced the wire package_id");

    // Cancel: the live signal (identical to the command's internal call on a started
    // transport) diverts the receiver to its cancel epilogue on the next announce…
    control.request_cancel(&wire_pkg);
    // …and the real command exercises the two-context command boundary (Ok on an
    // in-flight or already-terminal inbound row).
    cancel_incoming_package(&primary_ctx, &primary_sync, &wire_pkg)
        .await
        .expect("cancel_incoming_package command");

    // Sender terminalizes on the all-cancelled ack: Cancelled + the exact reason
    // string the UI keys "cancelled by receiver" off.
    wait_until(|| outbound_state(cdb, old_id) == "cancelled", WAIT).await;
    assert_eq!(
        outbound_last_error(cdb, old_id).as_deref(),
        Some("cancelled by receiver"),
        "the sender records the receiver-decline reason verbatim"
    );

    // Receiver terminal Cancelled, no files landed, and a cancelled receipt per
    // frame (the durable replay source — a later re-announce of THIS package would
    // replay the cancel from the log without re-fetching, as the receiver unit test
    // `cancel_before_fetch_acks_cancelled_and_replays` proves; here the two-context
    // evidence is the row state + the receipt log).
    wait_until(
        || matches!(inbound_row(pdb, &wire_pkg), Some((s, _, _)) if s == "cancelled"),
        WAIT,
    )
    .await;
    assert_eq!(landed_count(&designated), 0, "a declined package lands no files");
    assert_eq!(
        count(
            pdb,
            &format!(
                "SELECT COUNT(*) FROM sync_receipts WHERE package_id = '{wire_pkg}' AND outcome = 'cancelled'"
            ),
        ),
        N as i64,
        "the receiver logged a cancelled receipt per frame (the replay source)"
    );

    // Disarm the fault so the resend's NEW transfer can fetch and deliver — proving
    // delivery is genuine, and that the OLD batch stayed declined by the axis, not
    // because a fault blocked it.
    receiver_ep.set_fault(FaultPlan::default());

    // Resend via the sanctioned command. The declined row carries
    // `last_error = "cancelled by receiver"`, so this diverts into
    // `resend_declined_as_new_transfer`: a NEW `sync_outbound` row (new dir basename
    // ⇒ new wire `batch_uuid`) rather than a reset of the SAME row.
    let new_id =
        retry_sync_package(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, old_id, None)
            .await
            .expect("resend a receiver-declined package as a new transfer");
    assert_ne!(new_id, old_id, "a receiver-declined resend mints a NEW transfer row");

    // The NEW transfer confirms; N frames ingest; N files land — the whole point.
    wait_until(|| outbound_state(cdb, new_id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), N as i64, "every frame ingested on the new transfer");
    assert_eq!(landed_count(&designated), N, "every file landed on the new transfer");

    // TWO inbound rows on the receiver: the old DECLINED one (still `cancelled` with
    // `declined_at` set — untouched by the new batch) and the new DONE one.
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM sync_inbound"), 2, "the new batch identity created a SECOND inbound row");
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_inbound WHERE declined_at IS NOT NULL AND state = 'cancelled'"),
        1,
        "the old declined inbound row stays declined + cancelled — decline final per its batch_uuid"
    );
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_inbound WHERE state = 'done'"),
        1,
        "the new batch's inbound row reaches Done"
    );

    // The old SENDER row is kept as history — still Cancelled, reason intact.
    assert_eq!(outbound_state(cdb, old_id), "cancelled", "the declined sender row is kept as history");
    assert_eq!(
        outbound_last_error(cdb, old_id).as_deref(),
        Some("cancelled by receiver"),
        "the declined row keeps its receiver-decline reason"
    );

    // Retention followed the payload: the N `sync_sources` rows now key on the NEW
    // transfer's package_ref, none on the old one.
    let new_pkg_ref = outbound_package_ref(cdb, new_id);
    assert_ne!(new_pkg_ref, old_pkg_ref, "the new transfer owns a fresh payload dir");
    assert_eq!(sync_sources_live_count(cdb, &new_pkg_ref), N as i64, "sync_sources re-keyed onto the new transfer");
    assert_eq!(sync_sources_live_count(cdb, &old_pkg_ref), 0, "no sync_sources remain under the old package_ref");

    // The old payload dir was renamed away (gone from disk); the new dir is present
    // (post-confirm cleanup leaves it manifest-only, but the dir itself survives).
    assert!(!PathBuf::from(&old_pkg_ref).exists(), "the declined transfer's payload dir was renamed away");
    assert!(PathBuf::from(&new_pkg_ref).exists(), "the new transfer's payload dir is present");

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Owner smoke №7 (Decline Finality Axis §D3/§D4) — **a sender cancel is NOT a
/// receiver decline; the sender's own resend delivers**: cancel a live send via
/// the real command (`cancel_sync_package` → terminal row + `Revoke{Cancelled}`),
/// then resend via the real command (`retry_sync_package`). The receiver — whose
/// row was at most attempt-cancelled "by sender", never declined — must reset the
/// SAME batch-keyed row and ingest, and the sender must reach `confirmed`.
/// Pre-fix, the receiver hit cancelled-is-final and re-acked all-cancelled
/// forever: sender showed "cancelled by receiver", receiver "by sender".
///
/// The revoke races the engine's retry re-announces (a re-announce may have
/// stamped the row `failed` before the revoke lands, making the revoke a
/// no-op on a terminal row) — both interleavings are legitimate, so this test
/// asserts the interleaving-independent user story: after cancel + resend, the
/// transfer DELIVERS and nobody records a decline. The deterministic
/// revoke→cancelled→resend path is pinned by the receiver-level unit e2e
/// `sender_cancel_then_resend_fetches_and_ingests`.
#[tokio::test]
async fn sender_cancel_then_resend_delivers() {
    const N: usize = 3;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Hold the payload fetch aborting so the cancel always lands before any ingest.
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    let mut frame_ids: Vec<i64> = Vec::with_capacity(N);
    for idx in 0..N {
        let (fid, _uuid, _object, _exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
    }

    let _r = enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("enqueue selection");
    let old_id = latest_outbound_id(cdb);

    // Wait for the announce to surface an inbound row (re-arming the one-shot
    // abort fault each poll so no fetch completes), then CANCEL via the real
    // command: terminal `cancelled` + a Revoke{Cancelled} for the live wire id.
    wait_until(
        || {
            receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
            latest_inbound_package_id(pdb).is_some()
        },
        WAIT,
    )
    .await;
    // Re-arm once more right before the cancel: the exit iteration's one-shot
    // fault may have been consumed by a retry fetch in the gap, and a completed
    // fetch here would flake the landed_count == 0 assertion below.
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
    cancel_sync_package(&sender, old_id).await.expect("cancel the live send");
    wait_until(|| outbound_state(cdb, old_id) == "cancelled", WAIT).await;
    assert_eq!(landed_count(&designated), 0, "nothing landed before the cancel");
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_inbound WHERE declined_at IS NOT NULL"),
        0,
        "a sender cancel NEVER records a receiver decline"
    );

    // Disarm the fault and resend via the sanctioned command: the SAME row resets
    // and this time the transfer must DELIVER.
    receiver_ep.set_fault(FaultPlan::default());
    let resend_id =
        retry_sync_package(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, old_id, None)
            .await
            .expect("resend a sender-cancelled package");
    assert_eq!(resend_id, old_id, "resend-as-reset returns the SAME row");

    wait_until(|| outbound_state(cdb, old_id) == "confirmed", WAIT).await;
    assert_eq!(
        outbound_state(cdb, old_id),
        "confirmed",
        "the resend of a sender-cancelled transfer confirms — the smoke-№7 brick is gone"
    );
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), N as i64, "every frame ingested on the resend");
    assert_eq!(landed_count(&designated), N, "every file landed on the resend");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM sync_inbound"), 1, "ONE batch-keyed inbound row across cancel + resend");
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_inbound WHERE declined_at IS NOT NULL"),
        0,
        "delivery never invents a decline"
    );
    assert_eq!(
        outbound_last_error(cdb, old_id).as_deref(),
        None,
        "no cross-blame left on the sender row after the confirmed resend"
    );

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Acceptance §12 — **live per-file progress + inbound visibility**: during a
/// transfer the receiver emits a monotonic per-file byte trail and walks its
/// `sync_inbound` row through the fetch lifecycle, ending `Done` with
/// `bytes_done == byte_size`.
///
/// Honest observation: loopback fetch is synchronous and there is no delay knob
/// (`FaultPlan` can only ABORT a fetch, not slow it), so a mid-transfer poll that
/// reliably catches the row in `Fetching` is not achievable. Per the brief, the
/// mid-transfer observation is made through the RECORDED TRAIL instead: the
/// receiver's captured `sync-progress` stage events prove the row walked
/// `received → fetching → ingesting` (it WAS in Fetching), and the post-hoc row is
/// `Done`. The per-file `sync-file-progress` events are asserted per-file
/// monotonic, each ending at its total; the cumulative fetch tick ends at the
/// package byte size.
#[tokio::test]
async fn per_file_progress_is_monotonic_and_inbound_visible_while_fetching() {
    const N: usize = 5;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // A capturing emitter on the RECEIVER: the per-file + stage trail is the oracle.
    let recorder = Arc::new(RecordingEmitter::default());
    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    let mut frame_ids: Vec<i64> = Vec::with_capacity(N);
    for idx in 0..N {
        let (fid, _uuid, _object, _exptime) = insert_capture_frame(&capture_ctx, &capture_files, idx);
        frame_ids.push(fid);
    }

    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("enqueue selection");

    // Wait for the transfer to land, then read the inbound row.
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    let wire_pkg = latest_inbound_package_id(pdb).expect("an inbound row exists");
    // The row reaches Done (the terminal stamp is written after the ack).
    wait_until(
        || matches!(inbound_row(pdb, &wire_pkg), Some((s, _, _)) if s == "done"),
        WAIT,
    )
    .await;
    let (state, byte_size, bytes_done) = inbound_row(pdb, &wire_pkg).expect("inbound row present");
    assert_eq!(state, "done", "the inbound row is terminal Done");
    assert!(byte_size > 0, "the package carries a non-zero byte size");
    assert_eq!(bytes_done, byte_size, "the fetched bytes reached the announced total");

    // Wait until the receiver's captured trail is complete (the ingesting stage tick
    // is emitted just before the terminal ack).
    wait_until(|| recorder.received_stages().iter().any(|s| s == "ingesting"), WAIT).await;

    // Stage trail: the row WAS in Fetching — received → fetching → ingesting, in
    // that relative order (the honest mid-transfer observation).
    let stages = recorder.received_stages();
    let idx_received = stages.iter().position(|s| s == "received");
    let idx_fetching = stages.iter().position(|s| s == "fetching");
    let idx_ingesting = stages.iter().position(|s| s == "ingesting");
    assert!(idx_received.is_some(), "a received stage tick was emitted; saw {stages:?}");
    assert!(idx_fetching.is_some(), "a fetching stage tick was emitted; saw {stages:?}");
    assert!(idx_ingesting.is_some(), "an ingesting stage tick was emitted; saw {stages:?}");
    assert!(
        idx_received < idx_fetching && idx_fetching < idx_ingesting,
        "the inbound row walked received → fetching → ingesting; saw {stages:?}"
    );

    // Per-file progress: each file's ticks are non-decreasing and end at its total.
    let mut by_file: std::collections::BTreeMap<String, Vec<(u64, u64)>> =
        std::collections::BTreeMap::new();
    for (file, done, total) in recorder.file_progress() {
        by_file.entry(file).or_default().push((done, total));
    }
    assert_eq!(by_file.len(), N, "one per-file progress stream per package file");
    for (file, ticks) in &by_file {
        assert!(!ticks.is_empty(), "file {file} emitted progress ticks");
        let mut prev = 0u64;
        for (done, total) in ticks {
            assert!(*done >= prev, "file {file} bytes_done is monotonic non-decreasing");
            assert!(*done <= *total, "file {file} never exceeds its total");
            prev = *done;
        }
        let (last_done, last_total) = *ticks.last().unwrap();
        assert!(last_total > 0, "file {file} has a non-zero total");
        assert_eq!(last_done, last_total, "file {file} ends at its byte total");
    }

    // The cumulative fetch tick reaches the package byte size.
    let fetch_ticks = recorder.fetching_byte_ticks();
    assert!(!fetch_ticks.is_empty(), "at least one byte-carrying fetch tick was emitted");
    let (final_done, final_total) = *fetch_ticks.last().unwrap();
    assert_eq!(final_total, byte_size, "the fetch tick total equals the package byte size");
    assert_eq!(final_done, byte_size, "the cumulative fetch reached the package byte size");

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Task 4.2 — **send + receive concurrency contract** on loopback. The smoke
/// complaint was "can't receive while sending"; code analysis found no hard
/// serializer (an instance's send engine and receiver loop are independent tasks).
/// This is the loopback-level proof.
///
/// Instance A sends a multi-frame package to B WHILE B sends one to A, over the
/// same [`LoopbackNetwork`]. Each instance therefore drives BOTH halves at once —
/// its own send engine (outbound rows) AND its receiver loop (inbound fetch), over
/// the same catalog DB — which is exactly the "receive while sending" condition the
/// smoke report doubted. Both receivers pace every fetch read (`delay_per_read`) so
/// the two fetches take a controllable ~half-second each and genuinely overlap;
/// each `.await` on the pacing sleep hands the runtime to the other in-flight fetch,
/// so their receiver-stage timestamps interleave.
///
/// Asserts:
///   1. both packages reach `Confirmed`, and each catalog ingests the peer's N
///      frames (both directions completed);
///   2. the two directions' fetch windows OVERLAP in wall-clock time — the direct
///      proof neither fetch serialized behind the other;
///   3. each direction's announce→fetching gap stays far below the OTHER direction's
///      full fetch duration (serialization would force it to ≥ that duration), and
///      under a generous absolute ceiling.
///
/// All `Instant`s come from one process's monotonic clock, so cross-emitter timing
/// comparison is valid. Determinism: no wall-clock sleep is used for
/// synchronization (only the existing `wait_until` poll helper); the pacing sleep
/// shapes the fetch, it is never awaited as a barrier.
#[tokio::test]
async fn bidirectional_simultaneous_transfers_both_complete() {
    const N: usize = 10;
    // Pace each fetch read so a whole-package fetch takes ~N × delay (~0.5s here):
    // long enough that the two directions reliably overlap and a serialized second
    // fetch would be unmistakable, short enough to stay far under the WAIT budget.
    const DELAY_PER_READ: Duration = Duration::from_millis(50);
    // Generous absolute ceiling for the announce→fetching gap: 12× the harness poll
    // step (25ms) = 300ms — well above concurrent scheduling jitter, well below the
    // ~N×DELAY (~500ms+) a fetch serialized behind its peer's fetch would incur.
    const ANNOUNCE_FETCH_CEILING: Duration = Duration::from_millis(300);

    let tmp = tempfile::tempdir().unwrap();
    let a_dir = tmp.path().join("a");
    let b_dir = tmp.path().join("b");
    let a_files = a_dir.join("files");
    let b_files = b_dir.join("files");
    std::fs::create_dir_all(&a_files).unwrap();
    std::fs::create_dir_all(&b_files).unwrap();
    let a_db = a_dir.join("catalog.db");
    let b_db = b_dir.join("catalog.db");

    let ctx_a = Arc::new(ServiceContext::new_for_tests(a_db.clone()));
    let ctx_b = Arc::new(ServiceContext::new_for_tests(b_db.clone()));
    let dba = ctx_a.db.get().unwrap();
    let dbb = ctx_b.db.get().unwrap();

    let net = LoopbackNetwork::new();

    // ── One receiver per instance, each with its own recording emitter ───────────
    let store_a = Arc::new(CatalogSyncStore::open(&a_db).unwrap());
    let store_b = Arc::new(CatalogSyncStore::open(&b_db).unwrap());
    let recv_ep_a = Arc::new(net.endpoint());
    let recv_ep_b = Arc::new(net.endpoint());
    let node_a_recv = recv_ep_a.node_id();
    let node_b_recv = recv_ep_b.node_id();

    // Pace BOTH receivers' fetch so the two directions overlap in wall-clock time.
    recv_ep_a.set_fault(FaultPlan { delay_per_read: Some(DELAY_PER_READ), ..Default::default() });
    recv_ep_b.set_fault(FaultPlan { delay_per_read: Some(DELAY_PER_READ), ..Default::default() });

    let designated_a = tmp.path().join("incoming_a");
    let designated_b = tmp.path().join("incoming_b");
    designate_incoming(&ctx_a, &designated_a);
    designate_incoming(&ctx_b, &designated_b);
    let incoming_a = incoming_resolver_for(&ctx_a, a_dir.join("incoming"));
    let incoming_b = incoming_resolver_for(&ctx_b, b_dir.join("incoming"));

    let recorder_a = Arc::new(RecordingEmitter::default());
    let recorder_b = Arc::new(RecordingEmitter::default());

    let (_ia, receiver_a) = SyncReceiver::spawn(
        Arc::clone(&store_a),
        a_dir.clone(),
        incoming_a,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&recv_ep_a) as Arc<dyn SharingTransport>,
        Arc::clone(&recorder_a) as Arc<dyn ProgressEmitter>,
    )
    .await
    .expect("spawn receiver A");

    let (_ib, receiver_b) = SyncReceiver::spawn(
        Arc::clone(&store_b),
        b_dir.clone(),
        incoming_b,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&recv_ep_b) as Arc<dyn SharingTransport>,
        Arc::clone(&recorder_b) as Arc<dyn ProgressEmitter>,
    )
    .await
    .expect("spawn receiver B");

    // ── One send engine per instance, targeting the peer's receiver node. A long
    // ack_timeout keeps the paced fetch from triggering a premature retry — both
    // peers are already online, so delivery succeeds on the first attempt. ────────
    let (engine_a, sender_a, collab_a, sync_a) =
        inject_sender_engine(&net, &a_db, node_b_recv, Duration::from_secs(30)).await;
    let (engine_b, sender_b, collab_b, sync_b) =
        inject_sender_engine(&net, &b_db, node_a_recv, Duration::from_secs(30)).await;

    // Seed N frames on each instance (own files dir + catalog). A's frames carry
    // A-minted uuids and B's carry B-minted uuids, so neither side dedups the other.
    let mut ids_a: Vec<i64> = Vec::with_capacity(N);
    let mut ids_b: Vec<i64> = Vec::with_capacity(N);
    for idx in 0..N {
        ids_a.push(insert_capture_frame(&ctx_a, &a_files, idx).0);
        ids_b.push(insert_capture_frame(&ctx_b, &b_files, idx).0);
    }

    // ── Fire BOTH directions back-to-back. `enqueue_sync_selection` hands off to the
    // engine worker (async), so both packages are in flight before either paced
    // fetch can finish. ──────────────────────────────────────────────────────────
    let ra = enqueue_sync_selection(
        &ctx_a,
        &sender_a,
        Arc::clone(&collab_a),
        &sync_a,
        ResolvedDest { node: node_b_recv, endpoint_addr: None },
        ids_a.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("A → B enqueue");
    let rb = enqueue_sync_selection(
        &ctx_b,
        &sender_b,
        Arc::clone(&collab_b),
        &sync_b,
        ResolvedDest { node: node_a_recv, endpoint_addr: None },
        ids_b.clone(),
        None,
        None,
        None,
    )
    .await
    .expect("B → A enqueue");
    assert_eq!(ra.enqueued_count, N as u32);
    assert_eq!(rb.enqueued_count, N as u32);

    // ── (1) Both packages reach Confirmed; each catalog ingests the peer's N frames.
    wait_until(|| count(dba, "SELECT COUNT(*) FROM sync_outbound WHERE state = 'confirmed'") == 1, WAIT).await;
    wait_until(|| count(dbb, "SELECT COUNT(*) FROM sync_outbound WHERE state = 'confirmed'") == 1, WAIT).await;
    wait_until(
        || count(dba, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'ingested'") == N as i64,
        WAIT,
    )
    .await;
    wait_until(
        || count(dbb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'ingested'") == N as i64,
        WAIT,
    )
    .await;

    assert_eq!(landed_count(&designated_a), N, "B → A delivered all N frames to A's incoming root");
    assert_eq!(landed_count(&designated_b), N, "A → B delivered all N frames to B's incoming root");

    // ── (2) Genuine overlap: the two fetch windows intersect in wall-clock time.
    // recorder_b captured the A → B fetch (B is the receiver); recorder_a captured
    // the B → A fetch. The window is [fetching-start, ingesting] — `fetching` is
    // stamped just before `transport.fetch()`, `ingesting` just after it returns. ─
    let a2b_start = recorder_b.first_received_stage_at("fetching").expect("A→B fetching tick");
    let a2b_end = recorder_b.first_received_stage_at("ingesting").expect("A→B ingesting tick");
    let b2a_start = recorder_a.first_received_stage_at("fetching").expect("B→A fetching tick");
    let b2a_end = recorder_a.first_received_stage_at("ingesting").expect("B→A ingesting tick");

    assert!(a2b_start < a2b_end, "A→B fetch window is well-formed");
    assert!(b2a_start < b2a_end, "B→A fetch window is well-formed");
    assert!(
        a2b_start < b2a_end && b2a_start < a2b_end,
        "the two directions' fetch windows overlap (genuine concurrency): \
         A→B fetch [{a2b_start:?} .. {a2b_end:?}], B→A fetch [{b2a_start:?} .. {b2a_end:?}]"
    );

    // ── (3) Non-serialization: each direction's announce→fetching gap is far below
    // the OTHER direction's full fetch duration — had it serialized behind that
    // fetch, the gap would be ≥ that duration — and under a generous absolute
    // ceiling. ───────────────────────────────────────────────────────────────────
    let a2b_recv = recorder_b.first_received_stage_at("received").expect("A→B received tick");
    let b2a_recv = recorder_a.first_received_stage_at("received").expect("B→A received tick");
    let a2b_gap = a2b_start.duration_since(a2b_recv);
    let b2a_gap = b2a_start.duration_since(b2a_recv);
    let a2b_fetch = a2b_end.duration_since(a2b_start);
    let b2a_fetch = b2a_end.duration_since(b2a_start);

    assert!(
        a2b_gap < b2a_fetch,
        "A→B fetch did not serialize behind the concurrent B→A fetch \
         (announce→fetching {a2b_gap:?} < peer fetch {b2a_fetch:?})"
    );
    assert!(
        b2a_gap < a2b_fetch,
        "B→A fetch did not serialize behind the concurrent A→B fetch \
         (announce→fetching {b2a_gap:?} < peer fetch {a2b_fetch:?})"
    );
    assert!(
        a2b_gap < ANNOUNCE_FETCH_CEILING,
        "A→B announce→fetching gap within the generous ceiling ({a2b_gap:?} < {ANNOUNCE_FETCH_CEILING:?})"
    );
    assert!(
        b2a_gap < ANNOUNCE_FETCH_CEILING,
        "B→A announce→fetching gap within the generous ceiling ({b2a_gap:?} < {ANNOUNCE_FETCH_CEILING:?})"
    );

    engine_a.shutdown().await;
    engine_b.shutdown().await;
    receiver_a.shutdown().await;
    receiver_b.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// tv2 Task 8 — per-file resilience e2e suite. Each test drives the WHOLE loopback
// path (real `enqueue_sync_selection` sender + real `SyncReceiver`, both stores
// real SQLite) and pins ONE Transfers Status Model v2 cross-component invariant
// (spec `docs/superpowers/specs/2026-07-20-transfers-status-v2-design.md`,
// §D1/D2/D4/D5/D6/D7) at the level the T1–T7 unit suites leave uncovered — the
// full announce→fetch→ingest→ack chain across two catalogs, not one component in
// isolation.
// ─────────────────────────────────────────────────────────────────────────────

/// Write a real single-frame FITS at `abs_path` and insert its `files` + `frames`
/// rows into `ctx`'s catalog with the given metadata; returns `(frame_id, uuid)`.
/// The general sibling of [`insert_capture_frame`]: it places a frame at an
/// arbitrary (nested) path — for source-relative rel_path shaping — and stamps
/// `instrume`/`imagetyp` for the WBPP object-send shape. A per-path pixel seed
/// keeps every payload's content hash distinct.
fn insert_frame_at(
    ctx: &ServiceContext,
    abs_path: &Path,
    object: &str,
    exptime: f64,
    imagetyp: ImageType,
    instrume: Option<&str>,
) -> (i64, String) {
    std::fs::create_dir_all(abs_path.parent().unwrap()).unwrap();
    let filename = abs_path.file_name().unwrap().to_string_lossy().to_string();
    let kind = match imagetyp {
        ImageType::Dark => FrameKind::Dark,
        ImageType::Bias => FrameKind::Bias,
        ImageType::Flat => FrameKind::Flat,
        _ => FrameKind::Light,
    };
    let mut hb = HeaderBuilder::new(kind).object(object).exptime(exptime).filter("Ha");
    if let Some(i) = instrume {
        hb = hb.instrume(i);
    }
    let cards = hb.build().expect("build fixture header");
    // A process-unique seed per frame → distinct pixels → distinct full-content
    // xxh3, so no two fixture frames are content-deduped on ingest (path length is
    // NOT unique — `L_001.fits`/`L_002.fits` collide).
    static SEED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let seed = SEED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f32;
    let data = vec![seed; 8 * 8];
    write_fits_f32(abs_path, 8, 8, 1, &data, &cards).expect("write fixture fits");
    let size = std::fs::metadata(abs_path).unwrap().len() as i64;

    let db = ctx.db.get().expect("ctx db");
    let conn = db.conn();
    let file = File {
        id: None,
        path: abs_path.to_string_lossy().to_string(),
        filename: filename.clone(),
        size,
        modified_at: Utc::now(),
        format: FileFormat::FITS,
        created_at: Utc::now(),
        metadata_hash: None,
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    };
    let file_id = insert_file(&conn, &file).expect("insert file");
    let frame = Frame {
        file_id,
        object: Some(object.to_string()),
        exptime: Some(exptime),
        imagetyp: Some(imagetyp),
        instrume: instrume.map(|s| s.to_string()),
        ..Default::default()
    };
    let frame_id = insert_frame(&conn, &frame).expect("insert frame");
    let uuid: String = conn
        .query_row("SELECT uuid FROM frames WHERE id = ?1", [frame_id], |r| r.get(0))
        .expect("trigger uuid");
    (frame_id, uuid)
}

/// The durable `sync_inbound` row id for a wire `package_id` (the batch_key its
/// receiver journal + per-file rows hang off), or `None` before the announce lands.
fn inbound_id_of(db: &Database, package_id: &str) -> Option<i64> {
    db.conn()
        .query_row("SELECT id FROM sync_inbound WHERE package_id = ?1", [package_id], |r| r.get(0))
        .ok()
}

/// The persisted `display_name` of the `sync_inbound` row for `package_id`.
fn inbound_display_name(db: &Database, package_id: &str) -> Option<String> {
    db.conn()
        .query_row(
            "SELECT display_name FROM sync_inbound WHERE package_id = ?1",
            [package_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
}

/// Rel-path-ordered `(rel_path, byte_size)` of a `sync_inbound_files` set.
fn inbound_file_manifest(db: &Database, inbound_id: i64) -> Vec<(String, i64)> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT rel_path, byte_size FROM sync_inbound_files WHERE inbound_id = {inbound_id} ORDER BY rel_path"
        ))
        .unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

/// `(state, last_error)` of an outbound row in one read.
fn outbound_state_error(db: &Database, id: i64) -> (String, Option<String>) {
    db.conn()
        .query_row(
            "SELECT state, last_error FROM sync_outbound WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .expect("outbound row present")
}

/// The `kind`s in a batch's event journal (`sync_events`) for `direction`
/// (`"sent"`/`"received"`) + `batch_key` (the outbound/inbound row id, stored as
/// text). Test-controlled inputs, so inlined like the existing `count` probes.
fn journal_kinds(db: &Database, direction: &str, batch_key: i64) -> Vec<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT kind FROM sync_events WHERE direction = '{direction}' AND batch_key = '{batch_key}'"
        ))
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

/// All regular-file paths under `dir`, forward-slashed — the landing-tree oracle
/// the structured-layout tests match rel_path suffixes against.
fn landed_paths(dir: &Path) -> Vec<String> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_string_lossy().replace('\\', "/"))
        .collect()
}

/// Spec §D1/§D2/§D4 — **manifest visible before the payload lands, then a
/// structured (nested) batch lands correctly**. The sender enqueues a batch with
/// an explicit name and nested source-relative rel_paths toward a receiver whose
/// payload fetch is HELD aborting (the one-shot abort fault, re-armed each poll —
/// the same "hold the fetch" lever the receiver-cancel acceptance test uses). At
/// that hold point the receiver's `sync_inbound` row already carries the
/// `display_name` and the FULL `sync_inbound_files` manifest (correct nested
/// rel_paths + sizes) with `bytes_done == 0` and NOTHING ingested — no file row is
/// `done`, nothing under the landing root. Releasing the fetch then lands every
/// file under `<sender_slug>/<batch_slug>/<rel_path>` and settles the rows
/// `done`/`ingested`.
///
/// The literal `announced` file-state is unit-pinned by
/// `record_inbound_manifest_writes_name_and_announced_rows` (receiver.rs); on the
/// live single-threaded receiver loop a held fetch churns rows
/// `announced→fetching→failed` per retry, so this integration test pins the
/// deterministic, load-bearing properties — the manifest (name + rel_paths + sizes)
/// is fully known and nothing is ingested BEFORE any payload moves — rather than a
/// transient state string.
#[tokio::test]
async fn manifest_visible_before_payload_then_structured_batch_lands() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // Moderate ack timeout so the held package re-announces on a comfortable cadence
    // (each retry re-records the manifest, the re-armed abort keeps the payload from
    // landing). Retries stay far slower than the 25ms poll re-arm, so no fetch can
    // ever slip through disarmed under CI load — the manifest hold is deterministic.
    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(300)).await;

    // Nested source-relative layout: a common ancestor of `<capture_files>/proj`
    // with two divergent subtrees → the send derives multi-level rel_paths
    // (`A/lights/…`, `B/darks/…`), the structured-batch shape the receiver mirrors.
    let f0 = capture_files.join("proj/A/lights/frame_0.fits");
    let f1 = capture_files.join("proj/A/lights/frame_1.fits");
    let f2 = capture_files.join("proj/B/darks/frame_2.fits");
    let mut frame_ids = Vec::new();
    for p in [&f0, &f1, &f2] {
        frame_ids.push(insert_frame_at(&capture_ctx, p, "M31", 120.0, ImageType::Light, None).0);
    }
    let expected_rels = ["A/lights/frame_0.fits", "A/lights/frame_1.fits", "B/darks/frame_2.fits"];

    // Hold every fetch aborting BEFORE the enqueue so no payload can complete.
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("M31 Lights".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue structured batch");

    // Phase 1 — the announce persists the row + full manifest while the payload is
    // held; re-arm the one-shot abort each poll so nothing can land in between.
    let mut wire = String::new();
    wait_until(
        || {
            receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
            match latest_inbound_package_id(pdb) {
                Some(p) => {
                    let ready = inbound_id_of(pdb, &p)
                        .map(|iid| {
                            count(
                                pdb,
                                &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {iid}"),
                            ) == 3
                        })
                        .unwrap_or(false)
                        && inbound_display_name(pdb, &p).is_some();
                    if ready {
                        wire = p;
                    }
                    ready
                }
                None => false,
            }
        },
        WAIT,
    )
    .await;

    let inbound_id = inbound_id_of(pdb, &wire).expect("inbound row id");
    // The manifest is fully visible: name + every nested rel_path + its size.
    assert_eq!(
        inbound_display_name(pdb, &wire).as_deref(),
        Some("M31 Lights"),
        "the receiver shows the sender's batch name before any byte lands"
    );
    let manifest = inbound_file_manifest(pdb, inbound_id);
    let rels: Vec<&str> = manifest.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(rels, expected_rels, "the full nested manifest is announced pre-fetch");
    for (_, sz) in &manifest {
        assert!(*sz > 0, "each announced file carries its real byte size");
    }
    // …and NOTHING has been ingested or landed yet.
    let (_st, byte_size, bytes_done) = inbound_row(pdb, &wire).expect("inbound row");
    assert!(byte_size > 0, "the package byte size is known from the announce");
    assert_eq!(bytes_done, 0, "no payload bytes counted while the fetch is held");
    assert_eq!(
        count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {inbound_id} AND state = 'done'")),
        0,
        "no file is ingested before the payload moves"
    );
    assert_eq!(landed_count(&designated), 0, "nothing on disk in the landing tree yet");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 0, "no frame ingested before the fetch");

    // Phase 2 — release the fetch: the structured batch lands and settles.
    receiver_ep.set_fault(FaultPlan::default());
    wait_until(|| outbound_state(cdb, latest_outbound_id(cdb)) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 3, WAIT).await;

    // Every file landed under `<sender_slug>/M31_Lights/<nested rel_path>`.
    let landed = landed_paths(&designated);
    assert_eq!(landed.len(), 3, "all three frames landed: {landed:?}");
    for rel in expected_rels {
        let suffix = format!("M31_Lights/{rel}");
        assert!(
            landed.iter().any(|p| p.ends_with(&suffix)),
            "a file landed under the batch/nested tree {suffix}; landed = {landed:?}"
        );
    }
    // Per-file rows settled done/ingested.
    assert_eq!(
        count(
            pdb,
            &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {inbound_id} AND state = 'done' AND outcome = 'ingested'"),
        ),
        3,
        "every per-file row settled done/ingested after the fetch"
    );
    let (final_state, _bs, final_done) = inbound_row(pdb, &wire).expect("inbound row");
    assert_eq!(final_state, "done", "the inbound row is terminal Done");
    assert_eq!(final_done, byte_size, "the fetched bytes reached the announced total");

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Spec §D4/§D5 — **restart/resume restores the per-file picture, then confirms**.
/// The peer's ack is delayed so the sender rests in `Delivered` with every
/// `sync_outbound_files` row persisted `uploaded`; the sender engine is then killed
/// mid-flight (after serve complete, before the ack) and a FRESH engine over the
/// SAME catalog re-drives the row to `confirmed`, every file settling
/// `done`/`ingested`. At the resume point (before completion) `list_transfer_files`
/// already reflects the persisted per-file states — the picture is not lost across
/// the restart.
#[tokio::test]
async fn restart_resumes_per_file_picture_then_confirms() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Delay the receiver's ack so the sender rests in Delivered (fetch complete,
    // not yet confirmed) long enough to snapshot the resume point and kill the
    // engine before the ack lands.
    receiver_ep.set_fault(FaultPlan { delay_ack: Some(Duration::from_secs(2)), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // Long ack timeout: the first engine must NOT time out while the ack is held —
    // it should rest in Delivered until we kill it.
    let (engine_a, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Resume Batch".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let id = latest_outbound_id(cdb);

    // The peer pulled everything (ServeComplete) but has not acked → the row rests
    // Delivered with every file row persisted `uploaded`.
    wait_until(|| outbound_state(cdb, id) == "delivered", WAIT).await;
    let resume_files = list_transfer_files(&capture_ctx, Direction::Sent, id).expect("list outbound files");
    assert_eq!(resume_files.len(), N, "all per-file rows present at the resume point");
    for f in &resume_files {
        assert_eq!(
            f.state.as_deref(),
            Some("uploaded"),
            "the per-file picture is persisted `uploaded` at the resume point, not lost: {f:?}"
        );
    }
    assert_ne!(outbound_state(cdb, id), "confirmed", "not yet confirmed at the resume point");

    // Kill the engine mid-flight and drop the ack delay so the resumed engine's
    // replay ack lands promptly.
    engine_a.shutdown().await;
    receiver_ep.set_fault(FaultPlan::default());

    // A fresh engine over the SAME catalog re-drives the persisted Delivered row.
    let store_b = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
    let engine_b = Arc::new(SyncEngine::spawn(
        store_b as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_node,
    ));

    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;

    let final_files = list_transfer_files(&capture_ctx, Direction::Sent, id).expect("list outbound files");
    assert_eq!(final_files.len(), N);
    for f in &final_files {
        assert_eq!(f.state.as_deref(), Some("done"), "settled done after resume: {f:?}");
        assert_eq!(f.outcome.as_deref(), Some("ingested"), "settled ingested after resume: {f:?}");
    }
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), N as i64, "the peer ingested each frame exactly once");

    engine_b.shutdown().await;
    receiver.shutdown().await;
}

/// A loopback engine builder for `resurrect_pending_senders_with` that stands in
/// for the production `ensure_sender_engine` (which binds a real iroh transport,
/// unavailable over loopback) — exactly the same substitution
/// [`inject_sender_engine`] makes. Each call binds a fresh loopback endpoint on
/// `net`, opens the shared catalog store at `capture_db`, spawns an engine for
/// `peer`, and inserts it into `sender` under `peer` — byte-for-byte what
/// `ensure_sender_engine` does minus the transport build. Returns a closure
/// suitable as the `build` argument.
fn loopback_engine_builder<'a>(
    net: &'a LoopbackNetwork,
    capture_db: &'a Path,
    sender: &'a Arc<SyncSenderRuntime>,
) -> impl Fn(NodeId) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ApiError>> + 'a>> + 'a
{
    move |peer: NodeId| {
        let ep = net.endpoint();
        let sender = Arc::clone(sender);
        let capture_db = capture_db.to_path_buf();
        Box::pin(async move {
            let origin = node_id_hex(&ep.node_id());
            let store = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
            let engine = Arc::new(SyncEngine::spawn(
                store as Arc<dyn SyncStore>,
                Arc::new(ep) as Arc<dyn SharingTransport>,
                peer,
            ));
            sender.lock_inner().await.insert(
                peer,
                StartedSender { engine, origin_device: origin, peer },
            );
            Ok::<(), ApiError>(())
        })
    }
}

/// tv2 follow-up (sender resurrection at app start). Reproduces the owner's live
/// smoke: a pending send is left non-terminal when the sending app dies. On the
/// *current* code (no startup resurrection) a relaunched app has an EMPTY sender
/// runtime, so the orphaned row has no engine — it re-drives nothing, is invisible
/// in `get_sync_status().sender.active`, and cannot be cancelled. This asserts
/// `resurrect_pending_senders_with` (the injectable core the boot path spawns)
/// closes all three gaps: it rebuilds exactly one engine for the orphaned peer,
/// the row becomes visible-and-active again, and the resumed engine drives it to
/// `confirmed` once the peer acks. Also pins the no-double-engine guard: run
/// against a runtime that STILL has the peer's engine, the builder is never
/// invoked.
#[tokio::test]
async fn resurrect_rebuilds_orphaned_sender_then_lists_active_and_confirms() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Hold the ack so the row rests non-terminal (Delivered) while we simulate the
    // crash + resurrection, then release it so the resumed engine confirms.
    receiver_ep.set_fault(FaultPlan { delay_ack: Some(Duration::from_secs(3)), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // The pre-crash engine (long ack timeout so it rests in Delivered, not retry).
    let (engine_a, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Orphan Batch".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let id = latest_outbound_id(cdb);
    wait_until(|| outbound_state(cdb, id) == "delivered", WAIT).await;

    // ── No-double-engine guard: resurrection over a runtime that STILL owns the
    // peer's engine must skip it — the builder is never invoked. ──
    let guard_calls = Arc::new(AtomicUsize::new(0));
    let built_noop = {
        let guard_calls = Arc::clone(&guard_calls);
        resurrect_pending_senders_with(&capture_db, &sender, None, move |_peer| {
            let guard_calls = Arc::clone(&guard_calls);
            async move {
                guard_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<(), ApiError>(())
            }
        })
        .await
        .expect("resurrect enumeration ok")
    };
    assert_eq!(built_noop, 0, "an already-started peer is not rebuilt");
    assert_eq!(guard_calls.load(Ordering::SeqCst), 0, "builder must not run for a live peer");

    // ── Simulate process death: kill the old engine, stand up a FRESH empty
    // sender runtime over the SAME store (exactly what a relaunch does). ──
    engine_a.shutdown().await;
    let sender2 = Arc::new(SyncSenderRuntime::new());
    let sync2 = SyncRuntime::new();

    // Root-cause proof: with no engine the orphaned row is invisible + unreachable.
    assert!(
        sender2.current_for(&receiver_node).await.is_none(),
        "fresh runtime has no engine for the peer"
    );
    let pre = get_status(&capture_ctx, &sync2, &sender2).await.expect("status");
    assert!(
        !pre.sender.active.iter().any(|r| r.id == id),
        "orphaned row is invisible in the active list before resurrection"
    );

    // ── Resurrect: rebuild exactly one engine for the orphaned peer. ──
    let built = resurrect_pending_senders_with(
        &capture_db,
        &sender2,
        None,
        loopback_engine_builder(&net, &capture_db, &sender2),
    )
    .await
    .expect("resurrect ok");
    assert_eq!(built, 1, "exactly one orphaned peer engine was resurrected");
    assert!(
        sender2.current_for(&receiver_node).await.is_some(),
        "an engine now exists for the peer"
    );

    // The row is visible-and-active again (the snapshot reads the shared store).
    let post = get_status(&capture_ctx, &sync2, &sender2).await.expect("status");
    assert!(
        post.sender.active.iter().any(|r| r.id == id),
        "resurrected row is listed active again"
    );

    // Release the ack: the resumed engine drives the orphaned row to confirmed.
    receiver_ep.set_fault(FaultPlan::default());
    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM files"),
        N as i64,
        "the peer ingested each frame exactly once after resurrection"
    );

    if let Some((engine_b, _)) = sender2.current_for(&receiver_node).await {
        engine_b.shutdown().await;
    }
    receiver.shutdown().await;
}

/// tv2 follow-up — the cancel leg of the same smoke. After a restart resurrects
/// the orphaned engine, the user must be able to CANCEL the stuck send (the owner
/// had "no way to cancel"). `cancel_sync_package` routes through
/// [`active_engine_for_row`], which only finds the row once an engine is started
/// for its peer — so this fails on the current code (empty runtime → "package is
/// not active") and passes once resurrection has rebuilt the engine.
#[tokio::test]
async fn cancel_succeeds_on_resurrected_sender_row() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Hold the ack for the whole test so the row stays non-terminal (never confirms).
    receiver_ep.set_fault(FaultPlan { delay_ack: Some(Duration::from_secs(60)), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine_a, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(60)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Cancel After Restart".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let id = latest_outbound_id(cdb);
    wait_until(|| outbound_state(cdb, id) == "delivered", WAIT).await;

    // Process death: kill the engine, fresh empty runtime.
    engine_a.shutdown().await;
    let sender2 = Arc::new(SyncSenderRuntime::new());

    // Current code: with no engine the row cannot be cancelled at all.
    assert!(
        cancel_sync_package(&sender2, id).await.is_err(),
        "cancel is unreachable before resurrection (no active engine)"
    );

    // Resurrect, then cancel succeeds and drives the row terminal.
    let built = resurrect_pending_senders_with(
        &capture_db,
        &sender2,
        None,
        loopback_engine_builder(&net, &capture_db, &sender2),
    )
    .await
    .expect("resurrect ok");
    assert_eq!(built, 1);

    cancel_sync_package(&sender2, id).await.expect("cancel succeeds on the resurrected row");
    wait_until(|| outbound_state(cdb, id) == "cancelled", WAIT).await;

    if let Some((engine_b, _)) = sender2.current_for(&receiver_node).await {
        engine_b.shutdown().await;
    }
    receiver.shutdown().await;
}

/// Spec §D4/§D6/§D7 — **a receiver decline is recorded on BOTH sides with the v2
/// bookkeeping**. Complements the existing `receiver_cancel_terminates_sender_then_resend_delivers`
/// (which pins the sender-terminal + cancelled-receipts + resend legs): this one
/// pins the parts that acceptance test does NOT assert — the receiver's per-frame
/// `cancelled` `sync_history` rows (batch-named), the settled `cancelled` per-file
/// rows, and the `cancelled` entry in BOTH the sent and received event journals.
#[tokio::test]
async fn receiver_cancel_records_v2_history_files_and_both_journals() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Receiver-side cancel signal + a fresh runtime for the command boundary (same
    // wiring as the acceptance cancel test).
    let control = Arc::new(InboundControl::new());
    let primary_sync = SyncRuntime::new();

    // Hold the payload fetch aborting so no ingest can complete before the cancel.
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::clone(&control),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Cancel Me".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let out_id = latest_outbound_id(cdb);

    // Learn the wire package_id from the inbound row (re-arming the abort each poll).
    let mut wire = String::new();
    wait_until(
        || {
            receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
            match latest_inbound_package_id(pdb) {
                Some(p) => {
                    wire = p;
                    true
                }
                None => false,
            }
        },
        WAIT,
    )
    .await;

    // Decline: the live signal diverts the receiver to its cancel epilogue on the
    // next announce; the command exercises the command boundary too.
    control.request_cancel(&wire);
    cancel_incoming_package(&primary_ctx, &primary_sync, &wire)
        .await
        .expect("cancel_incoming_package command");

    // Both sides terminalize cancelled (the acceptance test pins the sender reason
    // + receipts; here these are the sync points for the v2 bookkeeping below).
    wait_until(|| outbound_state(cdb, out_id) == "cancelled", WAIT).await;
    wait_until(
        || matches!(inbound_row(pdb, &wire), Some((s, _, _)) if s == "cancelled"),
        WAIT,
    )
    .await;
    let inbound_id = inbound_id_of(pdb, &wire).expect("inbound row id");

    // (1) Receiver history: one `cancelled` row per frame, each batch-named.
    assert_eq!(
        count(
            pdb,
            "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'cancelled' AND batch_name = 'Cancel Me'",
        ),
        N as i64,
        "the receiver logged one batch-named cancelled history row per frame"
    );

    // (2) Per-file rows settled cancelled.
    assert_eq!(
        count(
            pdb,
            &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {inbound_id} AND outcome = 'cancelled'"),
        ),
        N as i64,
        "every inbound per-file row settled cancelled"
    );

    // (3) Both journals carry their `cancelled` entry.
    assert!(
        journal_kinds(cdb, "sent", out_id).contains(&"cancelled".to_string()),
        "the sent journal records the cancel"
    );
    assert!(
        journal_kinds(pdb, "received", inbound_id).contains(&"cancelled".to_string()),
        "the received journal records the cancel"
    );

    // No files landed (a declined package lands nothing).
    assert_eq!(landed_count(&designated), 0, "a declined package lands no files");

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Spec §D5/§D7 — **ack-timeout → waiting → recovery leaves no sticky error**. A
/// silent peer (its fetch aborts once, so no ack) drives one ack timeout: the row
/// arms `next_retry_at`, sets `last_error`, journals `ack_timeout` + `retry_scheduled`,
/// and `get_status` reports `displayState == "waiting"` with `stalledUntil`. On the
/// next attempt the serve tick clears `last_error` at the `Delivered`/serving stage
/// — BEFORE the confirm ack lands (the T4 Req-5 mechanism the unit suite checked
/// only at `confirmed`) — and the transfer finally confirms with a clean summary
/// and `ack_received` + `confirmed` journalled.
#[tokio::test]
async fn ack_timeout_waiting_then_recovery_clears_error_at_serving_stage() {
    const N: usize = 3;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Attempt 1 aborts (one-shot) → no ack → ack timeout. Attempt 2 completes but
    // holds the ack 300ms (< the 600ms timeout, so no re-timeout) so the row rests
    // in Delivered with the error already cleared, before it confirms.
    receiver_ep.set_fault(FaultPlan {
        abort_after_bytes: Some(1),
        delay_ack: Some(Duration::from_millis(300)),
        ..Default::default()
    });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(600)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Retry Batch".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let id = latest_outbound_id(cdb);

    // Phase 1 — the ack times out: last_error set, next_retry_at armed, journalled.
    wait_until(
        || {
            let (state, err) = outbound_state_error(cdb, id);
            err.is_some() && outbound_next_retry(cdb, id).is_some() && !is_terminal_state(&state)
        },
        WAIT,
    )
    .await;
    assert_eq!(
        outbound_last_error(cdb, id).as_deref(),
        Some("no ack from peer within timeout"),
        "the ack timeout is recorded as the (transient) last_error"
    );
    let kinds = journal_kinds(cdb, "sent", id);
    assert!(kinds.contains(&"ack_timeout".to_string()), "ack_timeout journalled: {kinds:?}");
    assert!(kinds.contains(&"retry_scheduled".to_string()), "retry_scheduled journalled: {kinds:?}");

    // …and `get_status` presents it as a neutral `waiting` with a stall deadline —
    // polled so it catches the (single) backoff window while it is open.
    {
        let deadline = Instant::now() + WAIT;
        loop {
            let st = get_status(&capture_ctx, &sync, &sender).await.expect("get_status");
            if let Some(row) = st.sender.active.iter().find(|r| r.id == id) {
                if row.display_state == "waiting" && row.stalled_until.is_some() {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "never saw displayState waiting with a stall deadline");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Phase 2 — recovery: the serve tick clears the stale error at the Delivered
    // (serving) stage, BEFORE the confirm ack.
    wait_until(
        || {
            let (state, err) = outbound_state_error(cdb, id);
            state == "delivered" && err.is_none()
        },
        WAIT,
    )
    .await;

    // …then the transfer confirms with a clean summary + journal.
    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    assert_eq!(outbound_last_error(cdb, id), None, "a confirmed transfer carries no stale reason");
    let kinds = journal_kinds(cdb, "sent", id);
    assert!(kinds.contains(&"ack_received".to_string()), "ack_received journalled: {kinds:?}");
    assert!(kinds.contains(&"confirmed".to_string()), "confirmed journalled: {kinds:?}");
    assert_eq!(landed_count(&designated), N, "the recovered transfer delivered every frame");

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// Spec §D2/§D4 — **WBPP-layout object send, end to end**. A frame set of two
/// lights that consume a Dark calibration set is object-sent (`frame_set_id` → the
/// WBPP `rel_path` layout, including the calibration level). The receiver disk tree
/// mirrors `<batch>/camera_*/DARKS_*/lights/…` (batch = the frame-set name), the
/// per-file + history rows carry the right filenames, and `list_transfer_files` on
/// BOTH sides returns the identical rel_path set — sender and receiver agree on the
/// whole structured tree.
#[tokio::test]
async fn wbpp_object_send_lands_identical_tree_on_both_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;

    // Two lights (camera "C") + a Dark master they consume — the object-send WBPP
    // shape. Frame-set membership via session_members; calibration link via
    // calibration_set_to_frames (same fixture pattern as archive_e2e).
    let l1 = capture_files.join("M31/L_001.fits");
    let l2 = capture_files.join("M31/L_002.fits");
    let dk = capture_files.join("Cal/MasterDark.fits");
    let (lf1, _u1) = insert_frame_at(&capture_ctx, &l1, "M31", 300.0, ImageType::Light, Some("C"));
    let (lf2, _u2) = insert_frame_at(&capture_ctx, &l2, "M31", 300.0, ImageType::Light, Some("C"));
    let (df, _ud) = insert_frame_at(&capture_ctx, &dk, "M31", 300.0, ImageType::Dark, Some("C"));
    {
        let db = capture_ctx.db.get().unwrap();
        let conn = db.conn();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        for fid in [lf1, lf2] {
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [fid]).unwrap();
        }
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')", []).unwrap();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, ?1)", [df]).unwrap();
        for fid in [lf1, lf2] {
            conn.execute(
                "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 500, 'Dark', '2025-10-12')",
                [fid],
            )
            .unwrap();
        }
    }

    // Object send: frame_set_id drives the WBPP layout; include the dark so the
    // calibration level is actually transferred. No explicit name → auto-name to
    // the frame-set name "M31".
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        vec![lf1, lf2, df],
        None,
        Some(1),
        None,
    )
    .await
    .expect("object send");

    wait_until(|| outbound_state(cdb, latest_outbound_id(cdb)) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 3, WAIT).await;

    let out_id = latest_outbound_id(cdb);
    let wire = latest_inbound_package_id(pdb).expect("inbound row");
    let in_id = inbound_id_of(pdb, &wire).expect("inbound row id");

    // Both sides agree on the exact rel_path set.
    let mut sent_rels: Vec<String> = list_transfer_files(&capture_ctx, Direction::Sent, out_id)
        .expect("sent files")
        .into_iter()
        .map(|f| f.rel_path)
        .collect();
    let mut recv_rels: Vec<String> = list_transfer_files(&primary_ctx, Direction::Received, in_id)
        .expect("received files")
        .into_iter()
        .map(|f| f.rel_path)
        .collect();
    sent_rels.sort();
    recv_rels.sort();
    assert_eq!(sent_rels, recv_rels, "sender and receiver agree on the whole structured tree");

    // The WBPP object-send layout, including the calibration (DARKS) level.
    let expected = {
        let mut e = vec![
            "camera_c/DARKS_500/MasterDark.fits".to_string(),
            "camera_c/DARKS_500/lights/L_001.fits".to_string(),
            "camera_c/DARKS_500/lights/L_002.fits".to_string(),
        ];
        e.sort();
        e
    };
    assert_eq!(sent_rels, expected, "WBPP rel_paths carry the camera + calibration + lights levels");

    // The receiver disk tree mirrors `<batch=M31>/camera_C/DARKS_500/…`.
    let landed = landed_paths(&designated);
    assert_eq!(landed.len(), 3, "all three frames landed: {landed:?}");
    for rel in &expected {
        let suffix = format!("M31/{rel}");
        assert!(
            landed.iter().any(|p| p.ends_with(&suffix)),
            "a file landed under the WBPP tree {suffix}; landed = {landed:?}"
        );
    }

    // Per-file rows settled done/ingested; history rows carry the filenames.
    assert_eq!(
        count(
            pdb,
            &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {in_id} AND state = 'done' AND outcome = 'ingested'"),
        ),
        3,
        "every per-file row settled done/ingested"
    );
    for filename in ["L_001.fits", "L_002.fits", "MasterDark.fits"] {
        assert!(
            count(
                pdb,
                &format!("SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND filename = '{filename}'"),
            ) >= 1,
            "received history carries the frame filename {filename}"
        );
    }

    engine.shutdown().await;
    receiver.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// tvb B8 — the batch-model contract, end to end. Five two-engine loopback
// scenarios composing the whole Transfers Batch Model stack (B1–B7) at the
// two-`ServiceContext` level: (1) a SENDER cancel aborts the receiver's in-flight
// fetch promptly and terminalizes both sides via Revoke; (2) a mid-fetch failure →
// resend delivers into the SAME batch-keyed inbound row (only the missing dedup
// subset travels); (3) the all-duplicate-after-announce race closes the stuck
// receiver row with Revoke(Superseded); (4) a restart mid-resend resumes the SAME
// row and confirms; (5) delete reclaims rows/files/events/history + the sender
// payload dir, scoped to its batch (a foreign batch survives). Deterministic waits
// only (the harness `wait_until`); generous bounded deadlines.
// ─────────────────────────────────────────────────────────────────────────────

/// A [`DedupResponder`] whose verdict flips on a shared flag: want-all while
/// `false` (session 1 gets a real announce + fetch), all-duplicate while `true`
/// (session 2's re-negotiate finds the peer already holds every frame). The e2e
/// twin of the engine-unit `SwitchableResponder`, used to construct the superseded
/// race (scenario 3) against a REAL [`SyncReceiver`] instead of a recording double.
struct SwitchableResponder {
    all_duplicate: Arc<AtomicBool>,
}
impl DedupResponder for SwitchableResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        if self.all_duplicate.load(Ordering::SeqCst) {
            (Vec::new(), entries.iter().map(|e| e.rel_path.clone()).collect())
        } else {
            (entries.iter().map(|e| e.rel_path.clone()).collect(), Vec::new())
        }
    }
    fn confirm_full_hashes(&self, _entries: &[FullHashEntry]) -> Vec<String> {
        Vec::new()
    }
}

/// `sync_outbound.generation` for a row — the user-facing "attempt N" counter,
/// bumped ONLY by a resend (never by an announce-retry, which bumps `attempts`).
fn outbound_generation(db: &Database, id: i64) -> i64 {
    db.conn()
        .query_row("SELECT generation FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

/// `sync_inbound.generation` for a row id — bumped on each receiver re-attempt of
/// the same `(peer, batch_uuid)`.
fn inbound_generation(db: &Database, inbound_id: i64) -> i64 {
    db.conn()
        .query_row("SELECT generation FROM sync_inbound WHERE id = ?1", [inbound_id], |r| r.get(0))
        .expect("inbound row present")
}

/// `sync_inbound.batch_uuid` for a row id — the durable per-transfer batch key the
/// receiver holds one long-lived row on (constant across a resend's wire rotation).
fn inbound_batch_uuid(db: &Database, inbound_id: i64) -> Option<String> {
    db.conn()
        .query_row("SELECT batch_uuid FROM sync_inbound WHERE id = ?1", [inbound_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
}

/// `sync_inbound.last_error` for a wire `package_id` — the terminal detail
/// (`"by sender"` / `"nothing to fetch (superseded by sender)"` / …) the
/// `InboundSummary.last_error` surfaces (B5b).
fn inbound_last_error(db: &Database, package_id: &str) -> Option<String> {
    db.conn()
        .query_row("SELECT last_error FROM sync_inbound WHERE package_id = ?1", [package_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
}

/// The on-disk payload dir (`sync_outbound.package_ref`) for an outbound row.
fn outbound_package_ref(db: &Database, id: i64) -> String {
    db.conn()
        .query_row("SELECT package_ref FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

/// The current wire id (`sync_outbound.wire_package_id`) for an outbound row — the
/// per-attempt id a resend rotates.
fn outbound_wire_id(db: &Database, id: i64) -> String {
    db.conn()
        .query_row("SELECT wire_package_id FROM sync_outbound WHERE id = ?1", [id], |r| r.get(0))
        .expect("outbound row present")
}

/// Like [`inject_sender_engine`] but spawns the engine with a host
/// [`ProgressEmitter`] (so a test can read each package's `{new, duplicate}` dedup
/// split off the send-side `sync-finished`) alongside the short `ack_timeout`.
async fn inject_sender_engine_with_emitter(
    net: &LoopbackNetwork,
    capture_db: &Path,
    peer: NodeId,
    ack_timeout: Duration,
    emitter: Arc<dyn ProgressEmitter>,
) -> (Arc<SyncEngineHandle>, Arc<SyncSenderRuntime>, Arc<SyncSenderRuntime>, SyncRuntime) {
    let sender_ep = net.endpoint();
    let sender_node = sender_ep.node_id();
    let engine_store = Arc::new(CatalogSyncStore::open(capture_db).unwrap());
    let engine = Arc::new(SyncEngine::spawn_with_config_and_emitter(
        engine_store as Arc<dyn SyncStore>,
        Arc::new(sender_ep) as Arc<dyn SharingTransport>,
        peer,
        SyncConfig { ack_timeout },
        Some(emitter),
    ));
    let sender = Arc::new(SyncSenderRuntime::new());
    let collab_sender = Arc::new(SyncSenderRuntime::new());
    let sync = SyncRuntime::new();
    {
        let mut guard = sender.lock_inner().await;
        guard.insert(
            peer,
            StartedSender { engine: Arc::clone(&engine), origin_device: node_id_hex(&sender_node), peer },
        );
    }
    (engine, sender, collab_sender, sync)
}

/// B8 scenario 1 — **sender cancel stops the receiver promptly (two-engine)**. The
/// two-context composition of the receiver-unit
/// `revoke_mid_fetch_aborts_promptly_not_at_fetch_completion`: a genuinely-blocking
/// fetch (10 files paced at 500ms/read ⇒ ~5.5s uninterrupted) is cancelled on the
/// SENDER engine mid-flight; the sender's `Revoke{Cancelled}` aborts the receiver's
/// in-flight fetch WITHIN a 3s deadline (« 5.5s, so NOT at fetch completion). Asserts
/// the whole revoke teardown across both catalogs: receiver row `cancelled`
/// ("by sender"), staging cleaned, in-flight tags absent, sender row `cancelled`,
/// both journals (`revoke_sent` sent / `revoked` received), and the single terminal
/// `sync-finished` the receiver emits on the revoke path (outcome == the row).
#[tokio::test]
async fn sender_cancel_aborts_receiver_fetch_promptly_and_revokes_both() {
    const N: usize = 10;
    // Pace each fetch read so an uninterrupted whole-package fetch (files + manifest)
    // takes ~5.5s — far longer than ABORT_DEADLINE, so "cancelled within the
    // deadline" proves the revoke aborted the fetch, not that it finished on its own.
    const DELAY: Duration = Duration::from_millis(500);
    const ABORT_DEADLINE: Duration = Duration::from_secs(3);

    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Pace the fetch so it is genuinely mid-flight when we fire the sender cancel.
    receiver_ep.set_fault(FaultPlan { delay_per_read: Some(DELAY), ..Default::default() });

    // A recording emitter on the RECEIVER: the revoke path's terminal `sync-finished`
    // is the observable the Transfers widget dismisses a row on.
    let recorder = Arc::new(RecordingEmitter::default());
    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::clone(&recorder) as Arc<dyn ProgressEmitter>,
    )
    .await
    .expect("spawn primary receiver");

    // Long ack timeout so the sender rests in Transferring (awaiting the never-coming
    // ack) rather than re-announcing during the paced fetch.
    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Cancel Mid-Fetch".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue");
    let out_id = latest_outbound_id(cdb);

    // Wait until the receiver row is `fetching` — the announce reached it (so the
    // sender is Transferring, `started=true`, and a cancel WILL Revoke) and the paced
    // fetch is in flight.
    let mut wire = String::new();
    wait_until(
        || match latest_inbound_package_id(pdb) {
            Some(p) => match inbound_row(pdb, &p) {
                Some((s, _, _)) if s == "fetching" => {
                    wire = p;
                    true
                }
                _ => false,
            },
            None => false,
        },
        WAIT,
    )
    .await;
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 0, "no frame ingested while the fetch is paced");

    // Fire the SENDER cancel.
    let fired = Instant::now();
    cancel_sync_package(&sender, out_id).await.expect("cancel the sending package");

    // The receiver's in-flight fetch aborts and its row terminalizes `cancelled`
    // ("by sender") WITHIN the deadline — well under the uninterrupted fetch time.
    wait_until(
        || matches!(inbound_row(pdb, &wire), Some((s, _, _)) if s == "cancelled"),
        ABORT_DEADLINE,
    )
    .await;
    let elapsed = fired.elapsed();
    assert!(
        elapsed < ABORT_DEADLINE,
        "the receiver aborted promptly ({elapsed:?} < {ABORT_DEADLINE:?}), NOT at fetch completion (~{:?})",
        DELAY * (N as u32 + 1)
    );
    assert_eq!(
        inbound_last_error(pdb, &wire).as_deref(),
        Some("by sender"),
        "the receiver records the sender-revoke reason"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 0, "a sender-cancelled transfer lands no frames");
    assert_eq!(landed_count(&designated), 0, "nothing on disk in the landing tree");

    // Staging cleaned (handle_revoke teardown) + in-flight tags released. The
    // loopback fetch never SEEDS an in-flight tag (only the cfg(test) seeder does, out
    // of an integration test's reach), so `list_in_flight_tags` pins the release-call
    // namespace-emptiness, not a seeded→released transition — the seeded transition is
    // the B7 unit `in_flight_tag_list_and_release_honor_the_transfer_namespace`.
    let staging = primary_dir.join("staging").join(&wire);
    wait_until(|| !staging.exists(), ABORT_DEADLINE).await;
    assert!(!staging.exists(), "the partially-fetched staging dir is fully cleaned on revoke");
    assert!(
        receiver_ep.list_in_flight_tags().await.unwrap().is_empty(),
        "no receiver in-flight tags remain after the revoke release"
    );

    // Sender terminalizes cancelled.
    wait_until(|| outbound_state(cdb, out_id) == "cancelled", WAIT).await;

    // Both journals carry the revoke crumbs: sent → revoke_sent, received → revoked.
    let sent_kinds = journal_kinds(cdb, "sent", out_id);
    assert!(sent_kinds.contains(&"revoke_sent".to_string()), "sent journal records revoke_sent: {sent_kinds:?}");
    let in_id = inbound_id_of(pdb, &wire).expect("inbound row id");
    let recv_kinds = journal_kinds(pdb, "received", in_id);
    assert!(recv_kinds.contains(&"revoked".to_string()), "received journal records revoked: {recv_kinds:?}");

    // The revoke path emitted a single terminal `sync-finished` whose outcome matches
    // the row (`cancelled`).
    wait_until(|| recorder.received_finished().iter().any(|(o, _)| o == "cancelled"), WAIT).await;
    let rf = recorder.received_finished();
    assert!(
        rf.iter().any(|(o, _)| o == "cancelled"),
        "sync-finished emitted on the revoke path with outcome == the row (cancelled): {rf:?}"
    );

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// B8 scenario 2 — **failure → resend → confirm into ONE batch-keyed row**. The
/// route (documented): a receiver fetch-abort never terminalizes the sender
/// (delivery-forever), and a receiver DECLINE is final per its `batch_uuid` (a
/// deliberate resend of a declined transfer mints a NEW transfer rather than
/// reviving the declined one — pinned by
/// `receiver_decline_then_resend_mints_new_transfer_and_delivers`). So the route
/// to a delivering resend into the SAME row is the **fail path**: the payload dir is renamed away
/// so the sender's next attempt hits the missing-payload terminal (`Revoke{Failed}` →
/// the receiver's already-`failed` row is a terminal no-op, staying non-cancelled and
/// re-drivable). The payload is restored, `resend_transfer` rotates the wire id
/// (generation 2) and re-drives — and the receiver maps it, via the durable
/// `batch_uuid`, onto the SAME inbound row it failed on. Two frames are pre-seeded on
/// the receiver (a prior clean transfer) so the dedup Want is a STRICT subset: only
/// the missing frames travel.
#[tokio::test]
async fn failed_fetch_then_resend_delivers_into_one_batch_row() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // Receiver WITH a catalog dedup responder so the resend's negotiate drops the
    // pre-seeded frames (strict-subset Want).
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let responder: Arc<dyn DedupResponder> =
        Arc::new(CatalogDedupResponder::new(Arc::clone(&primary_store)));
    let receiver_ep = Arc::new(net.endpoint_with_responder(responder));
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // Long ack timeout so, after attempt-1's single announce + aborted fetch, the
    // sender rests in Transferring and does NOT re-announce on its own — so the
    // receiver row is reset EXACTLY once (by the resend) → generation 2, deterministic.
    let emitter = Arc::new(RecordingEmitter::default());
    let (engine, sender, collab_sender, sync) = inject_sender_engine_with_emitter(
        &net,
        &capture_db,
        receiver_node,
        Duration::from_secs(30),
        Arc::clone(&emitter) as Arc<dyn ProgressEmitter>,
    )
    .await;

    // 5 frames; idx 0,1 are pre-seeded on the receiver, idx 2,3,4 are the missing
    // subset the batch delivers.
    let mut frame_ids = Vec::with_capacity(5);
    for idx in 0..5 {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }

    // ── Pre-seed: a prior CLEAN transfer of {0,1} → the receiver ingests them and
    // populates `files.content_hash` (the sampling hash the responder dedups on). ──
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids[0..2].to_vec(),
        Some("Pre-seed".to_string()),
        None,
        None,
    )
    .await
    .expect("pre-seed enqueue");
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 2, WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL") == 2, WAIT).await;

    // ── The batch: {0,1,2,3,4}. Negotiate drops {0,1} (dupes) → Want {2,3,4}. Arm a
    // one-shot abort so attempt-1's fetch of that subset aborts → inbound row failed. ──
    receiver_ep.set_fault(FaultPlan { abort_after_bytes: Some(1), ..Default::default() });
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Resend Into One Row".to_string()),
        None,
        None,
    )
    .await
    .expect("batch enqueue");
    let out_id = latest_outbound_id(cdb);

    // Attempt 1: a SECOND inbound row appears (the batch's), and its subset fetch
    // aborts → it lands `failed` at generation 1. Capture its id + attempt-1 wire.
    wait_until(
        || {
            count(pdb, "SELECT COUNT(*) FROM sync_inbound") == 2
                && matches!(latest_inbound_package_id(pdb), Some(p) if matches!(inbound_row(pdb, &p), Some((s, _, _)) if s == "failed"))
        },
        WAIT,
    )
    .await;
    let wire_attempt1 = latest_inbound_package_id(pdb).expect("batch inbound wire (attempt 1)");
    let in_id = inbound_id_of(pdb, &wire_attempt1).expect("batch inbound row id");
    let batch_uuid = inbound_batch_uuid(pdb, in_id).expect("batch inbound batch_uuid");
    assert_eq!(inbound_generation(pdb, in_id), 1, "the batch inbound row is at generation 1 after attempt 1");

    // ── Drive the sender terminal via the fail path: rename the payload away, kick a
    // re-attempt (missing-payload → `Failed`), then restore it. ──
    let pkg_ref = outbound_package_ref(cdb, out_id);
    let stashed = format!("{pkg_ref}.stashed");
    std::fs::rename(&pkg_ref, &stashed).expect("stash the payload dir");
    engine.kick(out_id).await.expect("kick a re-attempt onto the missing payload");
    wait_until(|| outbound_state(cdb, out_id) == "failed", WAIT).await;
    assert_eq!(
        outbound_last_error(cdb, out_id).as_deref(),
        Some("package payload missing on disk"),
        "the sender failed on the missing payload (the honest resend route)"
    );
    // The receiver row is STILL failed (the Revoke{Failed} hit an already-terminal
    // row → no-op), non-cancelled, generation still 1 — re-drivable.
    assert!(
        matches!(inbound_row(pdb, &wire_attempt1), Some((s, _, _)) if s == "failed"),
        "the receiver row stays failed (not cancelled) — a resend can re-drive it"
    );
    assert_eq!(inbound_generation(pdb, in_id), 1, "no stray re-announce bumped the inbound generation before the resend");
    std::fs::rename(&stashed, &pkg_ref).expect("restore the payload dir");

    // ── Resend: rotates the wire id (generation 2) and re-drives; the one-shot abort
    // already fired, so this fetch delivers. ──
    let resend_id = retry_sync_package(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, out_id, None)
        .await
        .expect("resend the failed transfer");
    assert_eq!(resend_id, out_id, "resend-as-reset returns the SAME outbound row");
    let wire_resend = outbound_wire_id(cdb, out_id);
    assert_ne!(wire_resend, wire_attempt1, "the resend rotates the wire id");

    wait_until(|| outbound_state(cdb, out_id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 5, WAIT).await;

    // ── The batch delivered into the SAME inbound row (id + batch_uuid constant),
    // generation 2, only the missing subset travelled. ──
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM sync_inbound"), 2, "still just the pre-seed row + the ONE batch row");
    assert_eq!(inbound_id_of(pdb, &wire_resend), Some(in_id), "the resend maps onto the SAME inbound row id");
    assert_eq!(inbound_batch_uuid(pdb, in_id).as_deref(), Some(batch_uuid.as_str()), "batch_uuid is constant across attempts");
    assert_eq!(inbound_generation(pdb, in_id), 2, "the receiver row is at generation 2 (reset exactly once, by the resend)");
    assert!(matches!(inbound_row(pdb, &wire_resend), Some((s, _, _)) if s == "done"), "the batch inbound row is terminal Done");

    // Only the 3 MISSING frames travelled: exactly the {2,3,4} per-file rows settled
    // done/ingested, and the 2 pre-seeded {0,1} were dedup-dropped by the sender
    // (never served, never ingested on the batch) — the strict-subset Want. (The
    // announce carries the full manifest; the dedup shows in what is FETCHED, so the
    // ingested subset is the honest "what travelled" oracle.)
    assert_eq!(
        count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {in_id} AND outcome = 'ingested'")),
        3,
        "exactly the 3 missing frames settled ingested on the batch row"
    );
    for idx in [2usize, 3, 4] {
        let fname = format!("light_{idx:04}.fits");
        assert_eq!(
            count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {in_id} AND outcome = 'ingested' AND rel_path LIKE '%{fname}'")),
            1,
            "the missing frame {fname} travelled on the batch"
        );
    }
    for idx in [0usize, 1] {
        let fname = format!("light_{idx:04}.fits");
        assert_eq!(
            count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {in_id} AND outcome = 'ingested' AND rel_path LIKE '%{fname}'")),
            0,
            "the pre-seeded (deduped) frame {fname} did NOT travel on the batch"
        );
    }

    // B5b at e2e: every received history row keys on the durable batch_uuid, NONE on a
    // wire id (no phantom wire-keyed groups across the resend).
    assert_eq!(
        count(pdb, &format!("SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND package_id IN ('{wire_attempt1}', '{wire_resend}')")),
        0,
        "no received history row keys on a wire id"
    );
    assert!(
        count(pdb, &format!("SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND package_id = '{batch_uuid}' AND outcome = 'ingested'")) >= 3,
        "the batch's received history keys on the batch_uuid"
    );

    // Generation 2 in BOTH summaries (list_terminal_transfers), attempts >= generation.
    assert_eq!(outbound_generation(cdb, out_id), 2, "sender outbound at generation 2");
    assert!(outbound_attempts(cdb, out_id) >= outbound_generation(cdb, out_id), "attempts >= generation");
    let terminal_sent = list_terminal_transfers(&capture_ctx, None).expect("sender terminal transfers");
    let sent_sum = terminal_sent.sent.iter().find(|s| s.id == out_id).expect("sender summary present");
    assert_eq!(sent_sum.state.as_str(), "confirmed", "sender summary is confirmed");
    assert_eq!(sent_sum.generation, 2, "sender summary carries generation 2");
    let terminal_recv = list_terminal_transfers(&primary_ctx, None).expect("receiver terminal transfers");
    let recv_sum = terminal_recv.received.iter().find(|s| s.id == in_id).expect("receiver summary present");
    assert_eq!(recv_sum.generation, 2, "receiver summary carries generation 2");

    // The resend's send-side `sync-finished` shows the strict-subset split (new=3,
    // dup=2); it is the LAST finished (the middle (0,0) is the fail's terminal emit).
    assert_eq!(
        emitter.sent_finished().last().copied(),
        Some((3, 2)),
        "the resend moved only the 3 missing frames (new=3, duplicate=2): {:?}",
        emitter.sent_finished()
    );

    engine.shutdown().await;
    receiver.shutdown().await;
}

/// B8 scenario 3 — **all-duplicate-after-announce race → Revoke(Superseded) closes
/// the receiver row**. B3's crash-resume superseded construction at the two-engine
/// level with a REAL [`SyncReceiver`]: session 1 (want-all) announces + the receiver
/// begins fetching, HELD by a long per-read pace so its row rests non-terminal
/// (`fetching`) with nothing ingested; the sender reaches Transferring and is killed.
/// The responder flips to all-duplicate; session 2 (fresh engine, SAME store)
/// crash-resumes → re-negotiates → all-duplicate → confirms locally AND revokes the
/// stuck row. The receiver's `handle_revoke(Superseded)` aborts the held fetch and
/// terminalizes the row `done` ("superseded") — no stuck `announced`/`fetching` row
/// remains.
#[tokio::test]
async fn all_duplicate_after_announce_supersedes_stuck_receiver_row() {
    const N: usize = 3;
    // Pace each fetch read so, once the FIRST file lands in staging, the fetch rests
    // inside a long per-read sleep — wide enough that the session-2 superseded revoke's
    // abort fires DURING the sleep (before the next file-open), so the fetch aborts
    // cleanly rather than tripping over the payload engine2 cleans on its all-duplicate
    // confirm. We kill/resume only AFTER file 0 is on disk in staging, so the fetch is
    // provably past `subset_src_files` (which ran while engine1 kept the payload
    // present) and mid-sleep — never at the payload-touching setup. Generous (12s) so
    // a slow-CI engine2 (crash-resume → re-negotiate → confirm → revoke, normally
    // sub-second) still aborts the fetch well within the sleep, before the next
    // file-open would touch the cleaned payload.
    const HOLD: Duration = Duration::from_secs(12);

    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // Receiver WITH a flipping responder (want-all → all-duplicate).
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let all_dup = Arc::new(AtomicBool::new(false));
    let responder: Arc<dyn DedupResponder> =
        Arc::new(SwitchableResponder { all_duplicate: Arc::clone(&all_dup) });
    let receiver_ep = Arc::new(net.endpoint_with_responder(responder));
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Hold every fetch so session 1's row rests `fetching`, nothing ingests.
    receiver_ep.set_fault(FaultPlan { delay_per_read: Some(HOLD), ..Default::default() });

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    // ── Session 1: want-all announce → Transferring; held fetch keeps the receiver
    // row non-terminal. Long ack timeout so the sender rests (doesn't re-announce). ──
    let (engine1, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;
    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Superseded Race".to_string()),
        None,
        None,
    )
    .await
    .expect("session 1 enqueue");
    let id = latest_outbound_id(cdb);

    // Sender Transferring AND the receiver row `fetching`. Capture wire W1.
    wait_until(|| outbound_state(cdb, id) == "transferring", WAIT).await;
    let mut w1 = String::new();
    wait_until(
        || match latest_inbound_package_id(pdb) {
            Some(p) => match inbound_row(pdb, &p) {
                Some((s, _, _)) if s == "fetching" => {
                    w1 = p;
                    true
                }
                _ => false,
            },
            None => false,
        },
        WAIT,
    )
    .await;
    let in_id = inbound_id_of(pdb, &w1).expect("session-1 inbound row id");
    // Wait until file 0 has landed in the per-package staging dir: the fetch is now
    // provably past `subset_src_files`/the first file's copy (which ran while engine1
    // kept the payload present) and resting inside the long per-read sleep — the safe
    // kill point where the superseded revoke's abort lands mid-sleep, before the next
    // file-open would trip over the payload engine2 cleans.
    let staged_file0 = primary_dir.join("staging").join(&w1).join("light_0000.fits");
    wait_until(|| staged_file0.exists(), WAIT).await;

    // Peer now holds everything; kill session 1 (row stays non-terminal; the fetch is
    // orphaned mid-sleep on the receiver).
    all_dup.store(true, Ordering::SeqCst);
    engine1.shutdown().await;

    // ── Session 2: fresh engine, SAME store → crash-resume re-negotiates → the peer
    // reports all-duplicate → confirm locally AND Revoke{Superseded}(W1). The revoke
    // aborts the receiver's mid-sleep fetch; `handle_revoke(Superseded)` then
    // terminalizes the row `done`. ──
    let store2 = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
    let engine2 = Arc::new(SyncEngine::spawn(
        store2 as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_node,
    ));

    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| matches!(inbound_row(pdb, &w1), Some((s, _, _)) if s == "done"), WAIT).await;

    // The superseded revoke closed the row honestly (`done`, "superseded"), nothing
    // landed, and the received journal records the revoke — proof `handle_revoke`
    // terminalized the row (not a stray ingest).
    assert_eq!(
        inbound_last_error(pdb, &w1).as_deref(),
        Some("nothing to fetch (superseded by sender)"),
        "the receiver row terminalizes done («superseded») via the revoke"
    );
    assert!(
        journal_kinds(pdb, "received", in_id).contains(&"revoked".to_string()),
        "the received journal records the revoke (handle_revoke ran on a non-terminal row)"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM frames"), 0, "the superseded receiver ingested nothing");
    assert_eq!(landed_count(&designated), 0, "nothing landed on disk");
    // No stuck announced/fetching row remains; exactly one inbound row, terminal.
    assert_eq!(
        count(pdb, "SELECT COUNT(*) FROM sync_inbound WHERE state IN ('announced', 'fetching')"),
        0,
        "no stuck announced/fetching inbound row remains"
    );
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM sync_inbound"), 1, "exactly one inbound row (the superseded one)");
    // The sender emitted the F3-orphan-closing Revoke{Superseded} (crash-resume path).
    assert!(
        journal_kinds(cdb, "sent", id).contains(&"revoke_sent".to_string()),
        "the sender journalled revoke_sent (the superseded revoke)"
    );

    engine2.shutdown().await;
    receiver.shutdown().await;
}

/// B8 scenario 4 — **restart mid-resend resumes the SAME row and confirms**. Cancel
/// (a sender-side cancel toward an OFFLINE peer — pending never `started`, so no
/// receiver row is cancelled and the resend is free to deliver), then resend
/// (generation 2), then kill the sender engine after the reset but before any
/// confirm (the peer is still offline, so nothing CAN confirm — the kill is
/// unambiguously mid-resend). A fresh engine over the SAME store crash-resumes the
/// gen-2 row; bringing the receiver online, it delivers and confirms — SAME row id,
/// generation UNCHANGED by the restart, and the receiver holds exactly ONE row.
#[tokio::test]
async fn restart_mid_resend_resumes_same_row_and_confirms() {
    const N: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // Mint the receiver endpoint (node id known) but keep it OFFLINE for now.
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    // Short ack timeout → ms-scale retries against the offline peer.
    let (engine1, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_millis(50)).await;

    let mut frame_ids = Vec::with_capacity(N);
    for idx in 0..N {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids.clone(),
        Some("Restart Mid-Resend".to_string()),
        None,
        None,
    )
    .await
    .expect("enqueue toward offline peer");
    let id = latest_outbound_id(cdb);

    // The package retries against the offline peer (non-terminal); no announce lands.
    wait_until(|| outbound_attempts(cdb, id) >= 2 && !is_terminal_state(&outbound_state(cdb, id)), WAIT).await;
    assert_eq!(outbound_generation(cdb, id), 1, "a fresh enqueue is at generation 1");

    // Cancel (sender-side, peer never started → no revoke, no receiver row).
    cancel_sync_package(&sender, id).await.expect("cancel the offline package");
    wait_until(|| outbound_state(cdb, id) == "cancelled", WAIT).await;

    // Resend → the SAME row, generation 2, re-driving (still offline).
    let resend_id = retry_sync_package(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, id, None)
        .await
        .expect("resend the cancelled package");
    assert_eq!(resend_id, id, "resend-as-reset returns the SAME row");
    wait_until(|| outbound_generation(cdb, id) == 2 && !is_terminal_state(&outbound_state(cdb, id)), WAIT).await;
    assert_eq!(outbound_generation(cdb, id), 2, "the resend advanced the row to generation 2");

    // Kill the engine mid-resend (peer still offline → nothing has confirmed).
    engine1.shutdown().await;
    assert_ne!(outbound_state(cdb, id), "confirmed", "not confirmed at the kill point — the resend is genuinely mid-flight");

    // Bring the receiver online, THEN stand up a fresh engine over the SAME store: its
    // crash-resume re-drives the persisted gen-2 row and, with the peer now reachable,
    // delivers and confirms — unattended.
    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");
    let store2 = Arc::new(CatalogSyncStore::open(&capture_db).unwrap());
    let engine2 = Arc::new(SyncEngine::spawn(
        store2 as Arc<dyn SyncStore>,
        Arc::new(net.endpoint()),
        receiver_node,
    ));

    wait_until(|| outbound_state(cdb, id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;

    // SAME row id, generation UNCHANGED by the restart (still 2), and the receiver
    // holds exactly ONE inbound row.
    assert_eq!(count(cdb, "SELECT COUNT(*) FROM sync_outbound"), 1, "still exactly one outbound row (resend-as-reset, no new row)");
    assert_eq!(outbound_generation(cdb, id), 2, "the restart did NOT bump generation (only a resend does)");
    assert!(outbound_attempts(cdb, id) >= outbound_generation(cdb, id), "attempts >= generation");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM sync_inbound"), 1, "the receiver holds exactly one inbound row");
    assert_eq!(count(pdb, "SELECT COUNT(*) FROM files"), N as i64, "the peer ingested each frame exactly once");
    assert_eq!(landed_count(&designated), N, "all frames landed under the designated root");

    engine2.shutdown().await;
    receiver.shutdown().await;
}

/// B8 scenario 5 — **delete reclaims, scoped to its batch**. After TWO confirmed
/// transfers, `delete_transfer_history` (both directions) on batch 1 removes its
/// rows / files / events / history and its sender payload dir, dropping
/// `get_transfer_storage().packages_bytes`/`packages_count` — while batch 2 (a
/// foreign batch) survives untouched, the e2e namespace-scoping proof (the blob-tag
/// namespace survival is the B7 unit `in_flight_tag_list_and_release_honor_the_transfer_namespace`,
/// unreachable over loopback where the fetch seeds no in-flight tag).
#[tokio::test]
async fn delete_transfer_history_reclaims_batch_and_leaves_foreign_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().join("capture");
    let primary_dir = tmp.path().join("primary");
    let capture_files = capture_dir.join("files");
    std::fs::create_dir_all(&capture_files).unwrap();
    std::fs::create_dir_all(&primary_dir).unwrap();
    let capture_db = capture_dir.join("catalog.db");
    let primary_db = primary_dir.join("catalog.db");

    let capture_ctx = Arc::new(ServiceContext::new_for_tests(capture_db.clone()));
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();

    let designated = tmp.path().join("designated_incoming");
    designate_incoming(&primary_ctx, &designated);
    let incoming = incoming_resolver_for(&primary_ctx, primary_dir.join("incoming"));

    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.clone(),
        incoming,
        allow_all_peers(),
        Default::default(),
        Arc::new(InboundControl::new()),
        Arc::clone(&receiver_ep) as Arc<dyn SharingTransport>,
        Arc::new(NullEmitter),
    )
    .await
    .expect("spawn primary receiver");

    let (engine, sender, collab_sender, sync) =
        inject_sender_engine(&net, &capture_db, receiver_node, Duration::from_secs(30)).await;

    // 5 distinct frames: batch 1 = {0,1,2}, batch 2 = {3,4}. No dedup (distinct uuids).
    let mut frame_ids = Vec::with_capacity(5);
    for idx in 0..5 {
        frame_ids.push(insert_capture_frame(&capture_ctx, &capture_files, idx).0);
    }

    // ── Batch 1 → confirmed. ──
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids[0..3].to_vec(),
        Some("Batch One".to_string()),
        None,
        None,
    )
    .await
    .expect("batch 1 enqueue");
    let out_id_1 = latest_outbound_id(cdb);
    wait_until(|| outbound_state(cdb, out_id_1) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 3, WAIT).await;
    let wire_1 = latest_inbound_package_id(pdb).expect("batch 1 inbound wire");
    let in_id_1 = inbound_id_of(pdb, &wire_1).expect("batch 1 inbound row id");

    // ── Batch 2 → confirmed (the foreign batch that must survive). ──
    enqueue_sync_selection(
        &capture_ctx,
        &sender,
        Arc::clone(&collab_sender),
        &sync,
        ResolvedDest { node: receiver_node, endpoint_addr: None },
        frame_ids[3..5].to_vec(),
        Some("Batch Two".to_string()),
        None,
        None,
    )
    .await
    .expect("batch 2 enqueue");
    let out_id_2 = latest_outbound_id(cdb);
    wait_until(|| outbound_state(cdb, out_id_2) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == 5, WAIT).await;
    let wire_2 = latest_inbound_package_id(pdb).expect("batch 2 inbound wire");
    let in_id_2 = inbound_id_of(pdb, &wire_2).expect("batch 2 inbound row id");

    // Batch 1's delete key (== the package-dir basename == its batch_uuid, B1 seam).
    let pkg_ref_1 = outbound_package_ref(cdb, out_id_1);
    let batch_key_1 =
        Path::new(&pkg_ref_1).file_name().unwrap().to_string_lossy().to_string();
    let payload_dir_1 = PathBuf::from(&pkg_ref_1);

    // A foreign-namespace survivor seeded into the sender catalog: a `frames_set`
    // unrelated to any transfer — the delete must not touch it (DB-namespace scoping).
    cdb.conn()
        .execute("INSERT INTO frames_set (id, name) VALUES (777, 'Foreign Keep')", [])
        .unwrap();

    // Storage before: two manifest-only payload dirs on the sender.
    let storage_before = get_transfer_storage(&capture_ctx).expect("transfer storage before");
    assert!(storage_before.packages_count >= 2, "both confirmed transfers keep a (manifest-only) payload dir");
    assert!(storage_before.packages_bytes > 0, "the manifest-only payload dirs carry bytes");
    assert!(payload_dir_1.exists(), "batch 1's payload dir is present before the delete");

    // ── Delete batch 1 both directions. ──
    let del_sync = SyncRuntime::new();
    let deleted_sent = delete_transfer_history(&capture_ctx, &del_sync, Direction::Sent, batch_key_1.clone())
        .await
        .expect("delete sent batch 1");
    let deleted_recv = delete_transfer_history(&primary_ctx, &del_sync, Direction::Received, batch_key_1.clone())
        .await
        .expect("delete received batch 1");
    assert_eq!(deleted_sent.rows, 1, "one sent row deleted");
    assert_eq!(deleted_recv.rows, 1, "one received row deleted");
    assert!(deleted_sent.history >= 3, "batch 1's sent history removed");
    assert!(deleted_recv.history >= 3, "batch 1's received history removed");

    // Batch 1: rows / files / events / history all gone; sender payload dir removed;
    // receiver in-flight tags absent.
    assert_eq!(count(cdb, &format!("SELECT COUNT(*) FROM sync_outbound WHERE id = {out_id_1}")), 0, "sent row gone");
    assert_eq!(count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound WHERE id = {in_id_1}")), 0, "received row gone");
    assert_eq!(count(cdb, &format!("SELECT COUNT(*) FROM sync_outbound_files WHERE outbound_id = {out_id_1}")), 0, "sent files gone");
    assert_eq!(count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound_files WHERE inbound_id = {in_id_1}")), 0, "received files gone");
    assert_eq!(count(cdb, &format!("SELECT COUNT(*) FROM sync_events WHERE direction = 'sent' AND batch_key = '{out_id_1}'")), 0, "sent events gone");
    assert_eq!(count(pdb, &format!("SELECT COUNT(*) FROM sync_events WHERE direction = 'received' AND batch_key = '{in_id_1}'")), 0, "received events gone");
    assert_eq!(count(cdb, &format!("SELECT COUNT(*) FROM sync_history WHERE package_id = '{batch_key_1}'")), 0, "sent history gone");
    assert_eq!(count(pdb, &format!("SELECT COUNT(*) FROM sync_history WHERE package_id = '{batch_key_1}'")), 0, "received history gone");
    assert!(!payload_dir_1.exists(), "the sender payload dir is REMOVED by the reclaim");
    assert!(receiver_ep.list_in_flight_tags().await.unwrap().is_empty(), "no receiver in-flight tags remain");

    // Batch 2 (foreign) survives untouched.
    assert_eq!(count(cdb, &format!("SELECT COUNT(*) FROM sync_outbound WHERE id = {out_id_2}")), 1, "foreign batch's sent row survives");
    assert_eq!(count(pdb, &format!("SELECT COUNT(*) FROM sync_inbound WHERE id = {in_id_2}")), 1, "foreign batch's received row survives");
    assert!(
        count(pdb, "SELECT COUNT(*) FROM sync_history WHERE direction = 'received' AND outcome = 'ingested'") >= 2,
        "the foreign batch's received history survives"
    );
    assert!(PathBuf::from(outbound_package_ref(cdb, out_id_2)).exists(), "the foreign batch's payload dir survives");
    assert_eq!(count(cdb, "SELECT COUNT(*) FROM frames_set WHERE id = 777"), 1, "the foreign namespace survivor is untouched");

    // Storage dropped by exactly batch 1's payload dir.
    let storage_after = get_transfer_storage(&capture_ctx).expect("transfer storage after");
    assert_eq!(storage_after.packages_count, storage_before.packages_count - 1, "one payload dir reclaimed");
    assert!(storage_after.packages_bytes < storage_before.packages_bytes, "packages_bytes dropped by the reclaimed dir");

    engine.shutdown().await;
    receiver.shutdown().await;
}
