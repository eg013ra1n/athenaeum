//! Collaboration-project commands — thin wrappers over `athenaeum_core::api::collab`
//! and `athenaeum_core::api::collab_exchange`.

use std::sync::Arc;

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab::{
    GateReport, LinkSuggestion, ModerationFrameView, PortalNewProjectLink, ProjectCard,
    ProjectDetail, PublishResult,
};
use athenaeum_core::api::collab_exchange as exchange;
use athenaeum_core::api::collab_exchange::ProjectFrameView;
use athenaeum_core::api::collab_live::surface;
use athenaeum_core::api::PathPolicy;
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::export::models::ExportResult;
use tauri::{AppHandle, State};

use super::AppState;
use crate::tauri_events::TauriProgressEmitter; // AppState lives in commands/mod.rs and is NOT re-exported at the crate root

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_projects(state: State<'_, AppState>) -> Result<Vec<ProjectCard>, String> {
    api::list_projects(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn refresh_collab_projects(
    state: State<'_, AppState>,
) -> Result<Vec<ProjectCard>, String> {
    api::refresh_projects(&state.ctx)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_project_detail(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDetail, String> {
    api::get_project_detail(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn evaluate_collab_gate(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<GateReport, String> {
    api::evaluate_project_gate(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_link_suggestions(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<LinkSuggestion>, String> {
    api::list_link_suggestions(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_link(
    state: State<'_, AppState>,
    project_id: String,
    frames_set_id: i64,
    linked: bool,
) -> Result<(), String> {
    if linked {
        api::link_frame_set(&state.ctx, &project_id, frames_set_id).map_err(|e| e.to_string())
    } else {
        api::unlink_frame_set(&state.ctx, &project_id, frames_set_id).map_err(|e| e.to_string())
    }
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn create_collab_link_intent(
    state: State<'_, AppState>,
    frames_set_id: i64,
) -> Result<PortalNewProjectLink, String> {
    api::record_project_link_intent(&state.ctx, frames_set_id).map_err(|e| e.to_string())
}

// ── Exchange (Task 11): publish, poll, list, download, moderate ──────────────

/// Publish the project's gate-passing calibrated lights per frame: calibrate
/// once into the Collaboration folder, seed by reference, announce.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn publish_collab_frames(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
) -> Result<PublishResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::publish_collab_frames(&state.ctx, &project_id, Some(emitter))
        .await
        .map_err(|e| e.to_string())
}

/// Re-publish every own frame of the project (P19): regenerate each one and
/// post a new content version only where the bytes changed.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn republish_collab_frames(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
) -> Result<PublishResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::republish_collab_frames(&state.ctx, &project_id, Some(emitter))
        .await
        .map_err(|e| e.to_string())
}

/// Every cached frame of a project (cache-only — no hub call), with its local
/// state and the live holder counts.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_frames(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectFrameView>, String> {
    surface::list_collab_frames(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// D3 §3.3: turn this project's auto-replication on or off (local preference —
/// the hub never learns of it). The live exchange re-reads the project's need
/// set at once.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_project_auto_replicate(
    state: State<'_, AppState>,
    project_id: String,
    enabled: bool,
) -> Result<(), String> {
    exchange::set_project_auto_replicate(&state.ctx, &project_id, enabled)
        .map_err(|e| e.to_string())
}

/// Collab v3 wave 2 Task 10 (R16, P13): turn this project's coalesced
/// auto-publish on or off (local preference — the hub never learns of it).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_project_auto_publish(
    state: State<'_, AppState>,
    project_id: String,
    enabled: bool,
) -> Result<(), String> {
    api::set_project_auto_publish(&state.ctx, &project_id, enabled)
        .await
        .map_err(|e| e.to_string())
}

/// Amendment A6: "Publish from this device" — make this device the one that
/// announces new frames of this account into the project. Returns the
/// refreshed card (`publishingDevice`, `publishingHere`).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_publishing_device(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectCard, String> {
    api::set_collab_publishing_device(&state.ctx, &project_id)
        .await
        .map_err(|e| e.to_string())
}

/// The project's local replication policy (collab v3, spec §5.3).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_policy(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<exchange::ReplicationPolicy, String> {
    exchange::get_collab_policy(&state.ctx, &project_id)
        .await
        .map_err(|e| e.to_string())
}

/// Store the project's replication policy; returns what it selects.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_policy(
    state: State<'_, AppState>,
    project_id: String,
    policy: exchange::ReplicationPolicy,
) -> Result<exchange::PolicyPreview, String> {
    exchange::set_collab_policy(&state.ctx, &project_id, policy)
        .await
        .map_err(|e| e.to_string())
}

/// What a replication policy would select, without storing it.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn preview_collab_policy(
    state: State<'_, AppState>,
    project_id: String,
    policy: exchange::ReplicationPolicy,
) -> Result<exchange::PolicyPreview, String> {
    exchange::preview_collab_policy(&state.ctx, &project_id, policy)
        .await
        .map_err(|e| e.to_string())
}

/// The coordinator's review queue: every PENDING frame (cache-only).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_moderation(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ModerationFrameView>, String> {
    api::list_moderation_queue(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// Approve a pending frame (coordinator only — enforced by the hub): hub
/// approve, then a manifest sync so the local cache picks up the new state.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn approve_collab_frame(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuid: String,
    trust: bool,
) -> Result<(), String> {
    api::approve_collab_frame(&state.ctx, &project_id, &frame_uuid, trust)
        .await
        .map_err(|e| e.to_string())
}

/// Reject a pending frame (coordinator only — enforced by the hub); `reason`
/// required, hub reject, then a manifest sync.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reject_collab_frame(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuid: String,
    reason: String,
) -> Result<(), String> {
    api::reject_collab_frame(&state.ctx, &project_id, &frame_uuid, reason)
        .await
        .map_err(|e| e.to_string())
}

// ── Project-scoped WBPP export (slice 5, "processor payoff") ─────────────────

/// Organize the project's frames (own and replica, from `project_frames_local`)
/// into a WBPP folder tree — one subtree per publisher under the project title (Д2). The
/// runner rides the standard export events with the Д3 sentinel `frame_set_id = -1`
/// and registers its cancel flag under that key, so the EXISTING `cancel_export`
/// command cancels a running project export (frontend: `api.invoke('cancel_export',
/// { frameSetId: -1 })`).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn export_collab_project(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
    output_dir: String,
    use_symlinks: bool,
) -> Result<ExportResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    exchange::export_project_for_wbpp(
        &state.ctx,
        &project_id,
        &output_dir,
        use_symlinks,
        Some(emitter),
    )
    .await
    .map_err(|e| e.to_string())
}

// ── Live exchange (collab v3 wave 3, Task 16) ────────────────────────────────

/// Sync now (L10): back-offs cleared, the event stream reopened, then a
/// digest check per project and a stat sweep.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn collab_sync_now(state: State<'_, AppState>) -> Result<(), String> {
    surface::collab_sync_now(&state.ctx).map_err(|e| e.to_string())
}

