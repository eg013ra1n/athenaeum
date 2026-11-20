//! The local channel an assistant's bridge connects to (MCP plan, Task 6): a Unix domain socket, or a named pipe on
//! Windows, behind one small abstraction ([`Endpoint`], [`bind`], [`Listener`], [`connect`], [`Stream`]).
//!
//! The channel files live in `<app-data>/mcp/`: `token` (kept across off/on), `endpoint` (where to connect),
//! `enabled` (the marker that lets the desktop bind early) and `tools.json` (the server's `tools/list`, which a bridge
//! serves while the app is not running). The wire is frozen and append-only: a later host still serves
//! [`HANDSHAKE_PREFIX`] `/1`.
//!
//! ```text
//! bridge -> host   ATHENAEUM-MCP/1 <token>\n
//! host -> bridge   OK <server name> <version>\n                  then MCP, newline-delimited JSON-RPC
//!                  ERR <starting|off|token|version> <message>\n  then the host closes
//! ```
//!
//! The Windows half is a thin mirror of the Unix half; it is compiled only by the Windows build.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The first word of the handshake line, with the channel protocol version after the slash.
pub const HANDSHAKE_PREFIX: &str = "ATHENAEUM-MCP/1";

/// The highest channel protocol version this host speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest handshake line a host reads, newline included.
pub const HANDSHAKE_MAX_BYTES: usize = 256;

/// A Unix socket path must fit `sun_path` (104 bytes on macOS, 108 on Linux, with the terminating NUL).
const SOCKET_PATH_MAX_BYTES: usize = 100;

/// Why a handshake was refused; the word after `ERR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// The app is up but its catalog is not open yet.
    Starting,
    /// Assistant access is turned off.
    Off,
    /// The key is wrong (or the line is not a handshake).
    Token,
    /// The bridge speaks a later channel protocol than this host.
    Version,
}

impl HandshakeError {
    /// The word on the wire: `starting`, `off`, `token`, `version`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Off => "off",
            Self::Token => "token",
            Self::Version => "version",
        }
    }

    /// The reply line, newline included.
    pub fn reply(self, message: &str) -> String {
        format!("ERR {} {message}\n", self.word())
    }
}

/// `<app-data>/mcp`: where the channel files live.
pub fn mcp_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("mcp")
}

/// Where a bridge connects: a Unix socket path, or a Windows pipe name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Unix(PathBuf),
    Pipe(String),
}

impl Endpoint {
    /// Reads the `endpoint` file's text: `unix:<path>` or `pipe:<name>`; surrounding whitespace is ignored.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(path) = text.strip_prefix("unix:") {
            return (!path.is_empty()).then(|| Self::Unix(PathBuf::from(path)));
        }
        if let Some(name) = text.strip_prefix("pipe:") {
            return (!name.is_empty()).then(|| Self::Pipe(name.to_string()));
        }
        None
    }

    /// The `endpoint` file's text (without a trailing newline).
    pub fn to_text(&self) -> String {
        match self {
            Self::Unix(path) => format!("unix:{}", path.to_string_lossy()),
            Self::Pipe(name) => format!("pipe:{name}"),
        }
    }
}

/// The first 4 bytes of the SHA-256 of the app-data dir's path, as 8 hex characters: a short, stable name for this
/// install's socket directory or pipe.
fn short_id(app_data_dir: &Path) -> String {
    let digest = Sha256::digest(app_data_dir.to_string_lossy().as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// The socket path for the channel files in `dir` (`<app-data>/mcp`): `<dir>/mcp.sock` when it fits a socket address,
/// else `<temp dir>/athenaeum-mcp-<8 hex>/mcp.sock`.
#[cfg_attr(not(unix), allow(dead_code))]
fn socket_path(dir: &Path) -> PathBuf {
    let near = dir.join("mcp.sock");
    if near.as_os_str().len() <= SOCKET_PATH_MAX_BYTES {
        return near;
    }
    let app_data = dir.parent().unwrap_or(dir);
    std::env::temp_dir().join(format!("athenaeum-mcp-{}", short_id(app_data))).join("mcp.sock")
}

/// The Windows pipe name for the channel files in `dir`.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name(dir: &Path) -> String {
    format!(r"\\.\pipe\athenaeum-mcp-{}", short_id(dir.parent().unwrap_or(dir)))
}

/// One duplex connection, either end, on either OS.
pub struct Stream(Box<dyn Duplex>);

trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}

