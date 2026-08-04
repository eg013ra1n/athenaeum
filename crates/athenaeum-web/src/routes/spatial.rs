// Spatial query route handlers - sky coordinates and location-based operations.
//
// Thin wrappers only: extraction + handler call + error mapping. Business
// logic lives in `athenaeum_core::api::spatial`, shared with the Tauri
// commands so both backends return identical data shapes.

use athenaeum_core::api::spatial as api;
use athenaeum_core::models::{ImagingLocation, SelectionBounds, SelectionCandidates};
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::WebAppState;

/// camelCase wrapper for the core `SelectionBounds` type.
/// Frontend sends `raMin`, `raMax`, etc. but the core struct uses snake_case.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBoundsArgs {
    pub ra_min: f64,
    pub ra_max: f64,
    pub dec_min: f64,
    pub dec_max: f64,
    #[serde(default)]
    pub crosses_meridian: Option<bool>,
    #[serde(default)]
    pub selected_object_ids: Option<Vec<i64>>,
}

impl From<SelectionBoundsArgs> for SelectionBounds {
    fn from(a: SelectionBoundsArgs) -> Self {
        SelectionBounds {
            ra_min: a.ra_min,
            ra_max: a.ra_max,
            dec_min: a.dec_min,
            dec_max: a.dec_max,
            crosses_meridian: a.crosses_meridian,
        }
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /api/get_imaging_locations`
///
/// Returns all imaging locations: organised frame sets and unorganised
/// coordinate clusters, so the sky map can display targets before the user
/// runs auto-generate.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_imaging_locations(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<ImagingLocation>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    api::get_imaging_locations(&db.conn())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /api/query_frames_in_bounds`
///
/// Returns candidate LIGHT frames whose RA/Dec falls inside the supplied
/// bounding box.  Handles the RA wrap-around at 0°/360°.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn query_frames_in_bounds(
    State(state): State<WebAppState>,
    Json(args): Json<SelectionBoundsArgs>,
) -> Result<Json<SelectionCandidates>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    api::query_frames_in_bounds(&db.conn(), args.into())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
