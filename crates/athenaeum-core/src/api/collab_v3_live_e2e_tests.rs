//! Collab v3 wave 3 — three instances on the live exchange: the spec §12
//! latency table (app part), amendments A5 and A6, the personal-priority
//! bound derived from what is in flight, and the one-copy disk ledger.
//!
//! Spec `docs/superpowers/specs/2026-09-25-collab-v3-live-exchange-design.md`
//! §12 and §15. Three in-process instances on one fake hub (which mirrors the
//! real hub, A6 included) and three real relay-disabled iroh nodes (see
//! [`ts::World3`]): A (`send`, the contributor), B (`send_receive`, the
//! processor), C (`send_receive`, the coordinator). A↔B and B↔C are paired,
//! A and C are not, so whatever C holds came from B. The hub's presence
//! rules are shortened (silence 1 s, grace 300 ms, a 250 ms beat) except
//! where a bound is about them (scenario 4 runs them at their real values);
//! the watcher aggregates for 200 ms, a deletion settles after 1 s, and the
//! collab store GC runs every 100 ms.
//!
//! "Starts fetching" is the executor's `Start` for the frame: it issues the
//! frame's first get right after it (spec §12). Every latency is measured
//! conservatively — from just before the triggering action, or from the last
//! poll that had not yet seen it — and printed (`--nocapture`) beside its
//! bound. A bound the code misses is fixed in the owning code, never widened
//! here; the bounds that depend on load are derived below from what is in
//! flight, each with its derivation.
//!
//! The run against the real test hub and the test relay is owed
//! (`docs/superpowers/open-items.md`, collab v3 wave 3).

use std::time::{Duration, Instant};

use crate::api::collab_live::executor::test_hooks;
use crate::api::collab_live::test_support::{self as ts, sent_bytes};
use crate::api::collab_live::{set_receive_streams, shutdown};
use crate::db::collab_frames::LocalState;
use crate::sharing::iroh::assign::{ItemOutcome, LiveVerdict};

/// Assert a measured latency against its bound, and print both.
fn within(scenario: &str, took: Duration, bound: Duration) {
    eprintln!("scenario {scenario}: {took:?} (bound {bound:?})");
    assert!(
        took <= bound,
        "scenario {scenario}: took {took:?}, bound {bound:?}"
    );
}

/// The get failed because the provider refused `hash` (`ERR_PERMISSION`),
/// never because of a transport fault or bad bytes.
fn refused(got: &(ItemOutcome, Vec<LiveVerdict>), hash: iroh_blobs::Hash) -> bool {
    matches!(got.0, ItemOutcome::Failed(_))
        && got
            .1
            .iter()
            .any(|v| matches!(v, LiveVerdict::Refused { hash: h, .. } if *h == hash))
}

/// With `ATHENAEUM_LOG` set (an `EnvFilter`, as the app reads it), the
/// test's tracing goes to the test output — the three instances share one
/// process, so their lines interleave.
fn log_if_asked() {
    if let Ok(filter) = std::env::var("ATHENAEUM_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_test_writer()
            .try_init();
    }
}

