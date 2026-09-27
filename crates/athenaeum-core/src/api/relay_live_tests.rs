//! Task 9: the C1 relay-eviction regression canary.
//!
//! One `#[ignore]`d, owner-run test that binds TWO iroh endpoints with the **same**
//! device secret against a real relay and asserts the FIRST connection observes the
//! eviction the moment the second binds. This is the field failure the shared-node
//! refactor (Tasks 1–8) exists to prevent — a relay permits exactly one active
//! connection per node id, so two endpoints from one key duel over the relay slot.
//! The shared node collapses the three roles onto ONE endpoint so this can never
//! happen in production; this canary proves the relay still evicts a genuine
//! same-key second endpoint, so a regression that re-introduces a second endpoint
//! would be caught here.
//!
//! Skipped (a clean no-op) unless `ATHENAEUM_TEST_RELAY` names a relay url — the
//! owner runs it explicitly against `test-relay.artfrom.space`:
//!
//! ```text
//! ATHENAEUM_TEST_RELAY=https://test-relay.artfrom.space:8443 \
//!   cargo test -p athenaeum-core --lib -- --ignored --exact \
//!   api::relay_live_tests::same_key_second_endpoint_evicts_first_on_real_relay --nocapture
//! ```

/// Bind two endpoints with the SAME device secret against the relay in
/// `ATHENAEUM_TEST_RELAY`; assert the first observes the relay eviction (its
/// `home_relay_status` transitions connected→disconnected) within a bounded window
/// after the second binds. `#[cfg(unix)]` (the owner runs it on macOS/Linux);
/// `#[ignore]`d so it never runs in a normal `cargo test` sweep.
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires ATHENAEUM_TEST_RELAY=<relay-url>; owner-run against test-relay.artfrom.space"]
async fn same_key_second_endpoint_evicts_first_on_real_relay() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMap, RelayMode, SecretKey, Watcher as _};
    use n0_future::StreamExt as _;

    let Ok(relay_url) = std::env::var("ATHENAEUM_TEST_RELAY") else {
        eprintln!(
            "skip same_key_second_endpoint_evicts_first_on_real_relay: set \
             ATHENAEUM_TEST_RELAY to a relay url (e.g. https://test-relay.artfrom.space:8443)"
        );
        return;
    };

    const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);
    const EVICTION_WINDOW: Duration = Duration::from_secs(30);
    // A fixed, distinctive device secret so BOTH endpoints present the SAME node id
    // (the whole point — the relay must evict the older connection for that id).
    const SECRET: [u8; 32] = [0x5b; 32];

    let build = || {
        let map = RelayMap::try_from_iter([relay_url.as_str()]).expect("valid relay url");
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&SECRET))
            .relay_mode(RelayMode::Custom(map))
            .bind()
    };

    // Endpoint 1 comes online on the relay first.
    let ep1 = build().await.expect("bind endpoint 1");
    tokio::time::timeout(ONLINE_TIMEOUT, ep1.online())
        .await
        .expect("endpoint 1 must reach its home relay before the second binds");

    // Watch endpoint 1 for a connected→disconnected transition (the eviction).
    let evicted = Arc::new(AtomicBool::new(false));
    let watcher = {
        let ep1 = ep1.clone();
        let evicted = Arc::clone(&evicted);
        tokio::spawn(async move {
            let mut stream = ep1.home_relay_status().stream();
            let mut was_connected = false;
            while let Some(statuses) = stream.next().await {
                let any_connected = statuses.iter().any(|s| s.is_connected());
                if was_connected && !any_connected {
                    evicted.store(true, Ordering::SeqCst);
                    return;
                }
                was_connected |= any_connected;
            }
        })
    };

    // Endpoint 2 binds with the SAME secret → one connection per node id, so the
    // relay evicts endpoint 1.
    let ep2 = build().await.expect("bind endpoint 2");
    let _ = tokio::time::timeout(ONLINE_TIMEOUT, ep2.online()).await;

    // Endpoint 1 must observe the eviction within the window.
    let start = Instant::now();
    while start.elapsed() < EVICTION_WINDOW && !evicted.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let observed = evicted.load(Ordering::SeqCst);

    watcher.abort();
    ep1.close().await;
    ep2.close().await;

    assert!(
        observed,
        "endpoint 1 must observe the relay eviction (home_relay_status \
         connected→disconnected) within {EVICTION_WINDOW:?} of endpoint 2 binding the same key"
    );
}

/// Owner-run (plan P21): pick the collab stream-limit defaults from a
/// measurement on the real relay. The provider holds 16 frames of 32 MiB in
/// its collab store; the fetcher dials it by its relay url alone and fetches
/// every frame over one live assignment run with `max_in_flight` ∈ {1, 2, 4,
/// 8, 16}, a fresh store per run, and prints MB/s for each, with the share
/// of the provider's sent bytes that crossed the relay (run the two ends on
/// one machine and hole punching may find a direct path — a share well below
/// 1.0 means the run did not measure the relay).
///
/// Rule for the defaults: `collab.max_receive_streams` = the smallest `n`
/// reaching ≥ 90 % of the best MB/s; `collab.max_upload_streams` = the same
/// value (one fetcher can saturate one provider). Until it has run, both
/// stay 8 (`settings/mod.rs`).
///
/// ```text
/// ATHENAEUM_TEST_RELAY=https://test-relay.artfrom.space:8443 \
///   cargo test -p athenaeum-core --lib -- --ignored --exact \
///   api::relay_live_tests::collab_stream_limits_throughput_on_real_relay --nocapture
/// ```
#[cfg(all(unix, feature = "render", feature = "solver"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ATHENAEUM_TEST_RELAY=<relay-url>; owner-run against test-relay.artfrom.space"]
async fn collab_stream_limits_throughput_on_real_relay() {
    let Ok(relay_url) = std::env::var("ATHENAEUM_TEST_RELAY") else {
        eprintln!("skip collab_stream_limits_throughput_on_real_relay: set ATHENAEUM_TEST_RELAY");
        return;
    };
    let pair =
        crate::api::collab_live::test_support::relay_pair(&relay_url, 16, 32 * 1024 * 1024).await;
    for n in [1usize, 2, 4, 8, 16] {
        let (bytes, elapsed, relay_share) = pair.fetch_all_fresh(n).await;
        eprintln!(
            "streams={n} bytes={bytes} secs={:.1} MB/s={:.1} relay_share={relay_share:.2}",
            elapsed.as_secs_f64(),
            bytes as f64 / 1e6 / elapsed.as_secs_f64()
        );
    }
}
