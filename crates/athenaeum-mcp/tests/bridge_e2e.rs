//! The bridge against real hosts (`athenaeum-core`'s `McpHost` in temp app-data dirs, enabled through core's own API), spoken
//! to by an rmcp client over `tokio::io::duplex` pipes in front of `run_bridge`. The same assertions on every OS: the only
//! platform-specific code is the bridge's own connect call.
//!
//! Every test ends by closing its client sessions (the bridge exits on its input's end of stream) and the hosts, so no
//! handle outlives its temp dir.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use athenaeum_core::events::NullEmitter;
use athenaeum_core::mcp::channel::{bind, mcp_dir, remove_endpoint};
use athenaeum_core::mcp::{mcp_server_name, HostKind, McpHost, McpIdentity};
use athenaeum_core::paths::{APP_DATA_DIR_NAME_DEV, APP_DATA_DIR_NAME_RELEASE};
use athenaeum_core::services::ServiceContext;
use athenaeum_mcp::{app_data_dir, parse_args, run_bridge, unavailable_text, BridgeOptions, Build, Unavailable, STATUS_TOOL};
use rmcp::handler::client::ClientHandler;
use rmcp::model::{CallToolRequestParams, CallToolResult, ProtocolVersion};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{ClientLifecycleMode, RoleClient};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

const VERSION: &str = "9.9.9-test";
const KEY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const KEY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// ---- the host side ----

struct Rig {
    host: Arc<McpHost>,
    ctx: Arc<ServiceContext>,
    base: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    /// The tools the app offers, by name, from the app's own list (`tools.json` is this same text): a new tool never
    /// leaves a stale list here.
    fn tools(&self) -> Vec<String> {
        let listed: Value = serde_json::from_str(&self.host.tool_list_json()).expect("the tool list is JSON");
        let mut names: Vec<String> = listed["tools"]
            .as_array()
            .expect("a tool list")
            .iter()
            .map(|t| t["name"].as_str().expect("a tool name").to_string())
            .collect();
        names.sort();
        names
    }

    fn dir(&self) -> PathBuf {
        mcp_dir(&self.base)
    }

    fn options(&self, build: Build) -> BridgeOptions {
        BridgeOptions { build, app_data_dir: Some(self.base.clone()) }
    }

    /// Turns assistant access on, as the Settings page does.
    fn enable(&self) {
        self.host.set_enabled(true).expect("assistant access turns on");
    }

    /// Stops serving and gives the connection tasks a moment to end, so no open catalog outlives the temp dir.
    async fn finish(self) {
        self.host.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(self.host);
        drop(self.ctx);
    }
}

/// A host named `server_name` over a fresh catalog; its catalog is open, assistant access is still off.
fn rig(server_name: &'static str) -> Rig {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let base = tmp.path().to_path_buf();
    let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
    let identity = McpIdentity { server_name, version: VERSION, kind: HostKind::Desktop, web_api_key_set: false };
    let host = McpHost::new(Arc::clone(&ctx), identity, Arc::new(NullEmitter), Arc::new(|| {}), Arc::new(|| true));
    host.on_catalog_open().expect("the catalog opens");
    Rig { host, ctx, base, _tmp: tmp }
}

fn dev_rig() -> Rig {
    rig(mcp_server_name())
}

/// What a bridge needs of a dir where no app runs: the `enabled` marker and the key (no `endpoint`).
fn write_marker_and_key(rig: &Rig) {
    std::fs::create_dir_all(rig.dir()).expect("the channel folder");
    std::fs::write(rig.dir().join("enabled"), "1").expect("the marker");
    std::fs::write(rig.dir().join("token"), KEY_A).expect("the key");
}

// ---- the client side ----

#[derive(Clone, Default)]
struct Probe {
    changed: Arc<AtomicUsize>,
}

impl ClientHandler for Probe {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }
}

/// An rmcp client in front of one `run_bridge`.
struct Session {
    client: RunningService<RoleClient, Probe>,
    bridge: JoinHandle<io::Result<()>>,
    changed: Arc<AtomicUsize>,
}

fn stateless() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }
}

