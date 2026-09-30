//! The live orchestrator end to end (Task 15): two in-process instances on
//! the fake hub, relay disabled, the hub's presence timings shortened (and
//! the beat with them). A publishes (`send`), B replicates (`send_receive`).

use std::time::{Duration, Instant};

use crate::api::collab_live::test_support as ts;
use crate::api::collab_live::{shutdown, status, LiveState};
use crate::db::collab_frames::LocalState;

async fn wait_status(ctx: &crate::services::ServiceContext, state: LiveState, within: Duration) {
    let deadline = Instant::now() + within;
    while status(ctx).state != state {
        assert!(
            Instant::now() < deadline,
            "live state {:?}, not {state:?}, after {within:?}",
            status(ctx).state
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// L3: a publish reaches B through the event stream alone — no version
/// poll, no per-frame holder lookup — and B's landings are reported
/// through its outbox.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_is_fetched_without_any_poll() {
    let w = ts::two_instances().await;
    let t0 = Instant::now();
    let uuids = w.a_publishes(3).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
    }
    assert!(t0.elapsed() < Duration::from_secs(20));
    assert_eq!(w.hub.requests_to("/me/project-versions").await, 0);
    assert_eq!(
        w.hub.requests_matching("/frames/", "/holders").await,
        0,
        "no per-frame holder lookups"
    );
    for u in &uuids {
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u));
        // B's landings were reported through the outbox: the hub counts B
        w.hub
            .wait_holder(ts::PID, u, &w.b.device(), Duration::from_secs(5))
            .await;
    }
}

/// A new holder (B landing A's publish) raises at least one
/// `collab-peers-changed` for the project on A's side — the live-connected
/// publisher — so its project page can refresh "online" without being
/// reopened. The exact throttle numbers (coalescing, window-end flush) are
/// covered by `PeerBurst`'s own pure unit tests in `runtime.rs`; this only
/// wires it to a real holder change end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_holder_raises_collab_peers_changed() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(1).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
    }
    ts::wait_until(
        "collab-peers-changed after a new holder",
        Duration::from_secs(10),
        || {
            !w.a.events
                .payloads(crate::api::collab_exchange::COLLAB_PEERS_CHANGED_EVENT)
                .is_empty()
        },
    )
    .await;
    for payload in
        w.a.events
            .payloads(crate::api::collab_exchange::COLLAB_PEERS_CHANGED_EVENT)
    {
        assert_eq!(payload["projectId"].as_str(), Some(ts::PID));
    }
}

/// Fix round 1: a member added to the project (a `ChangeKind::Members`
/// project event, no publish or land involved) also raises
/// `collab-peers-changed` — the Members tab must refresh even though nothing
/// was published or landed. Covers the `FeedEffect::MembersChanged` arm,
/// distinct from the holder-change coverage above.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_change_raises_collab_peers_changed() {
    let w = ts::two_instances().await;
    w.hub.add_member(ts::PID, "acc-c", "send", false);
    ts::wait_until(
        "collab-peers-changed after a member change",
        Duration::from_secs(10),
        || {
            !w.a.events
                .payloads(crate::api::collab_exchange::COLLAB_PEERS_CHANGED_EVENT)
                .is_empty()
        },
    )
    .await;
    for payload in
        w.a.events
            .payloads(crate::api::collab_exchange::COLLAB_PEERS_CHANGED_EVENT)
    {
        assert_eq!(payload["projectId"].as_str(), Some(ts::PID));
    }
}

/// P28: a clean exit leaves presence at once; a restart resumes from the
/// persisted holder map and claim digest — nothing is reported again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_exit_goes_offline_at_once_and_a_restart_resumes_without_rereporting() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(2).await;
    w.b.wait_all_held(Duration::from_secs(20)).await;
    for u in &uuids {
        w.hub
            .wait_holder(ts::PID, u, &w.b.device(), Duration::from_secs(5))
            .await;
    }
    let writes = w.hub.holder_writes();
    shutdown(&w.b.ctx).await;
    assert!(
        !w.hub.connected(ts::PID).contains(&w.b.device()),
        "DELETE /me/presence took effect"
    );
    assert_eq!(status(&w.b.ctx).state, LiveState::Off);
    w.b.restart_live().await;
    w.hub
        .wait_connected(ts::PID, &w.b.device(), Duration::from_secs(5))
        .await;
    wait_status(&w.b.ctx, LiveState::Live, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        w.hub.holder_writes(),
        writes,
        "digests match: zero re-reports"
    );
}

/// §7.4, I1, I6: a new version mid-download cancels the old fetch and the
/// new version lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_version_mid_download_cancels_and_lands_the_new_one() {
    let w = ts::two_instances_throttled(4 * 1024 * 1024).await; // A uploads at 4 MB/s
    let uuids = w.a_publishes_big(1, 32 * 1024 * 1024).await;
    w.b.wait_fetch_started(&uuids[0], Duration::from_secs(10))
        .await;
    w.a_republishes_changed(&uuids[0]).await;
    w.b.wait_state_version(&uuids[0], LocalState::Held, 2, Duration::from_secs(60))
        .await;
    assert_eq!(w.b.file_bytes(&uuids[0]), w.a.file_bytes(&uuids[0]));
}

/// L10, P26: Sync now reconnects at once and clears every back-off — here
/// a stream that the hub refused (an outdated build waits an hour).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_now_reconnects_and_clears_back_offs() {
    let w = ts::two_instances().await;
    w.hub.set_api_outdated(true);
    w.hub.kill_streams();
    wait_status(&w.b.ctx, LiveState::Outdated, Duration::from_secs(5)).await;
    w.hub.set_api_outdated(false);
    ts::sync_now_serial(&w.b.ctx).await;
    w.hub
        .wait_connected(ts::PID, &w.b.device(), Duration::from_secs(3))
        .await;
    wait_status(&w.b.ctx, LiveState::Live, Duration::from_secs(1)).await;
}

