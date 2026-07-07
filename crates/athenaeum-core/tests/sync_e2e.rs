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

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use athenaeum_core::api::retention::{evaluate, AppRetentionConfig};
use athenaeum_core::api::sync::enqueue_sync_selection;
use athenaeum_core::db::{insert_file, insert_frame, Database};
use athenaeum_core::events::NullEmitter;
use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::models::{File, FileFormat, Frame, ImageType};
use athenaeum_core::services::ServiceContext;
use athenaeum_core::sharing::loopback::LoopbackNetwork;
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::{
    node_id_hex, CatalogSyncStore, RetentionPolicy, StartedSender, SyncEngine, SyncReceiver,
    SyncSenderRuntime, SyncStore,
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
    let capture_ctx = ServiceContext::new_for_tests(capture_db.clone());
    let primary_ctx = ServiceContext::new_for_tests(primary_db.clone());
    let cdb = capture_ctx.db.get().unwrap();
    let pdb = primary_ctx.db.get().unwrap();

    // ── Primary receiver over loopback ───────────────────────────────────────
    let net = LoopbackNetwork::new();
    let primary_store = Arc::new(CatalogSyncStore::open(&primary_db).unwrap());
    let receiver_ep = Arc::new(net.endpoint());
    let receiver_node = receiver_ep.node_id();
    let (_info, receiver) = SyncReceiver::spawn(
        Arc::clone(&primary_store),
        primary_dir.join("incoming"),
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
    let sender = SyncSenderRuntime::new();
    {
        let mut guard = sender.lock_inner().await;
        *guard = Some(StartedSender {
            engine: Arc::clone(&engine),
            origin_device: node_id_hex(&sender_node),
            peer: receiver_node,
        });
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
    let r1 = enqueue_sync_selection(&capture_ctx, &sender, frame_ids.clone(), None)
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

    // ── (2) Re-run the identical enqueue → dedupe-safe ───────────────────────
    let r2 = enqueue_sync_selection(&capture_ctx, &sender, frame_ids.clone(), None)
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
