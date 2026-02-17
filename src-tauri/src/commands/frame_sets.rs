// Frame set commands - frame set management and operations

use crate::db::{self};
use crate::models::*;
use tauri::State;

use super::AppState;

#[tauri::command]
pub async fn auto_generate_frame_sets(
    project_id: i64,
    threshold_deg: Option<f64>,
    state: State<'_, AppState>,
) -> Result<AutoGenerateResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Use provided threshold or get from settings
    let threshold_deg = if let Some(custom_threshold) = threshold_deg {
        custom_threshold
    } else {
        state.settings
            .get_grouping_threshold_deg(&conn)
            .map_err(|e| e.to_string())?
    };

    // Fetch all LIGHT frames
    let all_frames = db::get_light_frames_for_project(&conn, project_id)
        .map_err(|e| e.to_string())?;

    // Get all frame IDs that are already in any set
    let existing_member_ids = db::get_all_frames_set_member_ids(&conn)
        .map_err(|e| e.to_string())?;
    let existing_members_set: std::collections::HashSet<i64> = existing_member_ids.into_iter().collect();

    // Filter out frames that are already in sets
    let mut frames_already_in_sets = 0;
    let frames: Vec<(i64, crate::models::Frame)> = all_frames
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
        return Ok(AutoGenerateResult {
            sets_created: 0,
            frames_clustered: 0,
            frames_excluded: 0,
            frames_already_in_sets,
            exclusion_reasons: Vec::new(),
        });
    }

    // Run clustering
    let (clusters, excluded) = crate::clustering::auto_generate_frame_sets(frames, threshold_deg)
        .map_err(|e| e.to_string())?;

    // Persist excluded frames to DB (clear old, insert new)
    db::clear_excluded_frames(&conn).map_err(|e| e.to_string())?;
    if !excluded.is_empty() {
        db::insert_excluded_frames(&conn, &excluded).map_err(|e| e.to_string())?;
    }

    // Create frame sets in a transaction
    let mut sets_created = 0;
    let mut frames_clustered = 0;

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = state.settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    for cluster in clusters {
        // Calculate metadata from cluster frames
        let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
            &cluster.member_frame_ids,
            &conn,
        ).map_err(|e| e.to_string())?;

        // Create frames_set
        let set_id = db::create_frames_set(
            &conn,
            cluster.name.as_deref(),
            false, // is_custom = false for auto-generated sets
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            metadata.avg_rotation,
            metadata.min_rotation,
            metadata.max_rotation,
        ).map_err(|e| e.to_string())?;

        // Get frames for session detection
        let frames = db::get_frames_with_files_by_ids(&conn, &cluster.member_frame_ids)
            .map_err(|e| e.to_string())?;

        // Detect sessions
        let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
            .map_err(|e| e.to_string())?;

        // Create imaging nights and sessions
        for night in detected_nights {
            let night_id = db::create_imaging_night(
                &conn,
                set_id,
                &night.start_time,
                &night.end_time,
            ).map_err(|e| e.to_string())?;

            for session in night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| e.to_string())?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(|e| e.to_string())?;
            }
        }

        sets_created += 1;
        frames_clustered += cluster.member_frame_ids.len();
    }

    Ok(AutoGenerateResult {
        sets_created,
        frames_clustered,
        frames_excluded: excluded.len(),
        frames_already_in_sets,
        exclusion_reasons: excluded.into_iter().map(|(_, reason)| reason).collect(),
    })
}

/// Get all frame sets for a project
#[tauri::command]
pub async fn get_frames_sets(
    project_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<FramesSetWithCount>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    let sets = db::get_frames_sets_by_project(&conn, project_id)
        .map_err(|e| e.to_string())?;

    Ok(sets
        .into_iter()
        .map(|(set, count)| FramesSetWithCount {
            frames_set: set,
            member_count: count,
        })
        .collect())
}

/// Delete a frames_set
#[tauri::command]
pub async fn delete_frames_set(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_frames_set(&conn, frames_set_id).map_err(|e| e.to_string())
}

