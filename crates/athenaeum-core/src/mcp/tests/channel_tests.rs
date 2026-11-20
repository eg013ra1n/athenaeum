//! The local channel, its token and the on/off lifecycle (MCP plan, Task 6). One abstraction, the same assertions on
//! every OS: a Unix socket here, a named pipe on Windows, both behind `channel::{bind, connect, Endpoint}`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::ClientLifecycleMode;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::channel::{bind, connect, mcp_dir, Endpoint, Stream, HANDSHAKE_PREFIX};
use super::{desktop_client_config, mcp_server_name, HostKind, McpHost, McpIdentity, NEEDS_API_KEY};
use crate::api::ApiError;
use crate::db;
use crate::events::NullEmitter;
use crate::services::ServiceContext;

const VERSION: &str = "9.9.9-test";
const TOOLS: [&str; 5] = ["catalog_overview", "file_details", "find_duplicates", "find_missing_metadata", "query_frames"];

// ---- the rig ----

struct Rig {
    host: Arc<McpHost>,
    ctx: Arc<ServiceContext>,
    base: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn dir(&self) -> PathBuf {
        mcp_dir(&self.base)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir().join(name)
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.file("token")).expect("the token file").trim().to_string()
    }

    /// The endpoint a bridge would read from the `endpoint` file.
    fn endpoint(&self) -> Endpoint {
        let text = std::fs::read_to_string(self.file("endpoint")).expect("the endpoint file");
        Endpoint::parse(&text).expect("a parseable endpoint")
    }

    fn kv(&self) -> Option<String> {
        let database = self.ctx.db.get().expect("db");
        let conn = database.conn();
        db::get_setting(&conn, "mcp.enabled").expect("read the setting")
    }

    /// Stop serving and give the connection tasks a moment to end, so no open catalog outlives the temp dir.
    async fn finish(self) {
        self.host.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn identity(kind: HostKind, web_api_key_set: bool) -> McpIdentity {
    McpIdentity { server_name: mcp_server_name(), version: VERSION, kind, web_api_key_set }
}

fn host_over(ctx: &Arc<ServiceContext>, kind: HostKind, key: bool) -> Arc<McpHost> {
    McpHost::new(Arc::clone(ctx), identity(kind, key), Arc::new(NullEmitter), Arc::new(|| {}), Arc::new(|| true))
}

fn rig(kind: HostKind, key: bool, open: bool) -> Rig {
    let (tmp, base) = crate::test_support::canonical_tempdir();
    let ctx = Arc::new(ServiceContext::new_for_tests(base.join("catalog.db")));
    let host = host_over(&ctx, kind, key);
    if open {
        host.on_catalog_open().expect("the catalog opens");
    }
    Rig { host, ctx, base, _tmp: tmp }
}

fn desktop() -> Rig {
    rig(HostKind::Desktop, false, true)
}

// ---- a raw client, as a bridge writes one ----

async fn read_line(stream: &mut Stream) -> String {
    let mut out = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = stream.read(&mut byte).await.expect("read a byte");
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        out.push(byte[0]);
    }
    String::from_utf8(out).expect("a text reply")
}

/// Connect, write one handshake line, read the reply line.
async fn shake(endpoint: &Endpoint, line: &str) -> (Stream, String) {
    let mut stream = connect(endpoint).await.expect("connect to the endpoint");
    stream.write_all(format!("{line}\n").as_bytes()).await.expect("write the handshake");
    let reply = tokio::time::timeout(Duration::from_secs(5), read_line(&mut stream)).await.expect("a reply in time");
    (stream, reply)
}

async fn tool_names(stream: Stream) -> Vec<String> {
    let client = rmcp::serve_client_with_lifecycle((), stream, ClientLifecycleMode::Initialize).await.expect("the client connects");
    let mut names: Vec<String> = client.list_all_tools().await.expect("tools").into_iter().map(|t| t.name.to_string()).collect();
    names.sort();
    let _ = client.cancel().await;
    names
}

