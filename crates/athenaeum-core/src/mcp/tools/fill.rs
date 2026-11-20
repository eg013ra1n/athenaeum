//! `plan_metadata_fill` (MCP plan Task 17): prepares a plan that fills the object, camera, date or frame type frames
//! miss, from evidence or from the user's word. The plan opens in Athenaeum's window and only the user applies it
//! (`mcp::plans::metadata` has the rules).

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::run;
use crate::api::ApiError;
use crate::mcp::plans::metadata::{self, PlanMetadataArgs};
use crate::mcp::{Answer, AthenaeumMcp, McpHost};

#[tool_router(router = fill_tools, vis = "pub(crate)")]
impl AthenaeumMcp {
    #[tool(
        description = "Prepare a plan that fills metadata frames miss: object (lights only), camera, date or frame_type. Choose frames with `scope`; only frames of the scope that miss a field are proposed for (find_missing_metadata shows which). Values come from evidence, and each item says which: the frame type from the nearest folder of its path named Lights, Darks, Flats, Bias or DarkFlats; the date from a date and time in the file name (taken as UTC: the camera clock's zone is unknown); the camera and the object from the other frames of the same folder when at least 3 have it set and all agree (for the object only lights count). A frame with no evidence is listed locked with the reason, never guessed. If the user tells you a value (\"these are M 31\"), pass it as `statement` {field, value}: it covers the frames in the scope that miss that field and the plan says \"by your instruction\"; never invent a value the user did not give. A date statement needs its time too (YYYY-MM-DD HH:MM:SS, UTC): the night a frame belongs to is decided by its time, and a date alone is refused. A frame that is a master keeps its master type (a master in a Darks folder is proposed MasterDark). With no `scope`, a statement or a field covers EVERY frame in the whole catalog that misses it, up to the plan's 2 000-item cap (a bigger request is refused): pass a scope (a folder, a night, a frame type) unless the user meant the whole catalog. Only the catalog changes (the files on disk are not rewritten) and the frames keep the value when Athenaeum scans the files again. Changing a frame's metadata removes it from its sessions and calibration links, which the plan's notes count. The plan opens in Athenaeum's window and nothing changes until the user presses Apply there; you cannot apply it. Ask plan_status for the outcome. A new plan replaces a plan the user has not decided yet. A request with nothing to fill is refused in words and leaves the waiting plan alone. The answer lists the plan in full: each value, how many frames, and every frame with its evidence; dates are grouped by folder, each frame with its own time.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn plan_metadata_fill(&self, Parameters(args): Parameters<PlanMetadataArgs>) -> CallToolResult {
        run(&self.host, move |host| plan_metadata_fill(host, args)).await
    }
}

#[tracing::instrument(skip_all, fields(tool = "plan_metadata_fill"), err)]
pub fn plan_metadata_fill(host: &Arc<McpHost>, args: PlanMetadataArgs) -> Result<Answer, ApiError> {
    // The plan is read, inferred and published before the answer: it is catalog reads and file names, a stat per file.
    metadata::prepare(host, args)
}
