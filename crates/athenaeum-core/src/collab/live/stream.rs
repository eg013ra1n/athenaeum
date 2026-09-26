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
    /// Any other non-2xx status, with a body sample (truncated to at most
    /// [`MAX_ERROR_BODY_BYTES`]) for diagnostics.
    Status(u16, String),
    Transport(String),
}

/// Cap on the diagnostic body text carried in [`OpenError::Status`] — enough
/// to read a hub error message, never enough to log an accidental multi-MB
/// error page.
const MAX_ERROR_BODY_BYTES: usize = 512;

/// Truncate to at most `MAX_ERROR_BODY_BYTES`, never splitting inside a
/// UTF-8 character.
fn truncate_body(body: &str) -> String {
    if body.len() <= MAX_ERROR_BODY_BYTES {
        return body.to_string();
    }
    let mut end = MAX_ERROR_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_string()
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
    let status = resp.status().as_u16();
    match status {
        200 => Ok(resp),
        401 => Err(OpenError::Unauthorized),
        403 => Err(OpenError::Forbidden),
        409 => {
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "event stream 409 body read failed");
                String::new()
            });
            if body.contains("collab_api_outdated") {
                Err(OpenError::Outdated)
            } else {
                Err(OpenError::Status(409, truncate_body(&body)))
            }
        }
        s => {
            let body = resp.text().await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, status = s, "event stream error body read failed");
                String::new()
            });
            Err(OpenError::Status(s, truncate_body(&body)))
        }
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
    pump_with(resp, tx, cancel, |_| {}).await
}

/// [`pump`], with `seen` called on every decoded event as it is read —
/// BEFORE the send, which may wait for a busy consumer (Task 15 fix round 2:
/// the session takes a connection's `hello` session id here, so the
/// presence beat never waits behind the event queue).
pub async fn pump_with(
    resp: reqwest::Response,
    tx: &tokio::sync::mpsc::Sender<LiveEvent>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    mut seen: impl FnMut(&LiveEvent),
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
                    let (name, data) = match frame {
                        SseFrame::Event { name, data } => (name, data),
                        SseFrame::TooLarge => {
                            tracing::warn!(
                                "event stream frame exceeded the size cap; ending the stream"
                            );
                            return StreamEnd::ReadError("event too large".to_string());
                        }
                        SseFrame::Comment | SseFrame::Retry(_) => continue,
                    };
                    match decode_event(&name, &data) {
                        Ok(LiveEvent::Unknown(n)) => {
                            tracing::debug!(kind = %n, "unknown event ignored")
                        }
                        Ok(ev) => {
                            seen(&ev);
                            // A full channel (a busy runtime) must not hold
                            // the stream past a cancel (Task 15 R1): the
                            // send races the cancel.
                            tokio::select! {
                                sent = tx.send(ev) => {
                                    if sent.is_err() {
                                        return StreamEnd::ReceiverGone;
                                    }
                                }
                                _ = cancel.changed() => return StreamEnd::Cancelled,
                            }
                        }
                        // `hello` re-establishes the session id and every
                        // project's cursor: losing it silently would run the
                        // rest of the stream against a stale/absent session,
                        // so a bad `hello` ends the connection outright
                        // instead of being merely logged.
                        Err(e) if name == "hello" => {
                            tracing::error!(error = %e, "hello event failed to decode; ending the stream");
                            return StreamEnd::ReadError(format!("hello decode failed: {e}"));
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

// A dropped malformed `project` or `holders` event is safe: the next one's
// `prev` won't match the stored cursor, and the applier resyncs over REST
// (I3). A malformed event of any other kind (`presence`, `account`,
// `resync`, `versions`) carries no `prev` cursor to detect the gap by, so it
// is simply logged and dropped — except `hello` (above), which ends the
// stream instead of leaving the session without one.

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

    #[tokio::test]
    async fn cancel_ends_the_pump_while_a_send_is_blocked() {
        // Two events, a channel of one, nobody reading: the second send
        // blocks. A cancel must still end the pump (Task 15 R1).
        let body = concat!(
            "event: versions\n",
            "data: {\"p1\":[1,2]}\n\n",
            "event: versions\n",
            "data: {\"p1\":[3,4]}\n\n",
        );
        let base = serve_fixed_body(body).await;
        let client = stream_http_client();
        let resp = open(&client, &base, "irrelevant").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let pumping = tokio::spawn(async move { pump(resp, &tx, &mut cancel).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!pumping.is_finished(), "the second send is blocked");
        cancel_tx.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pumping)
                .await
                .expect("the pump observes the cancel while a send is blocked")
                .unwrap(),
            StreamEnd::Cancelled
        );
    }

    /// A tiny axum server answering `/api/v1/me/events` with one fixed,
    /// finite body — for pinning `pump`'s per-frame decisions against exact
    /// bytes without driving the fake hub's whole state machine. Returns the
    /// base url `open` expects.
    async fn serve_fixed_body(body: &'static str) -> String {
        let app = axum::Router::new().route(
            "/api/v1/me/events",
            axum::routing::get(move || async move {
                axum::response::Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(body))
                    .expect("static response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixed-body server");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn unknown_and_malformed_non_hello_events_do_not_end_the_pump() {
        let body = concat!(
            "event: hello\n",
            "data: {\"sessionId\":\"s\",\"epoch\":\"e\",\"accountId\":\"a\",\"projects\":{}}\n\n",
            "event: future\n",
            "data: {}\n\n",
            "event: project\n",
            "data: not json\n\n",
            "event: versions\n",
            "data: {\"p1\":[1,2]}\n\n",
        );
        let base = serve_fixed_body(body).await;
        let client = stream_http_client();
        let resp = open(&client, &base, "irrelevant").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let end = tokio::time::timeout(Duration::from_secs(5), pump(resp, &tx, &mut cancel))
            .await
            .unwrap();
        // The unknown `future` event and the malformed `project` event were
        // both simply dropped — the stream ran to its natural, finite end
        // rather than aborting on either one.
        assert_eq!(end, StreamEnd::Closed);
        let hello = rx.try_recv().unwrap();
        assert!(matches!(hello, LiveEvent::Hello(_)));
        let versions = rx.try_recv().unwrap();
        assert!(matches!(versions, LiveEvent::Versions(_)));
        assert!(
            rx.try_recv().is_err(),
            "future/project must not be forwarded"
        );
    }

    #[tokio::test]
    async fn a_malformed_hello_ends_the_stream_with_read_error() {
        let body = "event: hello\ndata: not json\n\n";
        let base = serve_fixed_body(body).await;
        let client = stream_http_client();
        let resp = open(&client, &base, "irrelevant").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let end = tokio::time::timeout(Duration::from_secs(5), pump(resp, &tx, &mut cancel))
            .await
            .unwrap();
        assert!(matches!(end, StreamEnd::ReadError(_)), "{end:?}");
    }

    #[tokio::test]
    async fn an_oversized_frame_ends_the_pump_with_read_error() {
        let huge = "a".repeat(crate::collab::live::sse::MAX_DATA_BYTES + 1);
        let body: String = format!("event: hello\ndata: {huge}\n\n");
        let base = serve_fixed_body(Box::leak(body.into_boxed_str())).await;
        let client = stream_http_client();
        let resp = open(&client, &base, "irrelevant").await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let end = tokio::time::timeout(Duration::from_secs(10), pump(resp, &tx, &mut cancel))
            .await
            .unwrap();
        assert_eq!(end, StreamEnd::ReadError("event too large".to_string()));
    }
}