/// Get `hash` from `provider` until it is served, within `limit`.
async fn served_within(
    fetcher: &ts::Instance,
    provider: &ts::Instance,
    hash: iroh_blobs::Hash,
    limit: Duration,
) -> Duration {
    let t0 = Instant::now();
    loop {
        let out = fetcher.get_from(provider, hash).await.0;
        if matches!(out, ItemOutcome::Done) {
            return t0.elapsed();
        }
        assert!(
            t0.elapsed() < limit,
            "not served again within {limit:?}: {out:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for(what: &str, limit: Duration, mut pred: impl FnMut() -> bool) -> Duration {
    let t0 = Instant::now();
    while !pred() {
        assert!(t0.elapsed() < limit, "{what}: not within {limit:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    t0.elapsed()
}

/// Scenarios 1 and 2, and the one-copy disk ledger on all three instances.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_fetch_and_serve_onward_within_bounds() {
    log_if_asked();
    // 2 MiB frames: well above the store's 16 KiB inline limit (a landing
    // is the DIRECT export), and a payload the ledger's 1 % budget can see
    // a copy in.
    const SIZE: usize = 2 * 1024 * 1024;
    let w = ts::three_instances().await;
    let published = w.a_publishes_timed(3, SIZE).await;
    // 1: A publishes → B starts fetching ≤ 2 s.
    for (u, announced) in &published {
        let started = w.b.first_start_at(u, Duration::from_secs(10)).await;
        within(
            &format!("1 ({u})"),
            started.saturating_duration_since(*announced),
            Duration::from_secs(2),
        );
    }
    // 2: B lands → C (paired with B, not with A) can fetch it from B ≤ 3 s.
    let (u0, _) = &published[0];
    let landed =
        w.b.wait_state_since(u0, LocalState::Held, Duration::from_secs(30))
            .await;
    let named =
        w.c.first_start_with_provider(u0, &w.b.device(), Duration::from_secs(10))
            .await;
    within(
        "2",
        named.saturating_duration_since(landed),
        Duration::from_secs(3),
    );

    let uuids: Vec<String> = published.iter().map(|(u, _)| u.clone()).collect();
    w.wait_held_by_b_and_c(&uuids).await;
    for u in &uuids {
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u), "B's {u}");
        assert_eq!(w.c.file_bytes(u), w.a.file_bytes(u), "C's {u}");
        for d in [w.b.device(), w.c.device()] {
            w.hub
                .wait_holder(ts::PID, u, &d, Duration::from_secs(5))
                .await;
        }
    }
    w.assert_disk_ledger();
}

/// Scenarios 3 (clean exit), 5 (restart) and 4 (kill, the hub's real
/// timing rules).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_exit_kill_and_restart() {
    log_if_asked();
    let (w, uuids) = ts::three_instances_all_held(3).await;
    let b = w.b.device();
    w.c.wait_provider(&b, Duration::from_secs(5)).await;
    for u in &uuids {
        w.hub
            .wait_holder(ts::PID, u, &b, Duration::from_secs(5))
            .await;
        w.hub
            .wait_holder(ts::PID, u, &w.c.device(), Duration::from_secs(5))
            .await;
    }

    // 3: B quits cleanly → C stops dialing B ≤ 2 s (B leaves C's presence,
    // from which every provider list is derived).
    let t0 = Instant::now();
    shutdown(&w.b.ctx).await;
    let dropped = w.c.wait_not_a_provider(&b, Duration::from_secs(5)).await;
    within("3", dropped - t0, Duration::from_secs(2));

    // 5: B restarts → a provider again ≤ 3 s after connect; digests match,
    // so nothing is reported again.
    let writes = w.hub.holder_writes();
    w.b.restart_live().await;
    w.hub
        .wait_connected(ts::PID, &b, Duration::from_secs(10))
        .await;
    let connected = Instant::now();
    let back = w.c.wait_provider(&b, Duration::from_secs(10)).await;
    within("5", back - connected, Duration::from_secs(3));
    // Past one outbox flush (1 s) and the reconnect's reconciliation.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(w.hub.holder_writes(), writes, "scenario 5: zero re-reports");

    // 4: B is killed (no DELETE, its stream just drops) → C drops B ≤ 50 s
    // after B's last beat, under the hub's REAL rules (keepalive 20 s,
    // grace 10 s, silence 40 s).
    w.hub
        .set_timings(crate::collab::fake_hub::FakeTimings::default());
    w.b.kill().await;
    let last_beat = w
        .hub
        .last_beat(&b)
        .expect("the killed session is still known");
    let gone = w.c.wait_not_a_provider(&b, Duration::from_secs(60)).await;
    within("4", gone - last_beat, Duration::from_secs(50));
}

