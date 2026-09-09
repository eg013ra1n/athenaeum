use crate::{routes::api_err, WebAppState};
use athenaeum_core::{api::equipment as api, equipment::models::*};
use axum::{extract::State, http::StatusCode, Json};

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_equipment_profiles(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<EquipmentProfile>>, (StatusCode, String)> {
    api::get_equipment_profiles(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEquipmentProfileArgs {
    pub profile: EquipmentProfile,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn save_equipment_profile(
    State(state): State<WebAppState>,
    Json(args): Json<SaveEquipmentProfileArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::save_equipment_profile(&state.ctx, args.profile)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteEquipmentProfileArgs {
    pub id: i64,
    pub revision: i64,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_equipment_profile(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteEquipmentProfileArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::delete_equipment_profile(&state.ctx, args.id, args.revision)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetEquipmentEvidenceArgs {
    pub camera: String,
    pub after_id: i64,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_equipment_evidence(
    State(state): State<WebAppState>,
    Json(args): Json<GetEquipmentEvidenceArgs>,
) -> Result<Json<Vec<EquipmentEvidence>>, (StatusCode, String)> {
    api::get_equipment_evidence(&state.ctx, args.camera, args.after_id)
        .map(Json)
        .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmEquipmentMatchArgs {
    pub frame_id: i64,
    pub profile_id: i64,
    pub revision: i64,
    pub solved_at: String,
    pub scale: f64,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn confirm_equipment_match(
    State(state): State<WebAppState>,
    Json(args): Json<ConfirmEquipmentMatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::confirm_equipment_match(
        &state.ctx,
        args.frame_id,
        args.profile_id,
        args.revision,
        args.solved_at,
        args.scale,
    )
    .map(Json)
    .map_err(api_err)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearEquipmentMatchArgs {
    pub frame_id: i64,
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_equipment_match(
    State(state): State<WebAppState>,
    Json(args): Json<ClearEquipmentMatchArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::clear_equipment_match(&state.ctx, args.frame_id)
        .map(Json)
        .map_err(api_err)
}
