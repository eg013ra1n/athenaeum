//! The MCP server in core (MCP plan, Task 5): the host both Athenaeum hosts embed, the answer shape every tool
//! returns, and the read-only catalog questions served over one [`Scope`](crate::scope::Scope).
//!
//! `McpHost` owns what a server needs from the app (the `ServiceContext`, who the build is, a way to bring the window
//! forward) and the ONE read-only catalog connection every question reads through (plan R11). `AthenaeumMcp` is the
//! rmcp handler built over a host; a transport (Task 6's local channel, Task 8's Streamable HTTP) serves it.
//!
//! A question changes nothing and no tool applies a plan: changes are plans the user applies in Athenaeum's window.
//! `docs/mcp/README.md` is the reference.

pub mod answer;
pub mod server;
pub mod tools;

#[cfg(test)]
#[path = "tests/server_tests.rs"]
mod server_tests;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::api::ApiError;
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;

pub use answer::{to_tool_result, Answer};
pub use server::AthenaeumMcp;

/// Which host embeds the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    Desktop,
    Web,
}

/// Who the server is: the name every answer's first line carries, the app's version and the host.
#[derive(Debug, Clone)]
pub struct McpIdentity {
    /// [`mcp_server_name`] in production.
    pub server_name: &'static str,
    pub version: &'static str,
    pub kind: HostKind,
    /// Web: `ATHENAEUM_API_KEY` is set (the MCP cannot be enabled without it, O5). Ignored on Desktop.
    pub web_api_key_set: bool,
}

/// The server's name: `athenaeum-dev` in a debug build, `athenaeum` in a release build, so one assistant can have
/// both configured at once.
pub fn mcp_server_name() -> &'static str {
    if cfg!(debug_assertions) {
        "athenaeum-dev"
    } else {
        "athenaeum"
    }
}

/// What an answer says it came from; the structured half's `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSource {
    /// The build: the server name (`athenaeum` / `athenaeum-dev`).
    pub build: String,
    pub version: String,
    /// The catalog database's full path.
    pub catalog: String,
}

/// How long a question waits for the catalog's writer before it gives up.
const READER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The server's side of the app: one per process, shared by every connection of every transport.
pub struct McpHost {
    ctx: Arc<ServiceContext>,
    identity: McpIdentity,
    emitter: Arc<dyn ProgressEmitter>,
    bring_to_front: Arc<dyn Fn() + Send + Sync>,
    has_viewer: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The one `SQLITE_OPEN_READ_ONLY` connection (R11), opened by [`McpHost::on_catalog_open`].
    reader: OnceLock<Mutex<Connection>>,
}

impl McpHost {
    /// A host that has opened nothing: until [`McpHost::on_catalog_open`] runs, [`McpHost::reader`] and
    /// [`McpHost::source`] answer "Athenaeum is starting". `has_viewer` says whether a window shows the app (desktop:
    /// always; web: an SSE client is connected).
    pub fn new(
        ctx: Arc<ServiceContext>,
        identity: McpIdentity,
        emitter: Arc<dyn ProgressEmitter>,
        bring_to_front: Arc<dyn Fn() + Send + Sync>,
        has_viewer: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self { ctx, identity, emitter, bring_to_front, has_viewer, reader: OnceLock::new() })
    }

    /// The catalog is open: open the one read-only reader connection and nothing else. Calling it again is a no-op.
    ///
    /// Task 6 extends this same function with the `mcp.enabled` reconcile and the channel bind; keep it the one
    /// place a host tells the server its catalog is there.
    pub fn on_catalog_open(self: &Arc<Self>) -> Result<(), ApiError> {
        if self.reader.get().is_some() {
            return Ok(());
        }
        let database = crate::api::db(&self.ctx)?;
        let path = database.path().to_path_buf();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| {
                tracing::error!(error = %e, path = %path.display(), "mcp reader could not open the catalog");
                ApiError::Internal(format!("could not open the catalog for reading: {e}"))
            })?;
        conn.busy_timeout(READER_BUSY_TIMEOUT).map_err(|e| {
            tracing::error!(error = %e, "mcp reader could not set its busy timeout");
            ApiError::Internal(format!("could not open the catalog for reading: {e}"))
        })?;
        // A second caller that lost the race keeps the first connection; the extra one closes here.
        let _ = self.reader.set(Mutex::new(conn));
        tracing::info!(path = %path.display(), "mcp reader opened");
        Ok(())
    }

    pub fn ctx(&self) -> &Arc<ServiceContext> {
        &self.ctx
    }

    pub fn identity(&self) -> &McpIdentity {
        &self.identity
    }

    pub fn emitter(&self) -> &Arc<dyn ProgressEmitter> {
        &self.emitter
    }

    /// Show Athenaeum's window (a plan opens there; Task 12).
    pub fn bring_to_front(&self) {
        (self.bring_to_front)();
    }

    /// A window is showing the app: a plan that waits for the user can be seen.
    pub fn has_viewer(&self) -> bool {
        (self.has_viewer)()
    }

    /// The one read-only catalog connection (R11). Questions hold the guard for the length of one question; a
    /// write through it fails, so no question can change the catalog.
    pub fn reader(&self) -> Result<MutexGuard<'_, Connection>, ApiError> {
        let reader = self.reader.get().ok_or_else(starting)?;
        // A panic in an earlier question leaves a read-only connection as it was.
        Ok(reader.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Where this answer comes from. `Conflict("Athenaeum is starting ...")` before the catalog is open.
    pub fn source(&self) -> Result<McpSource, ApiError> {
        if self.reader.get().is_none() {
            return Err(starting());
        }
        let database = crate::api::db(&self.ctx)?;
        Ok(McpSource {
            build: self.identity.server_name.to_string(),
            version: self.identity.version.to_string(),
            catalog: database.path().to_string_lossy().into_owned(),
        })
    }

    /// The rmcp handler over this host; one per connection, all sharing the host.
    pub fn server(self: &Arc<Self>) -> AthenaeumMcp {
        AthenaeumMcp::new(Arc::clone(self))
    }
}

fn starting() -> ApiError {
    ApiError::Conflict("Athenaeum is starting; its catalog is not open yet. Ask again in a moment.".to_string())
}