/// Delete all auto-generated frames_sets (is_custom = false)
#[tauri::command]
pub async fn delete_auto_generated_frame_sets(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::delete_auto_generated_frame_sets(&conn).map_err(|e| e.to_string())
}

/// Rename a frames_set
#[tauri::command]
pub async fn rename_frames_set(
    frames_set_id: i64,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_frames_set_name(&conn, frames_set_id, &new_name).map_err(|e| e.to_string())
}

/// Mark a frames_set as custom (one-way conversion from auto-generated to custom)
#[tauri::command]
pub async fn mark_frame_set_custom(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FramesSet, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get current metadata to preserve it
    let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, &conn)
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
    ).map_err(|e| format!("Failed to update frame set: {}", e))?;

    // Return the updated frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    Ok(frames_set)
}

/// Recalculate frame set metadata from all member frames
#[tauri::command]
pub async fn recalculate_frame_set_metadata(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FramesSet, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Calculate metadata from all frames in the set
    let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(
        frames_set_id,
        &conn,
    ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    // Update the frame set with new metadata (mark as custom since it's been manually recalculated)
    db::update_frames_set_metadata(
        &conn,
        frames_set_id,
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        true, // Mark as custom after manual recalculation
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    ).map_err(|e| format!("Failed to update metadata: {}", e))?;

    // Return the updated frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    Ok(frames_set)
}

/// Update the flat_pattern preference for a frame set
#[tauri::command]
pub async fn update_frame_set_flat_pattern(
    frame_set_id: i64,
    flat_pattern: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::update_frames_set_flat_pattern(&conn, frame_set_id, Some(&flat_pattern))
        .map_err(|e| format!("Failed to update flat pattern: {}", e))?;

    Ok(())
}

/// Merge source frame set into target frame set
/// Dragged frame set (source) is merged into drop target (target)
/// Source frame set is deleted after merge
#[tauri::command]
pub async fn merge_frame_sets(
    source_id: i64,
    target_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    println!("Merging frame set {} into {}", source_id, target_id);

    if source_id == target_id {
        return Err("Cannot merge a frame set into itself".to_string());
    }

    // Perform all database operations in a scope so conn is dropped before we call get_frame_set_detail
    {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Get all nights from source frame set
        let source_nights = db::get_imaging_nights_for_set(&conn, source_id)
            .map_err(|e| format!("Failed to get source nights: {}", e))?;

        println!("Found {} nights in source frame set", source_nights.len());

        // Get all nights from target frame set
        let target_nights = db::get_imaging_nights_for_set(&conn, target_id)
            .map_err(|e| format!("Failed to get target nights: {}", e))?;

        println!("Found {} nights in target frame set", target_nights.len());

        // Process each source night
        for source_night in source_nights {
            let source_night_id = source_night.id.ok_or("Source night has no ID")?;

            // Try to find a matching night in target
            let matching_target_night_id = crate::frames_set_merge::find_matching_night(
                &source_night,
                &target_nights,
            ).map_err(|e| format!("Failed to find matching night: {}", e))?;

            if let Some(target_night_id) = matching_target_night_id {
                println!("Night {} matches target night {}", source_night_id, target_night_id);

                // Get the target night details for time range calculation
                let target_night = target_nights
                    .iter()
                    .find(|n| n.id == Some(target_night_id))
                    .ok_or("Target night not found")?;

                // Calculate union of time ranges
                let (new_start, new_end) = crate::frames_set_merge::calculate_time_range_union(
                    &source_night.start_time,
                    &source_night.end_time,
                    &target_night.start_time,
                    &target_night.end_time,
                ).map_err(|e| format!("Failed to calculate time range union: {}", e))?;

                println!("Updating target night {} time range to {} - {}", target_night_id, new_start, new_end);

                // Update target night time range
                db::update_imaging_night_time_range(&conn, target_night_id, &new_start, &new_end)
                    .map_err(|e| format!("Failed to update time range: {}", e))?;

                // Get all sessions from source night and move them to target night
                let source_sessions = db::get_sessions_for_night(&conn, source_night_id)
                    .map_err(|e| format!("Failed to get source sessions: {}", e))?;

                let session_ids: Vec<i64> = source_sessions
                    .iter()
                    .filter_map(|s| s.id)
                    .collect();

                println!("Moving {} sessions from source night {} to target night {}", session_ids.len(), source_night_id, target_night_id);

                if !session_ids.is_empty() {
                    db::move_sessions_to_night(&conn, &session_ids, target_night_id)
                        .map_err(|e| format!("Failed to move sessions: {}", e))?;
                }

                // Delete the now-empty source night
                // Note: This will cascade delete via FK constraints, but sessions were already moved
                // So we need to delete the night manually since it's now empty
                // Actually, we moved the sessions, so the night is empty and can be deleted
                // But the FK constraint will prevent deletion if there are still sessions
                // Wait, we already moved sessions, so the night should be empty now
                // Let's just leave it for now and let the frame set deletion handle it
            } else {
                println!("Night {} has no match in target, reassigning to target frame set", source_night_id);

                // No matching night found, reassign this night to target frame set
                db::reassign_imaging_night_to_frame_set(&conn, source_night_id, target_id)
                    .map_err(|e| format!("Failed to reassign night: {}", e))?;
            }
        }

        // Deduplicate frames in the target frame set
        println!("Deduplicating frames in target frame set");
        let duplicates_removed = db::deduplicate_session_members_in_set(&conn, target_id)
            .map_err(|e| format!("Failed to deduplicate: {}", e))?;

        println!("Removed {} duplicate frame references", duplicates_removed);

        // Recalculate metadata for target frame set and mark as custom
        println!("Recalculating metadata for target frame set");
        let metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(target_id, &conn)
            .map_err(|e| format!("Failed to calculate metadata: {}", e))?;

        db::update_frames_set_metadata(
            &conn,
            target_id,
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            true, // Mark as custom after merge
            metadata.avg_rotation,
            metadata.min_rotation,
            metadata.max_rotation,
        ).map_err(|e| format!("Failed to update metadata: {}", e))?;

        // Delete source frame set (cascade will handle cleanup)
        println!("Deleting source frame set {}", source_id);
        db::delete_frames_set(&conn, source_id)
            .map_err(|e| format!("Failed to delete source frame set: {}", e))?;

        println!("✅ Merge completed successfully");
    } // state_lock and conn are dropped here

    // Return the updated target frame set detail
    get_frame_set_detail(target_id, state).await
}

/// Check if a split operation is valid (won't leave source set empty)
#[tauri::command]
pub async fn can_split(
    source_set_id: i64,
    selection: crate::models::SplitSelection,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get all nights in the source set
    let all_nights = db::get_imaging_nights_for_set(&conn, source_set_id)
        .map_err(|e| format!("Failed to get nights: {}", e))?;

    match selection {
        crate::models::SplitSelection::Nights { ids } => {
            // Check if we're splitting all nights
            Ok(ids.len() < all_nights.len())
        }
        crate::models::SplitSelection::Sessions { ids } => {
            // Count total sessions in the set
            let mut total_sessions = 0;
            for night in all_nights {
                if let Some(night_id) = night.id {
                    let sessions = db::get_sessions_for_night(&conn, night_id)
                        .map_err(|e| format!("Failed to get sessions: {}", e))?;
                    total_sessions += sessions.len();
                }
            }

            // Can split if not all sessions are selected
            Ok(ids.len() < total_sessions)
        }
        crate::models::SplitSelection::Frames { ids } => {
            // Count total frames in the set
            let total_frames: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT sm.frame_id)
                 FROM session_members sm
                 JOIN sessions s ON sm.session_id = s.id
                 JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                 WHERE in_tbl.frames_set_id = ?1",
                [source_set_id],
                |row| row.get(0),
            ).map_err(|e| format!("Failed to count frames: {}", e))?;

            // Can split if not all frames are selected
            Ok(ids.len() < total_frames as usize)
        }
    }
}

