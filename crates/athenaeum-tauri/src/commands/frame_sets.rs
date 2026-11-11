// Frame set commands - frame set management and operations

use crate::db::{self};
use crate::models::*;
use std::sync::Arc;
use tauri::State;

use super::AppState;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn auto_generate_frame_sets(
    project_id: i64,
    threshold_deg: Option<f64>,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<AutoGenerateResult, String> {
    // Collaboration: emitter for the per-set project-match suggestions.
    let emitter = crate::tauri_events::TauriProgressEmitter(app_handle.clone());

    athenaeum_core::api::frame_sets::auto_generate_frame_sets(
        &state.ctx,
        Some(project_id),
        threshold_deg,
        &emitter,
    )
    .map_err(|e| e.to_string())
}

/// Get all frame sets for a project
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frames_sets(
    project_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<FramesSetWithCount>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    let sets = db::get_frames_sets_by_project(&conn, project_id).map_err(|e| e.to_string())?;
    let zip_counts = db::get_frames_set_zip_member_counts(&conn).map_err(|e| e.to_string())?;
    let duplicate_counts = db::get_frames_set_duplicate_counts(&conn).map_err(|e| e.to_string())?;

    Ok(sets
        .into_iter()
        .map(|(set, count)| FramesSetWithCount {
            zip_member_count: set.id.and_then(|id| zip_counts.get(&id).copied()).unwrap_or(0),
            duplicate_count: set.id.and_then(|id| duplicate_counts.get(&id).copied()).unwrap_or(0),
            frames_set: set,
            member_count: count,
        })
        .collect())
}

/// Delete a frames_set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_frames_set(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_frames_set(&conn, frames_set_id).map_err(|e| e.to_string())
}

/// Delete all auto-generated frames_sets (is_custom = false)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_auto_generated_frame_sets(state: State<'_, AppState>) -> Result<usize, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_auto_generated_frame_sets(&conn).map_err(|e| e.to_string())
}

/// Rename a frames_set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn rename_frames_set(
    frames_set_id: i64,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_frames_set_name(&conn, frames_set_id, &new_name).map_err(|e| e.to_string())
}

/// Mark a frames_set as custom (one-way conversion from auto-generated to custom)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn mark_frame_set_custom(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FramesSet, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get current metadata to preserve it
    let metadata =
        crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, &conn)
            .map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    // Update frame set to mark as custom
    db::update_frames_set_metadata(
        &conn,
        frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // Mark as custom
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    )
    .map_err(|e| format!("Failed to update frame set: {}", e))?;

    // Return the updated frame set
    let sets = db::get_frames_sets_by_project(&conn, 1).map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    Ok(frames_set)
}

/// Merge source frame set into target frame set
/// Dragged frame set (source) is merged into drop target (target)
/// Source frame set is deleted after merge
/// Merge `source_id` into `target_id` — nights re-derived from the union of
/// both memberships (`api::frame_sets::merge_frame_sets`), then the merged
/// set's detail.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn merge_frame_sets(
    source_id: i64,
    target_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    athenaeum_core::api::frame_sets::merge_frame_sets(&state.ctx, source_id, target_id)
        .map_err(|e| e.to_string())?;
    get_frame_set_detail(target_id, state).await
}

/// Recompute this set's center and rotation from its lights (Black Hole
/// lights left out, Athenaeum-solved lights preferred) — the refresh button
/// beside the coordinates on the object page.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn recompute_frame_set_coordinates(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::api::frame_sets::FrameSetPointingUpdate, String> {
    athenaeum_core::api::frame_sets::recompute_frame_set_coordinates(&state.ctx, frames_set_id)
        .map_err(|e| e.to_string())
}

/// Re-derive this set's nights and sessions from its member frames — the
/// repair for a set an older merge left with one night stored as two rows.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn recalculate_frame_set_nights(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    athenaeum_core::api::frame_sets::recalculate_frame_set_nights(&state.ctx, frames_set_id)
        .map_err(|e| e.to_string())?;
    get_frame_set_detail(frames_set_id, state).await
}

/// Cheap, idempotent nights/sessions check meant to run whenever a frame
/// set's page is opened — only rewrites the stored nights/sessions when
/// they actually disagree with a fresh derivation; returns just the
/// outcome, not the full detail (the caller reloads the page normally when
/// `changed` is true).
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn reconcile_frame_set_nights(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<athenaeum_core::api::frame_sets::ReconcileSummary, String> {
    athenaeum_core::api::frame_sets::reconcile_frame_set_nights(&state.ctx, frames_set_id)
        .map_err(|e| e.to_string())
}

/// Spec 2026-09-28 §6.1 (F5): the user's word that the set's lights are
/// calibrated by an external tool.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_frame_set_attestation(
    frames_set_id: i64,
    attested: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    athenaeum_core::api::frame_sets::set_frame_set_attestation(&state.ctx, frames_set_id, attested)
        .map_err(|e| e.to_string())
}

/// Split selected items from a frame set into a new frame set
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn split_frame_set(
    source_set_id: i64,
    selection: crate::models::SplitSelection,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    let new_set_id = athenaeum_core::api::frame_sets::split_frame_set(
        &state.ctx,
        source_set_id,
        selection,
        &new_name,
    )
    .map_err(|e| e.to_string())?;

    // Return the new frame set detail
    get_frame_set_detail(new_set_id, state).await
}

