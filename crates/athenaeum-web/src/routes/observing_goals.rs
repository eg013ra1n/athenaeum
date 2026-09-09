use crate::{routes::api_err, WebAppState};
use athenaeum_core::{api::observing_goals as api, observing_goals::models::*};
use axum::{extract::State, http::StatusCode, Json};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetObservingProgressArgs {
    pub frame_set_id: i64,
}
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_observing_progress(
    State(state): State<WebAppState>,
    Json(args): Json<GetObservingProgressArgs>,
) -> Result<Json<Vec<ObservingProgress>>, (StatusCode, String)> {
    api::get_observing_progress(&state.ctx, args.frame_set_id)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveObservingGoalArgs {
    pub goal: ObservingGoal,
}
#[tracing::instrument(skip_all, err(Debug))]
pub async fn save_observing_goal(
    State(state): State<WebAppState>,
    Json(args): Json<SaveObservingGoalArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::save_observing_goal(&state.ctx, args.goal)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteObservingGoalArgs {
    pub frame_set_id: i64,
    pub filter: String,
    pub revision: i64,
}
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_observing_goal(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteObservingGoalArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::delete_observing_goal(&state.ctx, args.frame_set_id, args.filter, args.revision)
        .map(Json)
        .map_err(api_err)
}
