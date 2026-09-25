//! The dedicated streaming client for `GET /me/events` (spec §4.1, plan P2).
//! The hub client's 30 s total deadline cannot carry a stream, so this one
//! has none: a per-read timeout of 2.5 × the hub's keepalive detects a dead
//! stream, and the session (api::collab_live::session) reconnects with the
//! full-jitter back-off.

use std::time::Duration;

use n0_future::StreamExt as _;

use super::sse::{SseFrame, SseParser};
use super::wire::{decode_event, LiveEvent};

pub const HUB_KEEPALIVE: Duration = Duration::from_secs(20);
pub const READ_TIMEOUT: Duration = Duration::from_secs(50);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SILENT_STREAM_WARN: Duration = Duration::from_secs(25);

/// A client dedicated to the event stream: a per-read timeout instead of the
/// hub client's total-deadline shape, since a stream has no natural end.
pub fn stream_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "event stream client build failed; using defaults");
            reqwest::Client::new()
        })
}

#[derive(Debug)]
pub enum OpenError {
    Unauthorized,
    Forbidden,
    Outdated,
    Status(u16),
    Transport(String),
}

/// Open the event stream. `token` is the device's bearer token; a 401 is the
/// re-authentication path, 403 never happens for the app (perseus-only), and
/// `Outdated` means the running build's collab api has aged out.
pub async fn open(
    client: &reqwest::Client,
    hub_url: &str,
    token: &str,
) -> Result<reqwest::Response, OpenError> {
    let url = format!("{}/api/v1/me/events", hub_url.trim_end_matches('/'));
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| OpenError::Transport(e.to_string()))?;
    match resp.status().as_u16() {
        200 => Ok(resp),
        401 => Err(OpenError::Unauthorized),
        403 => Err(OpenError::Forbidden),
        409 => {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("collab_api_outdated") {
                Err(OpenError::Outdated)
            } else {
                Err(OpenError::Status(409))
            }
        }
        s => Err(OpenError::Status(s)),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamEnd {
    Closed,
    ReadError(String),
    Cancelled,
    ReceiverGone,
}

/// Read the stream until it ends, sending every decoded event. Keepalive
/// comments reset the read timeout inside reqwest and are not forwarded.
pub async fn pump(
    resp: reqwest::Response,
    tx: &tokio::sync::mpsc::Sender<LiveEvent>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> StreamEnd {
    let mut body = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut got_byte = false;
    let silent = tokio::time::sleep(SILENT_STREAM_WARN);
    tokio::pin!(silent);
    loop {
        let chunk = tokio::select! {
            _ = cancel.changed() => return StreamEnd::Cancelled,
            _ = &mut silent, if !got_byte => {
                tracing::warn!(duration_ms = SILENT_STREAM_WARN.as_millis() as u64, "event stream connected but silent; a buffering proxy may sit in between");
                got_byte = true; // warn once
                continue;
            }
            c = body.next() => c,
        };
        match chunk {
            None => return StreamEnd::Closed,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "event stream read failed");
                return StreamEnd::ReadError(e.to_string());
            }
            Some(Ok(bytes)) => {
                got_byte = true;
                for frame in parser.push(&bytes) {
                    let SseFrame::Event { name, data } = frame else {
                        continue;
                    };
                    match decode_event(&name, &data) {
                        Ok(LiveEvent::Unknown(n)) => {
                            tracing::debug!(kind = %n, "unknown event ignored")
                        }
                        Ok(ev) => {
                            if tx.send(ev).await.is_err() {
                                return StreamEnd::ReceiverGone;
                            }
                        }
                        Err(e) => {
                            tracing::error!(kind = %name, error = %e, "event failed to decode; dropped")
                        }
                    }
                }
            }
        }
    }
}

// A dropped malformed event is safe: the next event's `prev` mismatches and
// the applier catches up over REST (I3).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::fake_hub::{FakeHub, FakeTimings};
    use crate::collab::live::wire::LiveEvent;

    #[tokio::test]
    async fn opens_reads_hello_and_ends_when_the_hub_closes() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        hub.add_project("p1", "m31", &[("acc", "send_receive", false)], false);
        hub.set_timings(FakeTimings {
            keepalive: Duration::from_millis(200),
            grace: Duration::from_millis(200),
            silence: Duration::from_secs(40),
        });
        let client = stream_http_client();
        let resp = open(&client, &hub.uri(), "tok").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let pumping = tokio::spawn(async move { pump(resp, &tx, &mut cancel).await });
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(first, LiveEvent::Hello(ref h) if h.projects.contains_key("p1")));
        hub.kill_streams();
        let end = tokio::time::timeout(Duration::from_secs(5), pumping)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(end, StreamEnd::Closed);
    }

    #[tokio::test]
    async fn open_types_401_and_409() {
        let hub = FakeHub::start().await;
        let client = stream_http_client();
        assert!(matches!(
            open(&client, &hub.uri(), "nobody").await,
            Err(OpenError::Unauthorized)
        ));
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        hub.set_api_outdated(true);
        assert!(matches!(
            open(&client, &hub.uri(), "tok").await,
            Err(OpenError::Outdated)
        ));
    }

    #[tokio::test]
    async fn cancel_ends_the_pump() {
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc", "Me", "AAA=", None);
        let client = stream_http_client();
        let resp = open(&client, &hub.uri(), "tok").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let pumping = tokio::spawn(async move { pump(resp, &tx, &mut cancel).await });
        cancel_tx.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pumping)
                .await
                .unwrap()
                .unwrap(),
            StreamEnd::Cancelled
        );
    }
}
