use super::AppState;
use athenaeum_core::{api::observing_goals as api, observing_goals::models::*};
use tauri::State;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_observing_progress(
    state: State<'_, AppState>,
    frame_set_id: i64,
) -> Result<Vec<ObservingProgress>, String> {
    api::get_observing_progress(&state.ctx, frame_set_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn save_observing_goal(
    state: State<'_, AppState>,
    goal: ObservingGoal,
) -> Result<(), String> {
    api::save_observing_goal(&state.ctx, goal).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_observing_goal(
    state: State<'_, AppState>,
    frame_set_id: i64,
    filter: String,
    revision: i64,
) -> Result<(), String> {
    api::delete_observing_goal(&state.ctx, frame_set_id, filter, revision)
        .map_err(|e| e.to_string())
}
