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

/// Every cached frame of a project (cache-only — no hub call).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_frames(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectFrameView>, String> {
    exchange::list_project_frames(&state.ctx, &project_id).map_err(|e| e.to_string())
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
