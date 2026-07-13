//! Web mirrors of the collab commands (one-for-one with commands/collab.rs).

use athenaeum_core::api::collab as api;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::routes::api_err;
use crate::WebAppState; // the web crate's state type — there is no `AppState` in athenaeum-web

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdArgs {
    project_id: String,
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
    api::refresh_projects(&state.ctx).await.map(Json).map_err(api_err)
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
