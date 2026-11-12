// Frame set route handlers - mirrors athenaeum-tauri frame_sets commands

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::routes::api_err;
use crate::WebAppState;

// ── Request body types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFramesSetsArgs {
    pub project_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFramesSetArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFramesSetArgs {
    pub frames_set_id: i64,
    pub new_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkFrameSetCustomArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeFrameSetsArgs {
    pub source_id: i64,
    pub target_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitFrameSetArgs {
    pub source_set_id: i64,
    pub selection: athenaeum_core::models::SplitSelection,
    pub new_name: String,
}

#[derive(Deserialize)]
pub struct CreateFrameSetFromSelectionArgs {
    pub name: String,
    pub frame_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoGenerateFrameSetsArgs {
    pub project_id: Option<i64>,
    pub threshold_deg: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAttestationArgs {
    pub frames_set_id: i64,
    pub attested: bool,
}

// ── Response types ────────────────────────────────────────────────────────────

// `AutoGenerateResult` lives in core beside the workflow that builds it — the
// wire shape (snake_case, no `rename_all`) is unchanged.
pub use athenaeum_core::api::frame_sets::AutoGenerateResult;

pub use athenaeum_core::api::frame_sets::FramesSetWithCount;

// ── Helpers ───────────────────────────────────────────────────────────────────

// The raw stderr prints formerly here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on every caller below, which already logs each returned Err at
// the command boundary — see the T7 sweep report.
fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}


// ── Handlers ──────────────────────────────────────────────────────────────────

/// Auto-generate frame sets by clustering light frames on sky coordinates.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn auto_generate_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<AutoGenerateFrameSetsArgs>,
) -> Result<Json<AutoGenerateResult>, (StatusCode, String)> {
    // Collaboration: emitter for the per-set project-match suggestions.
    let emitter = crate::events::SseProgressEmitter::new(state.event_tx.clone());

    // project_id is passed through and ignored by core — frame sets are global.
    let result = athenaeum_core::api::frame_sets::auto_generate_frame_sets(
        &state.ctx,
        args.project_id,
        args.threshold_deg,
        &emitter,
    )
    .map_err(db_err)?;

    Ok(Json(result))
}

/// Get all frame sets (project_id is accepted for API compatibility but ignored).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frames_sets(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesSetsArgs>,
) -> Result<Json<Vec<FramesSetWithCount>>, (StatusCode, String)> {
    let project_id = args.project_id.unwrap_or(1);
    athenaeum_core::api::frame_sets::get_frames_sets(&state.ctx, project_id)
        .map(Json)
        .map_err(api_err)
}

/// Get detailed structure for a single frame set (nights and sessions).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_detail(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::get_frame_set_detail(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(api_err)
}

/// Delete a frame set by ID (cascade removes nights/sessions/members).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::delete_frames_set(&state.ctx, args.frames_set_id).map_err(api_err)?;
    Ok(Json(()))
}

/// Delete all auto-generated frame sets (is_custom = false).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_auto_generated_frame_sets(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<usize>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::delete_auto_generated_frame_sets(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// Rename a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rename_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<RenameFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::rename_frames_set(&state.ctx, args.frames_set_id, &args.new_name).map_err(api_err)?;
    Ok(Json(()))
}

/// Mark a frame set as custom (one-way; also recalculates metadata).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn mark_frame_set_custom(
    State(state): State<WebAppState>,
    Json(args): Json<MarkFrameSetCustomArgs>,
) -> Result<Json<athenaeum_core::models::FramesSet>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::mark_frame_set_custom(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(api_err)
}

/// Merge source frame set into target frame set; source is deleted afterwards.
/// Merge `source_id` into `target_id` — nights re-derived from the union of
/// both memberships (`api::frame_sets::merge_frame_sets`), then the merged
/// set's detail. Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn merge_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<MergeFrameSetsArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::merge_frame_sets(&state.ctx, args.source_id, args.target_id)
        .map_err(db_err)?;
    athenaeum_core::api::frame_sets::get_frame_set_detail(&state.ctx, args.target_id)
        .map(Json)
        .map_err(api_err)
}

/// Recompute this set's center and rotation from its lights. Mirrors the
/// Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn recompute_frame_set_coordinates(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::api::frame_sets::FrameSetPointingUpdate>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::recompute_frame_set_coordinates(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(db_err)
}

/// Re-derive this set's nights and sessions from its member frames — the
/// repair for a set an older merge left with one night stored as two rows.
/// Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn recalculate_frame_set_nights(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::recalculate_frame_set_nights(&state.ctx, args.frames_set_id)
        .map_err(db_err)?;
    athenaeum_core::api::frame_sets::get_frame_set_detail(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(api_err)
}

/// Cheap, idempotent nights/sessions check meant to run whenever a frame
/// set's page is opened — only rewrites when the stored nights/sessions
/// actually disagree with a fresh derivation. Returns just the outcome, not
/// the full detail. Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reconcile_frame_set_nights(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::api::frame_sets::ReconcileSummary>, (StatusCode, String)> {
    let summary =
        athenaeum_core::api::frame_sets::reconcile_frame_set_nights(&state.ctx, args.frames_set_id)
            .map_err(db_err)?;
    Ok(Json(summary))
}