/// The live exchange's status (P27) — polled by the UI.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn get_collab_live_status(
    state: State<'_, AppState>,
) -> Result<athenaeum_core::api::collab_live::CollabLiveStatus, String> {
    Ok(surface::get_collab_live_status(&state.ctx))
}

/// A project's attention lists: changed files, the deletion choice, not
/// kept, other files (L4–L6).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_attention(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<surface::CollabAttention, String> {
    surface::list_collab_attention(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// Answer the deletion choice (L4) for all awaiting frames or the named ones.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resolve_collab_deletions(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuids: Option<Vec<String>>,
    action: surface::DeletionActionArg,
) -> Result<usize, String> {
    surface::resolve_collab_deletions(&state.ctx, &project_id, frame_uuids, action)
        .map_err(|e| e.to_string())
}

/// The last-copy warning a "Stop keeping" shows first (L4, I7).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn preview_collab_stop_keeping(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuids: Vec<String>,
) -> Result<Vec<surface::LastCopyView>, String> {
    surface::preview_collab_stop_keeping(&state.ctx, &project_id, frame_uuids)
        .map_err(|e| e.to_string())
}

/// "Keep again" (L6), for all not-kept frames or the named ones.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn keep_collab_frames_again(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuids: Option<Vec<String>>,
) -> Result<usize, String> {
    surface::keep_collab_frames_again(&state.ctx, &project_id, frame_uuids)
        .map_err(|e| e.to_string())
}

/// Answer a changed file (L5): re-fetch the original or delete it.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn resolve_collab_changed_file(
    state: State<'_, AppState>,
    project_id: String,
    frame_uuid: String,
    action: surface::ChangedActionArg,
    confirmed_delete: bool,
) -> Result<surface::ChangedFileOutcome, String> {
    surface::resolve_collab_changed_file(
        &state.ctx,
        &project_id,
        &frame_uuid,
        action,
        confirmed_delete,
    )
    .await
    .map_err(|e| e.to_string())
}

/// The Collaboration storage, with the replace offer or the take-over (§9.5).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_storage_status(
    state: State<'_, AppState>,
) -> Result<surface::CollabStorageStatus, String> {
    surface::get_collab_storage_status(&state.ctx)
        .await
        .map_err(|e| e.to_string())
}

/// "Check again": ask the hub who owns a folder whose marker names another
/// device (`root`, else the contested folder), record it, return the status.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn check_collab_folder_owner(
    state: State<'_, AppState>,
    root: Option<String>,
) -> Result<surface::CollabStorageStatus, String> {
    surface::check_collab_folder_owner(&state.ctx, root.as_deref(), &PathPolicy::AllowAll)
        .await
        .map_err(|e| e.to_string())
}

/// Replace another device of this account as the owner of the folder (the
/// offer's folder unless `root` names one).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn collab_replace_device(
    state: State<'_, AppState>,
    device_id: String,
    root: Option<String>,
) -> Result<surface::ReplaceOutcomeView, String> {
    surface::collab_replace_device(
        &state.ctx,
        &device_id,
        root.as_deref(),
        &PathPolicy::AllowAll,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Take over a folder whose marker names a device this account does not
/// list — only after its recorded refusal, and only once the user confirmed.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn take_over_collab_folder(
    state: State<'_, AppState>,
    root: String,
    confirmed: bool,
) -> Result<surface::ReplaceOutcomeView, String> {
    surface::take_over_collab_folder(&state.ctx, &root, confirmed, &PathPolicy::AllowAll)
        .await
        .map_err(|e| e.to_string())
}

/// `collab.max_upload_streams` (L11), applied live.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_max_upload_streams(
    state: State<'_, AppState>,
    max_upload_streams: usize,
) -> Result<(), String> {
    surface::set_collab_max_upload_streams(&state.ctx, max_upload_streams)
        .await
        .map_err(|e| e.to_string())
}

/// `collab.max_receive_streams` (L11), applied live.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_max_receive_streams(
    state: State<'_, AppState>,
    max_receive_streams: usize,
) -> Result<(), String> {
    surface::set_collab_max_receive_streams(&state.ctx, max_receive_streams)
        .map_err(|e| e.to_string())
}
