//! `plan_metadata_fill` (MCP plan Task 17) prepares a plan that fills the object, camera, date or frame type frames
//! miss, from evidence or from the user's word. The plan opens in Athenaeum's window and only the user applies it
//! (`mcp::plans::metadata` has the rules). Task 18 adds the plate-solve trio below (`plan_plate_solve`,
//! `solve_status`, `cancel_solve`; `mcp::plans::plate_solve` has the rules).

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

// ---- the plate-solve plan, its run's status and cancel (the solver's features only) ----

#[cfg(all(feature = "render", feature = "solver"))]
use crate::mcp::plans::plate_solve::{self, Resolvers};
#[cfg(all(feature = "render", feature = "solver"))]
use crate::mcp::plans::{PlanPlateSolveArgs, SolveRunArgs};

#[cfg(all(feature = "render", feature = "solver"))]
#[tool_router(router = solve_tools, vis = "pub(crate)")]
impl AthenaeumMcp {
    #[tool(
        description = "Prepare a plan that plate-solves frames: the light frames of `scope` (a folder, a night, an object ...), by default only those that miss coordinates (`onlyUnsolved` true; false plans every light of the scope). Other frame types are left out and counted. A frame the solver must not be handed is listed locked with the reason: the star catalog is not installed, the input gate refuses it (its own analysis says its stars are trails) or its file is not reachable. The answer says how many frames can be solved of how many, the refusals by reason, whether the star catalog and quad index packs are installed and how many frames a run solves at a time. The plan opens in Athenaeum's window and nothing starts until the user presses Apply there; you cannot apply it. Applying starts ONE solve run (the plan's report names it, `solve-...`): ask plan_status whether it started, then solve_status for the run's progress and result; never tell the user frames were solved before solve_status says so. cancel_solve stops only that run. A solved frame gets its coordinates in the catalog. A new plan replaces a plan the user has not decided yet. While any plate-solve batch runs (yours or one the user started in Athenaeum) no plan is made and the answer says who holds the run; a request with nothing to solve is refused in words and leaves the waiting plan alone.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn plan_plate_solve(&self, Parameters(args): Parameters<PlanPlateSolveArgs>) -> CallToolResult {
        run(&self.host, move |host| plan_plate_solve(host, args)).await
    }

    #[tool(
        description = "How a plate-solve run stands, by the `runId` of the plan's report (`solve-...`): while it runs, how many frames are done of the total, how many failed so far, the file being solved and each failure by file name and folder (never a solved count: a frame counts as solved only when the run has saved its result); after it, the result: solved, failed with every failed file and its error, and frames a cancel left unattempted (a cancel from Athenaeum's window is named as such). It changes nothing. Only runs this assistant started (since Athenaeum opened) are remembered; another id is answered in words.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn solve_status(&self, Parameters(args): Parameters<SolveRunArgs>) -> CallToolResult {
        run(&self.host, move |host| solve_status(host, args)).await
    }

    #[tool(
        description = "Stop the plate-solve run this assistant started, by its `runId`. It acts only while that exact run is the one running: a run the user started in Athenaeum's window, a run that already ended or an id nobody knows is answered in words and nothing is stopped. The frames being solved at that moment finish first; ask solve_status for the result. This stops a run, it does not decide a plan.",
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn cancel_solve(&self, Parameters(args): Parameters<SolveRunArgs>) -> CallToolResult {
        run(&self.host, move |host| cancel_solve(host, args)).await
    }
}

#[cfg(all(feature = "render", feature = "solver"))]
#[tracing::instrument(skip_all, fields(tool = "plan_plate_solve"), err)]
pub fn plan_plate_solve(host: &Arc<McpHost>, args: PlanPlateSolveArgs) -> Result<Answer, ApiError> {
    // The plan is read, gated and published before the answer: catalog reads, the input gate and a stat per file.
    plate_solve::prepare(host, args, Resolvers::real())
}

#[cfg(all(feature = "render", feature = "solver"))]
#[tracing::instrument(skip_all, fields(tool = "solve_status"), err)]
pub fn solve_status(host: &Arc<McpHost>, args: SolveRunArgs) -> Result<Answer, ApiError> {
    plate_solve::solve_status(host, &args.run_id)
}

#[cfg(all(feature = "render", feature = "solver"))]
#[tracing::instrument(skip_all, fields(tool = "cancel_solve"), err)]
pub fn cancel_solve(host: &Arc<McpHost>, args: SolveRunArgs) -> Result<Answer, ApiError> {
    plate_solve::cancel_solve(host, &args.run_id)
}
