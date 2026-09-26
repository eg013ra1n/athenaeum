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
    w.a_publishes_big(4, FRAME).await;
    w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
    let admitted = w.b.personal_acquire_timed().await; // time until a personal permit is granted
    let in_flight = (STREAMS as u64) * FRAME as u64;
    let bound = Duration::from_secs_f64(in_flight as f64 / RATE as f64) + Duration::from_secs(3);
    assert!(
        admitted < bound,
        "waited {admitted:?}: more than {STREAMS} frames of {FRAME} bytes at {RATE} B/s (+3 s) = {bound:?}"
    );
}
