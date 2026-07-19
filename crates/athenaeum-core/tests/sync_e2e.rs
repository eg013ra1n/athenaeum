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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use athenaeum_core::api::sync::{
    cancel_incoming_package, enqueue_sync_selection, retry_sync_package, ResolvedDest,
};
use athenaeum_core::api::retention::{evaluate, AppRetentionConfig};
use athenaeum_core::db::{insert_file, insert_frame, Database};
use athenaeum_core::events::{NullEmitter, ProgressEmitter};
use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::models::{File, FileFormat, Frame, ImageType};
use athenaeum_core::services::ServiceContext;
use athenaeum_core::sharing::loopback::{FaultPlan, LoopbackNetwork};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::{
    allow_all_peers, node_id_hex, CatalogDedupResponder, CatalogSyncStore, DedupResponder,
    InboundControl, RetentionPolicy, StartedSender, SyncConfig, SyncEngine, SyncEngineHandle,
    SyncReceiver, SyncRuntime, SyncSenderRuntime, SyncStore,
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
    let r1 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, frame_ids.clone(), None)
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
    let r2 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, frame_ids.clone(), None)
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
    let r1 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch1, None)
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
    let r2 = enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch2, None)
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
    enqueue_sync_selection(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, ResolvedDest { node: receiver_node, endpoint_addr: None }, batch3, None)
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

/// Acceptance §4/§6 — **two-sided cancel, then resend delivers**: a receiver
/// declining an inbound package drives the sender's row terminal (`cancelled`,
/// reason "cancelled by receiver"), lands no files, and leaves the receiver row
/// `cancelled` with a full set of `cancelled` receipts (the replay source). A
/// subsequent resend then delivers cleanly.
///
/// Deterministic mid-transfer cancel: the receiver endpoint's `abort_after_bytes`
/// fault is held armed so no payload fetch can ever COMPLETE (hence no ingest can
/// race the cancel), which also surfaces the wire `package_id` in `sync_inbound`.
/// Cancellation is then requested through the receiver's own
/// [`InboundControl::request_cancel`] — the exact call
/// [`cancel_incoming_package`] makes internally on a live transport — plus a real
/// [`cancel_incoming_package`] command invocation to exercise the command boundary
/// at the two-context level (see the test-2 note in the report: the loopback
/// receiver is not owned by a public `SyncRuntime`, so the command's live-control
/// leg is driven through the receiver's real control while its persisted-row leg is
/// exercised through the command).
#[tokio::test]
async fn receiver_cancel_terminates_sender_then_resend_delivers() {
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
    )
    .await
    .expect("enqueue selection");
    let old_id = latest_outbound_id(cdb);

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

    // Disarm the fault so the resend can fetch cleanly.
    receiver_ep.set_fault(FaultPlan::default());

    // Resend delivers via the sanctioned retry command (spec §1: "the row stays in
    // the sender's history and is retryable"; §4's retry payload-presence check
    // requires the payload to still be on disk after a receiver decline — the
    // all-cancelled-ack terminal epilogue mirrors `cancel_package` and keeps it,
    // same as a `Failed`/plain-`Cancelled` row).
    let resend_id =
        retry_sync_package(&capture_ctx, &sender, Arc::clone(&collab_sender), &sync, old_id, None)
            .await
            .expect("retry_sync_package resends a receiver-cancelled package");
    assert_ne!(resend_id, old_id, "the resend is a NEW durable row, not the cancelled one");

    wait_until(|| outbound_state(cdb, resend_id) == "confirmed", WAIT).await;
    wait_until(|| count(pdb, "SELECT COUNT(*) FROM frames") == N as i64, WAIT).await;
    assert_eq!(landed_count(&designated), N, "the resend delivers all frames to disk");
    // The declined row stays Cancelled across the resend — final, never resurrected.
    assert_eq!(
        inbound_row(pdb, &wire_pkg).map(|(s, _, _)| s).as_deref(),
        Some("cancelled"),
        "the original declined inbound row stays Cancelled"
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
