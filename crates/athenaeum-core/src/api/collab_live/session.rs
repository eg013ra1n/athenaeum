//! The event channel's client side (collab v3 wave 3, Task 15; spec §4.1,
//! §4.2, §4.6, plan P2/P3/P26/P28): open `GET /me/events`, hand every event
//! to the runtime, beat `POST /me/presence` every 15 s (and at once when the
//! serving map or the home relay changes), reconnect with full-jitter
//! back-off, reopen at once on `session_gone`, a wake from sleep or Sync
//! now, and send `DELETE /me/presence` on a clean exit.
//!
//! Two separate signals end a connection (Task 15 R1): the **reconnect**
//! signal ([`Shared::reconnect_now`]) cancels the stream and reconnects at
//! once; the session's **stop** cancels it and ends the session after the
//! leave.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{mpsc, watch};

use crate::account::AccountClientError;
use crate::api::collab_live::runtime::Shared;
use crate::api::collab_live::LiveState;
use crate::collab::hub_client::CollabClient;
use crate::collab::live::backoff::{reset_signal, sleep_or_reset, Backoff};
use crate::collab::live::presence::woke_from_sleep;
use crate::collab::live::stream::{self, OpenError, StreamEnd};
use crate::collab::live::wire::{BeatWire, LiveEvent};
use crate::sharing::iroh::node::SharedIrohNode;

/// How long a signed-out session waits before it looks for credentials
/// again (a sign-in also arms a fresh session).
pub const SIGNED_OUT_RECHECK: Duration = Duration::from_secs(60);
/// How long an outdated build waits before it tries the stream again.
pub const OUTDATED_RECHECK: Duration = Duration::from_secs(3600);
/// Consecutive failed connects before the state reads `unreachable` (P27;
/// it keeps retrying at the back-off cap).
pub const UNREACHABLE_AFTER: u32 = 3;
/// The clean exit's `DELETE /me/presence` bound (P28).
pub const LEAVE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a live connection ended.
enum Ended {
    Stream(StreamEnd),
    Reconnect,
    Stop,
}

/// Run one session until `stop` is set (or its sender drops). Events go to
/// `events` in stream order; the runtime applies them.
pub(crate) async fn run_session(
    shared: Arc<Shared>,
    node: Arc<SharedIrohNode>,
    events: mpsc::Sender<LiveEvent>,
    mut stop: watch::Receiver<bool>,
    beat_every: Duration,
) {
    let http = stream::stream_http_client();
    let mut backoff = Backoff::new();
    let mut reset = reset_signal();
    let mut reconnect = shared.reconnect_signal();
    let mut failures = 0u32;
    loop {
        if *stop.borrow() {
            return;
        }
        let creds = match crate::api::account::hub_credentials(&shared.ctx) {
            Ok(Some(c)) => Some(c),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "account could not be read; the event stream waits");
                None
            }
        };
        let Some((hub_url, token)) = creds else {
            shared.set_credentials(None);
            shared.set_state(LiveState::SignedOut, None);
            if wait(SIGNED_OUT_RECHECK, &mut stop, &mut reset, &mut reconnect).await {
                return;
            }
            continue;
        };
        shared.set_credentials(Some((hub_url.clone(), token.clone())));
        // A new connection: its `hello` (read by the pump below, never
        // through the runtime's event queue) names the session the beat
        // uses; an older connection's can no longer set it.
        let conn = shared.begin_connection();
        shared.set_state(
            if failures == 0 {
                LiveState::Connecting
            } else {
                LiveState::Reconnecting
            },
            None,
        );
        match stream::open(&http, &hub_url, &token).await {
            Ok(resp) => {
                failures = 0;
                backoff.reset();
                shared.set_state(LiveState::Live, None);
                tracing::info!("event stream connected");
                let client = match CollabClient::new(hub_url.clone()) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::error!(error = %e, "presence client could not be built; this session sends no beat");
                        None
                    }
                };
                let (end_tx, end_rx) = watch::channel(false);
                let beat = client.map(|c| {
                    tokio::spawn(beat_loop(
                        Arc::clone(&shared),
                        Arc::clone(&node),
                        c,
                        end_rx,
                        beat_every,
                    ))
                });
                let (cancel_tx, mut cancel_rx) = watch::channel(false);
                let hello_shared = Arc::clone(&shared);
                let pump = stream::pump_with(resp, &events, &mut cancel_rx, move |ev| {
                    if let LiveEvent::Hello(h) = ev {
                        hello_shared.set_session_for(conn, h.session_id.clone());
                    }
                });
                tokio::pin!(pump);
                let ended = tokio::select! {
                    end = &mut pump => Ended::Stream(end),
                    _ = stop.changed() => {
                        let _ = cancel_tx.send(true);
                        let _ = (&mut pump).await;
                        Ended::Stop
                    }
                    _ = reconnect.changed() => {
                        let _ = cancel_tx.send(true);
                        let _ = (&mut pump).await;
                        Ended::Reconnect
                    }
                };
                let _ = end_tx.send(true);
                if let Some(beat) = beat {
                    if let Err(e) = beat.await {
                        tracing::error!(error = %e, "presence beat task panicked");
                    }
                }
                match ended {
                    Ended::Stop => {
                        tracing::info!(outcome = "stopped", "event stream ended");
                        shared.leave(&hub_url).await;
                        return;
                    }
                    Ended::Reconnect => {
                        tracing::info!(outcome = "reconnect", "event stream ended");
                        continue;
                    }
                    Ended::Stream(StreamEnd::ReceiverGone) => {
                        tracing::info!(outcome = "receiver_gone", "event stream ended");
                        return;
                    }
                    Ended::Stream(end) => {
                        tracing::info!(outcome = ?end, "event stream ended");
                    }
                }
            }
            Err(OpenError::Unauthorized) => {
                tracing::warn!("event stream refused: signed out or device revoked");
                shared.set_state(LiveState::SignedOut, None);
                if wait(SIGNED_OUT_RECHECK, &mut stop, &mut reset, &mut reconnect).await {
                    return;
                }
                continue;
            }
            Err(OpenError::Forbidden) => {
                tracing::error!(refused = true, "event stream refused (403); retried later");
                shared.set_state(LiveState::SignedOut, None);
                if wait(SIGNED_OUT_RECHECK, &mut stop, &mut reset, &mut reconnect).await {
                    return;
                }
                continue;
            }
            Err(OpenError::Outdated) => {
                crate::account::client::warn_collab_api_outdated_once();
                shared.set_state(LiveState::Outdated, None);
                if wait(OUTDATED_RECHECK, &mut stop, &mut reset, &mut reconnect).await {
                    return;
                }
                continue;
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(attempt = failures, error = ?e, "event stream connect failed");
            }
        }
        let delay = backoff.next_delay();
        let state = if failures >= UNREACHABLE_AFTER {
            LiveState::Unreachable
        } else {
            LiveState::Reconnecting
        };
        shared.set_state(state, Some(delay));
        tracing::debug!(
            retry_in_ms = delay.as_millis() as u64,
            "event stream reconnects after its back-off"
        );
        tokio::select! {
            cut = sleep_or_reset(delay, &mut reset) => {
                if cut {
                    backoff.reset();
                }
            }
            _ = stop.changed() => return,
            _ = reconnect.changed() => backoff.reset(),
        }
    }
}