async fn session(opts: BridgeOptions, mode: ClientLifecycleMode) -> Session {
    let (bridge_in, client_out) = tokio::io::duplex(1 << 20);
    let (client_in, bridge_out) = tokio::io::duplex(1 << 20);
    let bridge = tokio::spawn(run_bridge(opts, bridge_in, bridge_out));
    let probe = Probe::default();
    let changed = Arc::clone(&probe.changed);
    let client = tokio::time::timeout(Duration::from_secs(10), rmcp::serve_client_with_lifecycle(probe, (client_in, client_out), mode))
        .await
        .expect("the client connects in time")
        .expect("the client connects");
    Session { client, bridge, changed }
}

async fn legacy(opts: BridgeOptions) -> Session {
    session(opts, ClientLifecycleMode::Initialize).await
}

impl Session {
    async fn tool_names(&self) -> Vec<String> {
        let tools = tokio::time::timeout(Duration::from_secs(10), self.client.list_all_tools())
            .await
            .expect("tools/list answers in time")
            .expect("tools/list answers");
        let mut names: Vec<String> = tools.into_iter().map(|t| t.name.to_string()).collect();
        names.sort();
        names
    }

    async fn ask(&self, tool: &str) -> CallToolResult {
        let params = CallToolRequestParams::new(tool.to_string()).with_arguments(serde_json::Map::new());
        tokio::time::timeout(Duration::from_secs(10), self.client.call_tool(params))
            .await
            .expect("the call is answered in time")
            .expect("the call is answered")
    }

    /// Asks `query_frames` until the app itself answers (the bridge looks again two seconds after its last try).
    async fn ask_until_the_app_answers(&self) -> CallToolResult {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let answer = self.ask("query_frames").await;
            if answer.is_error != Some(true) {
                return answer;
            }
            assert!(tokio::time::Instant::now() < deadline, "the app never answered; the bridge said: {}", text_of(&answer));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Ends the conversation: the bridge sees its input end and exits with `Ok`.
    async fn close(self) {
        let _ = self.client.cancel().await;
        tokio::time::timeout(Duration::from_secs(5), self.bridge)
            .await
            .expect("the bridge ends when its client goes")
            .expect("the bridge task joins")
            .expect("the bridge ends without an error");
    }
}

fn text_of(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.clone()).collect::<Vec<_>>().join("\n")
}

fn data_of(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("a structured half")
}

/// The reason in words, in both halves.
fn assert_says(r: &CallToolResult, reason: &Unavailable, build: Build) {
    let words = unavailable_text(reason, build);
    assert_eq!(r.is_error, Some(true), "{}", text_of(r));
    assert_eq!(text_of(r), words);
    let data = data_of(r);
    assert_eq!(data["state"], reason.state());
    assert_eq!(data["build"], build.server_name());
    assert_eq!(data["message"], words.as_str());
}

/// The answer names its build and a catalog in `rig`'s folder.
fn assert_from(r: &CallToolResult, server_name: &str, rig: &Rig) {
    assert_eq!(r.is_error, Some(false), "{}", text_of(r));
    assert!(text_of(r).starts_with(&format!("[{server_name} {VERSION} · catalog ")), "{}", text_of(r));
    let data = data_of(r);
    assert_eq!(data["source"]["build"], server_name);
    let catalog = PathBuf::from(data["source"]["catalog"].as_str().expect("a catalog path"));
    assert_eq!(catalog.file_name().and_then(|n| n.to_str()), Some("catalog.db"));
    assert_eq!(catalog.parent().and_then(Path::file_name), rig.base.file_name(), "the catalog is this build's own");
}

// ---- arguments and names ----