/// Scenarios 6 (a new version mid-download, and after a full landing) and
/// 7 (an interrupted landing).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn versions_mid_download_and_interrupted_landing() {
    log_if_asked();
    const RATE: u64 = 4 * 1024 * 1024; // A's upload cap
    const BIG: usize = 16 * 1024 * 1024; // ≈ 4 s at the cap
    let w = ts::three_instances_with(Some(RATE)).await;

    // 6a: v1 held, then v2 — v2 lands at the same path, v1 is never served
    // again.
    let small = w.a_publishes(1).await.remove(0);
    w.b.wait_state(&small, LocalState::Held, Duration::from_secs(30))
        .await;
    let v1 = w.b.hash_of(&small);
    let path = w.b.row(&small).unwrap().landed_path;
    w.a_republishes_changed(&small).await;
    w.b.wait_state_version(&small, LocalState::Held, 2, Duration::from_secs(30))
        .await;
    assert_eq!(w.b.row(&small).unwrap().landed_path, path, "the same path");
    assert_eq!(w.b.file_bytes(&small), w.a.file_bytes(&small));
    let out = w.c.get_from(&w.b, v1).await;
    assert!(
        refused(&out, v1),
        "scenario 6: v1 served after v2 landed: {out:?}"
    );

    // 6b: v2 published mid-download of v1 — the v1 fetch is cancelled, v2
    // lands, v1 is never served.
    let big = w.a_publishes_big(1, BIG).await.remove(0);
    w.b.wait_fetch_started(&big, Duration::from_secs(10)).await;
    let v1 = w.b.hash_of(&big);
    w.a_republishes_changed(&big).await;
    w.b.wait_state_version(&big, LocalState::Held, 2, Duration::from_secs(60))
        .await;
    assert_eq!(w.b.file_bytes(&big), w.a.file_bytes(&big));
    let store = w.b.node.collab_store().expect("B's collab store");
    let v1_bits = store.blobs().observe(v1).await;
    assert!(
        !v1_bits.as_ref().is_ok_and(|b| b.is_complete()),
        "scenario 6: the v1 fetch ran to completion instead of being cancelled"
    );
    let out = w.c.get_from(&w.b, v1).await;
    assert!(
        refused(&out, v1),
        "scenario 6: a superseded version served: {out:?}"
    );

    // 7: the v2 landing fails inside its export — at every observation the
    // target holds v1 or v2 in full (never missing, never torn); the retry
    // lands v2; no landing temp is left.
    let frame = w.a_publishes(1).await.remove(0);
    w.b.wait_state(&frame, LocalState::Held, Duration::from_secs(30))
        .await;
    let target = std::path::PathBuf::from(w.b.row(&frame).unwrap().landed_path.unwrap());
    let v1_bytes = std::fs::read(&target).unwrap();
    test_hooks::fail_next_landing(&w.b.root);
    w.a_republishes_changed(&frame).await;
    let v2_bytes = w.a.file_bytes(&frame);
    assert_ne!(v1_bytes, v2_bytes);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen_v1_after_fault = false;
    loop {
        let bytes = std::fs::read(&target)
            .unwrap_or_else(|e| panic!("scenario 7: the target vanished: {e}"));
        assert!(
            bytes == v1_bytes || bytes == v2_bytes,
            "scenario 7: the target holds neither version in full"
        );
        if !test_hooks::landing_fault_pending(&w.b.root) && bytes == v1_bytes {
            seen_v1_after_fault = true;
        }
        if w.b
            .row(&frame)
            .is_some_and(|r| r.local_state == LocalState::Held && r.content_version == 2)
        {
            break;
        }
        assert!(Instant::now() < deadline, "scenario 7: v2 never landed");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !test_hooks::landing_fault_pending(&w.b.root),
        "scenario 7: the injected fault never fired"
    );
    eprintln!("scenario 7: v1 observed intact after the fault: {seen_v1_after_fault}");
    assert_eq!(std::fs::read(&target).unwrap(), v2_bytes);
    assert_eq!(
        w.b.row(&frame).unwrap().landed_path.as_deref(),
        Some(target.to_string_lossy().as_ref())
    );
    let temps = ts::athtmp_under(&w.b.root);
    assert!(temps.is_empty(), "scenario 7: temps left: {temps:?}");
}

