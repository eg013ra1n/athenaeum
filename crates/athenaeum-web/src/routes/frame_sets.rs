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
pub struct CanSplitArgs {
    pub source_set_id: i64,
    pub selection: athenaeum_core::models::SplitSelection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitFrameSetArgs {
    pub source_set_id: i64,
    pub selection: athenaeum_core::models::SplitSelection,
    pub new_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCustomFramesSetArgs {
    pub name: String,
    pub session_ids: Vec<i64>,
}

#[derive(Deserialize)]
pub struct CreateFrameSetFromSelectionArgs {
    pub name: String,
    pub frame_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFrameSetFromExcludedArgs {
    pub file_ids: Vec<i64>,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFrameSetFlatPatternArgs {
    pub frames_set_id: i64,
    pub flat_pattern: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoGenerateFrameSetsArgs {
    pub project_id: Option<i64>,
    pub threshold_deg: Option<f64>,
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct AutoGenerateResult {
    pub sets_created: usize,
    pub frames_clustered: usize,
    pub frames_excluded: usize,
    pub frames_already_in_sets: usize,
    pub exclusion_reasons: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct FramesSetWithCount {
    pub frames_set: athenaeum_core::models::FramesSet,
    pub member_count: usize,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("frame_sets error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    eprintln!("frame_sets error: database not initialized");
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
pub async fn auto_generate_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<AutoGenerateFrameSetsArgs>,
) -> Result<Json<AutoGenerateResult>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    // Resolve threshold: use provided value or fall back to persisted setting.
    let threshold_deg = if let Some(t) = args.threshold_deg {
        t
    } else {
        state
            .ctx
            .settings
            .get_grouping_threshold_deg(&conn)
            .map_err(db_err)?
    };

    // project_id is ignored — frame sets are global.
    let project_id = args.project_id.unwrap_or(1);

    let all_frames = db::get_light_frames_for_project(&conn, project_id).map_err(db_err)?;

    let existing_member_ids = db::get_all_frames_set_member_ids(&conn).map_err(db_err)?;
    let existing_members_set: std::collections::HashSet<i64> =
        existing_member_ids.into_iter().collect();

    let mut frames_already_in_sets = 0;
    let frames: Vec<(i64, athenaeum_core::models::Frame)> = all_frames
        .into_iter()
        .filter(|(_, frame)| {
            if let Some(frame_id) = frame.id {
                if existing_members_set.contains(&frame_id) {
                    frames_already_in_sets += 1;
                    return false;
                }
            }
            true
        })
        .collect();

    if frames.is_empty() {
        return Ok(Json(AutoGenerateResult {
            sets_created: 0,
            frames_clustered: 0,
            frames_excluded: 0,
            frames_already_in_sets,
            exclusion_reasons: Vec::new(),
        }));
    }

    let (clusters, excluded) =
        athenaeum_core::clustering::auto_generate_frame_sets(frames, threshold_deg)
            .map_err(db_err)?;

    db::clear_excluded_frames(&conn).map_err(db_err)?;
    if !excluded.is_empty() {
        db::insert_excluded_frames(&conn, &excluded).map_err(db_err)?;
    }

    let gap_threshold_hours: f64 = state
        .ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    let mut sets_created = 0;
    let mut frames_clustered = 0;

    for cluster in clusters {
        let metadata = athenaeum_core::frames_set_metadata::calculate_metadata_from_frame_ids(
            &cluster.member_frame_ids,
            &conn,
        )
        .map_err(db_err)?;

        let set_id = db::create_frames_set(
            &conn,
            cluster.name.as_deref(),
            false, // auto-generated
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

        let frame_data = db::get_frames_with_files_by_ids(&conn, &cluster.member_frame_ids)
            .map_err(db_err)?;

        let detected_nights =
            athenaeum_core::sessions::detect_sessions(frame_data, gap_threshold_hours)
                .map_err(db_err)?;

        for night in detected_nights {
            let night_id =
                db::create_imaging_night(&conn, set_id, &night.start_time, &night.end_time)
                    .map_err(db_err)?;

            for session in night.sessions {
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

        sets_created += 1;
        frames_clustered += cluster.member_frame_ids.len();
    }

    let exclusion_reasons: Vec<String> = excluded.into_iter().map(|(_, reason)| reason).collect();

    Ok(Json(AutoGenerateResult {
        sets_created,
        frames_clustered,
        frames_excluded: exclusion_reasons.len(),
        frames_already_in_sets,
        exclusion_reasons,
    }))
}

/// Get all frame sets (project_id is accepted for API compatibility but ignored).
pub async fn get_frames_sets(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramesSetsArgs>,
) -> Result<Json<Vec<FramesSetWithCount>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
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
pub async fn get_frame_set_detail(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let detail = load_frame_set_detail(&conn, args.frames_set_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Delete a frame set by ID (cascade removes nights/sessions/members).
pub async fn delete_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::delete_frames_set(&conn, args.frames_set_id).map_err(db_err)?;
    Ok(Json(()))
}

/// Delete all auto-generated frame sets (is_custom = false).
pub async fn delete_auto_generated_frame_sets(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<usize>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    let n = athenaeum_core::db::delete_auto_generated_frame_sets(&conn).map_err(db_err)?;
    Ok(Json(n))
}

/// Rename a frame set.
pub async fn rename_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<RenameFramesSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock.as_ref().ok_or_else(no_db)?;
    let conn = db.conn();

    athenaeum_core::db::update_frames_set_name(&conn, args.frames_set_id, &args.new_name).map_err(db_err)?;
    Ok(Json(()))
}

/// Mark a frame set as custom (one-way; also recalculates metadata).
pub async fn mark_frame_set_custom(
    State(state): State<WebAppState>,
    Json(args): Json<MarkFrameSetCustomArgs>,
) -> Result<Json<athenaeum_core::models::FramesSet>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
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

/// Recalculate and persist frame set metadata from its current member frames.
pub async fn recalculate_frame_set_metadata(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<athenaeum_core::models::FramesSet>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let metadata = athenaeum_core::frames_set_metadata::calculate_metadata_for_frame_set(
        args.frames_set_id,
        &conn,
    )
    .map_err(|e| db_err(format!("Failed to calculate metadata: {}", e)))?;

    db::update_frames_set_metadata(
        &conn,
        args.frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // mark as custom after manual recalculation
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
pub async fn merge_frame_sets(
    State(state): State<WebAppState>,
    Json(args): Json<MergeFrameSetsArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    use athenaeum_core::db;

    if args.source_id == args.target_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot merge a frame set into itself".to_string(),
        ));
    }

    {
        let lock = state.ctx.db.lock().unwrap();
        let db_ref = lock.as_ref().ok_or_else(no_db)?;
        let conn = db_ref.conn();

        let source_nights =
            db::get_imaging_nights_for_set(&conn, args.source_id).map_err(db_err)?;
        let target_nights =
            db::get_imaging_nights_for_set(&conn, args.target_id).map_err(db_err)?;

        for source_night in source_nights {
            let source_night_id = source_night.id.ok_or_else(|| db_err("Source night has no ID"))?;

            let matching_target_night_id =
                athenaeum_core::frames_set_merge::find_matching_night(&source_night, &target_nights)
                    .map_err(|e| db_err(format!("Failed to find matching night: {}", e)))?;

            if let Some(target_night_id) = matching_target_night_id {
                let target_night = target_nights
                    .iter()
                    .find(|n| n.id == Some(target_night_id))
                    .ok_or_else(|| db_err("Target night not found"))?;

                let (new_start, new_end) = athenaeum_core::frames_set_merge::calculate_time_range_union(
                    &source_night.start_time,
                    &source_night.end_time,
                    &target_night.start_time,
                    &target_night.end_time,
                )
                .map_err(|e| db_err(format!("Failed to calculate time range union: {}", e)))?;

                db::update_imaging_night_time_range(&conn, target_night_id, &new_start, &new_end)
                    .map_err(db_err)?;

                let source_sessions =
                    db::get_sessions_for_night(&conn, source_night_id).map_err(db_err)?;

                let session_ids: Vec<i64> =
                    source_sessions.iter().filter_map(|s| s.id).collect();

                if !session_ids.is_empty() {
                    db::move_sessions_to_night(&conn, &session_ids, target_night_id)
                        .map_err(db_err)?;
                }
            } else {
                db::reassign_imaging_night_to_frame_set(&conn, source_night_id, args.target_id)
                    .map_err(db_err)?;
            }
        }

        db::deduplicate_session_members_in_set(&conn, args.target_id).map_err(db_err)?;

        let metadata =
            athenaeum_core::frames_set_metadata::calculate_metadata_for_frame_set(args.target_id, &conn)
                .map_err(|e| db_err(format!("Failed to calculate metadata: {}", e)))?;

        db::update_frames_set_metadata(
            &conn,
            args.target_id,
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            true,
            metadata.avg_rotation,
            metadata.min_rotation,
            metadata.max_rotation,
        )
        .map_err(db_err)?;

        db::delete_frames_set(&conn, args.source_id).map_err(db_err)?;
    } // lock released here

    // Re-acquire to return detail
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let detail = load_frame_set_detail(&conn, args.target_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Check whether the given selection can be split out of its source set
/// (i.e. the split would not leave the source empty).
pub async fn can_split(
    State(state): State<WebAppState>,
    Json(args): Json<CanSplitArgs>,
) -> Result<Json<bool>, (StatusCode, String)> {
    use athenaeum_core::db;
    use athenaeum_core::models::SplitSelection;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let all_nights = db::get_imaging_nights_for_set(&conn, args.source_set_id).map_err(db_err)?;

    let result = match args.selection {
        SplitSelection::Nights { ref ids } => ids.len() < all_nights.len(),
        SplitSelection::Sessions { ref ids } => {
            let mut total = 0;
            for night in &all_nights {
                if let Some(night_id) = night.id {
                    let sessions = db::get_sessions_for_night(&conn, night_id).map_err(db_err)?;
                    total += sessions.len();
                }
            }
            ids.len() < total
        }
        SplitSelection::Frames { ref ids } => {
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

    Ok(Json(result))
}

/// Split the selected items out of a frame set into a new frame set.
pub async fn split_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<SplitFrameSetArgs>,
) -> Result<Json<athenaeum_core::models::FrameSetDetail>, (StatusCode, String)> {
    use athenaeum_core::db;
    use athenaeum_core::models::SplitSelection;

    let new_set_id = {
        let lock = state.ctx.db.lock().unwrap();
        let db_ref = lock.as_ref().ok_or_else(no_db)?;
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
    }; // lock released here

    // Return detail of the newly created set.
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();
    let detail = load_frame_set_detail(&conn, new_set_id).map_err(db_err)?;
    Ok(Json(detail))
}

/// Create a custom frame set from a list of existing sessions (by cloning them).
pub async fn create_custom_frames_set(
    State(state): State<WebAppState>,
    Json(args): Json<CreateCustomFramesSetArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    // Collect frames for each selected session.
    let mut all_session_frames: Vec<(i64, Vec<(i64, athenaeum_core::models::File, athenaeum_core::models::Frame)>)> = Vec::new();
    for &session_id in &args.session_ids {
        let frame_ids = db::get_frame_ids_for_session(&conn, session_id).map_err(db_err)?;
        if !frame_ids.is_empty() {
            let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids).map_err(db_err)?;
            all_session_frames.push((session_id, frames));
        }
    }

    if all_session_frames.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No frames found in selected sessions".to_string(),
        ));
    }

    let gap_threshold_hours: f64 = state
        .ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    let mut session_frame_map: std::collections::HashMap<
        i64,
        Vec<(i64, athenaeum_core::models::File, athenaeum_core::models::Frame)>,
    > = std::collections::HashMap::new();
    let mut all_frames = Vec::new();
    let mut all_frame_ids = Vec::new();

    for (session_id, frames) in all_session_frames {
        for (_, _, frame) in &frames {
            if let Some(frame_id) = frame.id {
                all_frame_ids.push(frame_id);
            }
        }
        session_frame_map.insert(session_id, frames.clone());
        all_frames.extend(frames);
    }

    let metadata = athenaeum_core::frames_set_metadata::calculate_metadata_from_frame_ids(
        &all_frame_ids,
        &conn,
    )
    .map_err(db_err)?;

    let detected_nights = athenaeum_core::sessions::detect_sessions(all_frames, gap_threshold_hours)
        .map_err(db_err)?;

    if detected_nights.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "No imaging nights could be detected from the selected sessions. Frames may be missing date/time information.".to_string(),
        ));
    }

    let set_id = db::create_frames_set(
        &conn,
        Some(&args.name),
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
    .map_err(db_err)?;

    for night in &detected_nights {
        let night_id = db::create_imaging_night(&conn, set_id, &night.start_time, &night.end_time)
            .map_err(db_err)?;

        for &session_id in &args.session_ids {
            if let Some(session_frames) = session_frame_map.get(&session_id) {
                let session_frame_ids: std::collections::HashSet<i64> =
                    session_frames.iter().map(|(_, _, frame)| frame.id.unwrap()).collect();

                let night_frame_ids: std::collections::HashSet<i64> = night
                    .sessions
                    .iter()
                    .flat_map(|s| &s.frame_ids)
                    .copied()
                    .collect();

                let intersection: Vec<i64> =
                    session_frame_ids.intersection(&night_frame_ids).copied().collect();

                if !intersection.is_empty() {
                    db::clone_session(&conn, session_id, night_id).map_err(db_err)?;
                }
            }
        }
    }

    Ok(Json(set_id))
}

/// Create a custom frame set from a direct list of frame IDs.
pub async fn create_frame_set_from_selection(
    State(state): State<WebAppState>,
    Json(args): Json<CreateFrameSetFromSelectionArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let set_id = create_frame_set_inner(&conn, &args.name, &args.frame_ids, &state.ctx.settings)
        .map_err(db_err)?;

    Ok(Json(set_id))
}

/// Create a custom frame set from the excluded-frames list, then remove those
/// entries from the excluded table.
pub async fn create_frame_set_from_excluded(
    State(state): State<WebAppState>,
    Json(args): Json<CreateFrameSetFromExcludedArgs>,
) -> Result<Json<i64>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let frame_ids = db::get_frame_ids_for_file_ids(&conn, &args.file_ids).map_err(db_err)?;

    if frame_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No frames found for the selected files".to_string(),
        ));
    }

    // Ensure every frame has an imagetyp so session detection works.
    let placeholders: String = args.file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE frames SET imagetyp = 'Light' WHERE file_id IN ({}) AND (imagetyp IS NULL OR imagetyp = '')",
        placeholders
    );
    let params: Vec<rusqlite::types::Value> = args
        .file_ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(db_err)?;

    let set_id = create_frame_set_inner(&conn, &args.name, &frame_ids, &state.ctx.settings)
        .map_err(db_err)?;

    db::delete_excluded_frames_by_file_ids(&conn, &args.file_ids).map_err(db_err)?;

    Ok(Json(set_id))
}

// ── Excluded frames ───────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct ReclassifyResult {
    pub frames_updated: usize,
    pub cameras_refreshed: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclassifyExcludedFramesArgs {
    pub file_ids: Vec<i64>,
    pub new_imagetyp: String,
}

/// Get all excluded frames with file paths.
pub async fn get_excluded_frames(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<Vec<athenaeum_core::models::ExcludedFrameEntry>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let entries = athenaeum_core::db::get_excluded_frames(&conn).map_err(db_err)?;
    Ok(Json(entries))
}

/// Get count of excluded frames (lightweight check).
pub async fn get_excluded_frames_count(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<i64>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let count = athenaeum_core::db::get_excluded_frames_count(&conn).map_err(db_err)?;
    Ok(Json(count))
}

/// Reclassify excluded frames to a new image type, remove from excluded list,
/// and refresh calibration libraries.
pub async fn reclassify_excluded_frames(
    State(state): State<WebAppState>,
    Json(args): Json<ReclassifyExcludedFramesArgs>,
) -> Result<Json<ReclassifyResult>, (StatusCode, String)> {
    use athenaeum_core::db;

    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    let (frames_updated, cameras) =
        db::reclassify_excluded_frames(&conn, &args.file_ids, &args.new_imagetyp)
            .map_err(|e| db_err(format!("Failed to reclassify frames: {}", e)))?;

    eprintln!(
        "Reclassified {} frames, affected cameras: {:?}",
        frames_updated, cameras
    );

    // Refresh calibration library for each affected camera using the same
    // inline logic from calibration.rs (clear memberships, re-cluster, prune)
    for camera in &cameras {
        if let Err(e) = refresh_calibration_for_camera(&conn, camera) {
            eprintln!(
                "Warning: failed to refresh calibration library for {}: {}",
                camera, e
            );
        }
    }

    Ok(Json(ReclassifyResult {
        frames_updated,
        cameras_refreshed: cameras,
    }))
}

/// Refresh calibration library for a single camera (same logic as calibration.rs handler).
fn refresh_calibration_for_camera(
    conn: &rusqlite::Connection,
    instrume: &str,
) -> Result<(), String> {
    use athenaeum_core::calibration::scan_integration::{
        create_calibration_sets_from_scan_with_masters, MasterFrameIds,
    };

    conn.execute(
        "DELETE FROM calibration_set_frames
         WHERE set_id IN (
             SELECT id FROM calibration_set WHERE instrume = ?1 AND is_master_library = 0
         )",
        rusqlite::params![instrume],
    )
    .map_err(|e| format!("Failed to clear frame memberships: {}", e))?;

    conn.execute(
        "UPDATE calibration_set SET frame_count = 0
         WHERE instrume = ?1 AND is_master_library = 0",
        rusqlite::params![instrume],
    )
    .map_err(|e| format!("Failed to reset frame counts: {}", e))?;

    let query_ids = |imagetyp: &str| -> Result<Vec<i64>, String> {
        let mut stmt = conn
            .prepare("SELECT id FROM frames WHERE instrume = ?1 AND UPPER(imagetyp) = UPPER(?2)")
            .map_err(|e| e.to_string())?;
        let ids = stmt.query_map([instrume, imagetyp], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(ids)
    };

    let flat_ids = query_ids("FLAT")?;
    let dark_ids = query_ids("DARK")?;
    let bias_ids = query_ids("BIAS")?;
    let darkflat_ids = query_ids("DARKFLAT")?;
    let master_frame_ids = MasterFrameIds {
        master_dark_ids: query_ids("MASTERDARK")?,
        master_flat_ids: query_ids("MASTERFLAT")?,
        master_bias_ids: query_ids("MASTERBIAS")?,
        master_darkflat_ids: query_ids("MASTERDARKFLAT")?,
    };

    create_calibration_sets_from_scan_with_masters(
        conn, flat_ids, dark_ids, bias_ids, darkflat_ids, master_frame_ids,
    )
    .map_err(|e| format!("Failed to create calibration sets: {}", e))?;

    conn.execute(
        "DELETE FROM calibration_set
         WHERE instrume = ?1 AND is_master_library = 0 AND frame_count = 0",
        rusqlite::params![instrume],
    )
    .map_err(|e| format!("Failed to delete orphaned sets: {}", e))?;

    Ok(())
}

/// Store the flat_pattern preference for a frame set.
pub async fn update_frame_set_flat_pattern(
    State(state): State<WebAppState>,
    Json(args): Json<UpdateFrameSetFlatPatternArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    athenaeum_core::db::update_frames_set_flat_pattern(
        &conn,
        args.frames_set_id,
        args.flat_pattern.as_deref(),
    )
    .map_err(db_err)?;

    Ok(Json(()))
}

/// Archive a frame set.
pub async fn archive_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    athenaeum_core::db::set_frame_set_archived(&conn, args.frames_set_id, true).map_err(db_err)?;
    Ok(Json(()))
}

/// Unarchive a frame set.
pub async fn unarchive_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db_ref = lock.as_ref().ok_or_else(no_db)?;
    let conn = db_ref.conn();

    athenaeum_core::db::set_frame_set_archived(&conn, args.frames_set_id, false).map_err(db_err)?;
    Ok(Json(()))
}
