//! `athenaeum-mcp`: the bridge an assistant (an MCP client) starts as a program and talks to over stdio.
//!
//! It pipes the client's newline-delimited JSON-RPC to the running app's local channel after a token handshake
//! (`ATHENAEUM-MCP/1 <token>` / `OK <server name> <version>` or `ERR <starting|off|token|version> <message>`), and
//! whenever the app cannot answer (not running, off, starting, key changed, outdated, the other build) it answers in
//! words itself with a tiny rmcp server ([`FallbackServer`]) so the person reads the reason instead of a transport error.
//! It never opens the catalog and links no `athenaeum-core`: the handshake and the channel files are a wire contract,
//! pinned by `tests/bridge_e2e.rs` against the real host. stdout is the protocol; logs go to stderr and never carry
//! the key.
//!
//! The bridge reads `<app-data>/mcp/{enabled,token,endpoint,tools.json}` at EVERY connect attempt (a Windows pipe name
//! is new at every bind, and the key can be regenerated). While the app does not answer, the next request that arrives
//! two seconds or more after the last attempt tries again, and on success replays the client's `initialize` into the
//! new session and tells the client its tool list may have changed.

pub mod fallback;
pub mod paths;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub use fallback::{unavailable_text, FallbackServer, Unavailable, STATUS_TOOL};
pub use paths::{app_data_dir, parse_args, BridgeOptions, Build};

/// The first word of the handshake line; the version after the slash is the channel protocol's (frozen, append-only).
const HANDSHAKE_PREFIX: &str = "ATHENAEUM-MCP/1";
/// How long the app has to answer the handshake.
const HANDSHAKE_WINDOW: Duration = Duration::from_secs(5);
/// The longest handshake reply read; the real ones are well under a hundred bytes.
const REPLY_MAX_BYTES: usize = 1024;
/// While the app does not answer, a request arriving at least this long after the last attempt tries again.
const RETRY_AFTER: Duration = Duration::from_secs(2);
/// A connection lost this soon after it was made does not get an immediate new attempt (no tight reconnect loop).
const FLAP_WINDOW: Duration = Duration::from_millis(500);
/// How long the last answers get to reach a client that has closed its input.
const DRAIN_WINDOW: Duration = Duration::from_secs(2);
/// The id of the `initialize` the bridge replays into a new session; its reply never reaches the client.
const REPLAY_ID: &str = "athenaeum-bridge-replay";
const LIST_CHANGED: &[u8] = br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}

// ---- the app's channel (a copy of core's `Endpoint` parsing) ----

/// Where the app listens: the `endpoint` file's text is `unix:<path>` or `pipe:<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Endpoint {
    Unix(PathBuf),
    Pipe(String),
}

impl Endpoint {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(path) = text.strip_prefix("unix:") {
            return (!path.is_empty()).then(|| Self::Unix(PathBuf::from(path)));
        }
        if let Some(name) = text.strip_prefix("pipe:") {
            return (!name.is_empty()).then(|| Self::Pipe(name.to_string()));
        }
        None
    }
}

#[cfg(unix)]
async fn open(endpoint: &Endpoint) -> io::Result<Box<dyn Duplex>> {
    match endpoint {
        Endpoint::Unix(path) => Ok(Box::new(tokio::net::UnixStream::connect(path).await?)),
        Endpoint::Pipe(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "a named pipe endpoint on a Unix machine")),
    }
}

#[cfg(windows)]
async fn open(endpoint: &Endpoint) -> io::Result<Box<dyn Duplex>> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    match endpoint {
        Endpoint::Pipe(name) => {
            let mut tries = 0;
            loop {
                match ClientOptions::new().open(name) {
                    Ok(client) => return Ok(Box::new(client)),
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Endpoint::Unix(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "a Unix socket endpoint on a Windows machine")),
    }
}

/// Reads one reply line byte by byte, so nothing after it is consumed (after `OK` the bytes are MCP).
async fn read_reply<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<String> {
    let mut line = Vec::with_capacity(64);
    loop {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        if line.len() >= REPLY_MAX_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the handshake reply is too long"));
        }
        line.push(byte);
    }
}