/// Scenarios 8 (edited in place), 9 (touched), 10 (a single delete) and 11
/// (15 deletes at once).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edited_touched_and_deleted_replicas() {
    log_if_asked();
    let (w, uuids) = ts::three_instances_all_held(18).await;
    let cfg = ts::e2e_config();

    // 8: edited in place → the next get is refused, B's row is quarantined,
    // and a later v2 does not land over it.
    let u = &uuids[0];
    let hash = w.b.hash_of(u);
    let path = std::path::PathBuf::from(w.b.row(u).unwrap().landed_path.unwrap());
    ts::overwrite_same_size(&path);
    let edited = std::fs::read(&path).unwrap();
    let out = w.c.get_from(&w.b, hash).await;
    assert!(
        refused(&out, hash),
        "scenario 8: an edited replica served: {out:?}"
    );
    w.b.wait_state(u, LocalState::Quarantined, Duration::from_secs(10))
        .await;
    w.a_republishes_changed(u).await;
    wait_for(
        "scenario 8: B sees v2 waiting",
        Duration::from_secs(10),
        || {
            crate::api::collab_live::surface::list_collab_frames(&w.b.ctx, ts::PID)
                .unwrap()
                .into_iter()
                .any(|f| &f.frame_uuid == u && f.new_version_waiting)
        },
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        std::fs::read(&path).unwrap(),
        edited,
        "scenario 8: v2 landed over the edit"
    );
    assert_eq!(w.b.row(u).unwrap().local_state, LocalState::Quarantined);

    // 9: touched (same bytes, a new mtime) → served again after the rehash.
    let u = &uuids[1];
    let hash = w.b.hash_of(u);
    let path = std::path::PathBuf::from(w.b.row(u).unwrap().landed_path.unwrap());
    let before = std::fs::read(&path).unwrap();
    ts::set_mtime(&path, 10);
    let took = served_within(&w.c, &w.b, hash, Duration::from_secs(15)).await;
    eprintln!("scenario 9: served again after {took:?}");
    assert_eq!(w.b.row(u).unwrap().local_state, LocalState::Held);
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // 10: a single delete → wanted after the settle, re-landed from C (A is
    // offline, so C is B's only provider) once the store GC dropped the
    // dead entry. Bound: the aggregation and the settle, one GC run and one
    // GC probe (spec: "settle + GC"), plus 3 s for the re-fetch of a 64 KiB
    // frame — its dial, a provider back-off at most at the 1 s base, the
    // landing — on a loaded machine.
    shutdown(&w.a.ctx).await;
    w.b.wait_not_a_provider(&w.a.device(), Duration::from_secs(5))
        .await;
    let u = &uuids[2];
    let path = std::path::PathBuf::from(w.b.row(u).unwrap().landed_path.unwrap());
    let bytes = std::fs::read(&path).unwrap();
    let from_c = sent_bytes(&w.c.node);
    let t0 = Instant::now();
    std::fs::remove_file(&path).unwrap();
    wait_for("scenario 10: re-landed", Duration::from_secs(30), || {
        w.b.row(u)
            .is_some_and(|r| r.local_state == LocalState::Held)
            && path.exists()
    })
    .await;
    let bound = cfg.timings.aggregate
        + cfg.timings.settle
        + Duration::from_millis(100)
        + cfg.gc_probe
        + Duration::from_secs(3);
    within("10", t0.elapsed(), bound);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(
        sent_bytes(&w.c.node) - from_c >= bytes.len() as u64,
        "scenario 10: C served the re-fetch"
    );
    w.a.start_live_with(cfg);
    w.hub
        .wait_connected(ts::PID, &w.a.device(), Duration::from_secs(10))
        .await;

    // 11: 15 deletes at once → one deletion choice; B keeps fetching
    // another frame meanwhile.
    let mass = &uuids[3..18];
    for u in mass {
        std::fs::remove_file(w.b.row(u).unwrap().landed_path.unwrap()).unwrap();
    }
    let extra = w.a_publishes(1).await.remove(0);
    wait_for("scenario 11: the choice", Duration::from_secs(15), || {
        !w.b.events
            .payloads(crate::api::collab_live::COLLAB_DELETION_CHOICE_EVENT)
            .is_empty()
    })
    .await;
    w.b.wait_state(&extra, LocalState::Held, Duration::from_secs(30))
        .await;
    for u in mass {
        w.b.wait_state(u, LocalState::AwaitingChoice, Duration::from_secs(10))
            .await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let choices =
        w.b.events
            .payloads(crate::api::collab_live::COLLAB_DELETION_CHOICE_EVENT);
    assert_eq!(choices.len(), 1, "scenario 11: {choices:?}");
    // The choice counts every frame deleted in the rolling window: these 15
    // and scenario 10's single delete a few seconds earlier. That one was
    // re-fetched and stays held — it is not dragged into the choice.
    assert_eq!(choices[0]["count"], 16, "scenario 11: {choices:?}");
    assert_eq!(w.b.row(&uuids[2]).unwrap().local_state, LocalState::Held);
}