/// L1, §8: a personal transfer waits at most for the collab frames already
/// in flight. The bound is DERIVED (ledger [T13, T15, T18]): every unit in
/// flight finishes only its frame, so the wait is at most the bytes of the
/// units in flight at the upload cap, plus a margin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_personal_transfer_waits_at_most_one_frame() {
    const RATE: u64 = 8 * 1024 * 1024; // A's upload cap
    const FRAME: usize = 16 * 1024 * 1024;
    const STREAMS: usize = 2; // collab units in flight at once
    let w = ts::two_instances_throttled(RATE).await;
    w.b.set_receive_limit(1).await; // one lane: collab holds it
    crate::api::collab_live::set_receive_streams(&w.b.ctx, STREAMS);
    // M3 (fix round 1): eight frames — without the yield the lane would be
    // held for all of them (8 × 2 s), far past the bound.
    w.a_publishes_big(8, FRAME).await;
    w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
    let admitted = w.b.personal_acquire_timed().await; // time until a personal permit is granted
    let in_flight = (STREAMS as u64) * FRAME as u64;
    let bound = Duration::from_secs_f64(in_flight as f64 / RATE as f64) + Duration::from_secs(3);
    assert!(
        admitted < bound,
        "waited {admitted:?}: more than {STREAMS} frames of {FRAME} bytes at {RATE} B/s (+3 s) = {bound:?}"
    );
}

/// Task 15 R1 (the T9 carry): a manifest apply re-derives the replication
/// scope — a frame outside B's policy arrives `idle`, never fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_outside_the_policy_arrives_idle_and_is_never_fetched() {
    let w = ts::two_instances().await;
    crate::api::collab_exchange::set_collab_policy(
        &w.b.ctx,
        ts::PID,
        crate::api::collab_exchange::ReplicationPolicy {
            filters: vec!["Ha".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let uuids = w.a_publishes(1).await; // filter L
    w.b.wait_state(&uuids[0], LocalState::Idle, Duration::from_secs(10))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let row = w.b.row(&uuids[0]).unwrap();
    assert_eq!(row.local_state, LocalState::Idle);
    assert!(row.landed_path.is_none(), "never fetched");
}

/// P24, C10 (the wave-2 `identical_content_second_frame_is_linked_not_fetched`
/// on the live executor): a second frame whose content B already holds is
/// linked from the landed file — its bytes never cross the wire again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_content_is_linked_not_fetched() {
    const SIZE: usize = 256 * 1024;
    let w = ts::two_instances().await;
    let bytes = ts::FetchRig::pattern("twin", 1, SIZE);
    let first = w.a_publishes_bytes(&bytes).await;
    w.b.wait_state(&first, LocalState::Held, Duration::from_secs(20))
        .await;
    let sent = |n: &crate::sharing::iroh::node::SharedIrohNode| {
        let c = n.counters_snapshot_for_test();
        c.send_direct_bytes.saturating_add(c.send_relay_bytes)
    };
    let before = sent(&w.a.node);
    let second = w.a_publishes_bytes(&bytes).await;
    w.b.wait_state(&second, LocalState::Held, Duration::from_secs(20))
        .await;
    let moved = sent(&w.a.node).saturating_sub(before);
    assert!(
        moved < (SIZE / 2) as u64,
        "A sent {moved} bytes: the second frame was fetched, not linked"
    );
    assert_eq!(w.b.file_bytes(&second), bytes);
    assert_ne!(
        w.b.row(&first).unwrap().landed_path,
        w.b.row(&second).unwrap().landed_path
    );
}

fn arm(ctx: &std::sync::Arc<crate::services::ServiceContext>) {
    crate::api::collab_live::spawn_with(
        std::sync::Arc::clone(ctx),
        crate::api::collab_live::GateSource::Fixed(std::sync::Arc::new(
            crate::sync::receiver::InboundControl::new(),
        )),
        None,
        crate::api::collab_live::LiveConfig {
            beat: ts::LIVE_BEAT,
            ready_poll: Duration::from_millis(100),
            ..Default::default()
        },
    )
    .expect("armed");
}

/// The wave-2 `the_pass_is_a_no_op_signed_out_or_without_a_collaboration_root`
/// on the live exchange: without a Collaboration folder nothing starts (no
/// stream, no session); signed out, the session waits in `signedOut`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_runs_without_a_collaboration_folder_or_signed_out() {
    let (t, ctx, hub) = ts::signed_in_rig_no_root().await;
    let ctx = std::sync::Arc::new(ctx);
    arm(&ctx);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let s = status(&ctx);
    assert_eq!(s.storage, crate::api::collab_live::StorageStateView::NotSet);
    assert_ne!(s.state, LiveState::Live);
    assert!(hub.connected(ts::PID).is_empty(), "no event stream opened");
    // A folder designated later is picked up (the wave-2
    // `a_root_that_appears_after_startup_is_mounted_by_maintenance`).
    let requested = t.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        &ctx,
        requested.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await
    .unwrap();
    wait_status(&ctx, LiveState::Live, Duration::from_secs(5)).await;
    assert_eq!(
        status(&ctx).storage,
        crate::api::collab_live::StorageStateView::Available
    );
    shutdown(&ctx).await;

    let (_t2, ctx2, hub2) = ts::signed_in_rig().await;
    let ctx2 = std::sync::Arc::new(ctx2);
    crate::api::account::sign_out(&ctx2).await.unwrap();
    arm(&ctx2);
    wait_status(&ctx2, LiveState::SignedOut, Duration::from_secs(5)).await;
    assert!(hub2.connected(ts::PID).is_empty(), "no event stream opened");
    shutdown(&ctx2).await;
}

