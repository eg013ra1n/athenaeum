//! Assistant access on and off (MCP plan, Task 6): the `mcp.enabled` setting, the key, the channel's lifecycle and the
//! status the Settings page reads. Everything here is an `impl McpHost`; [`channel`](super::channel) is the transport.
//!
//! The rules, in one place:
//!
//! - **Off by default.** Turning it on stores `mcp.enabled = "true"`, makes the key (kept across off/on), writes the
//!   `enabled` marker, binds the channel and writes `endpoint` and `tools.json`. Turning it off stores `"false"`,
//!   cancels every connection, closes the listener and removes `endpoint`, `enabled` and `tools.json`; the key stays.
//! - **The database wins.** The desktop binds early (`start_early`, in the window's setup) when the `enabled` marker
//!   exists and answers `ERR starting` until the catalog is open; `on_catalog_open` then reconciles to the stored
//!   setting.
//! - **Only the desktop binds.** The web build serves `/mcp` itself (Task 8); here it has the key, the on/off state and
//!   the refusal to turn on without `ATHENAEUM_API_KEY`.
//! - **One instance owns the endpoint.** A second instance of one build finds it answering and is `elsewhere`; it
//!   never removes or rewrites the first one's files.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::channel::{
    self, endpoint_present, ensure_private_dir, mcp_dir, new_token, parse_hello, read_token, remove_endpoint, spawn_acceptor,
    spawn_closable, tokens_match, write_atomic, Closer, Endpoint, Hello, HandshakeError, Stream, HANDSHAKE_MAX_BYTES,
};
use super::{AthenaeumMcp, HostKind, McpHost};
use crate::api::ApiError;

/// Why the web build refuses to turn assistant access on (owner ruling O5).
pub const NEEDS_API_KEY: &str = "Assistant access cannot be turned on here: this Athenaeum is not protected by an API key, so anyone who can reach it could read your catalog. Set the ATHENAEUM_API_KEY environment variable of the container, restart it, then turn this on.";

/// What a second instance of one build says: the first one owns the endpoint.
const ELSEWHERE: &str = "another Athenaeum of this build serves the assistant";

/// How long a connection has to write its handshake line.
const HANDSHAKE_WINDOW: Duration = Duration::from_secs(5);

/// How often a serving host checks that its socket file is still there (`$TMPDIR` cleaners remove stale ones).
const ENDPOINT_RECHECK: Duration = Duration::from_secs(30);

/// The bridge binary's file name, next to the app's executable.
const BRIDGE_FILE: &str = if cfg!(windows) { "athenaeum-mcp.exe" } else { "athenaeum-mcp" };

/// What the Settings page shows of assistant access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    /// The setting: assistant access is turned on (it may still be starting, or served by another instance).
    pub enabled: bool,
    /// `off` | `starting` | `serving` | `elsewhere` | `failed`.
    pub state: String,
    /// `athenaeum` (release) or `athenaeum-dev` (debug build): the name an assistant lists this build under.
    pub server_name: String,
    /// Desktop: where the local channel listens (`unix:<path>` or `pipe:<name>`); `null` when nothing listens.
    pub endpoint: Option<String>,
    /// Web only: the key an assistant presents, shown so it can be copied; never set on the desktop.
    pub token: Option<String>,
    /// Desktop: where the bridge program is expected (next to the app).
    pub bridge_path: Option<String>,
    /// Desktop: the bridge program is there.
    pub bridge_found: bool,
    /// Desktop: the JSON an assistant's config file takes to start the bridge.
    pub client_config: Option<String>,
    /// Desktop: the shell command that adds the bridge to Claude Code.
    pub claude_code_command: Option<String>,
    /// Why it is not serving: the channel could not start, or another instance owns it.
    pub last_error: Option<String>,
    /// Web without `ATHENAEUM_API_KEY`: why turning it on is refused.
    pub blocked_reason: Option<String>,
}

