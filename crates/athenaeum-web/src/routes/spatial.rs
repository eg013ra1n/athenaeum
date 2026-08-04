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

/// Wire shape of `query_frames_in_bounds`: the frontend sends the core
/// `SelectionBounds` (snake_case, no `rename_all`) nested under a `bounds`
/// key — the same envelope the Tauri command (named `bounds` argument +
/// `rename_all = "snake_case"`) has always accepted. Mirrors the
/// `CreateFrameSetFromSelectionArgs` precedent in `frame_sets.rs`.
#[derive(Deserialize)]
pub struct QueryFramesInBoundsArgs {
    pub bounds: SelectionBounds,
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
    Json(args): Json<QueryFramesInBoundsArgs>,
) -> Result<Json<SelectionCandidates>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    api::query_frames_in_bounds(&db.conn(), args.bounds)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::QueryFramesInBoundsArgs;

    /// Byte-for-byte the payload `useRectangleSelection.ts` builds (nested
    /// `bounds`, snake_case — same envelope the Tauri command's named
    /// `bounds` argument + `rename_all = "snake_case"` accepts). Pinned so
    /// the web route can never again drift from the frontend's wire shape:
    /// it used to expect flat camelCase and 422'd every selection.
    #[test]
    fn deserializes_the_frontend_selection_payload() {
        let json = r#"{"bounds":{"ra_min":10.5,"ra_max":11.5,"dec_min":41.0,"dec_max":42.0,"crosses_meridian":false}}"#;
        let args: QueryFramesInBoundsArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.bounds.ra_min, 10.5);
        assert_eq!(args.bounds.ra_max, 11.5);
        assert_eq!(args.bounds.dec_min, 41.0);
        assert_eq!(args.bounds.dec_max, 42.0);
        assert_eq!(args.bounds.crosses_meridian, Some(false));
    }

    /// `crosses_meridian` is `#[serde(default)]` on the core type — an older
    /// client omitting it must still deserialize.
    #[test]
    fn crosses_meridian_is_optional() {
        let json = r#"{"bounds":{"ra_min":0.0,"ra_max":1.0,"dec_min":0.0,"dec_max":1.0}}"#;
        let args: QueryFramesInBoundsArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.bounds.crosses_meridian, None);
    }
}
