//! Web mirrors of the collab commands (one-for-one with commands/collab.rs).

use std::sync::Arc;

use athenaeum_core::api::collab as api;
use athenaeum_core::api::collab_exchange as exchange;
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::export::models::ExportResult;
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
pub struct AutoReplicateArgs {
    project_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideArgs {
    announcement_id: String,
    approve: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProjectArgs {
    project_id: String,
    output_dir: String,
    use_symlinks: bool,
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

/// Spawns the D3 swarm download (falling back to the Д6 sequential pull in the
/// same call) and returns immediately — the terminal `local_status` +
/// `sync-finished` SSE event carry the outcome, and the swarm path's live source
/// count rides `project-download-progress`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn download_collab_package(
    State(state): State<WebAppState>,
    Json(args): Json<DownloadArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let ctx = Arc::clone(&state.ctx);
    let sync = Arc::clone(&state.sync);
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    tokio::spawn(async move {
        if let Err(e) = exchange::download_project_package(
            &ctx,
            &sync,
            &args.project_id,
            &args.package_id,
            Some(emitter),
        )
        .await
        {
            tracing::error!(error = %format!("{e}"), "collab package download failed");
        }
    });
    Ok(Json(()))
}

/// D3 §3.3: turn this project's auto-replication on or off (local preference —
/// the hub never learns of it). The worker reads the column at the start of each
/// pass, so there is nothing to live-apply.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_project_auto_replicate(
    State(state): State<WebAppState>,
    Json(args): Json<AutoReplicateArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    exchange::set_project_auto_replicate(&state.ctx, &args.project_id, args.enabled)
        .map(Json)
        .map_err(api_err)
}

/// D3 §3.3 "Sync now": run one auto-replication pass for this project
/// immediately, with the toggle forced on (an explicit user act). Returns as soon
/// as the pass is spawned — progress rides the usual `local_status` +
/// `project-download-progress` / `sync-finished` SSE events.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn sync_project_now(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<()>, (axum::http::StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    exchange::sync_project_now(
        Arc::clone(&state.ctx),
        Arc::clone(&state.sync),
        &args.project_id,
        Some(emitter),
    )
    .map(Json)
    .map_err(api_err)
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
                error: Some(format!("Export path must be within {}", export_dir.display())),
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