/// Split selected items from a frame set into a new frame set
#[tauri::command]
pub async fn split_frame_set(
    source_set_id: i64,
    selection: crate::models::SplitSelection,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    println!("Splitting from frame set {}: new_name='{}'", source_set_id, new_name);

    // Perform all database operations in a scope
    let new_set_id = {
        let state_lock = state.db.lock().unwrap();
        let db = state_lock.as_ref().ok_or("Database not initialized")?;
        let conn = db.conn();

        // Validate that split won't leave source empty (inline to avoid nested locks)
        let all_nights = db::get_imaging_nights_for_set(&conn, source_set_id)
            .map_err(|e| format!("Failed to get nights: {}", e))?;

        let can_split_result = match &selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Check if we're splitting all nights
                ids.len() < all_nights.len()
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Count total sessions in the set
                let mut total_sessions = 0;
                for night in &all_nights {
                    if let Some(night_id) = night.id {
                        let sessions = db::get_sessions_for_night(&conn, night_id)
                            .map_err(|e| format!("Failed to get sessions: {}", e))?;
                        total_sessions += sessions.len();
                    }
                }

                // Can split if not all sessions are selected
                ids.len() < total_sessions
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Count total frames in the set
                let total_frames: i64 = conn.query_row(
                    "SELECT COUNT(DISTINCT sm.frame_id)
                     FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                     WHERE in_tbl.frames_set_id = ?1",
                    [source_set_id],
                    |row| row.get(0),
                ).map_err(|e| format!("Failed to count frames: {}", e))?;

                // Can split if not all frames are selected
                ids.len() < total_frames as usize
            }
        };

        if !can_split_result {
            return Err("Cannot split: operation would leave the source frame set empty".to_string());
        }

        // Get session gap threshold from settings
        let gap_threshold_hours: f64 = state.settings
            .get_session_gap_threshold_hours(&conn)
            .unwrap_or(6.0);

        // Collect frame IDs based on selection type
        let frame_ids: Vec<i64> = match &selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Get all frames from selected nights
                let mut frames = Vec::new();
                for night_id in ids {
                    let mut stmt = conn.prepare(
                        "SELECT DISTINCT sm.frame_id
                         FROM session_members sm
                         JOIN sessions s ON sm.session_id = s.id
                         WHERE s.imaging_night_id = ?1"
                    ).map_err(|e| format!("Failed to prepare query: {}", e))?;

                    let night_frames: Vec<i64> = stmt
                        .query_map([night_id], |row| row.get(0))
                        .map_err(|e| format!("Failed to query frames: {}", e))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| format!("Failed to collect frames: {}", e))?;

                    frames.extend(night_frames);
                }
                frames
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Get all frames from selected sessions
                let mut frames = Vec::new();
                for session_id in ids {
                    let session_frames = db::get_frame_ids_for_session(&conn, *session_id)
                        .map_err(|e| format!("Failed to get session frames: {}", e))?;
                    frames.extend(session_frames);
                }
                frames
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Use frame IDs directly
                ids.clone()
            }
        };

        println!("Split will move {} frames", frame_ids.len());

        if frame_ids.is_empty() {
            return Err("No frames to split".to_string());
        }

        // Calculate metadata for new frame set
        let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
            &frame_ids,
            &conn,
        ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

        // Create new frame set (always custom)
        let new_set_id = db::create_frames_set(
            &conn,
            Some(&new_name),
            true, // Always custom after split
            metadata.date_obs_start.as_deref(),
            metadata.date_obs_end.as_deref(),
            metadata.objctra.as_deref(),
            metadata.objctdec.as_deref(),
            metadata.total_exp_time,
            metadata.avg_rotation,
            metadata.min_rotation,
            metadata.max_rotation,
        ).map_err(|e| format!("Failed to create frame set: {}", e))?;

        println!("Created new frame set with id {}", new_set_id);

        // Get frames with file info for session detection
        let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
            .map_err(|e| format!("Failed to get frames: {}", e))?;

        // Detect nights for new frame set
        let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
            .map_err(|e| format!("Failed to detect sessions: {}", e))?;

        println!("Detected {} nights for new frame set", detected_nights.len());

        // Create nights and sessions for new frame set
        for night in detected_nights {
            let night_id = db::create_imaging_night(
                &conn,
                new_set_id,
                &night.start_time,
                &night.end_time,
            ).map_err(|e| format!("Failed to create night: {}", e))?;

            for session in night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| format!("Failed to create session: {}", e))?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)
                    .map_err(|e| format!("Failed to insert session members: {}", e))?;
            }
        }

        // Remove split frames from source frame set based on selection type
        match selection {
            crate::models::SplitSelection::Nights { ids } => {
                // Delete entire nights from source
                for night_id in ids {
                    conn.execute(
                        "DELETE FROM imaging_nights WHERE id = ?1",
                        [night_id],
                    ).map_err(|e| format!("Failed to delete night: {}", e))?;
                }
            }
            crate::models::SplitSelection::Sessions { ids } => {
                // Delete sessions from source
                for session_id in ids {
                    conn.execute(
                        "DELETE FROM sessions WHERE id = ?1",
                        [session_id],
                    ).map_err(|e| format!("Failed to delete session: {}", e))?;
                }
            }
            crate::models::SplitSelection::Frames { ids } => {
                // Remove specific frames from source frameset's sessions only
                for frame_id in ids {
                    conn.execute(
                        "DELETE FROM session_members
                         WHERE frame_id = ?1
                         AND session_id IN (
                            SELECT s.id FROM sessions s
                            JOIN imaging_nights n ON s.imaging_night_id = n.id
                            WHERE n.frames_set_id = ?2
                         )",
                        rusqlite::params![frame_id, source_set_id],
                    ).map_err(|e| format!("Failed to remove frame: {}", e))?;
                }
            }
        }

        // Recalculate metadata for source frame set and mark as custom
        println!("Recalculating metadata for source frame set");
        let source_metadata = crate::frames_set_metadata::calculate_metadata_for_frame_set(
            source_set_id,
            &conn,
        ).map_err(|e| format!("Failed to calculate source metadata: {}", e))?;

        db::update_frames_set_metadata(
            &conn,
            source_set_id,
            source_metadata.date_obs_start.as_deref(),
            source_metadata.date_obs_end.as_deref(),
            source_metadata.objctra.as_deref(),
            source_metadata.objctdec.as_deref(),
            source_metadata.total_exp_time,
            true, // Mark as custom after split
            source_metadata.avg_rotation,
            source_metadata.min_rotation,
            source_metadata.max_rotation,
        ).map_err(|e| format!("Failed to update source metadata: {}", e))?;

        println!("✅ Split completed successfully");

        new_set_id
    }; // state_lock and conn are dropped here

    // Return the new frame set detail
    get_frame_set_detail(new_set_id, state).await
}