/// `{"mcpServers":{"<name>":{"command":"<bridge>","args":["--build","<build>"(,"--app-data-dir","<dir>")]}}}` and the
/// `claude mcp add` command for the same bridge.
pub fn desktop_client_config(server_name: &str, bridge: &Path, build: &str, app_data_override: Option<&Path>) -> (String, String) {
    let mut args = vec!["--build".to_string(), build.to_string()];
    let mut command = format!("claude mcp add {server_name} -- \"{}\" --build {build}", bridge.display());
    if let Some(dir) = app_data_override {
        args.push("--app-data-dir".to_string());
        args.push(dir.to_string_lossy().into_owned());
        command.push_str(&format!(" --app-data-dir \"{}\"", dir.display()));
    }
    let config = serde_json::json!({
        "mcpServers": { server_name: { "command": bridge.to_string_lossy(), "args": args } }
    });
    (serde_json::to_string_pretty(&config).unwrap_or_default(), command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum State {
    #[default]
    Off,
    Starting,
    Serving,
    Elsewhere,
    Failed,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Serving => "serving",
            Self::Elsewhere => "elsewhere",
            Self::Failed => "failed",
        }
    }
}

/// The connections handed to clients, so that closing the channel drops every one of them at once (on Windows a pipe
/// instance that outlives the close keeps the pipe name taken).
#[derive(Clone, Default)]
struct Sessions(Arc<Mutex<SessionList>>);

#[derive(Default)]
struct SessionList {
    open: Vec<Closer>,
    closed: bool,
}

impl Sessions {
    fn lock(&self) -> MutexGuard<'_, SessionList> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, session: Closer) {
        let mut list = self.lock();
        if list.closed {
            drop(list);
            session.close();
            return;
        }
        list.open.retain(|s| !s.is_finished());
        list.open.push(session);
    }

    fn close_all(&self) {
        let open = {
            let mut list = self.lock();
            list.closed = true;
            std::mem::take(&mut list.open)
        };
        for session in open {
            session.close();
        }
    }
}

/// What a bind started: stopping it cancels the listener and every connection, and drops them.
struct Run {
    /// Parent of everything below; cancelled to close the channel.
    cancel: CancellationToken,
    /// Parent of every connection's token; cancelled and replaced when the key changes.
    conns: CancellationToken,
    /// The accept handler's token; replaced when the socket is rebound.
    listener: CancellationToken,
    /// The task that owns the listener (and on Windows the waiting pipe instance); closing it drops them.
    acceptor: Closer,
    sessions: Sessions,
    handle: tokio::runtime::Handle,
}

impl Run {
    /// Cancels everything and drops the listener and every connection before it returns.
    fn close(&self) {
        self.cancel.cancel();
        self.acceptor.close();
        self.sessions.close_all();
    }
}

impl Drop for Run {
    /// A host that goes away closes its channel: the accept loop and every connection end.
    fn drop(&mut self) {
        self.close();
    }
}

/// The mutable half of the lifecycle, behind one mutex. Connection tasks take it for a moment and never across an
/// await; `set_enabled` and friends hold it across the setting's write so two switches cannot interleave.
#[derive(Default)]
pub(super) struct Channel {
    state: State,
    /// The setting as last read or written.
    enabled: bool,
    last_error: Option<String>,
    endpoint: Option<Endpoint>,
    /// The key (also in the `token` file).
    token: Option<String>,
    run: Option<Run>,
}

/// The runtime the channel's tasks run on when the caller is not inside one (a window's setup closure).
fn fallback_handle() -> std::io::Result<tokio::runtime::Handle> {
    static RUNTIME: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
    let built = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("mcp-channel").enable_all().build()
    });
    match built {
        Ok(runtime) => Ok(runtime.handle().clone()),
        Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
    }
}

fn runtime_handle() -> std::io::Result<tokio::runtime::Handle> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(handle),
        Err(_) => fallback_handle(),
    }
}

fn remove_file_quiet(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %path.display(), "mcp channel file could not be removed");
        }
    }
}

impl McpHost {
    fn lock_channel(&self) -> MutexGuard<'_, Channel> {
        self.channel.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_desktop(&self) -> bool {
        self.identity.kind == HostKind::Desktop
    }

    fn catalog_open(&self) -> bool {
        self.reader.get().is_some()
    }