/// Scenarios 12 (storage unmounted) and 13 (a hub restart mid-transfer).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn storage_unmount_and_hub_restart() {
    log_if_asked();
    const RATE: u64 = 4 * 1024 * 1024;
    let w = ts::three_instances_with(Some(RATE)).await;
    let uuids = w.a_publishes(2).await;
    w.wait_held_by_b_and_c(&uuids).await;
    let b = w.b.device();

    // 12: the root is renamed away → B's serving goes false at the hub and
    // no frame changes state; renamed back → serving again, nothing
    // re-fetched.
    let snapshot = |i: &ts::Instance| {
        uuids
            .iter()
            .map(|u| {
                let r = i.row(u).unwrap();
                (r.local_state, r.content_version, r.landed_path)
            })
            .collect::<Vec<_>>()
    };
    wait_for("scenario 12: B serving", Duration::from_secs(10), || {
        w.hub.serving(ts::PID, &b)
    })
    .await;
    let rows = snapshot(&w.b);
    let served = sent_bytes(&w.a.node) + sent_bytes(&w.c.node);
    let away = w.b.root.with_file_name("Collab-away");
    std::fs::rename(&w.b.root, &away).unwrap();
    let off = wait_for(
        "scenario 12: serving false",
        Duration::from_secs(20),
        || !w.hub.serving(ts::PID, &b),
    )
    .await;
    eprintln!("scenario 12: serving false after {off:?}");
    assert_eq!(
        snapshot(&w.b),
        rows,
        "scenario 12: a state changed while unmounted"
    );
    std::fs::rename(&away, &w.b.root).unwrap();
    let on = wait_for(
        "scenario 12: serving again",
        Duration::from_secs(20),
        || w.hub.serving(ts::PID, &b),
    )
    .await;
    eprintln!("scenario 12: serving again after {on:?}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        snapshot(&w.b),
        rows,
        "scenario 12: a state changed after the remount"
    );
    assert!(
        sent_bytes(&w.a.node) + sent_bytes(&w.c.node) - served < ts::FETCH_FRAME_BYTES as u64,
        "scenario 12: re-fetched after the remount"
    );

    // 13: the hub restarts mid-transfer → the transfer completes; after
    // the reconnect nothing is duplicated or lost and the digest check
    // matches (Sync now's check writes nothing).
    let big = w.a_publishes_big(1, 16 * 1024 * 1024).await.remove(0);
    w.b.wait_fetch_started(&big, Duration::from_secs(10)).await;
    w.hub.kill_streams();
    w.b.wait_state(&big, LocalState::Held, Duration::from_secs(60))
        .await;
    assert_eq!(w.b.file_bytes(&big), w.a.file_bytes(&big));
    w.c.wait_state(&big, LocalState::Held, Duration::from_secs(60))
        .await;
    for d in [&w.a, &w.b, &w.c] {
        w.hub
            .wait_connected(ts::PID, &d.device(), Duration::from_secs(10))
            .await;
    }
    let all: Vec<String> = uuids.iter().cloned().chain([big.clone()]).collect();
    for u in &all {
        for d in [w.a.device(), w.b.device(), w.c.device()] {
            w.hub
                .wait_holder(ts::PID, u, &d, Duration::from_secs(10))
                .await;
        }
        let holders = w.hub.holders_of(ts::PID, u);
        let mut unique = holders.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), holders.len(), "scenario 13: {u}: {holders:?}");
        assert_eq!(holders.len(), 3, "scenario 13: {u}: {holders:?}");
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let writes = w.hub.holder_writes();
    ts::sync_now_serial(&w.b.ctx).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        w.hub.holder_writes(),
        writes,
        "scenario 13: the digest check did not match (a full report was sent)"
    );
}