/// The stream ends within two seconds: an EOF or a broken-connection error, never a hang.
async fn assert_cut_off(stream: &mut Stream) {
    let mut buf = [0u8; 16];
    let r = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
    assert!(matches!(r, Ok(Ok(0)) | Ok(Err(_))), "the connection was not cut off: {r:?}");
}

// ---- tests ----

#[tokio::test]
async fn off_by_default() {
    let rig = desktop();
    let status = rig.host.status();
    assert!(!status.enabled);
    assert_eq!(status.state, "off");
    assert_eq!(status.server_name, mcp_server_name());
    assert!(status.endpoint.is_none() && status.last_error.is_none() && status.blocked_reason.is_none());
    assert!(!rig.host.is_serving());
    assert_ne!(rig.kv().as_deref(), Some("true"));
    let leftovers = std::fs::read_dir(rig.dir()).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "off writes no channel file");
    assert!(crate::settings::defaults::all().contains(&(crate::settings::keys::MCP_ENABLED, "false")));
    assert_eq!(crate::settings::keys::MCP_ENABLED, "mcp.enabled");
    rig.finish().await;
}

#[tokio::test]
async fn web_refuses_enable_without_api_key() {
    let rig = rig(HostKind::Web, false, true);
    let blocked = rig.host.status();
    assert_eq!(blocked.blocked_reason.as_deref(), Some(NEEDS_API_KEY));
    let err = rig.host.set_enabled(true).expect_err("refused without a key");
    assert!(matches!(&err, ApiError::Invalid(m) if m == NEEDS_API_KEY), "{err:?}");
    assert_ne!(rig.kv().as_deref(), Some("true"), "the setting stays off");
    let status = rig.host.status();
    assert!(!status.enabled);
    assert_eq!(status.state, "off");
    assert_eq!(status.blocked_reason.as_deref(), Some(NEEDS_API_KEY));
    assert!(!rig.host.is_serving());
    rig.finish().await;
}

#[tokio::test]
async fn web_stored_true_without_a_key_boots_off_and_says_why() {
    let rig = rig(HostKind::Web, false, false);
    {
        let database = rig.ctx.db.get().expect("db");
        db::set_setting(&database.conn(), "mcp.enabled", "true").expect("store");
    }
    rig.host.on_catalog_open().expect("open");
    let status = rig.host.status();
    assert_eq!(status.state, "off");
    assert_eq!(status.last_error.as_deref(), Some(NEEDS_API_KEY));
    assert!(!rig.host.is_serving());
    rig.finish().await;
}

#[tokio::test]
async fn web_enables_with_api_key() {
    let rig = rig(HostKind::Web, true, true);
    assert!(rig.host.status().blocked_reason.is_none());
    let status = rig.host.set_enabled(true).expect("enabled with a key");
    assert!(status.enabled);
    assert_eq!(status.state, "serving");
    assert!(status.endpoint.is_none(), "the web build binds nothing");
    let token = status.token.clone().expect("the web shows its key");
    assert_eq!(rig.kv().as_deref(), Some("true"));
    assert!(rig.host.is_serving());
    assert!(rig.host.check_token(&token));
    assert!(!rig.host.check_token("not-the-token"));
    assert!(!rig.host.check_token(""));
    assert!(!rig.file("endpoint").exists(), "no socket on the web build");
    let off = rig.host.set_enabled(false).expect("disabled");
    assert_eq!(off.state, "off");
    assert!(!rig.host.is_serving());
    assert_eq!(rig.kv().as_deref(), Some("false"));
    rig.finish().await;
}

#[tokio::test]
async fn disable_keeps_the_token_and_on_again_gives_the_same_key() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let first = rig.token();
    assert!(first.len() >= 32, "a long random key: {}", first.len());
    rig.host.set_enabled(false).expect("off");
    assert_eq!(rig.token(), first, "off keeps the token");
    rig.host.set_enabled(true).expect("on again");
    assert_eq!(rig.token(), first, "on again gives the same key");
    assert!(rig.host.check_token(&first));
    let regenerated = rig.host.regenerate_token().expect("regenerate");
    assert_eq!(regenerated.state, "serving");
    assert_ne!(rig.token(), first, "only Regenerate changes it");
    assert!(!rig.host.check_token(&first));
    rig.finish().await;
}

