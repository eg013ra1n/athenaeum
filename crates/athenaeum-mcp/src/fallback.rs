//! What the bridge says itself when the app cannot: [`Unavailable`], its words, and [`FallbackServer`], the tiny rmcp
//! server that carries them to the client.
//!
//! A client that starts before the app (Claude desktop starts its bridges at its own launch) must still see the real
//! tools and get the reason when it calls one, so the fallback lists the app's own `tools/list` result from
//! `<app-data>/mcp/tools.json` (written at each bind) and answers a call of ANY tool name with the reason as an error
//! result, in both halves: the text and `structuredContent`.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Value};

use crate::paths::Build;

/// The one tool the fallback lists when the app's own list is not on disk.
pub const STATUS_TOOL: &str = "athenaeum_status";

/// Why the app did not answer the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No program listens: the app is not running (or its channel is gone).
    NotRunning,
    /// Assistant access is turned off in Settings.
    Off,
    /// The app is up but has not opened its catalog yet.
    Starting,
    /// The app refused the key: it was regenerated.
    TokenRejected,
    /// The app speaks an older or newer channel protocol than this bridge.
    Outdated,
    /// The connection reached the other build.
    WrongBuild { reached: String },
}

impl Unavailable {
    /// The word in `structuredContent.state`.
    pub fn state(&self) -> &'static str {
        match self {
            Self::NotRunning => "not_running",
            Self::Off => "off",
            Self::Starting => "starting",
            Self::TokenRejected => "token_rejected",
            Self::Outdated => "outdated",
            Self::WrongBuild { .. } => "wrong_build",
        }
    }
}

/// The sentence a person reads, for the build the bridge is set up for.
pub fn unavailable_text(u: &Unavailable, build: Build) -> String {
    let b = match build {
        Build::Release => "",
        Build::Dev => " (dev build)",
    };
    match u {
        Unavailable::NotRunning => format!("Athenaeum{b} is not running. Start it, then ask again."),
        Unavailable::Off => format!(
            "Assistant access is off in Athenaeum{b}: Settings → General → Assistant access (MCP). Turn it on, then ask again."
        ),
        Unavailable::Starting => format!("Athenaeum{b} is starting and has not opened its catalog yet. Ask again in a moment."),
        Unavailable::TokenRejected => format!(
            "Athenaeum{b} refused this connection: its assistant access key changed. Ask again; if this repeats, restart this server in your client."
        ),
        Unavailable::Outdated => format!(
            "Athenaeum{b} no longer accepts this assistant bridge (version mismatch). Restart Athenaeum so it refreshes its bridge copy, then restart this server in your client."
        ),
        Unavailable::WrongBuild { reached } => format!(
            "This connection is set up for Athenaeum{b} but reached {reached}. Copy the client configuration again from that build's Settings."
        ),
    }
}

/// The rmcp server a client talks to while the app does not answer.
pub struct FallbackServer {
    pub reason: Unavailable,
    pub build: Build,
    /// `<app-data>/mcp/tools.json`, when the app-data folder is known; read at each `tools/list`.
    pub tools_file: Option<PathBuf>,
}

impl FallbackServer {
    fn text(&self) -> String {
        unavailable_text(&self.reason, self.build)
    }

    /// The app's own tool list from `tools.json`; `None` when the file is missing, does not parse or lists nothing.
    fn app_tools(&self) -> Option<Vec<Tool>> {
        let path = self.tools_file.as_ref()?;
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(error = %e, path = %path.display(), "tool list file could not be read");
                }
                return None;
            }
        };
        let tools = serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| e.to_string())
            .and_then(|v| serde_json::from_value::<Vec<Tool>>(v.get("tools").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string()));
        match tools {
            Ok(tools) if !tools.is_empty() => Some(tools),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "tool list file does not parse");
                None
            }
        }
    }

    fn status_tool() -> Tool {
        let schema: JsonObject = json!({ "type": "object", "properties": {} }).as_object().cloned().unwrap_or_default();
        Tool::new(STATUS_TOOL, "Why Athenaeum's assistant access is not answering", Arc::new(schema))
    }
}

impl ServerHandler for FallbackServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build())
            .with_server_info(Implementation::new(self.build.server_name(), env!("CARGO_PKG_VERSION")))
            .with_instructions(self.text())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.app_tools().unwrap_or_else(|| vec![Self::status_tool()])))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let message = self.text();
        tracing::debug!(tool = %request.name, state = self.reason.state(), "tool call answered in words");
        let mut result = CallToolResult::error(vec![ContentBlock::text(message.clone())]);
        result.structured_content = Some(json!({
            "state": self.reason.state(),
            "build": self.build.server_name(),
            "message": message,
        }));
        Ok(result.into())
    }
}