/// The race behind `nothing_runs_without_a_collaboration_folder_or_signed_out`
/// (2 of 60 under load): the first designation of a folder and the live
/// runtime's first start — its lazy mount runs the same storage-marker check
/// — both found no marker and each minted its own store id, and the catalog
/// ended up recording one the disk does not carry: a `MarkerMismatch` that
/// outlives a restart and refuses a re-designation of the folder. The
/// designation is paused just before it writes its freshly minted marker
/// and the lazy mount's check then runs: it must wait for the designation
/// and adopt ITS marker, never mint a second one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_designation_and_a_concurrent_lazy_mount_adopt_one_store_id() {
    use crate::collab::storage::marker::{read_marker, test_hooks};
    let (t, ctx, _hub) = ts::signed_in_rig_no_root().await;
    let ctx = std::sync::Arc::new(ctx);
    let requested = t.path().join("Collab");
    std::fs::create_dir_all(&requested).unwrap();
    let armed = test_hooks::arm(&requested);

    let designation = tokio::spawn({
        let ctx = std::sync::Arc::clone(&ctx);
        let path = requested.to_string_lossy().to_string();
        async move {
            crate::api::scan_roots::set_collaboration_dir(
                &ctx,
                path,
                &crate::api::PathPolicy::AllowAll,
            )
            .await
        }
    });
    tokio::task::block_in_place(|| armed.paused.recv_timeout(Duration::from_secs(10)))
        .expect("the designation reaches its marker write");

    // The folder is designated (the row is written), its marker not yet:
    // what the runtime's first start finds.
    let root = crate::api::scan_roots::get_collaboration_dir(&ctx)
        .unwrap()
        .expect("designated");
    let lazy = tokio::spawn({
        let ctx = std::sync::Arc::clone(&ctx);
        let root = std::path::PathBuf::from(&root);
        async move { crate::api::collab_exchange::check_storage_marker(&ctx, &root).await }
    });
    // Either the lazy check waits for the designation's adoption (the
    // protocol), or it runs to completion while the designation is paused
    // (the race).
    let deadline = Instant::now() + Duration::from_secs(10);
    let ran_during_the_designation = loop {
        if armed.waiting.try_recv().is_ok() {
            break false;
        }
        if lazy.is_finished() {
            break true;
        }
        assert!(
            Instant::now() < deadline,
            "the lazy check neither waited nor finished"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    armed.release();
    designation.await.unwrap().expect("designate");
    let lazy = lazy.await.unwrap().expect("lazy check");

    let on_disk = read_marker(std::path::Path::new(&root))
        .unwrap()
        .expect("a marker on disk");
    let recorded = {
        let db = crate::api::db(&ctx).unwrap();
        crate::db::collab_live::recorded_store_marker(&db.conn()).unwrap()
    };
    assert_eq!(
        lazy.pending.as_ref(),
        Some(&on_disk),
        "the lazy mount adopted a store id the disk does not carry"
    );
    assert_eq!(
        recorded.as_ref(),
        Some(&on_disk),
        "the designation recorded the disk's marker"
    );
    assert!(
        !ran_during_the_designation,
        "the lazy check ran while the designation was minting"
    );
}

// ── Task 15 fix round 1 ─────────────────────────────────────────────────

/// Priority setup shared by the C1 tests: A uploads at 8 MB/s, B has one
/// receive lane (collab holds it) and two collab streams, A publishes eight
/// 16 MiB frames and B's fetch is under way. Returns the derived bound a
/// personal transfer may wait (the units in flight at the cap, plus 3 s).
async fn lane_held_by_collab(w: &ts::World) -> Duration {
    const RATE: u64 = 8 * 1024 * 1024;
    const FRAME: usize = 16 * 1024 * 1024;
    const STREAMS: usize = 2;
    w.b.set_receive_limit(1).await;
    crate::api::collab_live::set_receive_streams(&w.b.ctx, STREAMS);
    w.a_publishes_big(8, FRAME).await;
    w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
    let in_flight = (STREAMS as u64) * FRAME as u64;
    Duration::from_secs_f64(in_flight as f64 / RATE as f64) + Duration::from_secs(3)
}

/// The fake hub fails every holder catch-up (the feed worker retries
/// forever, `Background`), and a resync makes B ask for one. Returns once
/// B's feed worker is in that retry.
async fn feed_stuck_in_a_retry(w: &ts::World) {
    w.hub.set_failing("/holders", true);
    w.hub.set_failing("/holders/snapshot", true);
    let before = w.hub.requests_matching("/projects/p1/holders", "").await;
    w.hub.send_resync(ts::PID, "holders");
    let deadline = Instant::now() + Duration::from_secs(10);
    // both instances' first attempts, then at least one retry
    while w.hub.requests_matching("/projects/p1/holders", "").await < before + 3 {
        assert!(Instant::now() < deadline, "no holder catch-up retried");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// C1 (a): hub HTTP never holds the lane — a feed stuck in its retry, a
/// personal transfer is still admitted within the derived bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_personal_transfer_is_admitted_while_the_feed_is_stuck_in_a_retry() {
    let w = ts::two_instances_throttled(8 * 1024 * 1024).await;
    let bound = lane_held_by_collab(&w).await;
    feed_stuck_in_a_retry(&w).await;
    let admitted = w.b.personal_acquire_timed().await;
    assert!(admitted < bound, "waited {admitted:?} (bound {bound:?})");
}

/// C1 (b): stop completes cleanly within its bound — never by the timeout
/// abort — while the feed worker is stuck in a retry, and presence is left.
/// The runtime's own stop takes at most `LEAVE_TIMEOUT + SESSION_MARGIN`
/// (its waits run at once); `STOP_BOUND` adds `STOP_DISPATCH_MARGIN` for
/// the command to reach the loop (Task 18, flake-5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_completes_while_the_feed_worker_is_stuck_in_a_retry() {
    let w = ts::two_instances().await;
    feed_stuck_in_a_retry(&w).await;
    let t0 = Instant::now();
    shutdown(&w.b.ctx).await;
    let took = t0.elapsed();
    eprintln!("stop took {took:?}");
    assert!(
        took < crate::api::collab_live::runtime::STOP_BOUND,
        "stop took {took:?}: it hit the bound (the loop never read the stop)"
    );
    assert_eq!(status(&w.b.ctx).state, LiveState::Off);
    assert!(
        !w.hub.connected(ts::PID).contains(&w.b.device()),
        "DELETE /me/presence was sent"
    );
}

/// C1 (c): a slow sweep (Sync now's, delayed 30 s by the test hook) never
/// delays the lane's release at a yield.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_sweep_never_delays_a_yields_lane_release() {
    let mut cfg = ts::live_config();
    cfg.timings.sweep_delay = Duration::from_secs(30);
    let w = ts::two_instances_with(Some(8 * 1024 * 1024), cfg).await;
    let bound = lane_held_by_collab(&w).await;
    ts::sync_now_serial(&w.b.ctx).await; // queues the (slow) sweep
    tokio::time::sleep(Duration::from_millis(200)).await;
    let admitted = w.b.personal_acquire_timed().await;
    assert!(admitted < bound, "waited {admitted:?} (bound {bound:?})");
}

