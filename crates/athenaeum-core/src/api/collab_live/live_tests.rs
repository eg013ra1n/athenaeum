//! The live orchestrator end to end (Task 15): two in-process instances on
//! the fake hub, relay disabled, the hub's presence timings shortened (and
//! the beat with them). A publishes (`send`), B replicates (`send_receive`).

use std::time::{Duration, Instant};

use crate::api::collab_live::test_support as ts;
use crate::api::collab_live::{shutdown, status, sync_now, LiveState};
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
    sync_now(&w.b.ctx).unwrap();
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_completes_while_the_feed_worker_is_stuck_in_a_retry() {
    let w = ts::two_instances().await;
    feed_stuck_in_a_retry(&w).await;
    let t0 = Instant::now();
    shutdown(&w.b.ctx).await;
    let took = t0.elapsed();
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
    sync_now(&w.b.ctx).unwrap(); // queues the (slow) sweep
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

/// The Folders page's "change": clear the designation, designate `dir`.
async fn redesignate(
    ctx: &crate::services::ServiceContext,
    dir: &std::path::Path,
) -> std::path::PathBuf {
    crate::api::scan_roots::clear_collaboration_dir(ctx)
        .await
        .expect("clear the Collaboration folder");
    std::fs::create_dir_all(dir).unwrap();
    crate::api::scan_roots::set_collaboration_dir(
        ctx,
        dir.to_string_lossy().to_string(),
        &crate::api::PathPolicy::AllowAll,
    )
    .await
    .expect("re-designate the Collaboration folder");
    ts::collab_root(ctx)
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
    let new_root = redesignate(&w.b.ctx, &other.path().join("Collab2")).await;
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
    redesignate(&w.b.ctx, &other.path().join("Collab2")).await;
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
        surface::list_collab_frames(ctx, ts::PID)
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