/// Get frame set detail with imaging nights and sessions
#[tauri::command]
pub async fn get_frame_set_detail(
    frames_set_id: i64,
    state: State<'_, AppState>,
) -> Result<FrameSetDetail, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get the frame set
    let sets = db::get_frames_sets_by_project(&conn, 1)
        .map_err(|e| e.to_string())?;

    let frames_set = sets
        .into_iter()
        .find(|(set, _)| set.id == Some(frames_set_id))
        .ok_or("Frame set not found")?
        .0;

    // Check if sessions exist
    let sessions_exist = db::sessions_exist_for_frame_set(&conn, frames_set_id)
        .map_err(|e| e.to_string())?;

    if !sessions_exist {
        // Return empty nights instead of error
        return Ok(FrameSetDetail {
            frames_set,
            nights: Vec::new(),
        });
    }

    // Get the complete structure from database
    let nights = db::get_imaging_nights_with_sessions(&conn, frames_set_id)
        .map_err(|e| e.to_string())?;

    Ok(FrameSetDetail {
        frames_set,
        nights,
    })
}

/// Create a custom frames set with selected sessions
#[tauri::command]
pub async fn create_custom_frames_set(
    name: String,
    session_ids: Vec<i64>,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    println!("Creating custom frames set: name='{}', session_ids={:?}", name, session_ids);

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Get frames for all selected sessions to determine date ranges
    let mut all_session_frames = Vec::new();
    for session_id in &session_ids {
        println!("Getting frames for session {}", session_id);
        let frame_ids = db::get_frame_ids_for_session(&conn, *session_id)
            .map_err(|e| e.to_string())?;

        println!("Session {} has {} frames", session_id, frame_ids.len());

        if !frame_ids.is_empty() {
            let frames = db::get_frames_with_files_by_ids(&conn, &frame_ids)
                .map_err(|e| e.to_string())?;

            all_session_frames.push((*session_id, frames));
        }
    }

    if all_session_frames.is_empty() {
        return Err("No frames found in selected sessions".to_string());
    }

    println!("Total sessions with frames: {}", all_session_frames.len());

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = state.settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    // Flatten all frames and group by session_id for later
    let mut session_frame_map: std::collections::HashMap<i64, Vec<(i64, crate::models::File, crate::models::Frame)>> =
        std::collections::HashMap::new();
    let mut all_frames = Vec::new();
    let mut all_frame_ids = Vec::new();

    for (session_id, frames) in all_session_frames {
        // Extract frame IDs for metadata calculation
        for (_, _, frame) in &frames {
            if let Some(frame_id) = frame.id {
                all_frame_ids.push(frame_id);
            }
        }
        session_frame_map.insert(session_id, frames.clone());
        all_frames.extend(frames);
    }

    // Calculate metadata from all frames
    println!("Calculating metadata from {} frames", all_frame_ids.len());
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
        &all_frame_ids,
        &conn,
    ).map_err(|e| {
        let err_msg = format!("Failed to calculate metadata: {}", e);
        println!("{}", err_msg);
        err_msg
    })?;

    println!("Calculated metadata: date_obs_start={:?}, date_obs_end={:?}, coordinates={:?}/{:?}, total_exp_time={:?}",
             metadata.date_obs_start, metadata.date_obs_end, metadata.objctra, metadata.objctdec, metadata.total_exp_time);

    // Detect nights from all frames combined
    println!("Detecting nights from {} frames with gap threshold {} hours", all_frames.len(), gap_threshold_hours);
    let detected_nights = crate::sessions::detect_sessions(all_frames, gap_threshold_hours)
        .map_err(|e| {
            let err_msg = format!("Failed to detect sessions: {}", e);
            println!("{}", err_msg);
            err_msg
        })?;

    println!("Detected {} nights", detected_nights.len());

    if detected_nights.is_empty() {
        return Err("No imaging nights could be detected from the selected sessions. Frames may be missing date/time information.".to_string());
    }

    // Create the custom frames_set
    println!("Creating frames_set with name '{}'", name);
    let set_id = db::create_frames_set(
        &conn,
        Some(&name),
        true, // is_custom = true
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    ).map_err(|e| {
        let err_msg = format!("Failed to create frames_set: {}", e);
        println!("{}", err_msg);
        err_msg
    })?;

    println!("Created frames_set with id {}", set_id);

    // Create imaging_nights and clone sessions
    let mut total_sessions_cloned = 0;
    for (night_idx, night) in detected_nights.iter().enumerate() {
        println!("Processing night {}/{}: {} to {}", night_idx + 1, detected_nights.len(), night.start_time, night.end_time);

        let night_id = db::create_imaging_night(
            &conn,
            set_id,
            &night.start_time,
            &night.end_time,
        ).map_err(|e| {
            let err_msg = format!("Failed to create imaging_night: {}", e);
            println!("{}", err_msg);
            err_msg
        })?;

        println!("Created imaging_night with id {}", night_id);

        // For each original session, check if any of its frames belong to this night
        for &session_id in &session_ids {
            if let Some(session_frames) = session_frame_map.get(&session_id) {
                // Check if any frames from this session are in this night
                let session_frame_ids: std::collections::HashSet<i64> =
                    session_frames.iter().map(|(_, _, frame)| frame.id.unwrap()).collect();

                let night_frame_ids: std::collections::HashSet<i64> =
                    night.sessions.iter()
                        .flat_map(|s| &s.frame_ids)
                        .copied()
                        .collect();

                let intersection: Vec<i64> = session_frame_ids.intersection(&night_frame_ids)
                    .copied()
                    .collect();

                // If this session has frames in this night, clone it
                if !intersection.is_empty() {
                    println!("Cloning session {} to night {} ({} overlapping frames)", session_id, night_id, intersection.len());
                    db::clone_session(&conn, session_id, night_id)
                        .map_err(|e| {
                            let err_msg = format!("Failed to clone session {}: {}", session_id, e);
                            println!("{}", err_msg);
                            err_msg
                        })?;
                    total_sessions_cloned += 1;
                } else {
                    println!("Session {} has no frames in night {}", session_id, night_id);
                }
            }
        }
    }

    println!("Successfully created custom frames_set '{}' (id {}) with {} imaging nights and {} cloned sessions",
             name, set_id, detected_nights.len(), total_sessions_cloned);

    Ok(set_id)
}

