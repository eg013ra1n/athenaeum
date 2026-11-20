//! The rmcp handler: what a client sees of Athenaeum (spec §7). One `AthenaeumMcp` per connection, all over the one
//! [`McpHost`]. Both protocol revisions rmcp supports (2025-11-25 and 2026-07-28) are served as it ships them:
//! `supported_protocol_versions` is the default.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool_handler, ServerHandler};

use super::McpHost;

/// What a client is told once, on connecting.
pub const INSTRUCTIONS: &str = "Athenaeum answers about one astrophotography catalog; every answer starts with the build and catalog it came from. Questions change nothing. Changes are plans: a planning tool opens the plan in Athenaeum's window, where only the user can apply it — no tool applies a plan. Ask plan_status for the outcome and never report a plan as applied before plan_status says so. Name frames by file name and folder, not by handles.";

#[derive(Clone)]
pub struct AthenaeumMcp {
    pub(crate) host: Arc<McpHost>,
    tool_router: ToolRouter<Self>,
}

impl AthenaeumMcp {
    pub(crate) fn new(host: Arc<McpHost>) -> Self {
        Self { host, tool_router: Self::all_tools() }
    }
}

impl AthenaeumMcp {
    /// Every tool the server offers: the catalog questions, the plan tools, the metadata plan and (with the solver) the
    /// plate-solve plan with its run's status and cancel. Each later planning tool joins here.
    pub(crate) fn all_tools() -> ToolRouter<Self> {
        let router = Self::catalog_tools() + Self::plan_tools() + Self::fill_tools();
        #[cfg(all(feature = "render", feature = "solver"))]
        let router = router + Self::solve_tools();
        router
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AthenaeumMcp {
    fn get_info(&self) -> ServerConfig {
        let identity = self.host.identity();
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build())
            .with_server_info(Implementation::new(identity.server_name, identity.version))
            .with_instructions(INSTRUCTIONS)
    }
}