/// Get frame set detail with imaging nights and sessions
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_set_detail(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the frame set
    let sets = db::get_frames_sets_by_project(&conn, 1).map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    // Check if sessions exist
    let sessions_exist =
        db::sessions_exist_for_frame_set(&conn, frames_set_id).map_err(|e| e.to_string())?;

    if !sessions_exist {
        // Return empty nights instead of error
        return Ok(FrameSetDetail {
            frames_set,
            nights: Vec::new(),
        });
    }

    // Get the complete structure from database
    let nights =
        db::get_imaging_nights_with_sessions(&conn, frames_set_id).map_err(|e| e.to_string())?;

    Ok(FrameSetDetail { frames_set, nights })
}

#[tauri::command(rename_all = "snake_case")]
#[tracing::instrument(skip_all, err)]
pub async fn create_frame_set_from_selection(
    state: State<'_, AppState>,
    name: String,
    frame_ids: Vec<i64>,
    _description: Option<String>,
) -> Result<i64, String> {
    athenaeum_core::api::frame_sets::create_frame_set_from_selection(&state.ctx, &name, &frame_ids)
        .map_err(|e| e.to_string())
}

/// Get count of excluded frames (lightweight check)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_excluded_frames_count(state: State<'_, AppState>) -> Result<i64, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_excluded_frames_count(&conn).map_err(|e| e.to_string())
}

/// Get all excluded frames with full file + frame metadata. Drives the
/// Excluded Frames page's Missing-Metadata-style repair toolbar.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_excluded_frames_with_metadata(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::ExcludedFrameRow>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_excluded_frames_with_metadata(&conn).map_err(|e| e.to_string())
}

/// Remove the given file IDs from the `excluded_frames` table.
///
/// Used by the Excluded Frames page after a metadata edit: once the user
/// has fixed the offending fields the row should leave the excluded list,
/// even though the underlying frame may or may not satisfy the next
/// auto-generate pass — the user has explicitly told us the metadata is
/// now correct. Returns the number of rows actually deleted.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn remove_files_from_excluded(
    file_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_excluded_frames_by_file_ids(&conn, &file_ids)
        .map_err(|e| format!("Failed to remove from excluded: {}", e))
}

// DTOs for frame sets
// `AutoGenerateResult` now lives in core beside the workflow that builds it;
// re-exported so `commands::*` keeps exposing the same name.
pub use athenaeum_core::api::frame_sets::AutoGenerateResult;

#[derive(serde::Serialize)]
pub struct FramesSetWithCount {
    pub frames_set: FramesSet,
    pub member_count: usize,
    /// Members (counted in `member_count`) whose file lives inside a zip.
    pub zip_member_count: usize,
    /// Second copies of a frame the set already holds — not in `member_count`, never used.
    pub duplicate_count: usize,
}

/// Archive a frame set (move to Archive tab)
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn archive_frame_set(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::set_frame_set_archived(&conn, frames_set_id, true)
        .map_err(|e| format!("Failed to archive frame set: {}", e))
}

// ────────────────────────────────────────────────────────────────────────────
// Phase 2: auto-merge — find new images for a frame set and optionally merge
// ────────────────────────────────────────────────────────────────────────────

/// Find LIGHT frames in the catalog that would match this frame set by
/// coordinate proximity but aren't currently in any set.
///
/// When `scan_first` is true, the caller wants disks re-scanned before
/// matching. To avoid duplicate scan work:
///  - if any monitor-enabled root is already being scanned, wait for all
///    active scans to finish (polling `active_scans` every 500 ms, 5 min cap)
///  - otherwise, run one scan cycle across all monitor-enabled roots, then
///    do the match
/// The returned `scan_was_awaited` flag tells the UI whether it waited vs.
/// triggered a fresh scan, so the "Waiting for running scan…" banner can be
/// shown accurately.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn find_new_frames_for_set(
    frames_set_id: i64,
    scan_first: bool,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<FindNewFramesResult, String> {
    let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
        Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle));
    athenaeum_core::api::frame_sets::find_new_frames_for_set(
        state.ctx.clone(),
        frames_set_id,
        scan_first,
        Some(emitter),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Attach the given frames to the frame set (via session detection + append)
/// and record an audit-log entry. `source` must be "button" or "monitor".
///
/// Frames are re-resolved from `frame_ids` so the backend can't be tricked
/// by a stale client-supplied `CandidateFrame` payload.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn auto_merge_new_frames_for_set(
    frames_set_id: i64,
    frame_ids: Vec<i64>,
    source: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<MergeReport, String> {
    let emitter = crate::tauri_events::TauriProgressEmitter(app_handle);
    athenaeum_core::api::frame_sets::auto_merge_new_frames_for_set(
        &state.ctx,
        frames_set_id,
        &frame_ids,
        &source,
        &emitter,
    )
    .map_err(|e| e.to_string())
}

/// Read the auto-merge audit log for a frame set, newest first.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_frame_set_merge_log(
    frames_set_id: i64,
    limit: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<MergeLogEntry>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    crate::auto_merge::log_ops::get_log_entries(&conn, frames_set_id, limit)
        .map_err(|e| e.to_string())
}
