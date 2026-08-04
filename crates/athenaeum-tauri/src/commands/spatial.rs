// Spatial query commands - sky coordinates and location-based operations
//
// Thin wrappers only: state extraction + handler call + error mapping.
// Business logic lives in `athenaeum_core::api::spatial`.

use crate::models::*;
use tauri::State;

use athenaeum_core::api::spatial as api;

use super::AppState;

/// Get all imaging locations (both organized frame sets and unorganized clusters)
///
/// Returns a list of all locations where frames have been taken, including:
/// - Organized locations: Frames that are part of frame sets
/// - Unorganized locations: Frames not in any set, clustered by sky coordinates
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_imaging_locations(state: State<'_, AppState>) -> Result<Vec<ImagingLocation>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    api::get_imaging_locations(&db.conn()).map_err(|e| e.to_string())
}

/// Query candidate frames within a rectangular region of the sky
///
/// Returns frame coordinates so the frontend can do precise pixel-space verification.
/// This is important because RA/Dec bounding boxes break near the poles where a small
/// visual rectangle can span nearly 360° of RA.
#[tauri::command(rename_all = "snake_case")]
#[tracing::instrument(skip_all, err)]
pub async fn query_frames_in_bounds(
    state: State<'_, AppState>,
    bounds: SelectionBounds,
) -> Result<SelectionCandidates, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    api::query_frames_in_bounds(&db.conn(), bounds).map_err(|e| e.to_string())
}