#[test]
fn missing_build_argument_is_an_error() {
    let err = parse_args(std::iter::empty()).expect_err("--build is required");
    assert!(err.contains("--build release|dev"), "{err}");
    let err = parse_args(["--app-data-dir".to_string(), "x".to_string()].into_iter()).expect_err("--build is still required");
    assert!(err.contains("--build release|dev"), "{err}");
    let err = parse_args(["--build".to_string(), "beta".to_string()].into_iter()).expect_err("an unknown build");
    assert!(err.contains("--build release|dev"), "{err}");

    let dev = parse_args(["--build", "dev", "--app-data-dir", "/some/dir"].map(String::from).into_iter()).expect("both arguments");
    assert_eq!(dev, BridgeOptions { build: Build::Dev, app_data_dir: Some(PathBuf::from("/some/dir")) });
    assert_eq!(app_data_dir(&dev), Some(PathBuf::from("/some/dir")));
    let release = parse_args(["--build=release".to_string()].into_iter()).expect("the inline form");
    assert_eq!(release, BridgeOptions { build: Build::Release, app_data_dir: None });
    // Without the override: the platform data directory and the build's own folder name, nothing else.
    let guessed = app_data_dir(&release).expect("a platform data directory");
    assert_eq!(guessed.file_name().and_then(|n| n.to_str()), Some(Build::Release.dir_name()));
}

#[test]
fn dirs_match_core() {
    assert_eq!(Build::Dev.dir_name(), APP_DATA_DIR_NAME_DEV);
    assert_eq!(Build::Release.dir_name(), APP_DATA_DIR_NAME_RELEASE);
    assert_eq!(Build::Dev.server_name(), "athenaeum-dev");
    assert_eq!(Build::Release.server_name(), "athenaeum");
    // The name core's debug build gives its server is the dev build's.
    assert_eq!(mcp_server_name(), if cfg!(debug_assertions) { Build::Dev.server_name() } else { Build::Release.server_name() });
}

#[test]
fn the_words_are_the_briefs() {
    let dev = |u: Unavailable| unavailable_text(&u, Build::Dev);
    let release = |u: Unavailable| unavailable_text(&u, Build::Release);
    assert_eq!(release(Unavailable::NotRunning), "Athenaeum is not running. Start it, then ask again.");
    assert_eq!(dev(Unavailable::NotRunning), "Athenaeum (dev build) is not running. Start it, then ask again.");
    assert_eq!(
        release(Unavailable::Off),
        "Assistant access is off in Athenaeum: Settings → General → Assistant access (MCP). Turn it on, then ask again."
    );
    assert_eq!(dev(Unavailable::Starting), "Athenaeum (dev build) is starting and has not opened its catalog yet. Ask again in a moment.");
    assert_eq!(
        release(Unavailable::TokenRejected),
        "Athenaeum refused this connection: its assistant access key changed. Ask again; if this repeats, restart this server in your client."
    );
    assert_eq!(
        release(Unavailable::Outdated),
        "Athenaeum no longer accepts this assistant bridge (version mismatch). Restart Athenaeum so it refreshes its bridge copy, then restart this server in your client."
    );
    assert_eq!(
        dev(Unavailable::WrongBuild { reached: "athenaeum".to_string() }),
        "This connection is set up for Athenaeum (dev build) but reached athenaeum. Copy the client configuration again from that build's Settings."
    );
}

// ---- the app cannot answer: the bridge says why ----

