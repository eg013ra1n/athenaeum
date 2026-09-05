// Frame set route handlers - mirrors athenaeum-tauri frame_sets commands

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::WebAppState;

// ── Request body types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFramesSetsArgs {
    pub project_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFramesSetArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFramesSetArgs {
    pub frames_set_id: i64,
    pub new_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkFrameSetCustomArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeFrameSetsArgs {
    pub source_id: i64,
    pub target_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitFrameSetArgs {
    pub source_set_id: i64,
    pub selection: athenaeum_core::models::SplitSelection,
    pub new_name: String,
}

#[derive(Deserialize)]
pub struct CreateFrameSetFromSelectionArgs {
    pub name: String,
    pub frame_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoGenerateFrameSetsArgs {
    pub project_id: Option<i64>,
    pub threshold_deg: Option<f64>,
}

// ── Response types ────────────────────────────────────────────────────────────

// `AutoGenerateResult` lives in core beside the workflow that builds it — the
// wire shape (snake_case, no `rename_all`) is unchanged.
pub use athenaeum_core::api::frame_sets::AutoGenerateResult;

#[derive(serde::Serialize)]
pub struct FramesSetWithCount {
    pub frames_set: athenaeum_core::models::FramesSet,
    pub member_count: usize,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

// The raw stderr prints formerly here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on every caller below, which already logs each returned Err at
// the command boundary — see the T7 sweep report.
fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string())
}

/// Fetch the FrameSetDetail for the given `frames_set_id` using an open
/// connection.  Extracted so that handlers which need to return a detail after
/// mutating the DB can reuse it without re-locking.
fn load_frame_set_detail(
    conn: &rusqlite::Connection,
    frames_set_id: i64,
) -> Result<athenaeum_core::models::FrameSetDetail, String> {
    use athenaeum_core::db;

    let sets = db::get_frames_sets_by_project(conn, 1).map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or_else(|| "Frame set not found".to_string())?
        .0;

    let sessions_exist =
        db::sessions_exist_for_frame_set(conn, frames_set_id).map_err(|e| e.to_string())?;

    if !sessions_exist {
        return Ok(athenaeum_core::models::FrameSetDetail {
            frames_set,
            nights: Vec::new(),
        });
    }

    let nights =
        db::get_imaging_nights_with_sessions(conn, frames_set_id).map_err(|e| e.to_string())?;

    Ok(athenaeum_core::models::FrameSetDetail {
        frames_set,
        nights,
    })
}

/// Shared inner logic for creating a custom frame set from a list of frame IDs.
/// Returns the new frame set ID.
fn create_frame_set_inner(
    conn: &rusqlite::Connection,
    name: &str,
    frame_ids: &[i64],
    settings: &std::sync::Arc<athenaeum_core::settings::SettingsManager>,
) -> Result<i64, String> {
    use athenaeum_core::db;

    if frame_ids.is_empty() {
        return Err("Cannot create frame set with no frames".to_string());
    }

    let metadata = athenaeum_core::frames_set_metadata::calculate_metadata_from_frame_ids(
        frame_ids,
        conn,
    )
    .map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    let set_id = db::create_frames_set(
        conn,
        Some(name),
        true, // is_custom
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    )
    .map_err(|e| format!("Failed to create frames_set: {}", e))?;

    let frames = db::get_frames_with_files_by_ids(conn, frame_ids)
        .map_err(|e| format!("Failed to get frames: {}", e))?;

    let gap_threshold_hours: f64 = settings
        .get_session_gap_threshold_hours(conn)
        .unwrap_or(6.0);

    let detected_nights = athenaeum_core::sessions::detect_sessions(frames, gap_threshold_hours)
        .map_err(|e| format!("Failed to detect sessions: {}", e))?;

    if detected_nights.is_empty() {
        let now = chrono::Utc::now();
        let night_start =
            now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let night_end = (now + chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let night_id = db::create_imaging_night(conn, set_id, &night_start, &night_end)
            .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

        let session_id =
            db::create_session(conn, night_id, "Unknown", frame_ids.len() as i32, metadata.total_exp_time)
                .map_err(|e| format!("Failed to create session: {}", e))?;

        db::insert_session_members(conn, session_id, frame_ids)
            .map_err(|e| format!("Failed to add frames to session: {}", e))?;
    } else {
        for night in &detected_nights {
            let night_id = db::create_imaging_night(conn, set_id, &night.start_time, &night.end_time)
                .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

            for session in &night.sessions {
                let session_id = db::create_session(
                    conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                )
                .map_err(|e| format!("Failed to create session: {}", e))?;

                db::insert_session_members(conn, session_id, &session.frame_ids)
                    .map_err(|e| format!("Failed to add frames to session: {}", e))?;
            }
        }
    }

    Ok(set_id)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Auto-generate frame sets by clustering light frames on sky coordinates.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn auto_generate_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<AutoGenerateFrameSetsArgs>,
) -> Result<Json<AutoGenerateResult>, (StatusCode, String)> {
    // Collaboration: emitter for the per-set project-match suggestions.
    let emitter = crate::events::SseProgressEmitter::new(state.event_tx.clone());

    // project_id is passed through and ignored by core — frame sets are global.
    let result = athenaeum_core::api::frame_sets::auto_generate_frame_sets(
        &state.ctx,
        args.project_id,
        args.threshold_deg,
        &emitter,
    )
    .map_err(db_err)?;

    Ok(Json(result))
}

/// Get all frame sets (project_id is accepted for API compatibility but ignored).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frames_sets(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesSetsArgs>,
) -> Result<Json<Vec<FramesSetWithCount>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let project_id = args.project_id.unwrap_or(1);
    let sets = athenaeum_core::db::get_frames_sets_by_project(&conn, project_id)
        .map_err(db_err)?;

    Ok(Json(
        sets.into_iter()
            .map(|(frames_set, member_count)| FramesSetWithCount {
                frames_set,
                member_count,
            })
            .collect(),
    ))
}

/// Get detailed structure for a single frame set (nights and sessions).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_detail(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let detail = load_frame_set_detail(&conn, args.frames_set_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Delete a frame set by ID (cascade removes nights/sessions/members).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::delete_frames_set(&conn, args.frames_set_id).map_err(db_err)?;
    Ok(Json(()))
}