#[tokio::test]
async fn web_off_and_on_keeps_the_key_and_regenerate_changes_it() {
    let rig = rig(HostKind::Web, true, true);
    let key = rig.host.set_enabled(true).expect("on").token.expect("key");
    rig.host.set_enabled(false).expect("off");
    let again = rig.host.set_enabled(true).expect("on again").token.expect("key");
    assert_eq!(key, again);
    let fresh = rig.host.regenerate_token().expect("regenerate").token.expect("key");
    assert_ne!(key, fresh);
    assert!(rig.host.check_token(&fresh) && !rig.host.check_token(&key));
    rig.finish().await;
}

#[tokio::test]
async fn tools_json_is_written_on_enable() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(rig.file("tools.json")).expect("tools.json")).expect("json");
    assert_eq!(on_disk.to_string(), rig.host.tool_list_json().parse::<Value>().expect("json").to_string());
    // It is what a client of the running server is told.
    let (stream, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {}", rig.token())).await;
    assert!(reply.starts_with("OK "), "{reply}");
    let client = rmcp::serve_client_with_lifecycle((), stream, ClientLifecycleMode::Initialize).await.expect("client");
    let served = serde_json::to_value(client.list_all_tools().await.expect("tools")).expect("to json");
    let _ = client.cancel().await;
    assert_eq!(on_disk["tools"], served, "tools.json is the server's own tools/list");
    assert_eq!(on_disk["tools"].as_array().expect("tools").len(), TOOLS.len());
    // No temp file is left beside it.
    let names: Vec<String> = std::fs::read_dir(rig.dir()).expect("dir").map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    rig.finish().await;
}

#[tokio::test]
async fn frozen_v1_handshake_is_still_served() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    // The literal line an old bridge writes.
    let line = format!("ATHENAEUM-MCP/1 {}", rig.token());
    let (stream, reply) = shake(&rig.endpoint(), &line).await;
    assert_eq!(reply, format!("OK {} {VERSION}", mcp_server_name()));
    assert_eq!(tool_names(stream).await, TOOLS.map(String::from).to_vec());
    // A later channel protocol is refused in words, not served.
    let (_s, reply) = shake(&rig.endpoint(), &format!("ATHENAEUM-MCP/9 {}", rig.token())).await;
    assert!(reply.starts_with("ERR version "), "{reply}");
    rig.finish().await;
}

#[tokio::test]
async fn stale_socket_is_removed_after_a_crash() {
    let (_tmp, base) = crate::test_support::canonical_tempdir();
    let dir = mcp_dir(&base);
    std::fs::create_dir_all(&dir).expect("dir");
    // A listener that goes away without cleaning up leaves its socket file behind (a crash).
    let (first, listener) = bind(&dir).expect("first bind");
    drop(listener);
    let (second, _listener) = bind(&dir).expect("bind works over what the crash left");
    assert_eq!(first.to_text(), second.to_text());
    let stream = connect(&second).await;
    assert!(stream.is_ok(), "the new listener accepts: {stream:?}");
}

#[tokio::test]
async fn vanished_socket_is_rebound() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let endpoint = rig.endpoint();
    if let Endpoint::Unix(path) = &endpoint {
        std::fs::remove_file(path).expect("a $TMPDIR cleaner takes the socket file");
        assert!(connect(&endpoint).await.is_err(), "nothing answers at a vanished socket");
    }
    rig.host.recheck_endpoint();
    let (_s, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {}", rig.token())).await;
    assert!(reply.starts_with("OK "), "{reply}");
    assert_eq!(rig.host.status().state, "serving");
    rig.finish().await;
}

