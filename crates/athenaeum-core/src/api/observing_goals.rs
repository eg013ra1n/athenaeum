use super::{db, ApiError};
use crate::{
    observing_goals::{models::*, progress, storage},
    services::ServiceContext,
};

pub fn get_observing_progress(
    ctx: &ServiceContext,
    frame_set_id: i64,
) -> Result<Vec<ObservingProgress>, ApiError> {
    Ok(progress::progress(&db(ctx)?.conn(), frame_set_id)?)
}
pub fn save_observing_goal(ctx: &ServiceContext, goal: ObservingGoal) -> Result<(), ApiError> {
    Ok(storage::save(&db(ctx)?.conn(), goal)?)
}
pub fn delete_observing_goal(
    ctx: &ServiceContext,
    frame_set_id: i64,
    filter: String,
    revision: i64,
) -> Result<(), ApiError> {
    Ok(storage::remove(
        &db(ctx)?.conn(),
        frame_set_id,
        &filter,
        revision,
    )?)
}