/// Delete all auto-generated frame sets (is_custom = false).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_auto_generated_frame_sets(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let n = athenaeum_core::db::delete_auto_generated_frame_sets(&conn).map_err(db_err)?;
    Ok(Json(n))
}

/// Rename a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rename_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<RenameFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::update_frames_set_name(&conn, args.frames_set_id, &args.new_name).map_err(db_err)?;
    Ok(Json(()))
}

/// Mark a frame set as custom (one-way; also recalculates metadata).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn mark_frame_set_custom(
    State(state): State<WebAppState>,
    Json(args): Json<MarkFrameSetCustomArgs>,
) -> Result<Json<athenaeum_core::models::FramesSet>, (StatusCode, String)> {
    use athenaeum_core::db;

    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let metadata =
        athenaeum_core::frames_set_metadata::calculate_metadata_for_frame_set(args.frames_set_id, &conn)
            .map_err(|e| db_err(format!("Failed to calculate metadata: {}", e)))?;

    db::update_frames_set_metadata(
        &conn,
        args.frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // mark as custom
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    )
    .map_err(db_err)?;

    let sets = db::get_frames_sets_by_project(&conn, 1).map_err(db_err)?;
    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(args.frames_set_id))
        .ok_or_else(|| db_err("Frame set not found"))?
        .0;

    Ok(Json(frames_set))
}