#[tokio::test]
async fn handshake_before_catalog_says_starting() {
    let rig = rig(HostKind::Desktop, false, false);
    // What an earlier run left: the token and the `enabled` marker.
    std::fs::create_dir_all(rig.dir()).expect("dir");
    std::fs::write(rig.file("token"), "tok-before-catalog").expect("token");
    std::fs::write(rig.file("enabled"), "1").expect("marker");
    {
        let database = rig.ctx.db.get().expect("db");
        db::set_setting(&database.conn(), "mcp.enabled", "true").expect("store");
    }
    rig.host.start_early();
    assert_eq!(rig.host.status().state, "starting");
    let (_s, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} tok-before-catalog")).await;
    assert!(reply.starts_with("ERR starting "), "{reply}");
    // The catalog opens; the stored setting is still true: serving.
    rig.host.on_catalog_open().expect("open");
    assert_eq!(rig.host.status().state, "serving");
    let (_s, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} tok-before-catalog")).await;
    assert!(reply.starts_with("OK "), "{reply}");
    rig.finish().await;
}

#[tokio::test]
async fn stored_off_beats_the_marker_when_the_catalog_opens() {
    let rig = rig(HostKind::Desktop, false, false);
    std::fs::create_dir_all(rig.dir()).expect("dir");
    std::fs::write(rig.file("token"), "tok").expect("token");
    std::fs::write(rig.file("enabled"), "1").expect("marker");
    rig.host.start_early();
    assert_eq!(rig.host.status().state, "starting");
    let endpoint = rig.endpoint();
    rig.host.on_catalog_open().expect("open");
    let status = rig.host.status();
    assert_eq!((status.enabled, status.state.as_str()), (false, "off"));
    assert!(!rig.file("enabled").exists() && !rig.file("endpoint").exists());
    assert!(rig.file("token").exists(), "the token stays");
    assert!(connect(&endpoint).await.is_err(), "the early listener is closed");
    // Idempotent: a second open changes nothing.
    rig.host.on_catalog_open().expect("open again");
    assert_eq!(rig.host.status().state, "off");
    rig.finish().await;
}

#[tokio::test]
async fn no_marker_means_start_early_binds_nothing() {
    let rig = rig(HostKind::Desktop, false, false);
    rig.host.start_early();
    assert_eq!(rig.host.status().state, "off");
    assert!(!rig.file("endpoint").exists());
    rig.finish().await;
}

#[tokio::test]
async fn enable_then_handshake_ok_then_mcp_flows() {
    let rig = desktop();
    let status = rig.host.set_enabled(true).expect("on");
    assert_eq!((status.enabled, status.state.as_str()), (true, "serving"));
    assert!(status.token.is_none(), "the desktop never shows its key");
    assert_eq!(rig.kv().as_deref(), Some("true"));
    assert!(rig.file("enabled").exists());
    assert_eq!(status.endpoint.as_deref(), Some(rig.endpoint().to_text().as_str()));
    let (stream, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {}", rig.token())).await;
    assert_eq!(reply, format!("OK {} {VERSION}", mcp_server_name()));
    assert_eq!(tool_names(stream).await, TOOLS.map(String::from).to_vec());
    rig.finish().await;
}

#[tokio::test]
async fn wrong_token_is_refused() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    for line in [format!("{HANDSHAKE_PREFIX} not-the-token"), format!("{HANDSHAKE_PREFIX} "), "hello".to_string(), format!("{HANDSHAKE_PREFIX}")] {
        let (_s, reply) = shake(&rig.endpoint(), &line).await;
        assert!(reply.starts_with("ERR token "), "{line:?} -> {reply}");
        assert!(!reply.contains(&rig.token()), "the reply never echoes the key");
    }
    // A line over 256 bytes is refused, too.
    let long = format!("{HANDSHAKE_PREFIX} {}", "x".repeat(400));
    let (_s, reply) = shake(&rig.endpoint(), &long).await;
    assert!(reply.starts_with("ERR token "), "{reply}");
    rig.finish().await;
}

