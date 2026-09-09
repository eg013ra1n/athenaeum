use super::AppState;
use athenaeum_core::{api::equipment as api, equipment::models::*};
use tauri::State;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_equipment_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<EquipmentProfile>, String> {
    api::get_equipment_profiles(&state.ctx).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn save_equipment_profile(
    state: State<'_, AppState>,
    profile: EquipmentProfile,
) -> Result<(), String> {
    api::save_equipment_profile(&state.ctx, profile).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_equipment_profile(
    state: State<'_, AppState>,
    id: i64,
    revision: i64,
) -> Result<(), String> {
    api::delete_equipment_profile(&state.ctx, id, revision).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_equipment_evidence(
    state: State<'_, AppState>,
    camera: String,
    after_id: i64,
) -> Result<Vec<EquipmentEvidence>, String> {
    api::get_equipment_evidence(&state.ctx, camera, after_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn confirm_equipment_match(
    state: State<'_, AppState>,
    frame_id: i64,
    profile_id: i64,
    revision: i64,
    solved_at: String,
    scale: f64,
) -> Result<(), String> {
    api::confirm_equipment_match(&state.ctx, frame_id, profile_id, revision, solved_at, scale)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn clear_equipment_match(
    state: State<'_, AppState>,
    frame_id: i64,
) -> Result<(), String> {
    api::clear_equipment_match(&state.ctx, frame_id).map_err(|e| e.to_string())
}
