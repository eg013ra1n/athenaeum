//! Tauri commands for the persisted stacking reference frame.
//!
//! The plate-solve-era registration command trio (run / list / cancel a
//! frame-to-frame alignment pass over a frame set) was retired 2026-09-09 —
//! the M1 stacking pipeline (`commands/stacking.rs`) replaces that flow.
//! `set_frame_set_reference` / `get_frame_set_reference` stay: the Analysis
//! tab's "Set as reference" star writes here, and the stacking run reads it.

use tauri::State;

use athenaeum_core::registration::{
    get_frame_set_reference as core_get_frame_set_reference,
    set_frame_set_reference as core_set_frame_set_reference, FrameSetReference,
};

use super::AppState;

// ── commands ──────────────────────────────────────────────────────────────────

/// Persist the user-chosen reference frame for a frame set.
///
/// Validates that `frame_id` is a LIGHT member of the set before writing.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_frame_set_reference(
    state: State<'_, AppState>,
    frames_set_id: i64,
    frame_id: i64,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    core_set_frame_set_reference(&conn, frames_set_id, frame_id).map_err(|e| e.to_string())
}

/// Return the persisted user-chosen reference frame for a frame set, if any.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_set_reference(
    state: State<'_, AppState>,
    frames_set_id: i64,
) -> Result<Option<FrameSetReference>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    core_get_frame_set_reference(&conn, frames_set_id).map_err(|e| e.to_string())
}