/// Merge source frame set into target frame set; source is deleted afterwards.
/// Merge `source_id` into `target_id` — nights re-derived from the union of
/// both memberships (`api::frame_sets::merge_frame_sets`), then the merged
/// set's detail. Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn merge_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<MergeFrameSetsArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::merge_frame_sets(&state.ctx, args.source_id, args.target_id)
        .map_err(db_err)?;
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let detail = load_frame_set_detail(&conn, args.target_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Re-derive this set's nights and sessions from its member frames — the
/// repair for a set an older merge left with one night stored as two rows.
/// Mirrors the Tauri command.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn recalculate_frame_set_nights(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    athenaeum_core::api::frame_sets::recalculate_frame_set_nights(&state.ctx, args.frames_set_id)
        .map_err(db_err)?;
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let detail = load_frame_set_detail(&conn, args.frames_set_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Split the selected items out of a frame set into a new frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn split_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<SplitFrameSetArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    use athenaeum_core::db;
    use athenaeum_core::models::SplitSelection;

    let new_set_id = {
        let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
        let conn = db_ref.conn();

        // Validate: split must not leave the source empty.
        let all_nights =
            db::get_imaging_nights_for_set(&conn, args.source_set_id).map_err(db_err)?;

        let can_split = match &args.selection {
            SplitSelection::Nights { ids } => ids.len() < all_nights.len(),
            SplitSelection::Sessions { ids } => {
                let mut total = 0;
                for night in &all_nights {
                    if let Some(night_id) = night.id {
                        let sessions =
                            db::get_sessions_for_night(&conn, night_id).map_err(db_err)?;
                        total += sessions.len();
                    }
                }
                ids.len() < total
            }
            SplitSelection::Frames { ids } => {
                let total: i64 = conn
                    .query_row(
                        "SELECT COUNT(DISTINCT sm.frame_id)
                         FROM session_members sm
                         JOIN sessions s ON sm.session_id = s.id
                         JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                         WHERE in_tbl.frames_set_id = ?1",
                        [args.source_set_id],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                ids.len() < total as usize
            }
        };

        if !can_split {
            return Err((
                StatusCode::BAD_REQUEST,
                "Cannot split: operation would leave the source frame set empty".to_string(),
            ));
        }

        // Collect frame IDs from the selection.
        let frame_ids: Vec<i64> = match &args.selection {
            SplitSelection::Nights { ids } => {
                let mut frames = Vec::new();
                for night_id in ids {
                    let mut stmt = conn
                        .prepare(
                            "SELECT DISTINCT sm.frame_id
                             FROM session_members sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.imaging_night_id = ?1",
                        )
                        .map_err(db_err)?;
                    let night_frames: Vec<i64> = stmt
                        .query_map([night_id], |row| row.get(0))
                        .map_err(db_err)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(db_err)?;
                    frames.extend(night_frames);
                }
                frames
            }
            SplitSelection::Sessions { ids } => {
                let mut frames = Vec::new();
                for &session_id in ids {
                    let session_frames = db::get_frame_ids_for_session(&conn, session_id)
                        .map_err(db_err)?;
                    frames.extend(session_frames);
                }
                frames
            }
            SplitSelection::Frames { ids } => ids.clone(),
        };

        if frame_ids.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "No frames to split".to_string()));
        }

        let gap_threshold_hours: f64 = state
            .ctx
            .settings
            .get_session_gap_threshold_hours(&conn)
            .unwrap_or(6.0);

        let metadata = athenaeum_core::frames_set_metadata::calculate_metadata_from_frame_ids(
            &frame_ids,
            &conn,
        )
        .map_err(db_err)?;

        let new_set_id = db::create_frames_set(
            &conn,
            Some(&args.new_name),
            true, // always custom after split
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            metadata.avg_rotation,
            metadata.min_rotation,
            metadata.max_rotation,
        )
        .map_err(db_err)?;

        let frames_data = db::get_frames_with_files_by_ids(&conn, &frame_ids).map_err(db_err)?;
        let detected_nights =
            athenaeum_core::sessions::detect_sessions(frames_data, gap_threshold_hours)
                .map_err(db_err)?;

        for night in &detected_nights {
            let night_id = db::create_imaging_night(&conn, new_set_id, &night.start_time, &night.end_time)
                .map_err(db_err)?;

            for session in &night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                )
                .map_err(db_err)?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(db_err)?;
            }
        }

        // Remove the split frames from the source.
        match &args.selection {
            SplitSelection::Nights { ids } => {
                for night_id in ids {
                    conn.execute("DELETE FROM imaging_nights WHERE id = ?1", [night_id])
                        .map_err(db_err)?;
                }
            }
            SplitSelection::Sessions { ids } => {
                for session_id in ids {
                    conn.execute("DELETE FROM sessions WHERE id = ?1", [session_id])
                        .map_err(db_err)?;
                }
            }
            SplitSelection::Frames { ids } => {
                for frame_id in ids {
                    conn.execute(
                        "DELETE FROM session_members
                         WHERE frame_id = ?1
                         AND session_id IN (
                             SELECT s.id FROM sessions s
                             JOIN imaging_nights n ON s.imaging_night_id = n.id
                             WHERE n.frames_set_id = ?2
                         )",
                        rusqlite::params![frame_id, args.source_set_id],
                    )
                    .map_err(db_err)?;
                }
            }
        }

        // Recalculate and persist metadata for the now-smaller source.
        let source_metadata = athenaeum_core::frames_set_metadata::calculate_metadata_for_frame_set(
            args.source_set_id,
            &conn,
        )
        .map_err(db_err)?;

        db::update_frames_set_metadata(
            &conn,
            args.source_set_id,
            source_metadata.date_obs_start.as_deref(),
            source_metadata.date_obs_end.as_deref(),
            source_metadata.objctra.as_deref(),
            source_metadata.objctdec.as_deref(),
            source_metadata.total_exp_time,
            true,
            source_metadata.avg_rotation,
            source_metadata.min_rotation,
            source_metadata.max_rotation,
        )
        .map_err(db_err)?;

        new_set_id
    };

    // Return detail of the newly created set.
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let detail = load_frame_set_detail(&conn, new_set_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Create a custom frame set from a direct list of frame IDs.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn create_frame_set_from_selection(
    State(state): State<WebAppState>,
    Json(args): Json<CreateFrameSetFromSelectionArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let set_id = create_frame_set_inner(&conn, &args.name, &args.frame_ids, &state.ctx.settings)
        .map_err(db_err)?;

    Ok(Json(set_id))
}

// ── Excluded frames ───────────────────────────────────────────────────────────

/// Get all excluded frames with full file + frame metadata. Drives the
/// Excluded Frames page's Missing-Metadata-style repair toolbar.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_excluded_frames_with_metadata(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::ExcludedFrameRow>>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let rows = athenaeum_core::db::get_excluded_frames_with_metadata(&conn).map_err(db_err)?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromExcludedArgs {
    pub file_ids: Vec<i64>,
}