#[tokio::test]
async fn silent_connection_is_dropped_after_the_handshake_window() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let mut stream = connect(&rig.endpoint()).await.expect("connect");
    let mut buf = [0u8; 8];
    // Nothing is written: the host closes the connection after its 5 s window.
    let r = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut buf)).await.expect("closed within the window");
    assert!(matches!(r, Ok(0) | Err(_)), "{r:?}");
    rig.finish().await;
}

#[tokio::test]
async fn regenerate_cuts_off_connected_clients() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let old = rig.token();
    let (mut stream, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {old}")).await;
    assert!(reply.starts_with("OK "), "{reply}");
    rig.host.regenerate_token().expect("regenerate");
    assert_cut_off(&mut stream).await;
    let (_s, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {old}")).await;
    assert!(reply.starts_with("ERR token "), "the old token: {reply}");
    let (_s, reply) = shake(&rig.endpoint(), &format!("{HANDSHAKE_PREFIX} {}", rig.token())).await;
    assert!(reply.starts_with("OK "), "the new token: {reply}");
    rig.finish().await;
}

#[tokio::test]
async fn disable_removes_endpoint_marker_and_tool_list_keeps_the_token() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let endpoint = rig.endpoint();
    let token = rig.token();
    let (mut stream, reply) = shake(&endpoint, &format!("{HANDSHAKE_PREFIX} {token}")).await;
    assert!(reply.starts_with("OK "), "{reply}");
    let status = rig.host.set_enabled(false).expect("off");
    assert_eq!((status.enabled, status.state.as_str()), (false, "off"));
    assert!(status.endpoint.is_none());
    assert_eq!(rig.kv().as_deref(), Some("false"));
    for gone in ["endpoint", "enabled", "tools.json"] {
        assert!(!rig.file(gone).exists(), "{gone} is removed");
    }
    assert_eq!(rig.token(), token, "the token stays");
    assert_cut_off(&mut stream).await;
    assert!(connect(&endpoint).await.is_err(), "nobody listens any more");
    rig.finish().await;
}

#[tokio::test]
async fn shutdown_keeps_the_marker_and_the_tool_list_for_the_next_start() {
    let rig = desktop();
    rig.host.set_enabled(true).expect("on");
    let endpoint = rig.endpoint();
    rig.host.shutdown();
    assert!(connect(&endpoint).await.is_err());
    assert!(rig.file("enabled").exists() && rig.file("tools.json").exists() && rig.file("token").exists());
    assert_eq!(rig.kv().as_deref(), Some("true"), "quitting does not turn it off");
    rig.finish().await;
}

#[tokio::test]
async fn second_instance_of_one_build_does_not_steal_the_endpoint() {
    let first = desktop();
    first.host.set_enabled(true).expect("on");
    let endpoint = first.endpoint();
    let token = first.token();
    // Another process of this build over the same app-data dir: a second host on the same files.
    let second = host_over(&first.ctx, HostKind::Desktop, false);
    second.on_catalog_open().expect("open");
    let status = second.set_enabled(true).expect("the setting is on; serving is elsewhere");
    assert_eq!(status.state, "elsewhere");
    assert_eq!(status.last_error.as_deref(), Some("another Athenaeum of this build serves the assistant"));
    assert!(!second.is_serving());
    // The first still serves, and the second's off-switch leaves its files alone.
    let (_s, reply) = shake(&endpoint, &format!("{HANDSHAKE_PREFIX} {token}")).await;
    assert!(reply.starts_with("OK "), "{reply}");
    second.set_enabled(false).expect("off in the second");
    assert!(first.file("endpoint").exists() && first.file("enabled").exists() && first.file("tools.json").exists());
    let (_s, reply) = shake(&endpoint, &format!("{HANDSHAKE_PREFIX} {token}")).await;
    assert!(reply.starts_with("OK "), "the first one is untouched: {reply}");
    first.finish().await;
}