/// I2: a landing task that panics is reaped as that fetch's failure — the
/// core frees its slot (one stream: the next frame still lands) and a
/// yield still gets the lane back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_landing_frees_its_slot_and_the_lane() {
    let w = ts::two_instances().await;
    w.b.set_receive_limit(1).await;
    crate::api::collab_live::set_receive_streams(&w.b.ctx, 1);
    crate::api::collab_live::executor::test_hooks::panic_next_landing(&w.b.root);
    let uuids = w.a_publishes(2).await;
    // Both land in the end: the panicked one after its back-off, the other
    // through the slot the panic freed.
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
    }
    assert!(
        !crate::api::collab_live::executor::test_hooks::landing_panic_pending(&w.b.root),
        "a landing did panic"
    );
    // The lane is free again: a personal transfer is admitted at once.
    let admitted = w.b.personal_acquire_timed().await;
    assert!(admitted < Duration::from_secs(3), "waited {admitted:?}");
}

async fn wait_rows(
    ctx: &crate::services::ServiceContext,
    what: &str,
    within: Duration,
    pred: impl Fn(&[crate::db::collab_frames::LocalFrameRow]) -> bool,
) {
    let deadline = Instant::now() + within;
    loop {
        let rows = crate::db::collab_frames::list_for_project(
            &crate::api::db(ctx).unwrap().conn(),
            ts::PID,
        )
        .unwrap();
        if pred(&rows) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: not within {within:?}: {rows:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// I1 + owner rule A: re-designating the Collaboration folder mid-run
/// restarts the runtime on the new store; the replicas under the old folder
/// leave `held` through the L4 deletion path and a batch of ≤ 10 is
/// re-fetched into the new folder. Clearing the folder stops the exchange:
/// nothing is fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redesignated_folder_refetches_into_the_new_root_and_clearing_stops_it() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(3).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
    }
    let old_root = w.b.root.clone();
    let other = tempfile::tempdir().unwrap();
    let new_root = ts::redesignate(&w.b.ctx, &other.path().join("Collab2")).await;
    wait_rows(
        &w.b.ctx,
        "re-fetched under the new root",
        Duration::from_secs(30),
        |rows| {
            rows.iter()
                .filter(|r| uuids.contains(&r.frame_uuid))
                .all(|r| {
                    r.local_state == LocalState::Held
                        && r.landed_path
                            .as_deref()
                            .is_some_and(|p| std::path::Path::new(p).starts_with(&new_root))
                })
        },
    )
    .await;
    wait_status(&w.b.ctx, LiveState::Live, Duration::from_secs(5)).await;
    for u in &uuids {
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u));
    }
    // the old files are the user's: left where they were
    assert!(walk_files(&old_root) >= uuids.len());

    // Cleared: the runtime stops at its store's unmount; nothing is fetched.
    crate::api::scan_roots::clear_collaboration_dir(&w.b.ctx)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while status(&w.b.ctx).storage != crate::api::collab_live::StorageStateView::NotSet {
        assert!(Instant::now() < deadline, "status {:?}", status(&w.b.ctx));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_ne!(status(&w.b.ctx).state, LiveState::Live);
    let late = w.a_publishes(1).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        w.b.row(&late[0])
            .is_none_or(|r| r.local_state != LocalState::Held),
        "nothing fetched without a Collaboration folder"
    );
}

/// Owner rule A, the mass case: more than ten replicas outside the new
/// root in the window raise the one deletion choice instead of a re-fetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redesignation_over_ten_frames_raises_the_deletion_choice() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(11).await;
    // every named frame, not "every row B has seen so far": a batch that
    // is re-designated before the rest arrive rules ≤ 10
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
    }
    let other = tempfile::tempdir().unwrap();
    ts::redesignate(&w.b.ctx, &other.path().join("Collab2")).await;
    wait_rows(
        &w.b.ctx,
        "every replica awaits the choice",
        Duration::from_secs(30),
        |rows| {
            rows.iter()
                .filter(|r| uuids.contains(&r.frame_uuid))
                .all(|r| r.local_state == LocalState::AwaitingChoice)
        },
    )
    .await;
}

fn walk_files(root: &std::path::Path) -> usize {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".athenaeum"))
        .count()
}

/// Fix round 2 (I-1): the session id comes from the pump, never through
/// the runtime's event queue. With the feed worker stuck and more than
/// `EVENT_QUEUE` events waiting on it (the stream back-pressured), a
/// reconnect's `hello` still names the new session at once: the beat
/// resumes and the hub keeps the device online past its silence rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_beats_at_once_while_the_feed_worker_is_stuck() {
    let w = ts::two_instances().await;
    feed_stuck_in_a_retry(&w).await;
    // far more events than the worker's back-pressure lets through
    for _ in 0..(2 * super::runtime::EVENT_QUEUE + 64) {
        w.hub.send_versions();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    w.hub.kill_streams(); // a hub restart: every session gone
    let t0 = Instant::now();
    w.hub
        .wait_connected(
            ts::PID,
            &w.b.device(),
            ts::LIVE_BEAT * 4 + Duration::from_secs(2),
        )
        .await;
    let back = t0.elapsed();
    // Well past the fake hub's silence rule (1 s): only beats keep it.
    tokio::time::sleep(ts::LIVE_TIMINGS.silence * 3).await;
    assert!(
        w.hub.connected(ts::PID).contains(&w.b.device()),
        "beats keep the device online (reconnected after {back:?})"
    );
}

