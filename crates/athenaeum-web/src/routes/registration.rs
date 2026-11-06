//! Axum routes for the persisted stacking reference frame.
//!
//! Mirror of `crates/athenaeum-tauri/src/commands/registration.rs`. The
//! plate-solve-era registration route trio (run / list / cancel a
//! frame-to-frame alignment pass over a frame set) was retired 2026-09-09 —
//! the M1 stacking pipeline (`routes/stacking.rs`) replaces that flow.
//! `set_frame_set_reference` / `get_frame_set_reference` stay: the Analysis
//! tab's "Set as reference" star writes here, and the stacking run reads it.
//! `clear_frame_set_reference` lets the Stacking tab's reference star unset it.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::registration::{
    clear_frame_set_reference as core_clear_frame_set_reference,
    get_frame_set_reference as core_get_frame_set_reference,
    set_frame_set_reference as core_set_frame_set_reference, FrameSetReference,
};

use crate::WebAppState;

// ── request shapes ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFrameSetReferenceArgs {
    pub frames_set_id: i64,
    pub frame_id: i64,
}

// ── route handlers ────────────────────────────────────────────────────────────

/// POST /api/set_frame_set_reference
///
/// Persist the user-chosen reference frame for a frame set.
/// Validates that `frame_id` is a LIGHT member of the set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<SetFrameSetReferenceArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".into(),
    ))?;
    let conn = db.conn();
    core_set_frame_set_reference(&conn, args.frames_set_id, args.frame_id)
        .map(|()| Json(()))
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// POST /api/get_frame_set_reference
///
/// Return the persisted user-chosen reference frame for a frame set, if any.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<Option<FrameSetReference>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".into(),
    ))?;
    let conn = db.conn();
    core_get_frame_set_reference(&conn, args.frames_set_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/clear_frame_set_reference
///
/// Remove the persisted user-chosen reference frame for a frame set (if any).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "DB not initialized".into(),
    ))?;
    let conn = db.conn();
    core_clear_frame_set_reference(&conn, args.frames_set_id)
        .map(|()| Json(()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use crate::routes::tests::test_state;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// `POST /api/clear_frame_set_reference` is registered: with no DB in the
    /// test state it answers `500 DB not initialized` (the handler ran), not
    /// `404` (no route). The delete itself is pinned in core
    /// (`registration::db` `clear_frame_set_reference` test).
    #[tokio::test]
    async fn clear_frame_set_reference_route_is_registered() {
        let state = test_state(None);
        let router = crate::routes::build_router(state, None);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/clear_frame_set_reference")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"framesSetId": 1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"DB not initialized");
    }
}