impl Stream {
    fn new<T: AsyncRead + AsyncWrite + Send + Unpin + 'static>(inner: T) -> Self {
        Self(Box::new(inner))
    }
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stream")
    }
}

impl AsyncRead for Stream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

/// The error a second listener on one endpoint gets: another instance of this build serves it.
pub fn in_use_error() -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, "another Athenaeum of this build serves the assistant")
}

// ---- Unix ----

#[cfg(unix)]
pub struct Listener {
    inner: tokio::net::UnixListener,
}

#[cfg(unix)]
impl Listener {
    pub async fn accept(&self) -> io::Result<Stream> {
        let (stream, _) = self.inner.accept().await?;
        Ok(Stream::new(stream))
    }
}

/// Binds the channel for the files in `dir` (`<app-data>/mcp`) and says where it is.
///
/// Call it inside a tokio runtime context (it registers the socket with that runtime). A socket file nobody listens
/// on (a crash left it) is removed first; one that accepts a connection belongs to another instance of this build and
/// is an [`io::ErrorKind::AddrInUse`] error ([`in_use_error`]).
#[cfg(unix)]
pub fn bind(dir: &Path) -> io::Result<(Endpoint, Listener)> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let path = socket_path(dir);
    let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the socket path has no directory"))?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    // A directory that is not ours cannot be made private: refuse it rather than listen in someone else's place.
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    if std::fs::symlink_metadata(&path).is_ok() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            return Err(in_use_error());
        }
        std::fs::remove_file(&path)?;
    }
    let std_listener = std::os::unix::net::UnixListener::bind(&path)?;
    std_listener.set_nonblocking(true)?;
    let inner = tokio::net::UnixListener::from_std(std_listener)?;
    Ok((Endpoint::Unix(path), Listener { inner }))
}

/// Opens a connection to an endpoint (the host's own tests and any in-process client; the bridge has its own).
#[cfg(unix)]
pub async fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    match endpoint {
        Endpoint::Unix(path) => Ok(Stream::new(tokio::net::UnixStream::connect(path).await?)),
        Endpoint::Pipe(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "a named pipe endpoint on a Unix host")),
    }
}