/// Fix round 2 (M-1): a run that panics every time fails its fetches —
/// they back off — instead of re-queuing them at once into a spin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_that_keeps_panicking_backs_its_fetches_off() {
    let w = ts::two_instances().await;
    crate::api::collab_live::executor::test_hooks::panic_every_run(&w.b.root);
    let uuids = w.a_publishes(1).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let panics = crate::api::collab_live::executor::test_hooks::stop_run_panics(&w.b.root);
    // full-jitter back-off from 1 s: a handful in 3 s, never a spin
    assert!((1..=12).contains(&panics), "{panics} runs panicked in 3 s");
    w.b.wait_state(&uuids[0], LocalState::Held, Duration::from_secs(90))
        .await;
}

/// Task 16: the command surface reads the live holder map and presence —
/// B's frames list counts A (the publisher) as an online holder of each
/// frame, and as an offline one once A leaves; the last-copy warning is
/// raised (one other holder is fewer than two).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_frames_list_counts_live_holders_and_a_departure() {
    use crate::api::collab_live::surface;
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(2).await;
    w.b.wait_all_held(Duration::from_secs(20)).await;
    let counts = |ctx: &crate::services::ServiceContext| {
        surface::list_collab_frames(ctx, ts::PID, false)
            .unwrap()
            .into_iter()
            .filter(|f| uuids.contains(&f.frame_uuid))
            .map(|f| (f.local_state, f.holders_online, f.holders_total))
            .collect::<Vec<_>>()
    };
    let wait_for = |want: (usize, usize), what: &'static str| {
        let ctx = std::sync::Arc::clone(&w.b.ctx);
        let counts = &counts;
        async move {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let got = counts(&ctx);
                if got.len() == 2
                    && got
                        .iter()
                        .all(|c| c.0 == surface::LocalStateView::Held && (c.1, c.2) == want)
                {
                    return;
                }
                assert!(Instant::now() < deadline, "{what}: {got:?}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    wait_for((1, 1), "A online and holding").await;
    let preview =
        surface::preview_collab_stop_keeping(&w.b.ctx, ts::PID, vec![uuids[0].clone()]).unwrap();
    assert_eq!(
        (
            preview[0].holders_online,
            preview[0].holders_total,
            preview[0].at_risk
        ),
        (1, 1, true)
    );
    shutdown(&w.a.ctx).await;
    wait_for((0, 1), "A left, still a holder").await;
}

/// Amendment A6, the bug it fixes: a SECOND device of the account that
/// published the frames receives them as replicas under its own policy —
/// fetched, held, claimed on the hub, and served (a member of another
/// account paired with it alone gets them from it after the publishing
/// device went offline). It is not the publishing device ("Obs PC" is): an
/// announce from it is refused naming that device, until it switches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_device_of_the_account_replicates_serves_and_publishes_only_after_a_switch() {
    let w = ts::two_devices_of_one_account().await;
    let uuids = w.a_publishes(2).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
        let row = w.b.row(u).unwrap();
        assert_eq!(
            row.origin,
            crate::db::collab_frames::FrameOrigin::Replica,
            "another device's frame is a replica here, never own"
        );
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u));
        w.hub
            .wait_holder(ts::PID, u, &w.b.device(), Duration::from_secs(5))
            .await;
        assert_eq!(
            w.a.row(u).unwrap().origin,
            crate::db::collab_frames::FrameOrigin::Own,
            "still own on the device that published it"
        );
    }

    // Served: the publishing device goes offline; `c` (paired with `b`
    // only) gets every frame from `b`.
    shutdown(&w.a.ctx).await;
    w.start_c().await;
    for u in &uuids {
        w.c.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
        assert_eq!(w.c.file_bytes(u), w.b.file_bytes(u));
    }

    // Publishing: `b` is not the bound device — shown so, refused so.
    let card = crate::api::collab::refresh_projects(&w.b.ctx)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.project_id == ts::PID)
        .unwrap();
    assert!(!card.publishing_here);
    assert_eq!(
        card.publishing_device.map(|d| (d.device_id, d.name)),
        Some((w.a.device(), Some("Obs PC".to_string())))
    );
    let frame = |uuid: &str| crate::collab::hub_client::FrameInWire {
        frame_uuid: uuid.into(),
        file_name: format!("{uuid}.fits"),
        blake3: "a".repeat(64),
        byte_size: 10,
        xxh3: "0".repeat(16),
        filter_raw: "L".into(),
        filter_canonical: "L".into(),
        channel: "mono".into(),
        exptime_sec: 300.0,
        date_obs: None,
        gate_version: 0,
        meta: serde_json::json!({}),
    };
    let client = crate::collab::hub_client::CollabClient::new(w.hub.uri()).unwrap();
    let refused = client
        .announce_frames("tok-a2", ts::PID, &[frame("b1")])
        .await;
    assert!(
        matches!(&refused, Err(crate::account::AccountClientError::PublishingDevice { device_id, .. }) if *device_id == w.a.device()),
        "{refused:?}"
    );

    // "Publish from this device" on `b`: it publishes, `a` is refused.
    let card = crate::api::collab::set_collab_publishing_device(&w.b.ctx, ts::PID)
        .await
        .unwrap();
    assert!(card.publishing_here);
    client
        .announce_frames("tok-a2", ts::PID, &[frame("b1")])
        .await
        .expect("the bound device announces");
    assert!(matches!(
        client
            .announce_frames("tok-a", ts::PID, &[frame("a9")])
            .await,
        Err(crate::account::AccountClientError::PublishingDevice { .. })
    ));
}

// ── final fix, group A: liveness ─────────────────────────────────────────