/// Sleep `d`, cut short by a reset (Sync now) or a reconnect request.
/// `true` = stopped.
async fn wait(
    d: Duration,
    stop: &mut watch::Receiver<bool>,
    reset: &mut watch::Receiver<u64>,
    reconnect: &mut watch::Receiver<u64>,
) -> bool {
    tokio::select! {
        _ = sleep_or_reset(d, reset) => false,
        _ = reconnect.changed() => false,
        _ = stop.changed() => true,
    }
}

/// The presence beat of one connection (spec §4.2): waits for the session
/// id `hello` carries, then beats at once, every `beat_every`
/// ([`crate::collab::live::presence::BEAT_INTERVAL`] in production), and on
/// every change of the serving map or the home relay. `session_gone` and a
/// wake from sleep (a wall-clock jump past the monotonic clock, P28) reopen
/// the stream. A failed beat is retried at the next tick — a missed beat is
/// the hub's 40 s rule's business.
async fn beat_loop(
    shared: Arc<Shared>,
    node: Arc<SharedIrohNode>,
    client: CollabClient,
    mut end: watch::Receiver<bool>,
    beat_every: Duration,
) {
    let mut session = shared.session_signal();
    let session_id = loop {
        if let Some(id) = session.borrow_and_update().clone() {
            break id;
        }
        tokio::select! {
            changed = session.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = end.changed() => return,
        }
    };
    let mut serving = shared.serving_signal();
    let mut relay = node.home_relay_watch();
    let mut relay_open = true;
    let mut tick = tokio::time::interval(beat_every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_wall = SystemTime::now();
    let mut last_mono = Instant::now();
    loop {
        tokio::select! {
            _ = end.changed() => return,
            _ = tick.tick() => {}
            changed = serving.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = relay.changed(), if relay_open => {
                if changed.is_err() {
                    relay_open = false;
                    continue;
                }
            }
        }
        let wall = SystemTime::now()
            .duration_since(last_wall)
            .unwrap_or_default();
        let mono = last_mono.elapsed();
        last_wall = SystemTime::now();
        last_mono = Instant::now();
        if woke_from_sleep(wall, mono) {
            tracing::info!(
                duration_ms = wall.as_millis() as u64,
                "woke from sleep; reopening the event stream"
            );
            shared.reconnect_now();
            return;
        }
        let body = BeatWire {
            session_id: session_id.clone(),
            serving: serving.borrow_and_update().clone(),
            relay_url: node.home_relay_url(),
        };
        // M1 (fix round 1): a beat in flight never delays the clean exit's
        // `DELETE /me/presence` — the end cuts it.
        let beat = tokio::select! {
            biased;
            _ = end.changed() => return,
            beat = client.presence_beat(&body) => beat,
        };
        match beat {
            Ok(()) => {}
            Err(AccountClientError::SessionGone) => {
                tracing::info!("presence session gone; reopening the event stream");
                shared.reconnect_now();
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "presence beat failed; retried at the next beat")
            }
        }
    }
}