    /// Tells the host where the app-data dir is before the catalog is open (the desktop knows it in its window's setup
    /// but opens the database later). Needed by [`McpHost::start_early`] only; the first call wins.
    pub fn set_app_data_dir(&self, dir: PathBuf) {
        let _ = self.app_data_dir.set(dir);
    }

    fn app_data(&self) -> Option<PathBuf> {
        self.app_data_dir.get().cloned().or_else(|| self.ctx.app_data_dir())
    }

    fn channel_dir(&self) -> Result<PathBuf, ApiError> {
        self.app_data().map(|dir| mcp_dir(&dir)).ok_or_else(|| {
            ApiError::Conflict("Athenaeum is starting; its data folder is not known yet. Try again in a moment.".to_string())
        })
    }

    fn stored_enabled(&self) -> Result<bool, ApiError> {
        let database = crate::api::db(&self.ctx)?;
        let conn = database.conn();
        let value = crate::db::get_setting(&conn, crate::settings::keys::MCP_ENABLED).map_err(|e| {
            tracing::error!(error = %e, "mcp setting could not be read");
            ApiError::Internal(e.to_string())
        })?;
        Ok(value.as_deref() == Some("true"))
    }

    fn store_enabled(&self, on: bool) -> Result<(), ApiError> {
        let database = crate::api::db(&self.ctx)?;
        let conn = database.conn();
        self.ctx
            .settings
            .persist_setting(&conn, crate::settings::keys::MCP_ENABLED, if on { "true" } else { "false" })
            .map_err(|e| {
                tracing::error!(error = %e, "mcp setting could not be stored");
                ApiError::Internal(e.to_string())
            })
    }

    fn web_blocked(&self) -> bool {
        self.identity.kind == HostKind::Web && !self.identity.web_api_key_set
    }

    // ---- the public lifecycle ----

    /// The desktop's early bind, in the window's setup: when the `enabled` marker file exists, bind the channel now and
    /// answer `ERR starting` until [`McpHost::on_catalog_open`] has reconciled to the stored setting. Does nothing on
    /// the web build, without the marker, or when the app-data dir is not known (see
    /// [`McpHost::set_app_data_dir`]).
    pub fn start_early(self: &Arc<Self>) {
        if !self.is_desktop() {
            return;
        }
        let Ok(dir) = self.channel_dir() else {
            tracing::debug!("mcp early start skipped: the data folder is not known yet");
            return;
        };
        if !dir.join("enabled").is_file() {
            return;
        }
        let mut ch = self.lock_channel();
        if ch.state != State::Off {
            return;
        }
        ch.enabled = true;
        if self.ensure_token(&mut ch, &dir).is_err() {
            return;
        }
        self.open_channel(&mut ch, &dir, true);
    }

    /// The catalog is open: open the one read-only reader, then reconcile to the stored `mcp.enabled` (the database
    /// wins over an `enabled` marker): bind or unbind, and move `starting` to `serving`. Calling it again changes
    /// nothing.
    pub fn on_catalog_open(self: &Arc<Self>) -> Result<(), ApiError> {
        self.open_reader()?;
        let mut ch = self.lock_channel();
        let want = self.stored_enabled()?;
        ch.enabled = want;
        if !want {
            self.disengage(&mut ch);
            return Ok(());
        }
        if self.web_blocked() {
            ch.enabled = false;
            self.disengage(&mut ch);
            ch.last_error = Some(NEEDS_API_KEY.to_string());
            tracing::warn!("mcp stays off: the web build has no api key");
            return Ok(());
        }
        self.engage(&mut ch);
        Ok(())
    }

    /// Turns assistant access on or off: stores the setting, then binds or closes the channel. On the web build, on
    /// is refused (`Invalid(NEEDS_API_KEY)`) without `ATHENAEUM_API_KEY`. A channel that cannot start is not an
    /// error: the returned status says `failed` or `elsewhere` and why.
    pub fn set_enabled(self: &Arc<Self>, on: bool) -> Result<McpStatus, ApiError> {
        let mut ch = self.lock_channel();
        if on && self.web_blocked() {
            tracing::warn!("mcp turn-on refused: the web build has no api key");
            return Err(ApiError::Invalid(NEEDS_API_KEY.to_string()));
        }
        self.store_enabled(on)?;
        ch.enabled = on;
        if on {
            self.engage(&mut ch);
            tracing::info!(state = ch.state.word(), "mcp turned on");
        } else {
            self.disengage(&mut ch);
            tracing::info!("mcp turned off");
        }
        Ok(self.status_of(&ch))
    }