/// Scenarios 14 (epoch rotation) and 15 (a device revoked mid-transfer).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn epoch_rotation_and_revocation() {
    log_if_asked();
    let (w, uuids) = ts::three_instances_all_held(2).await;

    // 14: a restore lost the frames and rotated the epoch → full resync;
    // A's frames are re-announced under the same uuids.
    let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
    w.hub.forget_frames(ts::PID, &refs);
    w.hub.rotate_epoch();
    w.hub.kill_streams();
    let a = w.a.device();
    let back = wait_for("scenario 14: re-announced", Duration::from_secs(20), || {
        uuids.iter().all(|u| {
            w.hub
                .frame(ts::PID, u)
                .is_some_and(|f| f.publisher_device_id.as_deref() == Some(a.as_str()))
        })
    })
    .await;
    eprintln!("scenario 14: re-announced after {back:?}");
    w.wait_held_by_b_and_c(&uuids).await;
    for u in &uuids {
        for d in [w.a.device(), w.b.device(), w.c.device()] {
            w.hub
                .wait_holder(ts::PID, u, &d, Duration::from_secs(10))
                .await;
        }
    }

    // 15: C is revoked while fetching from B → B closes its connection to
    // C (the get it was serving ends) and admits C no more.
    w.b.node.set_upload_limit(1024 * 1024); // 8 MiB to C: ≈ 8 s
    let big = w.a_publishes_big(1, 8 * 1024 * 1024).await.remove(0);
    w.b.wait_state(&big, LocalState::Held, Duration::from_secs(30))
        .await;
    w.c.wait_fetch_started(&big, Duration::from_secs(20)).await;
    assert!(w.b.node.collab_streams_in_use() >= 1, "B serves C");
    let t0 = Instant::now();
    w.hub.revoke_device(&w.c.device(), false);
    wait_for(
        "scenario 15: C's get closed",
        Duration::from_secs(10),
        || w.b.node.collab_streams_in_use() == 0,
    )
    .await;
    eprintln!(
        "scenario 15: B closed C's collab connection after {:?}",
        t0.elapsed()
    );
    assert!(
        !w.b.node.admits(&w.c.node.node_id()),
        "scenario 15: C still admitted"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        w.c.row(&big)
            .is_none_or(|r| r.local_state != LocalState::Held),
        "scenario 15: C completed the frame after its revocation"
    );
}

/// Scenario 16: a personal transfer during collab fetches is admitted after
/// at most the frames already in flight (spec §8; ledger [T13, T15, T18]).
/// The bound is DERIVED: each running collab unit finishes only its
/// in-flight frame, so the wait is at most the units in flight × the frame
/// size at A's upload cap, plus 3 s for the lane release and the permit
/// hand-off on a loaded machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn personal_priority() {
    log_if_asked();
    const RATE: u64 = 4 * 1024 * 1024; // A's upload cap
    const FRAME: usize = 4 * 1024 * 1024;
    const UNITS: usize = 2; // collab receive streams on B
    let w = ts::three_instances_with(Some(RATE)).await;
    w.b.set_receive_limit(1).await; // one lane: collab holds it
    set_receive_streams(&w.b.ctx, UNITS);
    // Eight frames: without the yield the lane would stay collab's for all
    // of them (8 × 1 s), far past the bound.
    w.a_publishes_big(8, FRAME).await;
    w.b.wait_any_fetch_started(Duration::from_secs(10)).await;
    let waited = w.b.personal_acquire_timed().await;
    let bound =
        Duration::from_secs_f64((UNITS * FRAME) as f64 / RATE as f64) + Duration::from_secs(3);
    within("16", waited, bound);
}

/// Amendment A6 on the live exchange: a SECOND device of the publishing
/// account fetches the first's frames as replicas within the scenario-1
/// bound, holds them once on disk, and serves them (a member of another
/// account paired with it alone lands every one) — and publishes only after
/// "Publish from this device".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a6_a_second_device_replicates_serves_and_publishes_after_a_switch() {
    log_if_asked();
    let w = ts::two_devices_of_one_account().await;
    let base_b = w.b.disk_baseline();
    let base_c = w.c.disk_baseline();
    let published = w.a_publishes_timed(3).await;
    for (u, announced) in &published {
        let started = w.b.first_start_at(u, Duration::from_secs(10)).await;
        within(
            &format!("A6/1 ({u})"),
            started.saturating_duration_since(*announced),
            Duration::from_secs(2),
        );
    }
    for (u, _) in &published {
        w.b.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
        assert_eq!(
            w.b.row(u).unwrap().origin,
            crate::db::collab_frames::FrameOrigin::Replica,
            "another device's frame is a replica here"
        );
    }
    // Served by the second device alone: the publishing device goes away.
    shutdown(&w.a.ctx).await;
    w.start_c().await;
    for (u, _) in &published {
        w.c.wait_state(u, LocalState::Held, Duration::from_secs(30))
            .await;
        assert_eq!(w.c.file_bytes(u), w.b.file_bytes(u));
    }
    w.b.assert_disk_ledger(&base_b, "A6 second device");
    w.c.assert_disk_ledger(&base_c, "A6 other account");

    // Publishing: refused naming the bound device until the switch.
    let client = crate::collab::hub_client::CollabClient::new(w.hub.uri()).unwrap();
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
    let out = client
        .announce_frames("tok-a2", ts::PID, &[frame("b1")])
        .await;
    assert!(
        matches!(&out, Err(crate::account::AccountClientError::PublishingDevice { device_id, .. }) if *device_id == w.a.device()),
        "{out:?}"
    );
    let card = crate::api::collab::set_collab_publishing_device(&w.b.ctx, ts::PID)
        .await
        .unwrap();
    assert!(card.publishing_here);
    client
        .announce_frames("tok-a2", ts::PID, &[frame("b1")])
        .await
        .expect("the bound device announces");
}

