//! Shared catalog-only API for optical profiles and explicit review decisions.
use super::{db, ApiError};
use crate::{
    equipment::{
        models::{EquipmentEvidence, EquipmentProfile},
        storage,
    },
    services::ServiceContext,
};

pub fn get_equipment_profiles(ctx: &ServiceContext) -> Result<Vec<EquipmentProfile>, ApiError> {
    Ok(storage::profiles(&db(ctx)?.conn())?)
}
pub fn save_equipment_profile(
    ctx: &ServiceContext,
    profile: EquipmentProfile,
) -> Result<(), ApiError> {
    Ok(storage::save(&db(ctx)?.conn(), profile)?)
}
pub fn delete_equipment_profile(
    ctx: &ServiceContext,
    id: i64,
    revision: i64,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    if conn.execute(
        "
        DELETE FROM equipment_profiles WHERE id=?1 AND revision=?2
        ",
        rusqlite::params![id, revision],
    )? != 1
    {
        return Err(ApiError::Conflict(
            "Configuration changed; reload before deleting".into(),
        ));
    }
    Ok(())
}
pub fn get_equipment_evidence(
    ctx: &ServiceContext,
    camera: String,
    after_id: i64,
) -> Result<Vec<EquipmentEvidence>, ApiError> {
    Ok(storage::evidence(&db(ctx)?.conn(), &camera, after_id)?)
}
pub fn confirm_equipment_match(
    ctx: &ServiceContext,
    frame_id: i64,
    profile_id: i64,
    revision: i64,
    solved_at: String,
    scale: f64,
) -> Result<(), ApiError> {
    if frame_id <= 0 || !scale.is_finite() || scale <= 0.0 {
        return Err(ApiError::Invalid("Invalid frame or scale".into()));
    }
    Ok(storage::confirm(
        &db(ctx)?.conn(),
        frame_id,
        profile_id,
        revision,
        &solved_at,
        scale,
    )?)
}
pub fn clear_equipment_match(ctx: &ServiceContext, frame_id: i64) -> Result<(), ApiError> {
    db(ctx)?.conn().execute(
        "
        DELETE FROM equipment_frame_matches WHERE frame_id=?1
        ",
        [frame_id],
    )?;
    Ok(())
}