/// What a handshake reply means for a bridge set up for `build`.
fn judge_reply(reply: &str, build: Build) -> Result<(), Unavailable> {
    let mut words = reply.trim_end().splitn(3, ' ');
    match words.next() {
        Some("OK") => {
            let name = words.next().unwrap_or("");
            if name == build.server_name() {
                Ok(())
            } else {
                let reached: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
                Err(Unavailable::WrongBuild { reached: if reached.is_empty() { "another program".to_string() } else { reached } })
            }
        }
        Some("ERR") => match words.next() {
            Some("starting") => Err(Unavailable::Starting),
            Some("off") => Err(Unavailable::Off),
            Some("token") => Err(Unavailable::TokenRejected),
            Some("version") => Err(Unavailable::Outdated),
            other => {
                tracing::warn!(reason = other.unwrap_or(""), "app refused the handshake for a reason this bridge does not know");
                Err(Unavailable::NotRunning)
            }
        },
        _ => {
            tracing::warn!("the channel did not answer like Athenaeum");
            Err(Unavailable::NotRunning)
        }
    }
}

/// Reads the channel files and handshakes with the app: a live stream, or why not.
async fn connect(opts: &BridgeOptions) -> Result<(BoxRead, BoxWrite), Unavailable> {
    let build = opts.build;
    let Some(app_data) = app_data_dir(opts) else {
        tracing::warn!(build = build.server_name(), "the app data folder is not known; pass --app-data-dir");
        return Err(Unavailable::NotRunning);
    };
    let dir = app_data.join("mcp");
    if !dir.join("enabled").is_file() {
        return Err(Unavailable::Off);
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).map(|t| t.trim().to_string()).ok().filter(|t| !t.is_empty());
    let (Some(token), Some(endpoint_text)) = (read("token"), read("endpoint")) else {
        return Err(Unavailable::NotRunning);
    };
    let Some(endpoint) = Endpoint::parse(&endpoint_text) else {
        tracing::warn!(path = %dir.join("endpoint").display(), "the endpoint file does not parse");
        return Err(Unavailable::NotRunning);
    };
    let mut stream = match tokio::time::timeout(HANDSHAKE_WINDOW, open(&endpoint)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "the app's channel did not open");
            return Err(Unavailable::NotRunning);
        }
        Err(_) => {
            tracing::debug!(reason = "timeout", "the app's channel did not open");
            return Err(Unavailable::NotRunning);
        }
    };
    let hello = format!("{HANDSHAKE_PREFIX} {token}\n");
    let reply = tokio::time::timeout(HANDSHAKE_WINDOW, async {
        stream.write_all(hello.as_bytes()).await?;
        stream.flush().await?;
        read_reply(&mut stream).await
    })
    .await;
    let reply = match reply {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "the app closed the channel during the handshake");
            return Err(Unavailable::NotRunning);
        }
        Err(_) => {
            tracing::debug!(reason = "timeout", "the app did not answer the handshake");
            return Err(Unavailable::NotRunning);
        }
    };
    judge_reply(&reply, build)?;
    let (reader, writer) = tokio::io::split(stream);
    Ok((Box::new(reader), Box::new(writer)))
}

// ---- one side of the pipe: lines in, lines out ----

/// Writes each queued line and a newline, flushing every one; ends when the queue closes or a write fails (and says so).
/// Lines are bytes: the bridge never needs a line to be text.
async fn write_lines(mut out: BoxWrite, mut queue: mpsc::UnboundedReceiver<Vec<u8>>, to: &'static str) {
    while let Some(mut line) = queue.recv().await {
        line.push(b'\n');
        let written = match out.write_all(&line).await {
            Ok(()) => out.flush().await,
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            tracing::warn!(error = %e, to, "a write failed; this direction ends");
            break;
        }
    }
    let _ = out.shutdown().await;
}