    /// Makes a new key and cuts off every connected assistant; the old key stops working at once. On the web build
    /// this is the key an assistant presents to `/mcp`.
    pub fn regenerate_token(self: &Arc<Self>) -> Result<McpStatus, ApiError> {
        let mut ch = self.lock_channel();
        if ch.state == State::Elsewhere {
            // The shared key file belongs to the instance that serves; changing it would leave that one on the old key.
            tracing::warn!("mcp key not regenerated: another instance serves the assistant");
            return Err(ApiError::Conflict(ELSEWHERE.to_string()));
        }
        let dir = self.channel_dir()?;
        ensure_private_dir(&dir).map_err(|e| {
            tracing::error!(error = %e, path = %dir.display(), "mcp channel folder could not be created");
            ApiError::Internal(format!("could not create the assistant folder: {e}"))
        })?;
        let token = new_token();
        write_atomic(&dir.join("token"), token.as_bytes(), true).map_err(|e| {
            tracing::error!(error = %e, path = %dir.display(), "mcp key could not be written");
            ApiError::Internal(format!("could not write the assistant key: {e}"))
        })?;
        ch.token = Some(token);
        if let Some(run) = ch.run.as_mut() {
            run.conns.cancel();
            run.conns = run.cancel.child_token();
        }
        tracing::info!("mcp key regenerated; connections cut off");
        Ok(self.status_of(&ch))
    }

    /// What the Settings page shows.
    pub fn status(&self) -> McpStatus {
        let ch = self.lock_channel();
        self.status_of(&ch)
    }

    /// Whether `presented` is the key (compared as SHA-256 digests). The web `/mcp` route calls it.
    pub fn check_token(&self, presented: &str) -> bool {
        let ch = self.lock_channel();
        ch.token.as_deref().is_some_and(|stored| tokens_match(stored, presented))
    }

    /// Assistant access is on and answering.
    pub fn is_serving(&self) -> bool {
        self.lock_channel().state == State::Serving
    }

    /// The app is quitting: close the listener and every connection and remove the socket and `endpoint`. The `enabled`
    /// marker, `tools.json` and the key stay, so the next start binds early and a bridge still has its tool list.
    pub fn shutdown(&self) {
        let mut ch = self.lock_channel();
        self.close_channel(&mut ch, false);
        if ch.state != State::Elsewhere {
            ch.state = State::Off;
        }
    }

    /// The server's `tools/list` result, as written to `tools.json`: what a bridge serves when the app is not running.
    pub fn tool_list_json(&self) -> String {
        let tools = AthenaeumMcp::catalog_tools().list_all();
        match serde_json::to_value(&tools) {
            Ok(tools) => serde_json::json!({ "tools": tools }).to_string(),
            Err(e) => {
                tracing::error!(error = %e, "mcp tool list could not be serialized");
                r#"{"tools":[]}"#.to_string()
            }
        }
    }

    /// The 30-second check, callable on its own: when the socket file has vanished (a `$TMPDIR` cleaner), bind it again.
    /// Connections already made are not touched.
    pub fn recheck_endpoint(self: &Arc<Self>) {
        let mut ch = self.lock_channel();
        if !matches!(ch.state, State::Starting | State::Serving) {
            return;
        }
        let Some(endpoint) = ch.endpoint.clone() else { return };
        if endpoint_present(&endpoint) {
            return;
        }
        let Ok(dir) = self.channel_dir() else { return };
        let Some(run) = ch.run.as_mut() else { return };
        run.listener.cancel();
        run.acceptor.close();
        let handle = run.handle.clone();
        let rebound = {
            let _enter = handle.enter();
            channel::bind(&dir)
        };
        match rebound {
            Ok((endpoint, listener)) => {
                let listener_ct = run.cancel.child_token();
                run.listener = listener_ct.clone();
                let (acceptor, incoming) = spawn_acceptor(&handle, listener);
                run.acceptor = acceptor;
                handle.spawn(accept_loop(Arc::downgrade(self), incoming, listener_ct, run.sessions.clone()));
                if let Err(e) = write_atomic(&dir.join("endpoint"), endpoint.to_text().as_bytes(), false) {
                    tracing::warn!(error = %e, "mcp endpoint file could not be rewritten");
                }
                tracing::info!(addr = %endpoint.to_text(), "mcp socket rebound");
                ch.endpoint = Some(endpoint);
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp socket could not be rebound; trying again later");
                ch.last_error = Some(format!("the assistant channel was lost and could not be rebound: {e}"));
            }
        }
    }