/// Shared logic for creating a custom frame set from frame IDs.
/// Returns the new frame set ID.
fn create_frame_set_inner(
    conn: &rusqlite::Connection,
    name: &str,
    frame_ids: &[i64],
    settings: &std::sync::Arc<crate::settings::SettingsManager>,
) -> Result<i64, String> {
    if frame_ids.is_empty() {
        return Err("Cannot create frame set with no frames".to_string());
    }

    // Calculate metadata from frame IDs
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
        frame_ids,
        conn,
    ).map_err(|e| format!("Failed to calculate metadata: {}", e))?;

    println!("Calculated metadata: date_obs_start={:?}, date_obs_end={:?}, coordinates={:?}/{:?}, total_exp_time={:?}",
             metadata.date_obs_start, metadata.date_obs_end, metadata.objctra, metadata.objctdec, metadata.total_exp_time);

    // Create the custom frames_set
    let set_id = db::create_frames_set(
        conn,
        Some(name),
        true, // is_custom = true
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    ).map_err(|e| format!("Failed to create frames_set: {}", e))?;

    println!("Created frames_set with id {}", set_id);

    // Get frames with file info for session detection
    let frames = db::get_frames_with_files_by_ids(conn, frame_ids)
        .map_err(|e| format!("Failed to get frames: {}", e))?;

    // Detect nights from selected frames using gap threshold
    let gap_threshold_hours: f64 = settings
        .get_session_gap_threshold_hours(conn)
        .unwrap_or(6.0);

    println!("Detecting nights from {} frames with gap threshold {} hours", frames.len(), gap_threshold_hours);

    let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
        .map_err(|e| format!("Failed to detect sessions: {}", e))?;

    println!("Detected {} nights", detected_nights.len());

    if detected_nights.is_empty() {
        println!("No nights detected, creating single night with all frames");

        let now = chrono::Utc::now();
        let night_start = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let night_end = (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let night_id = db::create_imaging_night(conn, set_id, &night_start, &night_end)
            .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

        let session_id = db::create_session(conn, night_id, "Unknown", frame_ids.len() as i32, metadata.total_exp_time)
            .map_err(|e| format!("Failed to create session: {}", e))?;

        db::insert_session_members(conn, session_id, frame_ids)
            .map_err(|e| format!("Failed to add frames to session: {}", e))?;

        println!("Created custom frame set '{}' (id {}) with {} frames", name, set_id, frame_ids.len());
    } else {
        for (night_idx, night) in detected_nights.iter().enumerate() {
            println!("Processing night {}/{}: {} to {}", night_idx + 1, detected_nights.len(), night.start_time, night.end_time);

            let night_id = db::create_imaging_night(conn, set_id, &night.start_time, &night.end_time)
                .map_err(|e| format!("Failed to create imaging_night: {}", e))?;

            for session in &night.sessions {
                let session_id = db::create_session(
                    conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                ).map_err(|e| format!("Failed to create session: {}", e))?;

                db::insert_session_members(conn, session_id, &session.frame_ids)
                    .map_err(|e| format!("Failed to add frames to session: {}", e))?;
            }
        }

        println!("Created custom frame set '{}' (id {}) with {} nights and {} frames", name, set_id, detected_nights.len(), frame_ids.len());
    }

    Ok(set_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn create_frame_set_from_selection(
    state: State<'_, AppState>,
    name: String,
    frame_ids: Vec<i64>,
    _description: Option<String>,
) -> Result<i64, String> {
    println!(
        "Creating frame set from selection: name='{}', frame_count={}",
        name,
        frame_ids.len()
    );

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    create_frame_set_inner(&conn, &name, &frame_ids, &state.settings)
}

/// Create a custom frame set from excluded frames (identified by file_id),
/// then remove them from the excluded_frames table.
#[tauri::command]
pub async fn create_frame_set_from_excluded(
    file_ids: Vec<i64>,
    name: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    println!(
        "Creating frame set from {} excluded files: name='{}'",
        file_ids.len(),
        name
    );

    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    // Map file_ids → frame_ids
    let frame_ids = db::get_frame_ids_for_file_ids(&conn, &file_ids)
        .map_err(|e| format!("Failed to get frame IDs: {}", e))?;

    if frame_ids.is_empty() {
        return Err("No frames found for the selected files".to_string());
    }

    println!("Mapped {} file_ids to {} frame_ids", file_ids.len(), frame_ids.len());

    // Create the frame set
    let set_id = create_frame_set_inner(&conn, &name, &frame_ids, &state.settings)?;

    // Remove from excluded_frames
    let deleted = db::delete_excluded_frames_by_file_ids(&conn, &file_ids)
        .map_err(|e| format!("Failed to remove from excluded frames: {}", e))?;

    println!("Removed {} entries from excluded_frames, created frame set {}", deleted, set_id);

    Ok(set_id)
}

/// Reclassify excluded frames to a new image type (e.g., Flat or Dark),
/// remove them from the excluded list, and refresh calibration libraries.
#[tauri::command]
pub async fn reclassify_excluded_frames(
    file_ids: Vec<i64>,
    new_imagetyp: String,
    state: State<'_, AppState>,
) -> Result<ReclassifyResult, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    println!(
        "Reclassifying {} frames to {}",
        file_ids.len(),
        new_imagetyp
    );

    let (frames_updated, cameras) =
        db::reclassify_excluded_frames(&conn, &file_ids, &new_imagetyp)
            .map_err(|e| format!("Failed to reclassify frames: {}", e))?;

    println!(
        "Updated {} frames, affected cameras: {:?}",
        frames_updated, cameras
    );

    // Refresh calibration library for each affected camera
    for camera in &cameras {
        if let Err(e) = super::calibration::refresh_calibration_library_inner(&conn, camera) {
            eprintln!(
                "Warning: failed to refresh calibration library for {}: {}",
                camera, e
            );
        }
    }

    Ok(ReclassifyResult {
        frames_updated,
        cameras_refreshed: cameras,
    })
}

/// Get count of excluded frames (lightweight check)
#[tauri::command]
pub async fn get_excluded_frames_count(
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_excluded_frames_count(&conn).map_err(|e| e.to_string())
}

/// Get all excluded frames with file paths
#[tauri::command]
pub async fn get_excluded_frames(
    state: State<'_, AppState>,
) -> Result<Vec<crate::models::ExcludedFrameEntry>, String> {
    let state_lock = state.db.lock().unwrap();
    let db = state_lock.as_ref().ok_or("Database not initialized")?;
    let conn = db.conn();

    db::get_excluded_frames(&conn).map_err(|e| e.to_string())
}

// DTOs for frame sets
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
    pub frames_set: FramesSet,
    pub member_count: usize,
}

#[derive(serde::Serialize)]
pub struct ReclassifyResult {
    pub frames_updated: usize,
    pub cameras_refreshed: Vec<String>,
}