/// Spec 2026-09-28 §6.1 (F5): the user's word that the set's lights are
/// calibrated by an external tool. Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_frame_set_attestation(
    State(state): State<WebAppState>,
    Json(args): Json<SetAttestationArgs>,
) -> Result<StatusCode, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::set_frame_set_attestation(
        &state.ctx,
        args.frames_set_id,
        args.attested,
    )
    .map_err(api_err)?;
    Ok(StatusCode::OK)
}

/// Split the selected items out of a frame set into a new frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn split_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<SplitFrameSetArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    let new_set_id = athenaeum_core::api::frame_sets::split_frame_set(
        &state.ctx,
        args.source_set_id,
        args.selection,
        &args.new_name,
    )
    .map_err(api_err)?;

    // Return detail of the newly created set.
    athenaeum_core::api::frame_sets::get_frame_set_detail(&state.ctx, new_set_id)
        .map(Json)
        .map_err(api_err)
}

/// Create a custom frame set from a direct list of frame IDs.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_frame_set_from_selection(
    State(state): State<WebAppState>,
    Json(args): Json<CreateFrameSetFromSelectionArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let set_id = athenaeum_core::api::frame_sets::create_frame_set_from_selection(
        &state.ctx,
        &args.name,
        &args.frame_ids,
    )
    .map_err(api_err)?;

    Ok(Json(set_id))
}

// ── Excluded frames ───────────────────────────────────────────────────────────

/// Get all excluded frames with full file + frame metadata. Drives the
/// Excluded Frames page's Missing-Metadata-style repair toolbar.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_excluded_frames_with_metadata(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::ExcludedFrameRow>>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::get_excluded_frames_with_metadata(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromExcludedArgs {
    pub file_ids: Vec<i64>,
}

/// Remove the given file IDs from the `excluded_frames` table. Returns the
/// number of rows actually deleted.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn remove_files_from_excluded(
    State(state): State<WebAppState>,
    Json(args): Json<RemoveFromExcludedArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::remove_files_from_excluded(&state.ctx, &args.file_ids)
        .map(Json)
        .map_err(api_err)
}

/// Get count of excluded frames (lightweight check).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_excluded_frames_count(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<i64>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::get_excluded_frames_count(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// Archive a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn archive_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::archive_frame_set(&state.ctx, args.frames_set_id).map_err(api_err)?;
    Ok(Json(()))
}

// ────────────────────────────────────────────────────────────────────────────
// Phase 2: auto-merge web mirror
// ────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FindNewFramesArgs {
    pub frames_set_id: i64,
    pub scan_first: bool,
}

/// POST /api/find_new_frames_for_set — mirror of the Tauri command.
///
/// Note: in web mode the "scan disks first" option awaits any in-flight
/// scans but does not itself fire a fresh scan — callers can use the
/// existing /api/start_scan_with_progress route for that explicitly. This
/// keeps the request bounded (a fresh scan over many NAS roots would tie up
/// an Axum worker for minutes).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn find_new_frames_for_set(
    State(state): State<WebAppState>,
    Json(args): Json<FindNewFramesArgs>,
) -> Result<Json<athenaeum_core::models::FindNewFramesResult>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::find_new_frames_for_set(
        state.ctx.clone(),
        args.frames_set_id,
        args.scan_first,
        None,
    )
    .await
    .map(Json)
    .map_err(api_err)
}

#[derive(Deserialize)]
pub struct AutoMergeArgs {
    pub frames_set_id: i64,
    pub frame_ids: Vec<i64>,
    pub source: String,
}

/// POST /api/auto_merge_new_frames_for_set
#[tracing::instrument(skip_all, err(Debug))]
pub async fn auto_merge_new_frames_for_set(
    State(state): State<WebAppState>,
    Json(args): Json<AutoMergeArgs>,
) -> Result<Json<athenaeum_core::models::MergeReport>, (StatusCode, String)> {
    let emitter = crate::events::SseProgressEmitter::new(state.event_tx.clone());
    let report = athenaeum_core::api::frame_sets::auto_merge_new_frames_for_set(
        &state.ctx,
        args.frames_set_id,
        &args.frame_ids,
        &args.source,
        &emitter,
    )
    .map_err(api_err)?;

    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct GetMergeLogArgs {
    pub frames_set_id: i64,
    pub limit: Option<i64>,
}

/// POST /api/get_frame_set_merge_log
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_merge_log(
    State(state): State<WebAppState>,
    Json(args): Json<GetMergeLogArgs>,
) -> Result<Json<Vec<athenaeum_core::models::MergeLogEntry>>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::get_frame_set_merge_log(&state.ctx, args.frames_set_id, args.limit)
        .map(Json)
        .map_err(api_err)
}