    // ---- internals (the channel mutex is held) ----

    fn status_of(&self, ch: &Channel) -> McpStatus {
        let desktop = self.is_desktop();
        let build = if self.identity.server_name.ends_with("-dev") { "dev" } else { "release" };
        let bridge = if desktop { std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.join(BRIDGE_FILE))) } else { None };
        let app_data = if desktop { self.app_data() } else { None };
        let config = bridge.as_ref().map(|b| desktop_client_config(self.identity.server_name, b, build, app_data.as_deref()));
        McpStatus {
            enabled: ch.enabled,
            state: ch.state.word().to_string(),
            server_name: self.identity.server_name.to_string(),
            endpoint: ch.endpoint.as_ref().map(Endpoint::to_text),
            token: if desktop { None } else { ch.token.clone() },
            bridge_found: bridge.as_ref().is_some_and(|b| b.is_file()),
            bridge_path: bridge.map(|b| b.to_string_lossy().into_owned()),
            client_config: config.as_ref().map(|(json, _)| json.clone()),
            claude_code_command: config.map(|(_, command)| command),
            last_error: ch.last_error.clone(),
            blocked_reason: self.web_blocked().then(|| NEEDS_API_KEY.to_string()),
        }
    }

    fn fail(&self, ch: &mut Channel, message: String) {
        tracing::error!(error = %message, "mcp channel could not start");
        // Dropping the run closes the listener and every connection.
        drop(ch.run.take());
        if let Some(endpoint) = ch.endpoint.take() {
            remove_endpoint(&endpoint);
        }
        // What this instance wrote is stale now; another instance's `endpoint` file is not ours to remove.
        if ch.state != State::Elsewhere {
            if let Ok(dir) = self.channel_dir() {
                remove_file_quiet(&dir.join("endpoint"));
            }
        }
        ch.state = State::Failed;
        ch.last_error = Some(message);
    }

    /// Reads the key file, or makes the key and writes it.
    fn ensure_token(&self, ch: &mut Channel, dir: &Path) -> Result<(), ()> {
        let made = (|| -> std::io::Result<String> {
            ensure_private_dir(dir)?;
            if let Some(token) = read_token(dir)? {
                return Ok(token);
            }
            let token = new_token();
            write_atomic(&dir.join("token"), token.as_bytes(), true)?;
            Ok(token)
        })();
        match made {
            Ok(token) => {
                ch.token = Some(token);
                Ok(())
            }
            Err(e) => {
                self.fail(ch, format!("could not prepare the assistant key: {e}"));
                Err(())
            }
        }
    }

    /// Turn on, as the setting says (the setting is already stored): make the key, and on the desktop write the marker,
    /// bind (or keep the early bind) and write the channel files.
    fn engage(self: &Arc<Self>, ch: &mut Channel) {
        let dir = match self.channel_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.fail(ch, e.to_string());
                return;
            }
        };
        if self.ensure_token(ch, &dir).is_err() {
            return;
        }
        ch.last_error = None;
        if !self.is_desktop() {
            ch.state = State::Serving;
            return;
        }
        if ch.run.is_some() && matches!(ch.state, State::Starting | State::Serving) {
            // Bound already (the early start, or a repeated call): only the state and the tool list move on.
            if self.catalog_open() {
                ch.state = State::Serving;
            }
        } else {
            self.open_channel(ch, &dir, false);
        }
        if ch.run.is_some() {
            // The marker is the owner's, written only once this instance holds the endpoint.
            if let Err(e) = write_atomic(&dir.join("enabled"), b"1", false) {
                self.fail(ch, format!("could not write the assistant marker: {e}"));
                return;
            }
            if let Err(e) = write_atomic(&dir.join("tools.json"), self.tool_list_json().as_bytes(), false) {
                self.fail(ch, format!("could not write the assistant tool list: {e}"));
            }
        }
    }

    /// Binds the channel, starts the accept loop and the 30 s check, and writes `endpoint`.
    fn open_channel(self: &Arc<Self>, ch: &mut Channel, dir: &Path, early: bool) {
        let handle = match runtime_handle() {
            Ok(handle) => handle,
            Err(e) => {
                self.fail(ch, format!("the assistant channel has no runtime: {e}"));
                return;
            }
        };
        let bound = {
            let _enter = handle.enter();
            channel::bind(dir)
        };
        let (endpoint, listener) = match bound {
            Ok(bound) => bound,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::warn!("mcp channel is served by another instance of this build");
                ch.state = State::Elsewhere;
                ch.last_error = Some(ELSEWHERE.to_string());
                ch.endpoint = None;
                return;
            }
            Err(e) => {
                self.fail(ch, format!("the assistant channel could not be opened: {e}"));
                return;
            }
        };
        let cancel = CancellationToken::new();
        let sessions = Sessions::default();
        let (acceptor, incoming) = spawn_acceptor(&handle, listener);
        let run = Run {
            conns: cancel.child_token(),
            listener: cancel.child_token(),
            cancel,
            acceptor,
            sessions: sessions.clone(),
            handle: handle.clone(),
        };
        handle.spawn(accept_loop(Arc::downgrade(self), incoming, run.listener.clone(), sessions));
        handle.spawn(watch_endpoint(Arc::downgrade(self), run.cancel.clone()));
        if let Err(e) = write_atomic(&dir.join("endpoint"), endpoint.to_text().as_bytes(), false) {
            drop(run);
            remove_endpoint(&endpoint);
            self.fail(ch, format!("could not write the assistant endpoint: {e}"));
            return;
        }
        tracing::info!(addr = %endpoint.to_text(), "mcp channel bound");
        ch.endpoint = Some(endpoint);
        ch.run = Some(run);
        ch.last_error = None;
        ch.state = if early || !self.catalog_open() { State::Starting } else { State::Serving };
    }

    /// Turn off, as the setting says: close the channel, remove `endpoint`, `enabled` and `tools.json`; the key stays.
    fn disengage(&self, ch: &mut Channel) {
        self.close_channel(ch, true);
        ch.state = State::Off;
        ch.last_error = None;
    }

    /// Cancels the listener and every connection and removes what this instance made. A second instance's
    /// (`elsewhere`) files are not ours and stay. `forget` also removes the marker and the tool list.
    fn close_channel(&self, ch: &mut Channel, forget: bool) {
        let owned = ch.state != State::Elsewhere;
        // Dropping the run closes the listener and every connection before this returns.
        drop(ch.run.take());
        if let Some(endpoint) = ch.endpoint.take() {
            if owned {
                remove_endpoint(&endpoint);
            }
        }
        if !owned {
            return;
        }
        if let Ok(dir) = self.channel_dir() {
            remove_file_quiet(&dir.join("endpoint"));
            if forget {
                remove_file_quiet(&dir.join("enabled"));
                remove_file_quiet(&dir.join("tools.json"));
            }
        }
    }

    // ---- what a connection asks ----

    /// A token for a connection just accepted, or `None` when the channel is closed.
    fn connection_token(&self) -> Option<CancellationToken> {
        self.lock_channel().run.as_ref().map(|run| run.conns.child_token())
    }

    /// Judges one handshake line against the current state and key.
    fn judge(&self, line: &str) -> Result<(), (HandshakeError, String)> {
        match parse_hello(line) {
            Hello::Malformed => Err((HandshakeError::Token, "this is not an Athenaeum assistant handshake".to_string())),
            Hello::Later => Err((
                HandshakeError::Version,
                format!("this Athenaeum speaks assistant channel protocol {} at most; update it", channel::PROTOCOL_VERSION),
            )),
            Hello::Ok { token } => {
                let ch = self.lock_channel();
                if !matches!(ch.state, State::Starting | State::Serving) {
                    return Err((HandshakeError::Off, "assistant access is turned off in Athenaeum".to_string()));
                }
                if ch.state == State::Starting || !self.catalog_open() {
                    return Err((HandshakeError::Starting, "Athenaeum is starting; its catalog is not open yet".to_string()));
                }
                match ch.token.as_deref() {
                    Some(stored) if tokens_match(stored, token) => Ok(()),
                    _ => Err((HandshakeError::Token, "the key does not match; copy the configuration again from Athenaeum".to_string())),
                }
            }
        }
    }
}