#[tokio::test]
async fn off_answers_in_words() {
    let rig = dev_rig();
    // Assistant access was never turned on: no marker, no `tools.json`.
    let s = legacy(rig.options(Build::Dev)).await;
    assert_eq!(s.tool_names().await, [STATUS_TOOL]);
    let info = s.client.peer_info().expect("server info");
    assert_eq!(info.server_info.as_ref().map(|i| i.name.as_str()), Some("athenaeum-dev"));
    assert_eq!(info.instructions.as_deref(), Some(unavailable_text(&Unavailable::Off, Build::Dev).as_str()));
    // Acceptance 1: asked for anything, it says it is off.
    assert_says(&s.ask("query_frames").await, &Unavailable::Off, Build::Dev);
    assert_says(&s.ask("some_tool_nobody_has").await, &Unavailable::Off, Build::Dev);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn not_running_answers_in_words() {
    let rig = dev_rig();
    write_marker_and_key(&rig);
    let s = legacy(rig.options(Build::Dev)).await;
    assert_eq!(s.tool_names().await, [STATUS_TOOL]);
    assert_says(&s.ask("query_frames").await, &Unavailable::NotRunning, Build::Dev);
    s.close().await;
    // The release build says it without the build's note.
    let s = legacy(rig.options(Build::Release)).await;
    assert_says(&s.ask("query_frames").await, &Unavailable::NotRunning, Build::Release);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn starting_answers_in_words() {
    let rig = dev_rig();
    // The desktop binds early when the marker exists, and answers `starting` until its catalog is reconciled.
    std::fs::create_dir_all(rig.dir()).expect("the channel folder");
    std::fs::write(rig.dir().join("enabled"), "1").expect("the marker");
    rig.host.set_app_data_dir(rig.base.clone());
    rig.host.start_early();
    let s = legacy(rig.options(Build::Dev)).await;
    assert_says(&s.ask("query_frames").await, &Unavailable::Starting, Build::Dev);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn outdated_answers_in_words() {
    let rig = dev_rig();
    write_marker_and_key(&rig);
    // A host that finds the bridge's protocol too old or too new: `ERR version`. It also records what the bridge sent.
    let (endpoint, listener) = bind(&rig.dir()).expect("a channel to listen on");
    std::fs::write(rig.dir().join("endpoint"), endpoint.to_text()).expect("the endpoint");
    let host = tokio::spawn(async move {
        let mut stream = listener.accept().await.expect("the bridge connects");
        let mut line = Vec::new();
        loop {
            let byte = stream.read_u8().await.expect("a handshake byte");
            if byte == b'\n' {
                break;
            }
            line.push(byte);
        }
        stream.write_all(b"ERR version this Athenaeum speaks assistant channel protocol 1 at most; update it\n").await.expect("the reply");
        String::from_utf8(line).expect("a text line")
    });
    let s = legacy(rig.options(Build::Dev)).await;
    assert_says(&s.ask("query_frames").await, &Unavailable::Outdated, Build::Dev);
    s.close().await;
    // The handshake is the frozen wire line: the protocol word, a space, the key as it is on disk.
    assert_eq!(host.await.expect("the fake host ends"), format!("ATHENAEUM-MCP/1 {KEY_A}"));
    remove_endpoint(&endpoint);
    rig.finish().await;
}

#[tokio::test]
async fn a_changed_key_answers_in_words_and_is_picked_up() {
    let rig = dev_rig();
    rig.enable();
    let real_key = std::fs::read_to_string(rig.dir().join("token")).expect("the key").trim().to_string();
    // The client's bridge is started while the key on disk is not the host's.
    std::fs::write(rig.dir().join("token"), KEY_B).expect("another key");
    let s = legacy(rig.options(Build::Dev)).await;
    assert_says(&s.ask("query_frames").await, &Unavailable::TokenRejected, Build::Dev);
    // The key is read again at the next attempt: the right one connects.
    std::fs::write(rig.dir().join("token"), real_key).expect("the key back");
    let answer = s.ask_until_the_app_answers().await;
    assert_from(&answer, "athenaeum-dev", &rig);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn client_started_before_the_app_lists_the_real_tools() {
    let rig = dev_rig();
    // The app ran before and left its marker and its tool list; it is not running now.
    write_marker_and_key(&rig);
    std::fs::write(rig.dir().join("tools.json"), rig.host.tool_list_json()).expect("the tool list");
    let s = legacy(rig.options(Build::Dev)).await;
    assert_eq!(s.tool_names().await, rig.tools(), "the app's own list, before the app runs");
    assert_says(&s.ask("query_frames").await, &Unavailable::NotRunning, Build::Dev);
    // The app starts: the next call (the bridge looks again two seconds after its last try) is the app's own answer.
    let before = s.changed.load(Ordering::SeqCst);
    rig.enable();
    let answer = s.ask_until_the_app_answers().await;
    assert_from(&answer, "athenaeum-dev", &rig);
    assert!(s.changed.load(Ordering::SeqCst) > before, "the client was told its tool list may have changed");
    assert_eq!(s.tool_names().await, rig.tools());
    s.close().await;
    rig.finish().await;
}

// ---- the app comes and goes ----

#[tokio::test]
async fn app_quits_mid_conversation() {
    let rig = dev_rig();
    rig.enable();
    let s = legacy(rig.options(Build::Dev)).await;
    assert_eq!(s.tool_names().await, rig.tools());
    assert_from(&s.ask("query_frames").await, "athenaeum-dev", &rig);
    rig.host.shutdown();
    // The next question is answered in words, within five seconds, not left hanging.
    let answer = tokio::time::timeout(Duration::from_secs(5), s.ask("query_frames")).await.expect("answered within 5 s");
    assert_says(&answer, &Unavailable::NotRunning, Build::Dev);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn app_returns_and_the_bridge_reconnects() {
    let rig = dev_rig();
    rig.enable();
    let s = legacy(rig.options(Build::Dev)).await;
    assert_from(&s.ask("query_frames").await, "athenaeum-dev", &rig);
    rig.host.shutdown();
    assert_says(&s.ask("query_frames").await, &Unavailable::NotRunning, Build::Dev);
    let before = s.changed.load(Ordering::SeqCst);
    rig.enable();
    let answer = s.ask_until_the_app_answers().await;
    assert!(text_of(&answer).starts_with("[athenaeum-dev"), "{}", text_of(&answer));
    assert_from(&answer, "athenaeum-dev", &rig);
    assert!(s.changed.load(Ordering::SeqCst) > before, "the client saw tools/list_changed");
    assert_eq!(s.tool_names().await, rig.tools());
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn stateless_client_needs_no_replay() {
    let rig = dev_rig();
    rig.enable();
    let s = session(rig.options(Build::Dev), stateless()).await;
    assert_eq!(s.tool_names().await, rig.tools());
    assert_from(&s.ask("query_frames").await, "athenaeum-dev", &rig);
    rig.host.shutdown();
    assert_says(&s.ask("query_frames").await, &Unavailable::NotRunning, Build::Dev);
    rig.enable();
    assert_from(&s.ask_until_the_app_answers().await, "athenaeum-dev", &rig);
    s.close().await;
    rig.finish().await;
}

#[tokio::test]
async fn a_regenerated_key_is_picked_up() {
    let rig = dev_rig();
    rig.enable();
    let s = legacy(rig.options(Build::Dev)).await;
    assert_from(&s.ask("query_frames").await, "athenaeum-dev", &rig);
    rig.host.regenerate_token().expect("a new key");
    // The old connection is cut; the bridge reads the new key from disk when it looks again.
    assert_from(&s.ask_until_the_app_answers().await, "athenaeum-dev", &rig);
    s.close().await;
    rig.finish().await;
}

// ---- two builds, two bridges ----

#[tokio::test]
async fn dev_and_release_never_cross() {
    let dev = rig("athenaeum-dev");
    let release = rig("athenaeum");
    dev.enable();
    release.enable();

    let dev_bridge = legacy(dev.options(Build::Dev)).await;
    let release_bridge = legacy(release.options(Build::Release)).await;
    assert_from(&dev_bridge.ask("query_frames").await, "athenaeum-dev", &dev);
    assert_from(&release_bridge.ask("query_frames").await, "athenaeum", &release);

    // Acceptance 11: a bridge configured for one build, pointed at the other's folder, says so and answers nothing.
    let crossed_dev = legacy(release.options(Build::Dev)).await;
    assert_says(&crossed_dev.ask("query_frames").await, &Unavailable::WrongBuild { reached: "athenaeum".to_string() }, Build::Dev);
    let crossed_release = legacy(dev.options(Build::Release)).await;
    assert_says(&crossed_release.ask("query_frames").await, &Unavailable::WrongBuild { reached: "athenaeum-dev".to_string() }, Build::Release);

    for s in [dev_bridge, release_bridge, crossed_dev, crossed_release] {
        s.close().await;
    }
    dev.finish().await;
    release.finish().await;
}

#[tokio::test]
async fn two_bridges_one_host() {
    let rig = dev_rig();
    rig.enable();
    let a = legacy(rig.options(Build::Dev)).await;
    let b = session(rig.options(Build::Dev), stateless()).await;
    let (first, second) = tokio::join!(a.ask("query_frames"), b.ask("catalog_overview"));
    assert_from(&first, "athenaeum-dev", &rig);
    assert_from(&second, "athenaeum-dev", &rig);
    assert_eq!(data_of(&first)["source"], data_of(&second)["source"]);
    a.close().await;
    // The other conversation goes on.
    assert_from(&b.ask("query_frames").await, "athenaeum-dev", &rig);
    b.close().await;
    rig.finish().await;
}

// ---- raw lines: one answer per request, bytes untouched ----

const REPLAY_ID: &str = "athenaeum-bridge-replay";
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const CALL: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query_frames","arguments":{}}}"#;

/// A bridge driven by lines written by hand; every line it writes to the client is collected, whole.
struct Raw {
    input: tokio::io::DuplexStream,
    seen: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    reader: JoinHandle<()>,
    bridge: JoinHandle<io::Result<()>>,
}

impl Raw {
    fn start(opts: BridgeOptions) -> Self {
        let (bridge_in, input) = tokio::io::duplex(1 << 20);
        let (client_in, bridge_out) = tokio::io::duplex(1 << 20);
        let bridge = tokio::spawn(run_bridge(opts, bridge_in, bridge_out));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let reader = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(client_in);
            loop {
                let mut line = Vec::new();
                match tokio::io::AsyncBufReadExt::read_until(&mut lines, b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        line.pop();
                        sink.lock().expect("the sink").push(line);
                    }
                }
            }
        });
        Self { input, seen, reader, bridge }
    }

    async fn send(&mut self, line: &[u8]) {
        self.input.write_all(line).await.expect("the bridge reads");
        self.input.write_all(b"\n").await.expect("the bridge reads");
    }

    fn responses_to(&self, id: &Value) -> usize {
        self.seen.lock().expect("the sink").iter().filter_map(|l| serde_json::from_slice::<Value>(l).ok()).filter(|v| &v["id"] == id && v.get("method").is_none()).count()
    }

    async fn wait_for_response(&self, id: Value, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        while self.responses_to(&id) == 0 {
            assert!(tokio::time::Instant::now() < deadline, "no answer to {id} in {within:?}; got {} lines", self.seen.lock().expect("the sink").len());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Closes the client's input and returns every line the bridge wrote, parsed.
    async fn finish(self) -> Vec<Value> {
        let Self { input, seen, reader, bridge } = self;
        drop(input);
        tokio::time::timeout(Duration::from_secs(8), bridge).await.expect("the bridge ends").expect("joins").expect("ends cleanly");
        tokio::time::timeout(Duration::from_secs(5), reader).await.expect("the client side ends").expect("joins");
        let lines = seen.lock().expect("the sink").clone();
        lines.iter().map(|l| serde_json::from_slice(l).unwrap_or_else(|e| panic!("the bridge wrote a line that is not JSON ({e}): {}", String::from_utf8_lossy(l)))).collect()
    }
}

/// A host that handshakes, answers the `initialize` it is sent, records every line, and closes at the first `tools/call`.
async fn quitting_host(listener: athenaeum_core::mcp::channel::Listener) -> Vec<Vec<u8>> {
    use tokio::io::AsyncBufReadExt;
    let stream = listener.accept().await.expect("the bridge connects");
    let (read, mut write) = tokio::io::split(stream);
    let mut read = tokio::io::BufReader::new(read);
    let mut hello = Vec::new();
    read.read_until(b'\n', &mut hello).await.expect("the handshake");
    assert_eq!(hello, format!("ATHENAEUM-MCP/1 {KEY_A}\n").into_bytes());
    write.write_all(format!("OK athenaeum-dev {VERSION}\n").as_bytes()).await.expect("the reply");
    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        if read.read_until(b'\n', &mut line).await.expect("a line") == 0 {
            break;
        }
        line.pop();
        let is_initialize = serde_json::from_slice::<Value>(&line).is_ok_and(|v| v["method"] == "initialize");
        let is_call = serde_json::from_slice::<Value>(&line).is_ok_and(|v| v["method"] == "tools/call");
        lines.push(line);
        if is_initialize {
            let result = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"athenaeum-dev","version":"9.9.9-test"}}}"#;
            write.write_all(format!("{result}\n").as_bytes()).await.expect("the answer");
        }
        if is_call {
            break; // the app quits without answering the call
        }
    }
    lines
}

#[tokio::test]
async fn a_call_the_app_never_answers_is_answered_once_and_bytes_pass_untouched() {
    let rig = dev_rig();
    write_marker_and_key(&rig);
    let (endpoint, listener) = bind(&rig.dir()).expect("a channel to listen on");
    std::fs::write(rig.dir().join("endpoint"), endpoint.to_text()).expect("the endpoint");
    let host = tokio::spawn(quitting_host(listener));

    let not_json: &[u8] = b"this is not json at all";
    let not_utf8: &[u8] = b"\xff\xfe not utf-8 {\x80\x81}";
    let mut long = vec![b'x'; 70 * 1024];
    long[0] = b'#';

    let mut raw = Raw::start(rig.options(Build::Dev));
    raw.send(INITIALIZE.as_bytes()).await;
    raw.wait_for_response(json!(1), Duration::from_secs(10)).await;
    raw.send(INITIALIZED.as_bytes()).await;
    raw.send(not_json).await;
    raw.send(not_utf8).await;
    raw.send(&long).await;
    raw.send(CALL.as_bytes()).await;
    raw.wait_for_response(json!(2), Duration::from_secs(10)).await;
    // A second answer or a leaked reply would come after the first: give them time to show up.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let out = raw.finish().await;

    let forwarded = tokio::time::timeout(Duration::from_secs(5), host).await.expect("the fake host ends").expect("joins");
    let expected: Vec<&[u8]> = vec![INITIALIZE.as_bytes(), INITIALIZED.as_bytes(), not_json, not_utf8, &long, CALL.as_bytes()];
    assert_eq!(forwarded.len(), expected.len(), "the host saw every line once");
    for (got, want) in forwarded.iter().zip(&expected) {
        assert!(got == want, "a line reached the host changed ({} bytes in, {} out)", want.len(), got.len());
    }

    let answers_to = |id: Value| out.iter().filter(|v| v["id"] == id && v.get("method").is_none()).count();
    assert_eq!(answers_to(json!(1)), 1, "one answer to initialize: {out:?}");
    assert_eq!(answers_to(json!(2)), 1, "one answer to the call: {out:?}");
    assert_eq!(answers_to(json!(REPLAY_ID)), 0, "the replay's answer never reaches the client: {out:?}");
    assert_eq!(out.len(), 2, "nothing else was written to the client: {out:?}");
    let call = out.iter().find(|v| v["id"] == 2).expect("the call's answer");
    let words = unavailable_text(&Unavailable::NotRunning, Build::Dev);
    assert!(call.to_string().contains(&words), "the call is answered in words: {call}");

    remove_endpoint(&endpoint);
    rig.finish().await;
}

#[tokio::test]
async fn a_hung_app_costs_one_handshake_wait_not_one_per_request() {
    let rig = dev_rig();
    write_marker_and_key(&rig);
    let (endpoint, listener) = bind(&rig.dir()).expect("a channel to listen on");
    std::fs::write(rig.dir().join("endpoint"), endpoint.to_text()).expect("the endpoint");
    // Accepts every connection and never says a word.
    let host = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok(stream) = listener.accept().await {
            held.push(stream);
        }
    });

    let started = tokio::time::Instant::now();
    let mut raw = Raw::start(rig.options(Build::Dev));
    raw.send(INITIALIZE.as_bytes()).await;
    raw.wait_for_response(json!(1), Duration::from_secs(20)).await;
    raw.send(INITIALIZED.as_bytes()).await;
    raw.send(CALL.as_bytes()).await;
    raw.wait_for_response(json!(2), Duration::from_secs(20)).await;
    let took = started.elapsed();
    // The bridge's first connect waits out the 5 s handshake once; the requests that follow it right away do not wait again.
    assert!(took < Duration::from_secs(8), "two requests took {took:?}: each one waited for the hung app");
    let out = raw.finish().await;
    assert_eq!(out.len(), 2, "{out:?}");

    host.abort();
    let _ = host.await;
    remove_endpoint(&endpoint);
    rig.finish().await;
}