#[tokio::test]
async fn long_app_data_dir_uses_short_endpoint() {
    let (_tmp, base) = crate::test_support::canonical_tempdir();
    let app_data = base.join("a-deliberately-long-application-support-folder-name").join("and-another-long-folder-name-below-it");
    let dir = mcp_dir(&app_data);
    std::fs::create_dir_all(&dir).expect("dir");
    let (endpoint, _listener) = bind(&dir).expect("bind");
    match &endpoint {
        Endpoint::Unix(path) => {
            assert!(path.as_os_str().len() <= 100, "a socket path must fit sun_path: {path:?}");
            let id = short_id_of(&app_data);
            assert!(path.to_string_lossy().contains(&format!("athenaeum-mcp-{id}")), "{path:?} names {id}");
            assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("mcp.sock"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path.parent().expect("parent")).expect("meta").permissions().mode();
                assert_eq!(mode & 0o077, 0, "the socket's directory is private: {mode:o}");
            }
            let _ = std::fs::remove_file(path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
        Endpoint::Pipe(name) => assert!(name.starts_with(r"\\.\pipe\"), "{name}"),
    }
    // Both forms round-trip through the `endpoint` file's text.
    assert_eq!(Endpoint::parse(&endpoint.to_text()).map(|e| e.to_text()), Some(endpoint.to_text()));
    assert!(Endpoint::parse("").is_none() && Endpoint::parse("tcp:1.2.3.4").is_none());
}

/// The 8 hex characters the endpoint derives from the app-data dir: the first 4 bytes of its path's SHA-256.
fn short_id_of(app_data: &Path) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(app_data.to_string_lossy().as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn set_setting_refuses_mcp_enabled() {
    let rig = desktop();
    let monitor = crate::monitor::MonitorService::new();
    let err = crate::api::settings::set_setting(&rig.ctx, "mcp.enabled", "true", &monitor).expect_err("refused");
    assert!(matches!(&err, ApiError::Invalid(m) if m == "use set_mcp_enabled"), "{err:?}");
    assert_ne!(rig.kv().as_deref(), Some("true"), "nothing was stored");
    // Any other key still goes through.
    crate::api::settings::set_setting(&rig.ctx, "test.other", "1", &monitor).expect("another key");
    rig.finish().await;
}

#[tokio::test]
async fn the_api_wrappers_report_and_switch() {
    let rig = desktop();
    assert_eq!(crate::api::mcp::get_mcp_status(&rig.host).state, "off");
    let on = crate::api::mcp::set_mcp_enabled(&rig.host, true).expect("on");
    assert_eq!(on.state, "serving");
    let before = rig.token();
    crate::api::mcp::regenerate_mcp_token(&rig.host).expect("regenerate");
    assert_ne!(rig.token(), before);
    let json = serde_json::to_value(crate::api::mcp::get_mcp_status(&rig.host)).expect("json");
    assert!(json.get("serverName").is_some() && json.get("lastError").is_some() && json.get("blockedReason").is_some(), "{json}");
    rig.finish().await;
}

#[test]
fn client_config_names_build_and_server() {
    let bridge = Path::new("bridge-dir").join("athenaeum-mcp");
    let (config, command) = desktop_client_config("athenaeum-dev", &bridge, "dev", None);
    let parsed: Value = serde_json::from_str(&config).expect("valid JSON");
    assert_eq!(parsed, json!({ "mcpServers": { "athenaeum-dev": { "command": bridge.to_string_lossy(), "args": ["--build", "dev"] } } }));
    assert_eq!(command, format!("claude mcp add athenaeum-dev -- \"{}\" --build dev", bridge.display()));

    let data = Path::new("app-data");
    let (config, command) = desktop_client_config("athenaeum", &bridge, "release", Some(data));
    let parsed: Value = serde_json::from_str(&config).expect("valid JSON");
    assert_eq!(
        parsed["mcpServers"]["athenaeum"]["args"],
        json!(["--build", "release", "--app-data-dir", data.to_string_lossy()])
    );
    assert!(command.starts_with("claude mcp add athenaeum -- "), "{command}");
    assert!(command.ends_with(&format!("--build release --app-data-dir \"{}\"", data.display())), "{command}");
}