/// Final fix A-I1: "Publish from this device" never waits on a hub retry.
/// The switch owes a re-announce of `b`'s own frame the hub lost while `a`
/// was bound; every announce answers 500 — the command still returns at
/// once, the feed worker keeps retrying (`Background`), and the frame is
/// back on the hub once the hub is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publishing_switch_returns_at_once_while_the_reannounce_retries() {
    let w = ts::two_devices_of_one_account().await;
    let uuid = w.b_publishes_own().await;
    w.hub.set_publishing_device(ts::PID, "acc-a", &w.a.device());
    crate::api::collab::refresh_projects(&w.b.ctx)
        .await
        .unwrap();
    w.hub.forget_frames(ts::PID, &[uuid.as_str()]);
    w.hub.set_failing("/projects/p1/frames", true);
    let announces = || w.hub.requests_to("/projects/p1/frames");
    let before = announces().await;
    let t0 = Instant::now();
    let card = tokio::time::timeout(
        Duration::from_secs(5),
        crate::api::collab::set_collab_publishing_device(&w.b.ctx, ts::PID),
    )
    .await
    .expect("the switch never waits on the re-announce's retries")
    .unwrap();
    assert!(card.publishing_here);
    assert!(
        t0.elapsed() < Duration::from_secs(3),
        "took {:?}",
        t0.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while announces().await < before + 2 {
        assert!(
            Instant::now() < deadline,
            "the feed worker retries the re-announce"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    w.hub.set_failing("/projects/p1/frames", false);
    ts::sync_now_serial(&w.b.ctx).await; // the retry's back-off restarts
    let deadline = Instant::now() + Duration::from_secs(20);
    while w
        .hub
        .frame(ts::PID, &uuid)
        .and_then(|f| f.publisher_device_id)
        .as_deref()
        != Some(w.b.device().as_str())
    {
        assert!(Instant::now() < deadline, "the frame is re-announced by b");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Final fix A-I2: a catalog write never runs on the runtime's loop. With
/// another connection holding the write lock (past the 5 s busy timeout)
/// while a replication-scope re-derive is due, the loop still serves a
/// command at once — a new fetch starts within a second — and the
/// re-derive that failed under the lock is retried, never dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_write_lock_never_stalls_the_loop_and_a_failed_rederive_is_retried() {
    use crate::api::collab_live::executor::test_hooks::sightings;
    let w = ts::two_instances_throttled(1024 * 1024).await;
    crate::api::collab_live::set_receive_streams(&w.b.ctx, 1);
    let uuids = w.a_publishes_big(4, 4 * 1024 * 1024).await;
    w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
    // Every manifest row applied (a later apply would rewrite `state`).
    wait_rows(
        &w.b.ctx,
        "every frame cached",
        Duration::from_secs(10),
        |rows| {
            uuids
                .iter()
                .all(|u| rows.iter().any(|r| &r.frame_uuid == u))
        },
    )
    .await;
    let starts = || {
        sightings(&w.b.node_key())
            .iter()
            .filter(|s| s.start)
            .count()
    };
    // The last frame leaves the scope at the next re-derive.
    let dropped = uuids.last().unwrap().clone();
    let path = {
        let d = crate::api::db(&w.b.ctx).unwrap();
        d.conn()
            .execute(
                "UPDATE project_frames_local SET state = 'rejected'
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                rusqlite::params![ts::PID, dropped],
            )
            .unwrap();
        d.path().to_path_buf()
    };
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let locked_at = Instant::now();
    crate::api::collab_live::runtime::mark_policy_dirty_for_test(&w.b.ctx, ts::PID);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = starts();
    let t0 = Instant::now();
    crate::api::collab_live::set_receive_streams(&w.b.ctx, 2);
    while starts() == before {
        assert!(
            t0.elapsed() < Duration::from_secs(4),
            "the loop served no command while the write lock was held"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let served = t0.elapsed();
    assert!(
        served < Duration::from_secs(1),
        "a new fetch started after {served:?}"
    );
    // Past the busy timeout: the re-derive under the lock failed.
    tokio::time::sleep(Duration::from_secs(6).saturating_sub(locked_at.elapsed())).await;
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    w.b.wait_state(&dropped, LocalState::Idle, Duration::from_secs(10))
        .await;
}

/// Final fix A-I3: a worker that dies takes the runtime down with it and
/// `supervise` builds a new one — hub events apply again and frames land,
/// for the feed worker and for the storage task alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_worker_restarts_the_runtime() {
    use crate::api::collab_live::runtime::{panic_worker_for_test, runtime_starts, TestWorker};
    let w = ts::two_instances().await;
    assert_eq!(runtime_starts(&w.b.ctx), 1);
    for (i, worker) in [TestWorker::Feed, TestWorker::Storage]
        .into_iter()
        .enumerate()
    {
        panic_worker_for_test(&w.b.ctx, worker);
        let deadline = Instant::now() + Duration::from_secs(10);
        while runtime_starts(&w.b.ctx) < i + 2 {
            assert!(
                Instant::now() < deadline,
                "{worker:?}: the runtime never restarted"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        w.hub
            .wait_connected(ts::PID, &w.b.device(), Duration::from_secs(10))
            .await;
        let uuid = w.a_publishes(1).await.remove(0);
        w.b.wait_state(&uuid, LocalState::Held, Duration::from_secs(20))
            .await;
    }
}

/// Final fix A-M1: a runtime that stops replaces the serve oracle only
/// while it is still its own — one installed meanwhile (a runtime armed
/// again during the stop window) survives the stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopping_runtime_never_replaces_its_successors_oracle() {
    let w = ts::two_instances().await;
    let successor = crate::api::collab_live::serve_oracle::catalog_oracle(&w.b.ctx).unwrap();
    w.b.node
        .set_collab_serve_oracle(Some(std::sync::Arc::clone(&successor)));
    shutdown(&w.b.ctx).await;
    let now =
        w.b.node
            .collab_serve_oracle()
            .expect("an oracle is installed");
    assert!(
        std::ptr::addr_eq(
            std::sync::Arc::as_ptr(&now),
            std::sync::Arc::as_ptr(&successor)
        ),
        "the successor's oracle survived the stop"
    );
}

/// Final fix A-M2: a stop is observed while the armed task waits for its
/// node (the node lock held elsewhere) and while the session opens a
/// stream the hub never answers — both stop at once, never by the bound's
/// abort.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_is_observed_while_starting_and_while_the_stream_opens() {
    let w = ts::two_instances().await;
    shutdown(&w.b.ctx).await;
    {
        let _node = w.b.ctx.iroh_node.lock().await; // `ready` waits for it
        w.b.start_live();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let t0 = Instant::now();
        shutdown(&w.b.ctx).await;
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "stop took {:?}",
            t0.elapsed()
        );
    }
    // A hub that accepts the connection and never answers.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((s, _)) = silent.accept().await {
            open.push(s);
        }
    });
    crate::api::collab_exchange::test_support::wire_hub(&w.b.ctx, &url, "tok-b");
    w.b.start_live();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t0 = Instant::now();
    shutdown(&w.b.ctx).await;
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "stop took {:?}",
        t0.elapsed()
    );
    held.abort();
}