/// Remove the given file IDs from the `excluded_frames` table. Returns the
/// number of rows actually deleted.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn remove_files_from_excluded(
    State(state): State<WebAppState>,
    Json(args): Json<RemoveFromExcludedArgs>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let deleted = athenaeum_core::db::delete_excluded_frames_by_file_ids(&conn, &args.file_ids)
        .map_err(db_err)?;
    Ok(Json(deleted))
}

/// Get count of excluded frames (lightweight check).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_excluded_frames_count(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let count = athenaeum_core::db::get_excluded_frames_count(&conn).map_err(db_err)?;
    Ok(Json(count))
}

/// Archive a frame set.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn archive_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    athenaeum_core::db::set_frame_set_archived(&conn, args.frames_set_id, true).map_err(db_err)?;
    Ok(Json(()))
}

// ────────────────────────────────────────────────────────────────────────────
// Phase 2: auto-merge web mirror
// ────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FindNewFramesArgs {
    pub frames_set_id: i64,
    pub scan_first: bool,
}

/// POST /api/find_new_frames_for_set — mirror of the Tauri command.
///
/// Note: in web mode the "scan disks first" option awaits any in-flight
/// scans but does not itself fire a fresh scan — callers can use the
/// existing /api/start_scan_with_progress route for that explicitly. This
/// keeps the request bounded (a fresh scan over many NAS roots would tie up
/// an Axum worker for minutes).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn find_new_frames_for_set(
    State(state): State<WebAppState>,
    Json(args): Json<FindNewFramesArgs>,
) -> Result<Json<athenaeum_core::models::FindNewFramesResult>, (StatusCode, String)> {
    let mut scan_was_awaited = false;

    if args.scan_first {
        let active_initially = {
            let scans = state.ctx.active_scans.lock().unwrap();
            !scans.is_empty()
        };

        if active_initially {
            scan_was_awaited = true;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5 * 60);
            loop {
                let still_active = {
                    let scans = state.ctx.active_scans.lock().unwrap();
                    !scans.is_empty()
                };
                if !still_active {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err((
                        StatusCode::GATEWAY_TIMEOUT,
                        "Timed out waiting for active scan to finish".to_string(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }

    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let threshold_deg = state
        .ctx
        .settings
        .get_grouping_threshold_deg(&conn)
        .map_err(db_err)?;
    let candidates = athenaeum_core::auto_merge::find_candidates_for_set(
        &conn,
        args.frames_set_id,
        threshold_deg,
    )
    .map_err(db_err)?;

    Ok(Json(athenaeum_core::models::FindNewFramesResult {
        candidates,
        scan_was_awaited,
    }))
}

#[derive(Deserialize)]
pub struct AutoMergeArgs {
    pub frames_set_id: i64,
    pub frame_ids: Vec<i64>,
    pub source: String,
}

/// POST /api/auto_merge_new_frames_for_set
#[tracing::instrument(skip_all, err(Debug))]
pub async fn auto_merge_new_frames_for_set(
    State(state): State<WebAppState>,
    Json(args): Json<AutoMergeArgs>,
) -> Result<Json<athenaeum_core::models::MergeReport>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;

    let candidates = {
        let conn = db_ref.conn();
        let threshold_deg = state
            .ctx
            .settings
            .get_grouping_threshold_deg(&conn)
            .map_err(db_err)?;
        let all = athenaeum_core::auto_merge::find_candidates_for_set(
            &conn,
            args.frames_set_id,
            threshold_deg,
        )
        .map_err(db_err)?;
        let wanted: std::collections::HashSet<i64> = args.frame_ids.iter().copied().collect();
        all.into_iter()
            .filter(|c| wanted.contains(&c.frame_id))
            .collect::<Vec<_>>()
    };

    let threshold_arcmin = {
        let conn = db_ref.conn();
        state
            .ctx
            .settings
            .get_grouping_threshold_arcsec(&conn)
            .map_err(db_err)?
            / 60.0
    };
    let gap_hours = {
        let conn = db_ref.conn();
        state
            .ctx
            .settings
            .get_session_gap_threshold_hours(&conn)
            .unwrap_or(6.0)
    };

    let frames_set_name: Option<String> = {
        let conn = db_ref.conn();
        conn.query_row(
            "SELECT name FROM frames_set WHERE id = ?1",
            rusqlite::params![args.frames_set_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    };

    let mut conn = db_ref.conn();
    let report = athenaeum_core::auto_merge::merge_candidates(
        &mut conn,
        args.frames_set_id,
        candidates,
        &args.source,
        threshold_arcmin,
        gap_hours,
    )
    .map_err(db_err)?;

    if report.added_count > 0 {
        let payload = athenaeum_core::monitor::orchestrator::AutoMergeCompleteEvent {
            frames_set_id: args.frames_set_id,
            frames_set_name,
            source: report.source.clone(),
            added_count: report.added_count,
            skipped_count: report.skipped_count,
            threshold_arcmin,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let emitter = crate::events::SseProgressEmitter::new(state.event_tx.clone());
        athenaeum_core::events::emit_event(&emitter, "auto-merge-complete", &payload);
    }

    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct GetMergeLogArgs {
    pub frames_set_id: i64,
    pub limit: Option<i64>,
}

/// POST /api/get_frame_set_merge_log
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_frame_set_merge_log(
    State(state): State<WebAppState>,
    Json(args): Json<GetMergeLogArgs>,
) -> Result<Json<Vec<athenaeum_core::models::MergeLogEntry>>, (StatusCode, String)> {
    let db_ref = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let entries = athenaeum_core::auto_merge::log_ops::get_log_entries(
        &conn,
        args.frames_set_id,
        args.limit,
    )
    .map_err(db_err)?;
    Ok(Json(entries))
}