/// Reads newline-ended lines as bytes (a client that sends something that is not UTF-8 is passed through, not refused).
/// A line cut short by a cancelled `next_line` stays in `buf` and is continued by the next call.
struct ByteLines {
    reader: BufReader<BoxRead>,
    buf: Vec<u8>,
}

impl ByteLines {
    fn new(read: BoxRead) -> Self {
        Self { reader: BufReader::new(read), buf: Vec::new() }
    }

    /// The next line without its `\n`, or `None` at the end of the stream.
    async fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.reader.read_until(b'\n', &mut self.buf).await?;
        if self.buf.last() == Some(&b'\n') {
            self.buf.pop();
            return Ok(Some(std::mem::take(&mut self.buf)));
        }
        // The stream ended: a last line without its newline is still a line.
        Ok((!self.buf.is_empty()).then(|| std::mem::take(&mut self.buf)))
    }
}

/// A peer's two directions. Writes go through a queue to a task of their own, so a peer that is slow to read can never
/// stop the bridge from reading the other side.
struct Link {
    lines: ByteLines,
    queue: mpsc::UnboundedSender<Vec<u8>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Link {
    fn new(read: BoxRead, write: BoxWrite, mut tasks: Vec<JoinHandle<()>>) -> Self {
        let (queue, rx) = mpsc::unbounded_channel();
        tasks.push(tokio::spawn(write_lines(write, rx, "the app")));
        Self { lines: ByteLines::new(read), queue, tasks }
    }

    /// Queues a line; `false` when the peer's writer has ended.
    fn send(&self, line: Vec<u8>) -> bool {
        self.queue.send(line).is_ok()
    }

    /// Drops both directions and waits for the tasks to end, so no stream outlives this call.
    async fn close(self) {
        let Self { lines, queue, tasks } = self;
        drop(queue);
        drop(lines);
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// Who answers the client now.
enum Side {
    /// The running app.
    Live,
    /// The bridge's own server, saying why the app does not.
    Down(Unavailable),
}

struct Peer {
    side: Side,
    link: Link,
}

// ---- JSON-RPC, as much as the bridge needs ----

struct Shape {
    id: Option<Value>,
    /// `error.message` of a response that carries an `error` (`Some("")` when it has none).
    error: Option<String>,
    method: Option<String>,
    /// `params.requestId` of a `notifications/cancelled`.
    cancelled: Option<Value>,
}

impl Shape {
    /// What the bridge needs to know of a line; `None` when it is not a JSON object (it is passed on all the same).
    fn of(line: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(line).ok()?;
        let object = value.as_object()?;
        let method = object.get("method").and_then(Value::as_str).map(str::to_string);
        let cancelled = (method.as_deref() == Some("notifications/cancelled"))
            .then(|| object.get("params").and_then(|p| p.get("requestId")).cloned())
            .flatten();
        let error = object.get("error").map(|e| e.get("message").and_then(Value::as_str).unwrap_or("").chars().take(200).collect());
        Some(Self { id: object.get("id").filter(|id| !id.is_null()).cloned(), error, method, cancelled })
    }

    fn is_request(&self) -> bool {
        self.id.is_some() && self.method.is_some()
    }

    fn is_response(&self) -> bool {
        self.id.is_some() && self.method.is_none()
    }
}

/// A request the peer has not answered yet.
struct Pending {
    line: Vec<u8>,
    is_initialize: bool,
}

struct Bridge {
    opts: BridgeOptions,
    client: mpsc::UnboundedSender<Vec<u8>>,
    peer: Peer,
    /// The client's `initialize` request and `notifications/initialized`, kept to replay into a new session.
    initialize: Option<Vec<u8>>,
    initialized: Option<Vec<u8>>,
    /// Requests sent and not answered, by id (as JSON text), in the order they were sent.
    inflight: BTreeMap<u64, (String, Pending)>,
    next_seq: u64,
    /// When the last connect attempt ENDED (an attempt that hangs for the handshake window must not make the next
    /// request look overdue for another one).
    last_try: Instant,
}

impl Bridge {
    async fn start(opts: BridgeOptions, client: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        let attempt = connect(&opts).await;
        let last_try = Instant::now();
        let peer = match attempt {
            Ok((read, write)) => {
                tracing::info!(build = opts.build.server_name(), "connected to the app");
                Peer { side: Side::Live, link: Link::new(read, write, Vec::new()) }
            }
            Err(reason) => {
                tracing::info!(build = opts.build.server_name(), state = reason.state(), "the app does not answer; answering in words");
                fallback_peer(&opts, reason)
            }
        };
        Self { opts, client, peer, initialize: None, initialized: None, inflight: BTreeMap::new(), next_seq: 0, last_try }
    }

    fn to_client(&self, line: Vec<u8>) {
        let _ = self.client.send(line);
    }

    async fn on_client_line(&mut self, line: Vec<u8>) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let shape = Shape::of(&line);
        let is_request = shape.as_ref().is_some_and(Shape::is_request);
        if is_request && matches!(self.peer.side, Side::Down(_)) && self.last_try.elapsed() >= RETRY_AFTER {
            self.try_again().await;
        }
        if let Some(shape) = &shape {
            match shape.method.as_deref() {
                Some("initialize") if is_request => self.initialize = Some(line.clone()),
                Some("notifications/initialized") => self.initialized = Some(line.clone()),
                _ => {}
            }
            if let Some(cancelled) = &shape.cancelled {
                let key = cancelled.to_string();
                self.inflight.retain(|_, (id, _)| *id != key);
            }
            if is_request {
                if let Some(id) = &shape.id {
                    let pending = Pending { line: line.clone(), is_initialize: shape.method.as_deref() == Some("initialize") };
                    self.inflight.insert(self.next_seq, (id.to_string(), pending));
                    self.next_seq += 1;
                }
            }
        }
        if !self.peer.link.send(line) {
            self.peer_lost().await;
        }
    }

    async fn on_peer_line(&mut self, line: io::Result<Option<Vec<u8>>>) {
        match line {
            Ok(Some(line)) => {
                if let Some(shape) = Shape::of(&line) {
                    if shape.is_response() {
                        let id = shape.id.unwrap_or(Value::Null);
                        if id.as_str() == Some(REPLAY_ID) {
                            // The replay's answer is the bridge's own business, but a refusal is worth a line.
                            if let Some(message) = &shape.error {
                                tracing::warn!(error = %message, "the app refused the replayed initialize");
                            }
                            return;
                        }
                        let key = id.to_string();
                        self.inflight.retain(|_, (pending_id, _)| *pending_id != key);
                    }
                }
                self.to_client(line);
            }
            Ok(None) => self.peer_lost().await,
            Err(e) => {
                tracing::debug!(error = %e, "the peer's stream failed");
                self.peer_lost().await;
            }
        }
    }

    /// While the app does not answer: look again. A live app takes over; another reason replaces the words.
    async fn try_again(&mut self) {
        let attempt = connect(&self.opts).await;
        self.last_try = Instant::now();
        match attempt {
            Ok((read, write)) => {
                tracing::info!(build = self.opts.build.server_name(), "connected to the app");
                self.install(Peer { side: Side::Live, link: Link::new(read, write, Vec::new()) }, true).await;
            }
            Err(reason) => {
                let same = matches!(&self.peer.side, Side::Down(current) if *current == reason);
                if !same {
                    tracing::info!(build = self.opts.build.server_name(), state = reason.state(), "the app still does not answer");
                    let peer = fallback_peer(&self.opts, reason);
                    self.install(peer, false).await;
                }
            }
        }
    }

    /// The peer's stream ended or failed.
    async fn peer_lost(&mut self) {
        match &self.peer.side {
            Side::Down(reason) => {
                // The bridge's own server ended: it is not a transport to retry. Answer what it owed, start a fresh one.
                let reason = reason.clone();
                tracing::warn!(state = reason.state(), "the bridge's own server ended");
                let text = unavailable_text(&reason, self.opts.build);
                for (_, (id, _)) in std::mem::take(&mut self.inflight) {
                    let id: Value = serde_json::from_str(&id).unwrap_or(Value::Null);
                    let error = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": text } });
                    self.to_client(error.to_string().into_bytes());
                }
                let peer = fallback_peer(&self.opts, reason);
                self.install(peer, false).await;
            }
            Side::Live => {
                tracing::info!(build = self.opts.build.server_name(), "the app closed the connection");
                let flapping = self.last_try.elapsed() < FLAP_WINDOW;
                let attempt = if flapping { Err(Unavailable::NotRunning) } else { connect(&self.opts).await };
                self.last_try = Instant::now();
                match attempt {
                    Ok((read, write)) => {
                        tracing::info!(build = self.opts.build.server_name(), "connected to the app");
                        self.install(Peer { side: Side::Live, link: Link::new(read, write, Vec::new()) }, true).await;
                    }
                    Err(reason) => {
                        tracing::info!(build = self.opts.build.server_name(), state = reason.state(), "the app does not answer; answering in words");
                        let peer = fallback_peer(&self.opts, reason);
                        self.install(peer, false).await;
                    }
                }
            }
        }
    }

    /// Makes `peer` the one that answers: replays the client's `initialize` into it (its reply is dropped), resends every
    /// request still unanswered and, for a live app, tells the client its tool list may have changed.
    async fn install(&mut self, peer: Peer, announce: bool) {
        let old = std::mem::replace(&mut self.peer, peer);
        old.link.close().await;
        // An `initialize` still waiting for its reply is resent with the rest, under its own id.
        let waiting = self.inflight.values().any(|(_, pending)| pending.is_initialize);
        if let (Some(init), false) = (&self.initialize, waiting) {
            match serde_json::from_slice::<Value>(init) {
                Ok(mut value) => {
                    value["id"] = json!(REPLAY_ID);
                    self.peer.link.send(value.to_string().into_bytes());
                    if let Some(done) = &self.initialized {
                        self.peer.link.send(done.clone());
                    }
                }
                Err(e) => tracing::warn!(error = %e, "the kept initialize request does not parse"),
            }
        }
        for (_, pending) in self.inflight.values() {
            self.peer.link.send(pending.line.clone());
        }
        // A client whose `initialize` is still unanswered has not been told the server is up: the new session answers
        // that `initialize`, and a list-changed before it would reach the client too early.
        if announce && !waiting {
            self.to_client(LIST_CHANGED.to_vec());
        }
    }

    async fn finish(self) {
        self.peer.link.close().await;
    }
}

/// The bridge's own rmcp server behind an in-memory pipe, saying `reason`.
fn fallback_peer(opts: &BridgeOptions, reason: Unavailable) -> Peer {
    let (near, far) = tokio::io::duplex(1 << 20);
    let server = FallbackServer {
        reason: reason.clone(),
        build: opts.build,
        tools_file: app_data_dir(opts).map(|dir| dir.join("mcp").join("tools.json")),
    };
    let task = tokio::spawn(async move {
        match rmcp::serve_server(server, far).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::debug!(error = %e, "the bridge's own session ended before it started"),
        }
    });
    let (read, write) = tokio::io::split(near);
    Peer { side: Side::Down(reason), link: Link::new(Box::new(read), Box::new(write), vec![task]) }
}

/// Runs the bridge until the client closes its input (then `Ok`): every line from `client_in` goes to the app, or to the
/// bridge's own server while the app does not answer; every line back goes to `client_out`.
pub async fn run_bridge<R, W>(opts: BridgeOptions, client_in: R, client_out: W) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (to_client, queue) = mpsc::unbounded_channel();
    let writer = tokio::spawn(write_lines(Box::new(client_out), queue, "the client"));
    let mut from_client = ByteLines::new(Box::new(client_in));
    let mut bridge = Bridge::start(opts, to_client).await;
    let outcome = loop {
        tokio::select! {
            line = from_client.next_line() => match line {
                Ok(Some(line)) => bridge.on_client_line(line).await,
                Ok(None) => break Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "the client's input failed");
                    break Err(e);
                }
            },
            line = bridge.peer.link.lines.next_line() => bridge.on_peer_line(line).await,
        }
    };
    drop(from_client);
    bridge.finish().await;
    // The last answers still queued for the client get a moment to leave.
    let mut writer = writer;
    if tokio::time::timeout(DRAIN_WINDOW, &mut writer).await.is_err() {
        writer.abort();
        let _ = writer.await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_map_to_reasons() {
        assert_eq!(judge_reply("OK athenaeum 0.7.1", Build::Release), Ok(()));
        assert_eq!(judge_reply("OK athenaeum-dev 0.7.1", Build::Dev), Ok(()));
        assert_eq!(judge_reply("OK athenaeum 0.7.1", Build::Dev), Err(Unavailable::WrongBuild { reached: "athenaeum".to_string() }));
        assert_eq!(judge_reply("ERR starting wait", Build::Dev), Err(Unavailable::Starting));
        assert_eq!(judge_reply("ERR off it is off", Build::Dev), Err(Unavailable::Off));
        assert_eq!(judge_reply("ERR token no", Build::Dev), Err(Unavailable::TokenRejected));
        assert_eq!(judge_reply("ERR version old", Build::Dev), Err(Unavailable::Outdated));
        assert_eq!(judge_reply("ERR what is this", Build::Dev), Err(Unavailable::NotRunning));
        assert_eq!(judge_reply("hello", Build::Dev), Err(Unavailable::NotRunning));
    }

    #[test]
    fn endpoint_text_parses_like_the_hosts() {
        assert_eq!(Endpoint::parse("unix:/tmp/a/mcp.sock\n"), Some(Endpoint::Unix(PathBuf::from("/tmp/a/mcp.sock"))));
        assert_eq!(Endpoint::parse(r"pipe:\\.\pipe\athenaeum-mcp-x"), Some(Endpoint::Pipe(r"\\.\pipe\athenaeum-mcp-x".to_string())));
        assert_eq!(Endpoint::parse("unix:"), None);
        assert_eq!(Endpoint::parse("tcp:1"), None);
    }

    #[test]
    fn shapes_tell_requests_from_responses() {
        let request = Shape::of(br#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).expect("parses");
        assert!(request.is_request() && !request.is_response());
        let response = Shape::of(br#"{"jsonrpc":"2.0","id":"x","result":{}}"#).expect("parses");
        assert!(response.is_response());
        let note = Shape::of(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).expect("parses");
        assert!(!note.is_request() && !note.is_response());
        assert!(Shape::of(b"not json").is_none());
        assert!(Shape::of(b"\xff\xfe").is_none());
        let refusal = Shape::of(br#"{"jsonrpc":"2.0","id":"x","error":{"code":-32600,"message":"no"}}"#).expect("parses");
        assert!(refusal.is_response() && refusal.error.as_deref() == Some("no"));
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn lines_are_bytes() {
        let input: &[u8] = b"{\"a\":1}\n\xff\xfe\x80\n\nlast without newline";
        let mut lines = ByteLines::new(Box::new(input));
        assert_eq!(lines.next_line().await.expect("a line"), Some(b"{\"a\":1}".to_vec()));
        assert_eq!(lines.next_line().await.expect("a line"), Some(b"\xff\xfe\x80".to_vec()));
        assert_eq!(lines.next_line().await.expect("a line"), Some(Vec::new()));
        assert_eq!(lines.next_line().await.expect("a line"), Some(b"last without newline".to_vec()));
        assert_eq!(lines.next_line().await.expect("the end"), None);
    }
}
