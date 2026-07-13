//! Collaboration-project commands — thin wrappers over `athenaeum_core::api::collab`.

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab::{
    GateReport, LinkSuggestion, PortalNewProjectLink, ProjectCard, ProjectDetail,
};
use tauri::State;

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