/// Removes what a listener left on disk: the socket file and, when it sat in a directory of ours in the temp dir,
/// that directory if it is empty.
#[cfg(unix)]
pub fn remove_endpoint(endpoint: &Endpoint) {
    if let Endpoint::Unix(path) = endpoint {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(error = %e, path = %path.display(), "mcp socket file could not be removed");
            }
        }
        if let Some(parent) = path.parent() {
            if parent.file_name().is_some_and(|n| n.to_string_lossy().starts_with("athenaeum-mcp-")) {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// Whether the endpoint's file still exists; a pipe has none to lose.
#[cfg(unix)]
pub fn endpoint_present(endpoint: &Endpoint) -> bool {
    match endpoint {
        Endpoint::Unix(path) => std::fs::symlink_metadata(path).is_ok(),
        Endpoint::Pipe(_) => true,
    }
}

// ---- Windows: a thin mirror of the above ----

#[cfg(windows)]
pub struct Listener {
    name: String,
    next: tokio::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
impl Listener {
    pub async fn accept(&self) -> io::Result<Stream> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let mut next = self.next.lock().await;
        next.connect().await?;
        // The connected instance is handed out; a fresh one waits for the next client.
        let fresh = ServerOptions::new().reject_remote_clients(true).create(&self.name)?;
        let connected = std::mem::replace(&mut *next, fresh);
        Ok(Stream::new(connected))
    }
}

/// See the Unix `bind`. Windows refuses a second first instance of the pipe as access denied. That refusal means
/// "another instance of this build serves it" only when a client finds something answering on the pipe; otherwise
/// (a leftover instance still closing) it is retried a few times and then reported as the OS error.
#[cfg(windows)]
pub fn bind(dir: &Path) -> io::Result<(Endpoint, Listener)> {
    use tokio::net::windows::named_pipe::ServerOptions;
    const BIND_TRIES: u32 = 5;
    let name = pipe_name(dir);
    let mut tries = 0;
    let first = loop {
        match ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true).create(&name) {
            Ok(first) => break first,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                if pipe_answers(&name) {
                    return Err(in_use_error());
                }
                tries += 1;
                if tries >= BIND_TRIES {
                    tracing::warn!(error = %e, tries, "mcp pipe is refused and nothing answers on it");
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    };
    Ok((Endpoint::Pipe(name.clone()), Listener { name, next: tokio::sync::Mutex::new(first) }))
}

/// The Windows counterpart of the Unix connect probe: a client that opens the pipe (or finds every instance busy)
/// shows that a live server owns it. Writes nothing and is dropped at once.
#[cfg(windows)]
fn pipe_answers(name: &str) -> bool {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    match ClientOptions::new().open(name) {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_PIPE_BUSY),
    }
}

#[cfg(windows)]
pub async fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    match endpoint {
        Endpoint::Pipe(name) => {
            let mut tries = 0;
            loop {
                match ClientOptions::new().open(name) {
                    Ok(client) => return Ok(Stream::new(client)),
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                        tries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Endpoint::Unix(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "a Unix socket endpoint on a Windows host")),
    }
}

/// A pipe has no file to remove: it goes with its last instance.
#[cfg(windows)]
pub fn remove_endpoint(_endpoint: &Endpoint) {}

/// A pipe cannot vanish while its listener lives.
#[cfg(windows)]
pub fn endpoint_present(_endpoint: &Endpoint) -> bool {
    true
}

// ---- tasks whose futures can be dropped from outside ----

/// What the driver of a closable task shares with its [`Closer`].
struct CloseState {
    fut: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    waker: Option<Waker>,
    closed: bool,
    finished: bool,
}

fn lock_state(state: &Mutex<CloseState>) -> MutexGuard<'_, CloseState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Closes a task made by [`spawn_closable`]: [`Closer::close`] drops the task's future (and what it owns: a listener,
/// a connection, a Windows pipe instance) on the calling thread, at once, rather than when the runtime next polls it.
#[derive(Clone)]
pub struct Closer {
    state: Arc<Mutex<CloseState>>,
}

impl Closer {
    /// Drops the future now. A future being polled at this very moment on another thread is dropped by that poll's end.
    /// Calling it again does nothing.
    pub fn close(&self) {
        let (fut, waker) = {
            let mut state = lock_state(&self.state);
            state.closed = true;
            (state.fut.take(), state.waker.take())
        };
        // Dropped outside the lock: what the future owns may run its own `Drop`.
        drop(fut);
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// The future ended, by itself or by [`Closer::close`].
    pub fn is_finished(&self) -> bool {
        let state = lock_state(&self.state);
        state.finished || state.closed
    }
}

/// Runs `fut` on `handle` like a spawned task, but the returned [`Closer`] can drop it synchronously. The future is
/// held in a slot and taken out only while it is polled, so `close` never waits on a poll (a poll may take the
/// caller's own locks).
pub fn spawn_closable<F>(handle: &tokio::runtime::Handle, fut: F) -> Closer
where
    F: Future<Output = ()> + Send + 'static,
{
    let state = Arc::new(Mutex::new(CloseState { fut: Some(Box::pin(fut)), waker: None, closed: false, finished: false }));
    let shared = Arc::clone(&state);
    handle.spawn(std::future::poll_fn(move |cx| {
        let mut fut = {
            let mut state = lock_state(&shared);
            if state.closed {
                state.finished = true;
                return Poll::Ready(());
            }
            state.waker = Some(cx.waker().clone());
            match state.fut.take() {
                Some(fut) => fut,
                None => {
                    state.finished = true;
                    return Poll::Ready(());
                }
            }
        };
        let polled = fut.as_mut().poll(cx);
        let mut state = lock_state(&shared);
        match polled {
            Poll::Pending if !state.closed => {
                state.fut = Some(fut);
                Poll::Pending
            }
            _ => {
                state.finished = true;
                // `state` (declared last) is released before `fut` drops.
                Poll::Ready(())
            }
        }
    }));
    Closer { state }
}

/// Accepts connections on `listener` in a closable task and sends each to the returned receiver. Dropping the task (the
/// [`Closer`]) drops the listener, and with it the waiting pipe instance on Windows.
pub fn spawn_acceptor(
    handle: &tokio::runtime::Handle,
    listener: Listener,
) -> (Closer, tokio::sync::mpsc::UnboundedReceiver<io::Result<Stream>>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let closer = spawn_closable(handle, async move {
        loop {
            let accepted = listener.accept().await;
            let failed = accepted.is_err();
            if tx.send(accepted).is_err() {
                return;
            }
            if failed {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    });
    (closer, rx)
}

/// Creates the channel folder, private to the owner on Unix. A folder that already exists and is not ours cannot be made
/// private: that is an error rather than a channel in someone else's place.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

// ---- the channel files ----

/// A new key: 64 hex characters from the OS random source (two v4 UUIDs' random bits).
pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Writes a channel file through a temp file and a rename, so a reader never sees half a file. `private` makes it
/// readable by the owner only (the token).
pub fn write_atomic(path: &Path, contents: &[u8], private: bool) -> io::Result<()> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The key in the `token` file, if there is one.
pub fn read_token(dir: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(dir.join("token")) {
        Ok(text) => {
            let text = text.trim().to_string();
            Ok((!text.is_empty()).then_some(text))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether two keys are equal, compared as SHA-256 digests with no early exit.
pub fn tokens_match(stored: &str, presented: &str) -> bool {
    let a = Sha256::digest(stored.as_bytes());
    let b = Sha256::digest(presented.as_bytes());
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What a handshake line asks for, before the host judges it.
#[derive(Debug, PartialEq, Eq)]
pub enum Hello<'a> {
    /// A channel protocol this host speaks, with the key the bridge presented.
    Ok { token: &'a str },
    /// Not a handshake.
    Malformed,
    /// A later channel protocol than ours.
    Later,
}

/// Parses `ATHENAEUM-MCP/<n> <token>` (newline already stripped).
pub fn parse_hello(line: &str) -> Hello<'_> {
    let line = line.trim_end_matches(['\r', '\n']);
    let Some(rest) = line.strip_prefix("ATHENAEUM-MCP/") else {
        return Hello::Malformed;
    };
    let (version, token) = rest.split_once(' ').unwrap_or((rest, ""));
    match version.parse::<u32>() {
        Ok(n) if n > PROTOCOL_VERSION => Hello::Later,
        Ok(n) if n >= 1 => Hello::Ok { token },
        _ => Hello::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_lines_parse() {
        assert_eq!(parse_hello("ATHENAEUM-MCP/1 abc"), Hello::Ok { token: "abc" });
        assert_eq!(parse_hello("ATHENAEUM-MCP/1 abc\r"), Hello::Ok { token: "abc" });
        assert_eq!(parse_hello("ATHENAEUM-MCP/1"), Hello::Ok { token: "" });
        assert_eq!(parse_hello("ATHENAEUM-MCP/9 abc"), Hello::Later);
        assert_eq!(parse_hello("ATHENAEUM-MCP/0 abc"), Hello::Malformed);
        assert_eq!(parse_hello("ATHENAEUM-MCP/x abc"), Hello::Malformed);
        assert_eq!(parse_hello("hello"), Hello::Malformed);
        assert_eq!(parse_hello(""), Hello::Malformed);
    }

    #[test]
    fn tokens_compare_by_digest() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", ""));
    }
}