/// Takes the connections the acceptor task hands over until cancelled; each gets its own closable task, which the run's
/// sessions list can drop at once. Holds the host weakly, so a dropped host ends it.
async fn accept_loop(
    host: Weak<McpHost>,
    mut incoming: tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Stream>>,
    ct: CancellationToken,
    sessions: Sessions,
) {
    loop {
        let accepted = tokio::select! {
            biased;
            () = ct.cancelled() => return,
            accepted = incoming.recv() => accepted,
        };
        match accepted {
            None => return,
            Some(Ok(stream)) => {
                let Some(host) = host.upgrade() else { return };
                let Some(conn_ct) = host.connection_token() else { continue };
                let session = spawn_closable(&tokio::runtime::Handle::current(), serve_connection(host, stream, conn_ct));
                sessions.add(session);
            }
            Some(Err(e)) => tracing::warn!(error = %e, "mcp channel accept failed"),
        }
    }
}

/// The 30 s check that the socket file is still there.
async fn watch_endpoint(host: Weak<McpHost>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(ENDPOINT_RECHECK) => {}
        }
        let Some(host) = host.upgrade() else { return };
        host.recheck_endpoint();
    }
}

/// Reads one handshake line without reading past it (the next bytes are MCP). `Ok(None)`: too long.
async fn read_handshake_line(stream: &mut Stream) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(64);
    loop {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        if line.len() + 1 >= HANDSHAKE_MAX_BYTES {
            return Ok(None);
        }
        line.push(byte);
    }
}

