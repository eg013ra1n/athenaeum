//! Web mirrors of the collab commands (one-for-one with commands/collab.rs).

use std::sync::Arc;

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab_exchange as exchange;
use athenaeum_core::api::collab_live::surface;
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::export::models::ExportResult;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::routes::scan_roots::allowed_roots_policy;
use crate::WebAppState; // the web crate's state type — there is no `AppState` in athenaeum-web

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdArgs {
    project_id: String,
}

/// `projectId` absent or `null`: every project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalProjectArgs {
    #[serde(default)]
    project_id: Option<String>,
}

/// I1: `withContributorState` (default `false`) opts into the own-row
/// contributor chip — see `list_collab_frames`'s doc comment.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListCollabFramesArgs {
    project_id: String,
    #[serde(default)]
    with_contributor_state: bool,
}

/// Wave 2 (plan 2026-09-30 Task 1): `frameIds` absent or `null` publishes
/// (or republishes) every gate-passing candidate, as before; present, it
/// restricts the run to those gate rows.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishArgs {
    pub project_id: String,
    #[serde(default)]
    pub frame_ids: Option<Vec<i64>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLinkArgs {
    project_id: String,
    frames_set_id: i64,
    linked: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentArgs {
    frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameArgs {
    project_id: String,
    frame_uuid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoReplicateArgs {
    project_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishModeArgs {
    project_id: String,
    mode: athenaeum_core::db::collab::PublishMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyArgs {
    project_id: String,
    policy: exchange::ReplicationPolicy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveFrameArgs {
    project_id: String,
    frame_uuid: String,
    trust: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectFrameArgs {
    project_id: String,
    frame_uuid: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludeFrameArgs {
    project_id: String,
    frame_uuid: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProjectArgs {
    project_id: String,
    output_dir: String,
    use_symlinks: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionsArgs {
    project_id: String,
    #[serde(default)]
    frame_uuids: Option<Vec<String>>,
    action: surface::DeletionActionArg,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopKeepingArgs {
    project_id: String,
    frame_uuids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepAgainArgs {
    project_id: String,
    #[serde(default)]
    frame_uuids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFileArgs {
    project_id: String,
    frame_uuid: String,
    action: surface::ChangedActionArg,
    confirmed_delete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceDeviceArgs {
    device_id: String,
    #[serde(default)]
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderOwnerArgs {
    #[serde(default)]
    root: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverArgs {
    root: String,
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStreamsArgs {
    max_upload_streams: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveStreamsArgs {
    max_receive_streams: usize,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_projects(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<api::ProjectCard>>, (axum::http::StatusCode, String)> {
    api::list_projects(&state.ctx).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn refresh_collab_projects(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<api::ProjectCard>>, (axum::http::StatusCode, String)> {
    api::refresh_projects(&state.ctx)
        .await
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_project_detail(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::ProjectDetail>, (axum::http::StatusCode, String)> {
    api::get_project_detail(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn evaluate_collab_gate(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::GateReport>, (axum::http::StatusCode, String)> {
    api::evaluate_project_gate(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFilterMappingsArgs {
    project_id: String,
    mappings: Vec<api::FilterMappingEdit>,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_filter_mapping_sheet(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::FilterMappingSheet>, (axum::http::StatusCode, String)> {
    api::get_filter_mapping_sheet(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_filter_mappings(
    State(state): State<WebAppState>,
    Json(args): Json<SetFilterMappingsArgs>,
) -> Result<Json<api::GateReport>, (axum::http::StatusCode, String)> {
    api::set_filter_mappings(&state.ctx, &args.project_id, args.mappings)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_link_suggestions(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<api::LinkSuggestion>>, (axum::http::StatusCode, String)> {
    api::list_link_suggestions(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_link(
    State(state): State<WebAppState>,
    Json(args): Json<SetLinkArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let r = if args.linked {
        api::link_frame_set(&state.ctx, &args.project_id, args.frames_set_id)
    } else {
        api::unlink_frame_set(&state.ctx, &args.project_id, args.frames_set_id)
    };
    r.map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_collab_link_intent(
    State(state): State<WebAppState>,
    Json(args): Json<IntentArgs>,
) -> Result<Json<api::PortalNewProjectLink>, (axum::http::StatusCode, String)> {
    api::record_project_link_intent(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(api_err)
}

/// Spec §8.2 — what the frame set's page shows about projects: its links
/// (per-frame contributor state + counts) or, unlinked, nearby candidates.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_project_status(
    State(state): State<WebAppState>,
    Json(args): Json<IntentArgs>,
) -> Result<Json<api::FrameSetProjectStatus>, (axum::http::StatusCode, String)> {
    api::get_frame_set_project_status(&state.ctx, args.frames_set_id)
        .map(Json)
        .map_err(api_err)
}

// ── Exchange (Task 11): publish, poll, list, download, moderate ──────────────

#[tracing::instrument(skip_all, err(Debug))]
pub async fn publish_collab_frames(
    State(state): State<WebAppState>,
    Json(args): Json<PublishArgs>,
) -> Result<Json<api::PublishResult>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::publish_collab_frames(
        &state.ctx,
        &args.project_id,
        args.frame_ids.as_deref(),
        Some(emitter),
    )
    .await
    .map(Json)
    .map_err(api_err)
}

/// Calibrate the project's Ready frames into prepared files for review
/// (spec 2026-10-01 §4.3): no seeding, no announce, no hub call.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn calibrate_collab_frames(
    State(state): State<WebAppState>,
    Json(args): Json<PublishArgs>,
) -> Result<Json<api::PublishResult>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::calibrate_collab_frames(
        &state.ctx,
        &args.project_id,
        args.frame_ids.as_deref(),
        Some(emitter),
    )
    .await
    .map(Json)
    .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn republish_collab_frames(
    State(state): State<WebAppState>,
    Json(args): Json<PublishArgs>,
) -> Result<Json<api::PublishResult>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::republish_collab_frames(
        &state.ctx,
        &args.project_id,
        args.frame_ids.as_deref(),
        Some(emitter),
    )
    .await
    .map(Json)
    .map_err(api_err)
}

/// Every cached frame of a project (cache-only — no hub call), with its local
/// state and the live holder counts.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_frames(
    State(state): State<WebAppState>,
    Json(args): Json<ListCollabFramesArgs>,
) -> Result<Json<Vec<exchange::ProjectFrameView>>, (axum::http::StatusCode, String)> {
    surface::list_collab_frames(&state.ctx, &args.project_id, args.with_contributor_state)
        .map(Json)
        .map_err(api_err)
}

/// One row per LIGHT frame of the project's linked sets (spec 2026-09-29
/// §5.1) — the project page's "My frames" tab.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_project_own_frames(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<api::OwnFrameRow>>, (axum::http::StatusCode, String)> {
    api::list_project_own_frames(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// Who holds one frame (spec 2026-09-29 §5.3) — the project page's frame
/// drawer.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_frame_holders(
    State(state): State<WebAppState>,
    Json(args): Json<FrameArgs>,
) -> Result<Json<Vec<surface::FrameHolderView>>, (axum::http::StatusCode, String)> {
    surface::get_collab_frame_holders(&state.ctx, &args.project_id, &args.frame_uuid)
        .map(Json)
        .map_err(api_err)
}

/// The project page's Members tab (spec 2026-09-29 §5.4): role, devices,
/// published contribution, per-camera quality, holdings share, last seen.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_member_summary(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<surface::MemberSummary>>, (axum::http::StatusCode, String)> {
    surface::get_collab_member_summary(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// The live exchange's flows (spec 2026-09-29 §6.4): what Transfers and the
/// project's Exchange tab load on mount, peers named. Empty when no live
/// exchange runs.
#[tracing::instrument(level = "debug", skip_all, err(Debug))]
pub async fn get_collab_exchange(
    State(state): State<WebAppState>,
    Json(args): Json<OptionalProjectArgs>,
) -> Result<Json<surface::ExchangeSnapshot>, (axum::http::StatusCode, String)> {
    surface::get_collab_exchange(&state.ctx, args.project_id.as_deref())
        .map(Json)
        .map_err(api_err)
}

/// Collab observability wave 1 (Task 13): the project's (or every project's)
/// receive sessions, newest first — the Transfers/project page's collapsed
/// view of a burst of landings, real sources named.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsArgs {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_receive_sessions(
    State(state): State<WebAppState>,
    Json(args): Json<SessionsArgs>,
) -> Result<Json<Vec<surface::ReceiveSessionView>>, (axum::http::StatusCode, String)> {
    surface::list_collab_receive_sessions(&state.ctx, args.project_id.as_deref(), args.limit)
        .map(Json)
        .map_err(api_err)
}

/// D3 §3.3: turn this project's auto-replication on or off (local preference —
/// the hub never learns of it). The live exchange re-reads the project's need
/// set at once.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_project_auto_replicate(
    State(state): State<WebAppState>,
    Json(args): Json<AutoReplicateArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    exchange::set_project_auto_replicate(&state.ctx, &args.project_id, args.enabled)
        .map(Json)
        .map_err(api_err)
}

/// Spec 2026-10-01 P3: set this project's publishing mode (local preference —
/// the hub never learns of it).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_project_publish_mode(
    State(state): State<WebAppState>,
    Json(args): Json<PublishModeArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::set_project_publish_mode(&state.ctx, &args.project_id, args.mode)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Amendment A6: "Publish from this device" — make this device the one that
/// announces new frames of this account into the project. Returns the
/// refreshed card (`publishingDevice`, `publishingHere`).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_publishing_device(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::ProjectCard>, (axum::http::StatusCode, String)> {
    api::set_collab_publishing_device(&state.ctx, &args.project_id)
        .await
        .map(Json)
        .map_err(api_err)
}

/// The project's local replication policy (collab v3, spec §5.3).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_policy(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<exchange::ReplicationPolicy>, (axum::http::StatusCode, String)> {
    exchange::get_collab_policy(&state.ctx, &args.project_id)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Store the project's replication policy; returns what it selects.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_policy(
    State(state): State<WebAppState>,
    Json(args): Json<PolicyArgs>,
) -> Result<Json<exchange::PolicyPreview>, (axum::http::StatusCode, String)> {
    exchange::set_collab_policy(&state.ctx, &args.project_id, args.policy)
        .await
        .map(Json)
        .map_err(api_err)
}

/// What a replication policy would select, without storing it.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn preview_collab_policy(
    State(state): State<WebAppState>,
    Json(args): Json<PolicyArgs>,
) -> Result<Json<exchange::PolicyPreview>, (axum::http::StatusCode, String)> {
    exchange::preview_collab_policy(&state.ctx, &args.project_id, args.policy)
        .await
        .map(Json)
        .map_err(api_err)
}

/// The coordinator's review queue: every PENDING frame (cache-only).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_moderation(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<api::ModerationFrameView>>, (axum::http::StatusCode, String)> {
    api::list_moderation_queue(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// Approve a pending frame (coordinator only — enforced by the hub): hub
/// approve, then a manifest sync so the local cache picks up the new state.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn approve_collab_frame(
    State(state): State<WebAppState>,
    Json(args): Json<ApproveFrameArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::approve_collab_frame(&state.ctx, &args.project_id, &args.frame_uuid, args.trust)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Reject a pending frame (coordinator only — enforced by the hub); `reason`
/// required, hub reject, then a manifest sync.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn reject_collab_frame(
    State(state): State<WebAppState>,
    Json(args): Json<RejectFrameArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::reject_collab_frame(&state.ctx, &args.project_id, &args.frame_uuid, args.reason)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Exclude a published frame (coordinator only — enforced by the hub);
/// `reason` required (1..=500 characters), hub `PATCH`, then a manifest sync.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn exclude_collab_frame(
    State(state): State<WebAppState>,
    Json(args): Json<ExcludeFrameArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::exclude_collab_frame(&state.ctx, &args.project_id, &args.frame_uuid, args.reason)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Restore a previously excluded frame (coordinator only — enforced by the
/// hub); hub `PATCH` with `accepted: true`, then a manifest sync.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn restore_collab_frame(
    State(state): State<WebAppState>,
    Json(args): Json<FrameArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::restore_collab_frame(&state.ctx, &args.project_id, &args.frame_uuid)
        .await
        .map(Json)
        .map_err(api_err)
}

// ── Project-scoped WBPP export (slice 5, "processor payoff") ─────────────────

/// Web mirror of `export_collab_project`. Validates `output_dir` is within the
/// server-configured export directory BEFORE running — mirroring `routes/export.rs`
/// exactly: a violation returns HTTP 200 with a `success:false` ExportResult body
/// (never a 4xx), and the check is skipped entirely when `export_dir` is `None`.
/// The runner rides the `-1` sentinel export events; `cancel_export` with
/// `frameSetId=-1` cancels a running export.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn export_collab_project(
    State(state): State<WebAppState>,
    Json(args): Json<ExportProjectArgs>,
) -> Result<Json<ExportResult>, (axum::http::StatusCode, String)> {
    // Validate output path is within the configured export directory.
    if let Some(ref export_dir) = state.export_dir {
        if !std::path::Path::new(&args.output_dir).starts_with(export_dir) {
            return Ok(Json(ExportResult {
                success: false,
                output_dir: args.output_dir.clone(),
                files_organized: 0,
                scripts_generated: Vec::new(),
                warnings: Vec::new(),
                error: Some(format!(
                    "Export path must be within {}",
                    export_dir.display()
                )),
            }));
        }
    }

    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    exchange::export_project_for_wbpp(
        &state.ctx,
        &args.project_id,
        &args.output_dir,
        args.use_symlinks,
        Some(emitter),
    )
    .await
    .map(Json)
    .map_err(api_err)
}

// ── Live exchange (collab v3 wave 3, Task 16) ────────────────────────────────

/// Sync now (L10): back-offs cleared, the event stream reopened, then a
/// digest check per project and a stat sweep.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn collab_sync_now(
    State(state): State<WebAppState>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    surface::collab_sync_now(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// The live exchange's status (P27) — polled by the UI.
#[tracing::instrument(skip_all, level = "debug")]
pub async fn get_collab_live_status(
    State(state): State<WebAppState>,
) -> Json<athenaeum_core::api::collab_live::CollabLiveStatus> {
    Json(surface::get_collab_live_status(&state.ctx))
}

/// A project's attention lists: changed files, the deletion choice, not
/// kept, other files (L4–L6).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_attention(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<surface::CollabAttention>, (axum::http::StatusCode, String)> {
    surface::list_collab_attention(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// Answer the deletion choice (L4) for all awaiting frames or the named ones.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn resolve_collab_deletions(
    State(state): State<WebAppState>,
    Json(args): Json<DeletionsArgs>,
) -> Result<Json<usize>, (axum::http::StatusCode, String)> {
    surface::resolve_collab_deletions(&state.ctx, &args.project_id, args.frame_uuids, args.action)
        .map(Json)
        .map_err(api_err)
}

/// The last-copy warning a "Stop keeping" shows first (L4, I7).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn preview_collab_stop_keeping(
    State(state): State<WebAppState>,
    Json(args): Json<StopKeepingArgs>,
) -> Result<Json<Vec<surface::LastCopyView>>, (axum::http::StatusCode, String)> {
    surface::preview_collab_stop_keeping(&state.ctx, &args.project_id, args.frame_uuids)
        .map(Json)
        .map_err(api_err)
}

/// "Keep again" (L6), for all not-kept frames or the named ones.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn keep_collab_frames_again(
    State(state): State<WebAppState>,
    Json(args): Json<KeepAgainArgs>,
) -> Result<Json<usize>, (axum::http::StatusCode, String)> {
    surface::keep_collab_frames_again(&state.ctx, &args.project_id, args.frame_uuids)
        .map(Json)
        .map_err(api_err)
}

/// Answer a changed file (L5): re-fetch the original or delete it.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn resolve_collab_changed_file(
    State(state): State<WebAppState>,
    Json(args): Json<ChangedFileArgs>,
) -> Result<Json<surface::ChangedFileOutcome>, (axum::http::StatusCode, String)> {
    surface::resolve_collab_changed_file(
        &state.ctx,
        &args.project_id,
        &args.frame_uuid,
        args.action,
        args.confirmed_delete,
    )
    .await
    .map(Json)
    .map_err(api_err)
}

/// The Collaboration storage, with the replace offer or the take-over (§9.5).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_storage_status(
    State(state): State<WebAppState>,
) -> Result<Json<surface::CollabStorageStatus>, (axum::http::StatusCode, String)> {
    surface::get_collab_storage_status(&state.ctx)
        .await
        .map(Json)
        .map_err(api_err)
}

/// "Check again": ask the hub who owns a folder whose marker names another
/// device (`root`, else the contested folder), record it, return the status.
/// A `root` with `..` is refused, then held to the allowed roots.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_collab_folder_owner(
    State(state): State<WebAppState>,
    Json(args): Json<FolderOwnerArgs>,
) -> Result<Json<surface::CollabStorageStatus>, (axum::http::StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    surface::check_collab_folder_owner(&state.ctx, args.root.as_deref(), &policy)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Replace another device of this account as the owner of the folder (the
/// offer's folder unless `root` names one — refused when it contains `..`,
/// then held to the allowed roots).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn collab_replace_device(
    State(state): State<WebAppState>,
    Json(args): Json<ReplaceDeviceArgs>,
) -> Result<Json<surface::ReplaceOutcomeView>, (axum::http::StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    surface::collab_replace_device(&state.ctx, &args.device_id, args.root.as_deref(), &policy)
        .await
        .map(Json)
        .map_err(api_err)
}

/// Take over a folder whose marker names a device this account does not
/// list — only after its recorded refusal, and only once the user confirmed.
/// A `root` with `..` is refused before anything else.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn take_over_collab_folder(
    State(state): State<WebAppState>,
    Json(args): Json<TakeOverArgs>,
) -> Result<Json<surface::ReplaceOutcomeView>, (axum::http::StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    surface::take_over_collab_folder(&state.ctx, &args.root, args.confirmed, &policy)
        .await
        .map(Json)
        .map_err(api_err)
}

/// `collab.max_upload_streams` (L11), applied live.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_max_upload_streams(
    State(state): State<WebAppState>,
    Json(args): Json<UploadStreamsArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    surface::set_collab_max_upload_streams(&state.ctx, args.max_upload_streams)
        .await
        .map(Json)
        .map_err(api_err)
}

/// `collab.max_receive_streams` (L11), applied live.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_max_receive_streams(
    State(state): State<WebAppState>,
    Json(args): Json<ReceiveStreamsArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    surface::set_collab_max_receive_streams(&state.ctx, args.max_receive_streams)
        .map(Json)
        .map_err(api_err)
}

#[cfg(test)]
mod live_surface_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    use crate::events::SseEvent;

    fn test_state(db: Database, allowed_paths: Vec<PathBuf>) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths,
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    /// Task 16 (R3): a root with `..` is refused with 400 before the policy,
    /// the filesystem or the hub are touched — on every folder route.
    #[tokio::test]
    async fn a_root_with_a_parent_step_is_refused_on_every_folder_route() {
        let tmp = TempDir::new().unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let state = test_state(
            Database::new(tmp.path().join("catalog.db")).unwrap(),
            vec![allowed.clone()],
        );
        let sneaky = format!("{}/../../etc", allowed.display());

        let err = take_over_collab_folder(
            State(state.clone()),
            Json(TakeOverArgs {
                root: sneaky.clone(),
                confirmed: true,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "{}", err.1);
        assert!(err.1.contains(".."), "{}", err.1);

        let err = check_collab_folder_owner(
            State(state.clone()),
            Json(FolderOwnerArgs {
                root: Some(sneaky.clone()),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "{}", err.1);
        assert!(err.1.contains(".."), "{}", err.1);

        let err = collab_replace_device(
            State(state),
            Json(ReplaceDeviceArgs {
                device_id: "old-id".into(),
                root: Some(sneaky),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "{}", err.1);
        assert!(err.1.contains(".."), "{}", err.1);
    }
}

#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_collab_publish_run(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<
    Json<athenaeum_core::api::collab_publish_run::CollabPublishRunView>,
    (axum::http::StatusCode, String),
> {
    athenaeum_core::api::collab_publish_run::get_collab_publish_run(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_collab_publish(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    athenaeum_core::api::collab_publish_run::cancel_collab_publish(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// Whole-branch review finding 1: `SessionsArgs` lacked `camelCase`, so the
/// frontend's `{"projectId":...}` body deserialized with `project_id: None`
/// via `#[serde(default)]` — the web build silently returned every
/// project's sessions instead of the requested one. These pin the camelCase
/// boundary for every args struct this task touches.
#[cfg(test)]
mod args_serde_tests {
    use super::*;

    #[test]
    fn sessions_args_reads_camel_case_fields() {
        let args: SessionsArgs = serde_json::from_str(r#"{"projectId":"p","limit":5}"#)
            .expect("SessionsArgs should deserialize camelCase body");
        assert_eq!(args.project_id.as_deref(), Some("p"));
        assert_eq!(args.limit, Some(5));
    }

    #[test]
    fn frame_args_reads_camel_case_fields() {
        let args: FrameArgs = serde_json::from_str(r#"{"projectId":"p","frameUuid":"u"}"#)
            .expect("FrameArgs should deserialize camelCase body");
        assert_eq!(args.project_id, "p");
        assert_eq!(args.frame_uuid, "u");
    }

    /// Wave 2 Task 3: `ExcludeFrameArgs` must carry `camelCase` too — the
    /// missing-attribute bug this whole test module pins against.
    #[test]
    fn exclude_args_reads_camel_case_fields() {
        let args: ExcludeFrameArgs =
            serde_json::from_str(r#"{"projectId":"p","frameUuid":"u","reason":"r"}"#)
                .expect("ExcludeFrameArgs should deserialize camelCase body");
        assert_eq!(args.project_id, "p");
        assert_eq!(args.frame_uuid, "u");
        assert_eq!(args.reason, "r");
    }

    #[test]
    fn optional_project_args_reads_camel_case_field() {
        let args: OptionalProjectArgs = serde_json::from_str(r#"{"projectId":"p"}"#)
            .expect("OptionalProjectArgs should deserialize camelCase body");
        assert_eq!(args.project_id.as_deref(), Some("p"));
    }

    /// Wave 2 (plan 2026-09-30 Task 1): `frameIds` reads camelCase and
    /// defaults to `None` when absent, so a plain publish/republish body
    /// keeps working.
    #[test]
    fn publish_args_read_camel_case_frame_ids() {
        let a: PublishArgs = serde_json::from_str(r#"{"projectId":"p","frameIds":[3,5]}"#).unwrap();
        assert_eq!(
            (a.project_id.as_str(), a.frame_ids),
            ("p", Some(vec![3, 5]))
        );
        let b: PublishArgs = serde_json::from_str(r#"{"projectId":"p"}"#).unwrap();
        assert_eq!(b.frame_ids, None);
    }
}
