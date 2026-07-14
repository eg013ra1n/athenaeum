//! Collaboration-project commands — thin wrappers over `athenaeum_core::api::collab`
//! and `athenaeum_core::api::collab_exchange`.

use std::sync::Arc;

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab::{
    GateReport, LinkSuggestion, ModerationItem, PortalNewProjectLink, ProjectCard, ProjectDetail,
    PublishResult,
};
use athenaeum_core::api::collab_exchange as exchange;
use athenaeum_core::api::collab_exchange::{ContributionView, PackageStateChange, ProjectPackageView};
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::export::models::ExportResult;
use tauri::{AppHandle, State};

use crate::tauri_events::TauriProgressEmitter;
use super::AppState; // AppState lives in commands/mod.rs and is NOT re-exported at the crate root

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
    api::refresh_projects(&state.ctx).await.map_err(|e| e.to_string())
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

/// Build + announce a stamped package of the project's gate-passing calibrated
/// lights, record it locally (with Д9 supersedes), and push-seed the first
/// receive-capable member. Rides the host-owned collab sender map.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn publish_collab_package(
    state: State<'_, AppState>,
    app: AppHandle,
    project_id: String,
) -> Result<PublishResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    api::publish_collab_frames(&state.ctx, &state.collab_sender, &project_id, Some(emitter))
        .await
        .map_err(|e| e.to_string())
}

/// Poll every cached project's announcements into `project_packages`, returning
/// the state changes the frontend turns into `notify()` calls.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn refresh_collab_packages(
    state: State<'_, AppState>,
) -> Result<Vec<PackageStateChange>, String> {
    exchange::refresh_all_project_packages(&state.ctx)
        .await
        .map_err(|e| e.to_string())
}

/// Every known package for a project (cache-only — no hub call).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_packages(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectPackageView>, String> {
    exchange::list_project_packages(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// Start the Д6 explicit sequential-holder download of a project package. Spawns
/// the pull and returns immediately — the terminal `local_status` + `sync-finished`
/// event carry the outcome.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn download_collab_package(
    state: State<'_, AppState>,
    project_id: String,
    package_id: String,
) -> Result<(), String> {
    let ctx = Arc::clone(&state.ctx);
    let sync = Arc::clone(&state.sync);
    tokio::spawn(async move {
        if let Err(e) =
            exchange::download_project_package(&ctx, &sync, &project_id, &package_id).await
        {
            tracing::error!(error = %format!("{e}"), "collab package download failed");
        }
    });
    Ok(())
}

/// Every received contribution for a project (cache-only — no hub call).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_contributions(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ContributionView>, String> {
    exchange::list_contributions(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// The coordinator's review queue: every PENDING package with its landed review
/// frames + parsed metrics (cache-only).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn list_collab_moderation(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ModerationItem>, String> {
    api::list_moderation_queue(&state.ctx, &project_id).map_err(|e| e.to_string())
}

/// Decide a pending announcement (coordinator only — enforced by the hub).
/// `approve` ⇒ hub approve + flip local state; reject ⇒ `reason` required, hub
/// reject, then remove the local review copy.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn decide_collab_announcement(
    state: State<'_, AppState>,
    announcement_id: String,
    approve: bool,
    reason: Option<String>,
) -> Result<(), String> {
    api::decide_announcement(&state.ctx, &announcement_id, approve, reason)
        .await
        .map_err(|e| e.to_string())
}

// ── Project-scoped WBPP export (slice 5, "processor payoff") ─────────────────

/// Organize the project's received contributions ∪ own calibrated outputs into a
/// WBPP folder tree — one subtree per publisher under the project title (Д2). The
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
    exchange::export_project_for_wbpp(&state.ctx, &project_id, &output_dir, use_symlinks, Some(emitter))
        .await
        .map_err(|e| e.to_string())
}