/// Amendment A5 mid-run: the Collaboration folder is re-designated while a
/// fetch is in flight. The runtime restarts on the new store; the replica
/// under the old folder leaves `held` through the L4 path and is re-fetched
/// into the new folder; the in-flight frame lands there too; the old
/// folder keeps the user's old file only (no partial frame, no temp); the
/// new folder passes the disk ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a5_a_redesignation_mid_run_refetches_into_the_new_folder() {
    log_if_asked();
    const RATE: u64 = 4 * 1024 * 1024;
    let w = ts::three_instances_with(Some(RATE)).await;
    let small = w.a_publishes(1).await.remove(0);
    w.b.wait_state(&small, LocalState::Held, Duration::from_secs(30))
        .await;
    let old_small = std::path::PathBuf::from(w.b.row(&small).unwrap().landed_path.unwrap());
    let big = w.a_publishes_big(1, 16 * 1024 * 1024).await.remove(0);
    w.b.wait_fetch_started(&big, Duration::from_secs(10)).await;

    let old_root = w.b.root.clone();
    let other = tempfile::tempdir().unwrap();
    let new_root = ts::redesignate(&w.b.ctx, &other.path().join("Collab2")).await;
    let store_base = ts::dir_bytes(&new_root.join(".athenaeum"));
    let store = |root: &std::path::Path| ts::dir_bytes(&root.join(".athenaeum"));
    for u in [&small, &big] {
        wait_for(
            &format!("A5: {u} under the new folder"),
            Duration::from_secs(60),
            || {
                w.b.row(u).is_some_and(|r| {
                    r.local_state == LocalState::Held
                        && r.landed_path
                            .as_deref()
                            .is_some_and(|p| std::path::Path::new(p).starts_with(&new_root))
                })
            },
        )
        .await;
        assert_eq!(w.b.file_bytes(u), w.a.file_bytes(u), "A5: {u}");
    }
    // The old folder: the user's old file stays, nothing else was written.
    assert!(old_small.exists(), "A5: the old file is the user's");
    let old_frames: Vec<_> = ts::files_under(&old_root)
        .into_iter()
        .filter(|p| {
            !p.strip_prefix(&old_root)
                .is_ok_and(|r| r.starts_with(".athenaeum"))
        })
        .collect();
    assert_eq!(old_frames, vec![old_small.clone()], "A5: the old folder");
    assert!(ts::athtmp_under(&old_root).is_empty());
    // The new folder: one file per frame, byte for byte; the store grown by
    // < 1 % of them (never a copy).
    let payload = (ts::FETCH_FRAME_BYTES + 16 * 1024 * 1024) as u64;
    let new_frames: Vec<_> = ts::files_under(&new_root)
        .into_iter()
        .filter(|p| {
            !p.strip_prefix(&new_root)
                .is_ok_and(|r| r.starts_with(".athenaeum"))
        })
        .collect();
    assert_eq!(new_frames.len(), 2, "A5: {new_frames:?}");
    let frame_bytes = ts::dir_bytes(&new_root) - store(&new_root);
    assert_eq!(frame_bytes, payload, "A5: the new folder's frame files");
    let store_grown = store(&new_root).saturating_sub(store_base);
    assert!(
        store_grown * 100 < payload,
        "A5: the new store grew {store_grown} B for {payload} B of frames"
    );
    assert!(ts::athtmp_under(&new_root).is_empty());
    eprintln!(
        "ledger A5: new folder frame files {frame_bytes} B for {payload} B of frames, store {store_base} -> {} B",
        store(&new_root)
    );
}
