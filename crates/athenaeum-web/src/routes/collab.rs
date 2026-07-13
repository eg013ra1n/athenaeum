//! Web mirrors of the collab commands (one-for-one with commands/collab.rs).

use std::sync::Arc;

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab_exchange as exchange;
use athenaeum_core::events::ProgressEmitter;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::events::SseProgressEmitter;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadArgs {
    project_id: String,
    package_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideArgs {
    announcement_id: String,
    approve: bool,
    reason: Option<String>,
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

// ── Exchange (Task 11): publish, poll, list, download, moderate ──────────────

#[tracing::instrument(skip_all, err(Debug))]
pub async fn publish_collab_package(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::PublishResult>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    api::publish_collab_frames(&state.ctx, &state.collab_sender, &args.project_id, Some(emitter))
        .await
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn refresh_collab_packages(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<exchange::PackageStateChange>>, (axum::http::StatusCode, String)> {
    exchange::refresh_all_project_packages(&state.ctx)
        .await
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_packages(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<exchange::ProjectPackageView>>, (axum::http::StatusCode, String)> {
    exchange::list_project_packages(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

/// Spawns the Д6 download and returns immediately — the terminal `local_status` +
/// `sync-finished` SSE event carry the outcome.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn download_collab_package(
    State(state): State<WebAppState>,
    Json(args): Json<DownloadArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let ctx = Arc::clone(&state.ctx);
    let sync = Arc::clone(&state.sync);
    tokio::spawn(async move {
        if let Err(e) =
            exchange::download_project_package(&ctx, &sync, &args.project_id, &args.package_id).await
        {
            tracing::error!(error = %format!("{e}"), "collab package download failed");
        }
    });
    Ok(Json(()))
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_contributions(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<exchange::ContributionView>>, (axum::http::StatusCode, String)> {
    exchange::list_contributions(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn list_collab_moderation(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<Vec<api::ModerationItem>>, (axum::http::StatusCode, String)> {
    api::list_moderation_queue(&state.ctx, &args.project_id)
        .map(Json)
        .map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn decide_collab_announcement(
    State(state): State<WebAppState>,
    Json(args): Json<DecideArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    api::decide_announcement(&state.ctx, &args.announcement_id, args.approve, args.reason)
        .await
        .map(Json)
        .map_err(api_err)
}