/// One client: the handshake line, the reply, then MCP over the same stream until either side ends or `ct` is
/// cancelled.
async fn serve_connection(host: Arc<McpHost>, mut stream: Stream, ct: CancellationToken) {
    let line = match tokio::time::timeout(HANDSHAKE_WINDOW, read_handshake_line(&mut stream)).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            let reply = HandshakeError::Token.reply("the handshake line is too long");
            let _ = stream.write_all(reply.as_bytes()).await;
            tracing::debug!(reason = "too_long", "mcp client refused");
            // Closing with the rest of the line unread can reset the connection before the client reads the reply.
            let mut rest = [0u8; 512];
            let _ = tokio::time::timeout(Duration::from_millis(100), async {
                while matches!(stream.read(&mut rest).await, Ok(n) if n > 0) {}
            })
            .await;
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "mcp client left before its handshake");
            return;
        }
        Err(_) => {
            tracing::debug!(reason = "timeout", "mcp client refused");
            return;
        }
    };
    if let Err((refusal, message)) = host.judge(&line) {
        tracing::debug!(reason = refusal.word(), "mcp client refused");
        let _ = stream.write_all(refusal.reply(&message).as_bytes()).await;
        let _ = stream.shutdown().await;
        return;
    }
    let identity = host.identity();
    let reply = format!("OK {} {}\n", identity.server_name, identity.version);
    if let Err(e) = stream.write_all(reply.as_bytes()).await {
        tracing::debug!(error = %e, "mcp client left before its reply");
        return;
    }
    tracing::debug!("mcp client connected");
    let (reader, writer) = tokio::io::split(stream);
    match rmcp::service::serve_server_with_ct(host.server(), (reader, writer), ct).await {
        Ok(running) => {
            let _ = running.waiting().await;
        }
        Err(e) => tracing::debug!(error = %e, "mcp session ended before it started"),
    }
    tracing::debug!("mcp client gone");
}