/// Final fix A-M4: Sync now while the armed runtime waits to start (no
/// Collaboration folder) is queued and applied when it starts — its digest
/// check reaches the hub — never a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_now_before_the_runtime_runs_is_applied_when_it_starts() {
    let w = ts::two_instances().await;
    crate::api::scan_roots::clear_collaboration_dir(&w.b.ctx)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while status(&w.b.ctx).storage != crate::api::collab_live::StorageStateView::NotSet {
        assert!(Instant::now() < deadline, "status {:?}", status(&w.b.ctx));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let reports = || w.hub.requests_to("/projects/p1/holders/self");
    let before = reports().await;
    ts::sync_now_serial(&w.b.ctx).await;
    let other = tempfile::tempdir().unwrap();
    let dir = other.path().join("Collab2");
    std::fs::create_dir_all(&dir).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        &w.b.ctx,
        dir.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await
    .unwrap();
    wait_status(&w.b.ctx, LiveState::Live, Duration::from_secs(10)).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while reports().await == before {
        assert!(
            Instant::now() < deadline,
            "the queued Sync now's digest check never reached the hub"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── final fix, group B: a moved or copied Collaboration folder ──────────

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    for e in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(Result::ok)
    {
        let rel = e.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else if e.file_type().is_file() {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// Every frame file under `root` outside `.athenaeum`.
fn frame_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".athenaeum"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn landed_total(i: &ts::Instance) -> u64 {
    i.events
        .payloads(crate::api::collab_exchange::COLLAB_FRAMES_LANDED_EVENT)
        .iter()
        .map(|p| p["landed"].as_u64().unwrap_or(0))
        .sum()
}

/// B's folder moved or copied elsewhere (`copy`: the old one stays), with
/// its store, and the new place designated — every replica must be
/// re-adopted where the new folder holds it (final fix B-I1).
async fn readopted_after_relocation(copy: bool) {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(12).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
    }
    let old_root = w.b.root.clone();
    let landed = landed_total(&w.b);
    let choices =
        w.b.events
            .payloads(crate::api::collab_live::COLLAB_DELETION_CHOICE_EVENT)
            .len();
    crate::api::scan_roots::clear_collaboration_dir(&w.b.ctx)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while status(&w.b.ctx).storage != crate::api::collab_live::StorageStateView::NotSet {
        assert!(Instant::now() < deadline, "status {:?}", status(&w.b.ctx));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let other = tempfile::tempdir().unwrap();
    let new_dir = other.path().join("Collab-elsewhere");
    if copy {
        copy_tree(&old_root, &new_dir);
    } else {
        std::fs::rename(&old_root, &new_dir).unwrap();
    }
    // A moved store's entries still name the old paths: the GC (on for the
    // store opened now) drops them once the frames re-seeded at the new ones.
    crate::sharing::iroh::node::test_gc::arm(Some(std::sync::Arc::clone(&gate)));
    crate::api::scan_roots::set_collaboration_dir(
        &w.b.ctx,
        new_dir.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await
    .unwrap();
    crate::sharing::iroh::node::test_gc::arm(None);
    let new_root = ts::collab_root(&w.b.ctx);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut next_sync = Instant::now() + Duration::from_secs(3);
    loop {
        let rows = crate::db::collab_frames::list_for_project(
            &crate::api::db(&w.b.ctx).unwrap().conn(),
            ts::PID,
        )
        .unwrap();
        let held_here = rows
            .iter()
            .filter(|r| uuids.contains(&r.frame_uuid))
            .all(|r| {
                r.local_state == LocalState::Held
                    && r.landed_path
                        .as_deref()
                        .is_some_and(|p| std::path::Path::new(p).starts_with(&new_root))
            });
        if held_here {
            break;
        }
        assert!(
            rows.iter()
                .all(|r| r.local_state != LocalState::AwaitingChoice),
            "no deletion choice: {rows:#?}"
        );
        assert!(
            Instant::now() < deadline,
            "re-adopted in the new folder: {rows:#?}"
        );
        if Instant::now() >= next_sync {
            // A parked frame (a moved store's dead entry) is retried by the
            // next sweep once the GC dropped its entry.
            ts::sync_now_serial(&w.b.ctx).await;
            next_sync = Instant::now() + Duration::from_secs(3);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(landed_total(&w.b), landed, "no frame was fetched again");
    assert_eq!(
        w.b.events
            .payloads(crate::api::collab_live::COLLAB_DELETION_CHOICE_EVENT)
            .len(),
        choices,
        "no deletion choice"
    );
    let files = frame_files(&new_root);
    assert_eq!(
        files.len(),
        uuids.len(),
        "one file per frame, no `_1` beside it: {files:?}"
    );
    for u in &uuids {
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u));
    }
    if copy {
        assert_eq!(
            frame_files(&old_root).len(),
            uuids.len(),
            "the old folder is the user's"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copied_collaboration_folder_is_readopted_not_refetched() {
    readopted_after_relocation(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_collaboration_folder_is_readopted_not_refetched() {
    readopted_after_relocation(false).await;
}

/// Final fix B-I1 follow-up — a move to another disk in Finder is a copy,
/// then a delete: B's whole folder (store included) is copied, the copy
/// designated, then the OLD folder deleted. Every frame is parked while the
/// copied store still names the old file, re-seeded from its new path once
/// the GC dropped that entry, and then reads through the store with the old
/// folder gone — and `c`, paired with `b` only, gets every frame from it.
/// Nothing is fetched again on `b`. The copy is named so the OLD path sorts
/// FIRST in the store's path union (the case that goes dead).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_copied_then_deleted_keeps_serving_from_its_new_place() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let w = ts::two_devices_of_one_account().await;
    let uuids = w.a_publishes(12).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
    }
    let landed = landed_total(&w.b);
    let old_root = w.b.root.clone();
    crate::api::scan_roots::clear_collaboration_dir(&w.b.ctx)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while status(&w.b.ctx).storage != crate::api::collab_live::StorageStateView::NotSet {
        assert!(Instant::now() < deadline, "status {:?}", status(&w.b.ctx));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut name = old_root.file_name().unwrap().to_os_string();
    name.push("z"); // "Collab/" sorts before "Collabz/"
    let new_dir = old_root.with_file_name(name);
    copy_tree(&old_root, &new_dir);
    crate::sharing::iroh::node::test_gc::arm(Some(std::sync::Arc::clone(&gate)));
    crate::api::scan_roots::set_collaboration_dir(
        &w.b.ctx,
        new_dir.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await
    .unwrap();
    crate::sharing::iroh::node::test_gc::arm(None);
    let new_root = ts::collab_root(&w.b.ctx);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut next_sync = Instant::now() + Duration::from_secs(2);
    loop {
        let rows = crate::db::collab_frames::list_for_project(
            &crate::api::db(&w.b.ctx).unwrap().conn(),
            ts::PID,
        )
        .unwrap();
        if rows
            .iter()
            .filter(|r| uuids.contains(&r.frame_uuid))
            .all(|r| {
                r.local_state == LocalState::Held
                    && r.landed_path
                        .as_deref()
                        .is_some_and(|p| std::path::Path::new(p).starts_with(&new_root))
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "re-seeded in the new folder: {rows:#?}"
        );
        if Instant::now() >= next_sync {
            // the parked retry also runs at every sweep
            ts::sync_now_serial(&w.b.ctx).await;
            next_sync = Instant::now() + Duration::from_secs(2);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::fs::remove_dir_all(&old_root).unwrap();
    for u in &uuids {
        let hash = w.b.row(u).unwrap().blake3.parse().unwrap();
        assert_eq!(
            w.b.node.collab_blob_health(hash).await.unwrap(),
            crate::sharing::iroh::node::BlobHealth::Readable,
            "{u}: reads through the store with the old folder deleted"
        );
    }
    w.start_c().await;
    for u in &uuids {
        w.c.wait_state(u, LocalState::Held, Duration::from_secs(60))
            .await;
        assert_eq!(w.c.file_bytes(u), w.b.file_bytes(u));
    }
    assert_eq!(landed_total(&w.b), landed, "nothing fetched again on b");
    assert_eq!(
        frame_files(&new_root).len(),
        uuids.len(),
        "one file per frame"
    );
}

/// Task 11: A's node meters every collab serve to B under
/// `FlowDirection::Send`, keyed by the peer that pulled it, and the item
/// leaves nothing in flight once the transfer completes.
#[tokio::test(flavor = "multi_thread")]
async fn the_serving_side_meters_what_it_sends_and_to_whom() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(2).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
    }
    let flows =
        w.a.node
            .exchange_meter()
            .snapshot(std::time::Instant::now());
    let to_b = flows
        .iter()
        .find(|f| {
            f.direction == crate::collab::live::meter::FlowDirection::Send
                && f.device == w.b.device()
        })
        .expect("A metered its upload to B");
    assert_eq!(to_b.project_id, ts::PID);
    assert!(
        to_b.bytes_session >= 2 * ts::FETCH_FRAME_BYTES as i64,
        "{to_b:?}"
    );
    assert!(
        to_b.in_flight.is_empty(),
        "finished serves leave nothing in flight"
    );
}

/// Task 12 (spec 2026-09-29 §6.4): B's runtime emits
/// `collab-exchange-progress` naming A's device as the source of what it
/// receives, then — once nothing moves — the quiet payload for the project,
/// every receive flow `moving: false` (its final byte total riding along).
#[tokio::test(flavor = "multi_thread")]
async fn the_receiver_emits_live_flows_from_the_publisher_then_a_quiet_event() {
    let w = ts::two_instances().await;
    let uuids = w.a_publishes(3).await;
    for u in &uuids {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(20))
            .await;
    }
    let a_device = w.a.device();
    let event = crate::api::collab_live::COLLAB_EXCHANGE_PROGRESS_EVENT;
    let recv_of = |e: &serde_json::Value| -> Vec<serde_json::Value> {
        e["projects"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p["projectId"] == ts::PID)
            .flat_map(|p| p["recv"].as_array().cloned().unwrap_or_default())
            .collect()
    };
    let names_a = |e: &serde_json::Value| {
        recv_of(e).iter().any(|f| {
            f["device"] == a_device.as_str() && f["bytesSession"].as_i64().unwrap_or(0) > 0
        })
    };
    let is_quiet = |e: &serde_json::Value| {
        let recv = recv_of(e);
        !recv.is_empty() && recv.iter().all(|f| f["moving"] == false)
    };
    ts::wait_until(
        "a progress event naming A, then a quiet one",
        crate::collab::live::meter::MOVING + Duration::from_secs(15),
        || {
            let events = w.b.events.payloads(event);
            events
                .iter()
                .position(|e| names_a(e))
                .is_some_and(|i| events[i + 1..].iter().any(|e| is_quiet(e)))
        },
    )
    .await;
    let events = w.b.events.payloads(event);
    let quiet = events
        .iter()
        .rev()
        .find(|e| is_quiet(e))
        .expect("quiet event");
    let from_a = recv_of(quiet)
        .into_iter()
        .find(|f| f["device"] == a_device.as_str())
        .expect("the quiet payload keeps A's flow with its final total");
    assert!(
        from_a["bytesSession"].as_i64().unwrap_or(0) >= 3 * ts::FETCH_FRAME_BYTES as i64,
        "{from_a}"
    );
    assert_eq!(from_a["completed"], 3, "{from_a}");
    assert_eq!(from_a["rateBps"], 0.0, "{from_a}");
    assert_eq!(quiet["projects"][0]["toGo"], 0, "{quiet}");
    assert!(
        quiet["projects"][0]["waitingForPublisher"].is_null(),
        "the event never reads the catalog: {quiet}"
    );
}
