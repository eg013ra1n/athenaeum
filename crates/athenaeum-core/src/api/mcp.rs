//! Assistant access (MCP, plan 2026-11-19): what both hosts' `get_mcp_status`, `set_mcp_enabled` and
//! `regenerate_mcp_token` commands call, and the plan window's `get_mcp_plan` and `answer_mcp_plan`. The logic is `McpHost`'s (`mcp/lifecycle.rs`); these are the command-facing
//! names. `settings::set_setting` refuses `mcp.enabled`, so `set_mcp_enabled` is the only way to turn it on or off.

use std::sync::Arc;

use crate::api::ApiError;
use crate::mcp::{McpHost, McpPlanView, McpStatus, PlanAnswer};

/// What the Settings page shows: on or off, the state of the channel, the key (web), the bridge and the
/// configuration to copy (desktop).
pub fn get_mcp_status(host: &McpHost) -> McpStatus {
    host.status()
}

/// Turns assistant access on or off. On the web build, on is refused with an `Invalid` error that says why when
/// `ATHENAEUM_API_KEY` is not set.
pub fn set_mcp_enabled(host: &Arc<McpHost>, enabled: bool) -> Result<McpStatus, ApiError> {
    host.set_enabled(enabled)
}

/// Makes a new key and cuts off every connected assistant.
pub fn regenerate_mcp_token(host: &Arc<McpHost>) -> Result<McpStatus, ApiError> {
    host.regenerate_token()
}

/// The plan the window shows: the one by `plan_id`, or the newest when it is `None` (a page that opens later finds the
/// plan that waits). `None` when there is no such plan, because Athenaeum restarted since or it is older than a day.
pub fn get_mcp_plan(host: &McpHost, plan_id: Option<String>) -> Option<McpPlanView> {
    host.plans.view(plan_id.as_deref())
}

/// The window's answer: Apply the user's selection, or Cancel. The one way a plan is applied or cancelled; no MCP tool
/// calls it. An apply returns at once with the plan `applying`; the window learns the end from `mcp-plan-updated`.
pub fn answer_mcp_plan(host: &Arc<McpHost>, plan_id: String, answer: PlanAnswer) -> Result<McpPlanView, ApiError> {
    host.plans.answer(host, &plan_id, answer)
}
