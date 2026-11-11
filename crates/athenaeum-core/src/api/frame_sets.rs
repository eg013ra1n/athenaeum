//! Frame-set workflows shared by both transports.
//!
//! Single source of truth for `auto_generate_frame_sets`: the Tauri command
//! (`commands/frame_sets.rs`) and the Axum route (`routes/frame_sets.rs`) are
//! thin delegations onto the function below. The body is carried over verbatim
//! from the pre-move Tauri copy — only the error adaptation
//! (`map_err(|e| e.to_string())` → `?` on `anyhow::Result`) and the now-injected
//! `emitter` differ.

use anyhow::{anyhow, Result};

use crate::api::ApiError;
use crate::db::{self};
use crate::events::ProgressEmitter;
use crate::models::{MergeReport, SplitSelection};
use crate::services::ServiceContext;
use crate::sessions::{ReconcileOutcome, RederiveSummary};
use rusqlite::{Connection, OptionalExtension};

/// A frame set's own center in degrees, parsed from its stored
/// `objctra`/`objctdec` — the derivation `auto_generate_frame_sets` uses for
/// its `project-set-match` hook right after creating a set, extracted (Task
/// 7) so a caller with just a set id (not fresh clustering metadata) can
/// compute the same thing. `None` when the set doesn't exist or has no
/// usable center coordinates — never an error, same as
/// `record_project_link_intent`'s own read of these two columns.
pub fn frame_set_center_deg(conn: &Connection, frames_set_id: i64) -> Result<Option<(f64, f64)>> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT objctra, objctdec FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(ra_str), Some(dec_str))) = row else {
        return Ok(None);
    };
    Ok(
        match (
            crate::coordinates::parse_ra_sexagesimal(&ra_str),
            crate::coordinates::parse_dec_sexagesimal(&dec_str),
        ) {
            (Ok(ra), Ok(dec)) => Some((ra, dec)),
            _ => None,
        },
    )
}

/// `is_custom` of a frame set, or an error naming the id when there is no
/// such set.
fn frame_set_is_custom(conn: &Connection, frames_set_id: i64) -> Result<bool> {
    let flag: Option<i64> = conn
        .query_row(
            "SELECT is_custom FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |row| row.get(0),
        )
        .optional()?;
    flag.map(|v| v != 0)
        .ok_or_else(|| anyhow!("frame set {frames_set_id} not found"))
}

/// Recompute the set's aggregate metadata from its (re-derived) membership.
fn refresh_frame_set_metadata(
    conn: &Connection,
    frames_set_id: i64,
    is_custom: bool,
) -> Result<()> {
    let m = crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, conn)?;
    db::update_frames_set_metadata(
        conn,
        frames_set_id,
        m.date_obs_start.as_deref(),
        m.date_obs_end.as_deref(),
        m.objctra.as_deref(),
        m.objctdec.as_deref(),
        m.total_exp_time,
        is_custom,
        m.avg_rotation,
        m.min_rotation,
        m.max_rotation,
    )?;
    Ok(())
}

/// A frame set's center and rotation after "refresh coordinates".
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetPointingUpdate {
    pub objctra: Option<String>,
    pub objctdec: Option<String>,
    pub avg_rotation: Option<f64>,
    pub min_rotation: Option<f64>,
    pub max_rotation: Option<f64>,
    /// Lights the values were taken from.
    pub lights_used: u32,
    /// True when those lights are all plate-solved by Athenaeum.
    pub solved: bool,
}

/// Recompute a frame set's center and rotation from its lights by the one
/// rule (`frames_set_metadata::set_pointing`: Black Hole lights left out,
/// Athenaeum-solved lights preferred) and store them. Nights, sessions,
/// dates and hours are untouched.
pub fn recompute_frame_set_coordinates(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<FrameSetPointingUpdate> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let conn = db.conn();
    let m = crate::frames_set_metadata::calculate_metadata_for_frame_set(frames_set_id, &conn)?;
    let updated = db::update_frames_set_pointing(
        &conn,
        frames_set_id,
        m.objctra.as_deref(),
        m.objctdec.as_deref(),
        m.avg_rotation,
        m.min_rotation,
        m.max_rotation,
    )?;
    if updated == 0 {
        return Err(anyhow!("frame set {frames_set_id} not found"));
    }
    tracing::info!(
        frame_set_id = frames_set_id,
        count = m.pointing_frames,
        solved_only = m.pointing_solved,
        "frame set coordinates recomputed"
    );
    Ok(FrameSetPointingUpdate {
        objctra: m.objctra,
        objctdec: m.objctdec,
        avg_rotation: m.avg_rotation,
        min_rotation: m.min_rotation,
        max_rotation: m.max_rotation,
        lights_used: m.pointing_frames as u32,
        solved: m.pointing_solved,
    })
}

/// Re-derive one frame set's nights and sessions from its member frames and
/// refresh its aggregate metadata — the manual "Recalculate nights" action,
/// which repairs a set an older merge left with one night stored as two rows.
pub fn recalculate_frame_set_nights(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<RederiveSummary> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let gap_hours = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);
    let tx = conn.transaction()?;
    let is_custom = frame_set_is_custom(&tx, frames_set_id)?;
    let summary = crate::sessions::rederive_for_frame_set(&tx, frames_set_id, &[], gap_hours)?;
    refresh_frame_set_metadata(&tx, frames_set_id, is_custom)?;
    tx.commit()?;
    Ok(summary)
}

/// Result of [`reconcile_frame_set_nights`]. `nights`/`sessions` are only
/// meaningful when `changed` is `true` — an unchanged reconcile recomputed
/// nothing worth reporting and leaves them at `0`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileSummary {
    pub changed: bool,
    pub nights: usize,
    pub sessions: usize,
}

/// Cheap, idempotent nights/sessions check meant to run on every frame-set
/// page open — unlike [`recalculate_frame_set_nights`], which always
/// rewrites (and so always mints a fresh `sessions.uuid` per session, since
/// `sessions` is a UUID table), this only writes when the stored rows
/// actually disagree with a fresh derivation
/// ([`crate::sessions::reconcile_for_frame_set`]).
///
/// Skips archived sets outright — an archived set's membership is frozen,
/// so there is nothing to reconcile, and its members are never read.
pub fn reconcile_frame_set_nights(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<ReconcileSummary> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();

    let archived_at: Option<String> = conn
        .query_row(
            "SELECT archived_at FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("frame set {frames_set_id} not found"))?;
    if archived_at.is_some() {
        tracing::debug!(frames_set_id, "nights unchanged");
        return Ok(ReconcileSummary {
            changed: false,
            nights: 0,
            sessions: 0,
        });
    }

    let gap_hours = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);
    let tx = conn.transaction()?;
    let outcome = crate::sessions::reconcile_for_frame_set(&tx, frames_set_id, gap_hours)?;
    let summary = match outcome {
        ReconcileOutcome::Unchanged => {
            tx.commit()?;
            tracing::debug!(frames_set_id, "nights unchanged");
            return Ok(ReconcileSummary {
                changed: false,
                nights: 0,
                sessions: 0,
            });
        }
        ReconcileOutcome::Rewritten(summary) => summary,
    };
    let is_custom = frame_set_is_custom(&tx, frames_set_id)?;
    refresh_frame_set_metadata(&tx, frames_set_id, is_custom)?;
    tx.commit()?;
    tracing::info!(
        frames_set_id,
        nights = summary.nights,
        sessions = summary.sessions,
        "nights reconciled"
    );
    Ok(ReconcileSummary {
        changed: true,
        nights: summary.nights,
        sessions: summary.sessions,
    })
}

/// Merge `source_id` into `target_id`: every source night moves over, the
/// target's nights and sessions are re-derived from the union of both
/// memberships (a night is derived data — recomputed, never stitched by
/// date + overlap, which stored one night as two rows), the target's
/// metadata is refreshed and marked custom, and the source set is deleted.
/// One transaction; the same body serves both transports.
pub fn merge_frame_sets(ctx: &ServiceContext, source_id: i64, target_id: i64) -> Result<()> {
    if source_id == target_id {
        return Err(anyhow!("Cannot merge a frame set into itself"));
    }
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let gap_hours = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);
    let tx = conn.transaction()?;
    frame_set_is_custom(&tx, source_id)?;
    frame_set_is_custom(&tx, target_id)?;
    tracing::info!(source_id, target_id, "merging frame sets");

    for night in db::get_imaging_nights_for_set(&tx, source_id)? {
        let night_id = night.id.ok_or_else(|| anyhow!("source night has no id"))?;
        db::reassign_imaging_night_to_frame_set(&tx, night_id, target_id)?;
    }
    let summary = crate::sessions::rederive_for_frame_set(&tx, target_id, &[], gap_hours)?;
    refresh_frame_set_metadata(&tx, target_id, true)?;
    db::delete_frames_set(&tx, source_id)?;
    tx.commit()?;
    tracing::info!(
        source_id,
        target_id,
        frames = summary.frames,
        nights = summary.nights,
        "merge completed"
    );
    Ok(())
}

/// Outcome of one auto-generate run.
///
/// Serialized straight to the frontend by BOTH transports. There is deliberately
/// no `#[serde(rename_all = "camelCase")]`: the wire shape is snake_case, which
/// is what `src/types/helpers.ts::AutoGenerateResult` mirrors. Do not "fix" the
/// casing — it would silently break the Objects page.
#[derive(serde::Serialize)]
pub struct AutoGenerateResult {
    pub sets_created: usize,
    pub frames_clustered: usize,
    pub frames_excluded: usize,
    pub frames_already_in_sets: usize,
    pub exclusion_reasons: Vec<String>,
}

/// Cluster every not-yet-grouped LIGHT frame into frame sets, persisting the
/// excluded ones, detecting imaging nights/sessions per new set, and emitting a
/// `project-set-match` suggestion for any set whose center falls inside one of
/// my collaboration projects.
///
/// `_project_id` is accepted for API compatibility and ignored — frame sets are
/// global (CLAUDE.md); `db::get_light_frames_for_project` ignores it too.
/// `threshold_deg` overrides the persisted grouping threshold when supplied.
pub fn auto_generate_frame_sets(
    ctx: &ServiceContext,
    _project_id: Option<i64>,
    threshold_deg: Option<f64>,
    emitter: &dyn ProgressEmitter,
) -> Result<AutoGenerateResult> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let conn = db.conn();

    // Use provided threshold or get from settings
    let threshold_deg = if let Some(custom_threshold) = threshold_deg {
        custom_threshold
    } else {
        ctx.settings.get_grouping_threshold_deg(&conn)?
    };

    // Fetch all LIGHT frames
    let all_frames = db::get_light_frames_for_project(&conn, _project_id.unwrap_or(1))?;

    // Get all frame IDs that are already in any set
    let existing_member_ids = db::get_all_frames_set_member_ids(&conn)?;
    let existing_members_set: std::collections::HashSet<i64> =
        existing_member_ids.into_iter().collect();

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
    let (clusters, excluded) = crate::clustering::auto_generate_frame_sets(frames, threshold_deg)?;

    // Persist excluded frames to DB (clear old, insert new)
    db::clear_excluded_frames(&conn)?;
    if !excluded.is_empty() {
        db::insert_excluded_frames(&conn, &excluded)?;
    }

    // Create frame sets in a transaction
    let mut sets_created = 0;
    let mut frames_clustered = 0;

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    for cluster in clusters {
        // Calculate metadata from cluster frames
        let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(
            &cluster.member_frame_ids,
            &conn,
        )?;

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
        )?;

        // Collaboration: suggest linking a new set whose center falls inside one
        // of my projects' target radius (spec §7 join-first-shoot-later; never
        // auto-link — the notification is a suggestion). A center-read failure
        // (a corrupt objctra/objctdec, a DB hiccup) must never abort clustering
        // for every REMAINING cluster in this batch — sweep item 7: warn and
        // skip the suggestion for this one set, same fail-open shape as the
        // `find_matching_projects` arm right below it.
        match frame_set_center_deg(&conn, set_id) {
            Ok(Some((ra, dec))) => {
                match crate::api::collab::find_matching_projects(&conn, ra, dec, set_id) {
                    Ok(matches) if !matches.is_empty() => {
                        crate::events::emit_event(
                            emitter,
                            "project-set-match",
                            &crate::api::collab::ProjectSetMatchEvent {
                                frames_set_id: set_id,
                                set_name: cluster.name.clone(),
                                matches,
                            },
                        );
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(set_id, error = %format!("{err:#}"), "project match check failed")
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(set_id, error = %format!("{err:#}"), "frame set center read failed; skipping the collab set-match suggestion for this set");
            }
        }

        // Get frames for session detection
        let frames = db::get_frames_with_files_by_ids(&conn, &cluster.member_frame_ids)?;

        // Detect sessions
        let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)?;

        // Create imaging nights and sessions
        for night in detected_nights {
            let night_id =
                db::create_imaging_night(&conn, set_id, &night.start_time, &night.end_time)?;

            for session in night.sessions {
                let session_id = db::create_session(
                    &conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                )?;

                db::insert_session_members(&conn, session_id, &session.frame_ids)?;
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

/// Spec 2026-09-28 §6.1 (F5): the user's word that the set's lights are
/// calibrated by an external tool. Refuses a zipped set (its files are not
/// on disk to seed). Marks the set dirty for auto-publish.
pub fn set_frame_set_attestation(
    ctx: &ServiceContext,
    frames_set_id: i64,
    attested: bool,
) -> Result<(), ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let zipped: Option<Option<String>> = conn
        .query_row(
            "SELECT archived_at FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ApiError::Internal(format!("read frames_set: {e}")))?;
    match zipped {
        None => {
            tracing::warn!(frames_set_id, "attestation refused: no such set");
            return Err(ApiError::NotFound(format!(
                "frame set {frames_set_id} not found"
            )));
        }
        Some(Some(_)) => {
            tracing::warn!(frames_set_id, "attestation refused: the set is zipped");
            return Err(ApiError::Invalid(
                "This set is zipped — unarchive it before attesting its calibration.".into(),
            ));
        }
        Some(None) => {}
    }
    crate::db::collab::set_frames_set_attestation(&conn, frames_set_id, attested)
        .map_err(|e| ApiError::Internal(format!("write attestation: {e:#}")))?;
    tracing::info!(frames_set_id, attested, "frame set attestation set");
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_autopublish::request_auto_publish_for_sets(&[frames_set_id]);
    Ok(())
}

/// `Internal` error with the "Failed to <what>: <cause>" text both hosts show.
fn failed<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ApiError {
    move |e| ApiError::Internal(format!("Failed to {what}: {e}"))
}

/// Split the selected nights, sessions or frames of `source_set_id` into a new
/// custom frame set named `new_name`, re-detect the new set's nights and
/// sessions, take the split items out of the source and refresh the source's
/// metadata (marked custom). Returns the new set's id; each host builds the
/// returned detail from it. A split that would leave the source empty, or that
/// selects no frames, is refused with `Invalid`. No transaction: the statements
/// run one by one on the catalog connection, as they always did.
pub fn split_frame_set(
    ctx: &ServiceContext,
    source_set_id: i64,
    selection: SplitSelection,
    new_name: &str,
) -> Result<i64, ApiError> {
    tracing::info!(source_set_id, new_name = %new_name, "splitting frame set");

    let db = crate::api::db(ctx)?;
    let conn = db.conn();

    // Validate that split won't leave source empty (inline to avoid nested locks)
    let all_nights =
        db::get_imaging_nights_for_set(&conn, source_set_id).map_err(failed("get nights"))?;

    let can_split_result = match &selection {
        SplitSelection::Nights { ids } => {
            // Check if we're splitting all nights
            ids.len() < all_nights.len()
        }
        SplitSelection::Sessions { ids } => {
            // Count total sessions in the set
            let mut total_sessions = 0;
            for night in &all_nights {
                if let Some(night_id) = night.id {
                    let sessions = db::get_sessions_for_night(&conn, night_id)
                        .map_err(failed("get sessions"))?;
                    total_sessions += sessions.len();
                }
            }

            // Can split if not all sessions are selected
            ids.len() < total_sessions
        }
        SplitSelection::Frames { ids } => {
            // Count total frames in the set
            let total_frames: i64 = conn
                .query_row(
                    "SELECT COUNT(DISTINCT sm.frame_id)
                     FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights in_tbl ON s.imaging_night_id = in_tbl.id
                     WHERE in_tbl.frames_set_id = ?1",
                    [source_set_id],
                    |row| row.get(0),
                )
                .map_err(failed("count frames"))?;

            // Can split if not all frames are selected
            ids.len() < total_frames as usize
        }
    };

    if !can_split_result {
        return Err(ApiError::Invalid(
            "Cannot split: operation would leave the source frame set empty".to_string(),
        ));
    }

    // Get session gap threshold from settings
    let gap_threshold_hours: f64 = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .unwrap_or(6.0);

    // Collect frame IDs based on selection type
    let frame_ids: Vec<i64> = match &selection {
        SplitSelection::Nights { ids } => {
            // Get all frames from selected nights
            let mut frames = Vec::new();
            for night_id in ids {
                let mut stmt = conn
                    .prepare(
                        "SELECT DISTINCT sm.frame_id
                         FROM session_members sm
                         JOIN sessions s ON sm.session_id = s.id
                         WHERE s.imaging_night_id = ?1",
                    )
                    .map_err(failed("prepare query"))?;

                let night_frames: Vec<i64> = stmt
                    .query_map([night_id], |row| row.get(0))
                    .map_err(failed("query frames"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(failed("collect frames"))?;

                frames.extend(night_frames);
            }
            frames
        }
        SplitSelection::Sessions { ids } => {
            // Get all frames from selected sessions
            let mut frames = Vec::new();
            for session_id in ids {
                let session_frames = db::get_frame_ids_for_session(&conn, *session_id)
                    .map_err(failed("get session frames"))?;
                frames.extend(session_frames);
            }
            frames
        }
        SplitSelection::Frames { ids } => {
            // Use frame IDs directly
            ids.clone()
        }
    };

    tracing::debug!(count = frame_ids.len(), "split will move frames");

    if frame_ids.is_empty() {
        return Err(ApiError::Invalid("No frames to split".to_string()));
    }

    // Calculate metadata for new frame set
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(&frame_ids, &conn)
        .map_err(failed("calculate metadata"))?;

    // Create new frame set (always custom)
    let new_set_id = db::create_frames_set(
        &conn,
        Some(new_name),
        true, // Always custom after split
        metadata.date_obs_start.as_deref(),
        metadata.date_obs_end.as_deref(),
        metadata.objctra.as_deref(),
        metadata.objctdec.as_deref(),
        metadata.total_exp_time,
        metadata.avg_rotation,
        metadata.min_rotation,
        metadata.max_rotation,
    )
    .map_err(failed("create frame set"))?;

    tracing::debug!(set_id = new_set_id, "created new frame set");

    // Get frames with file info for session detection
    let frames =
        db::get_frames_with_files_by_ids(&conn, &frame_ids).map_err(failed("get frames"))?;

    // Detect nights for new frame set
    let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
        .map_err(failed("detect sessions"))?;

    tracing::debug!(
        count = detected_nights.len(),
        "detected nights for new frame set"
    );

    // Create nights and sessions for new frame set
    for night in detected_nights {
        let night_id = db::create_imaging_night(&conn, new_set_id, &night.start_time, &night.end_time)
            .map_err(failed("create night"))?;

        for session in night.sessions {
            let session_id = db::create_session(
                &conn,
                night_id,
                &session.instrume,
                session.frame_ids.len() as i32,
                session.total_exp_time,
            )
            .map_err(failed("create session"))?;

            db::insert_session_members(&conn, session_id, &session.frame_ids)
                .map_err(failed("insert session members"))?;
        }
    }

    // Remove split frames from source frame set based on selection type
    match selection {
        SplitSelection::Nights { ids } => {
            // Delete entire nights from source
            for night_id in ids {
                conn.execute("DELETE FROM imaging_nights WHERE id = ?1", [night_id])
                    .map_err(failed("delete night"))?;
            }
        }
        SplitSelection::Sessions { ids } => {
            // Delete sessions from source
            for session_id in ids {
                conn.execute("DELETE FROM sessions WHERE id = ?1", [session_id])
                    .map_err(failed("delete session"))?;
            }
        }
        SplitSelection::Frames { ids } => {
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
                )
                .map_err(failed("remove frame"))?;
            }
        }
    }

    // Recalculate metadata for source frame set and mark as custom
    tracing::debug!(source_set_id, "recalculating metadata for source frame set");
    let source_metadata =
        crate::frames_set_metadata::calculate_metadata_for_frame_set(source_set_id, &conn)
            .map_err(failed("calculate source metadata"))?;

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
    )
    .map_err(failed("update source metadata"))?;

    tracing::info!(source_set_id, new_set_id, "split completed successfully");

    Ok(new_set_id)
}

/// Create a custom frame set from a list of frame ids: metadata from the
/// frames, nights and sessions detected with the session-gap setting, or one
/// single night with an "Unknown" session when detection finds none. Returns the
/// new set's id. No transaction, as before.
pub fn create_frame_set_from_selection(
    ctx: &ServiceContext,
    name: &str,
    frame_ids: &[i64],
) -> Result<i64, ApiError> {
    tracing::info!(name = %name, count = frame_ids.len(), "creating frame set from selection");

    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let conn: &Connection = &conn;

    if frame_ids.is_empty() {
        return Err(ApiError::Internal(
            "Cannot create frame set with no frames".to_string(),
        ));
    }

    // Calculate metadata from frame IDs
    let metadata = crate::frames_set_metadata::calculate_metadata_from_frame_ids(frame_ids, conn)
        .map_err(failed("calculate metadata"))?;

    tracing::debug!(
        date_obs_start = ?metadata.date_obs_start,
        date_obs_end = ?metadata.date_obs_end,
        objctra = ?metadata.objctra,
        objctdec = ?metadata.objctdec,
        total_exp_time = ?metadata.total_exp_time,
        "calculated frame set metadata"
    );

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
    )
    .map_err(failed("create frames_set"))?;

    tracing::debug!(set_id, "created frames_set");

    // Get frames with file info for session detection
    let frames =
        db::get_frames_with_files_by_ids(conn, frame_ids).map_err(failed("get frames"))?;

    // Detect nights from selected frames using gap threshold
    let gap_threshold_hours: f64 = ctx
        .settings
        .get_session_gap_threshold_hours(conn)
        .unwrap_or(6.0);

    tracing::debug!(
        count = frames.len(),
        gap_threshold_hours,
        "detecting nights from frames"
    );

    let detected_nights = crate::sessions::detect_sessions(frames, gap_threshold_hours)
        .map_err(failed("detect sessions"))?;

    tracing::debug!(count = detected_nights.len(), "nights detected");

    if detected_nights.is_empty() {
        tracing::debug!("no nights detected, creating single night with all frames");

        let now = chrono::Utc::now();
        let night_start = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let night_end =
            (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let night_id = db::create_imaging_night(conn, set_id, &night_start, &night_end)
            .map_err(failed("create imaging_night"))?;

        let session_id = db::create_session(
            conn,
            night_id,
            "Unknown",
            frame_ids.len() as i32,
            metadata.total_exp_time,
        )
        .map_err(failed("create session"))?;

        db::insert_session_members(conn, session_id, frame_ids)
            .map_err(failed("add frames to session"))?;

        tracing::debug!(name = %name, set_id, count = frame_ids.len(), "created custom frame set");
    } else {
        for (night_idx, night) in detected_nights.iter().enumerate() {
            tracing::debug!(
                night_index = night_idx + 1,
                total_nights = detected_nights.len(),
                start = %night.start_time,
                end = %night.end_time,
                "processing night"
            );

            let night_id = db::create_imaging_night(conn, set_id, &night.start_time, &night.end_time)
                .map_err(failed("create imaging_night"))?;

            for session in &night.sessions {
                let session_id = db::create_session(
                    conn,
                    night_id,
                    &session.instrume,
                    session.frame_ids.len() as i32,
                    session.total_exp_time,
                )
                .map_err(failed("create session"))?;

                db::insert_session_members(conn, session_id, &session.frame_ids)
                    .map_err(failed("add frames to session"))?;
            }
        }

        tracing::debug!(name = %name, set_id, nights = detected_nights.len(), frames = frame_ids.len(), "created custom frame set");
    }

    Ok(set_id)
}

/// Attach the given frames to the frame set (via session detection + append)
/// and record an audit-log entry. `source` is "button" or "monitor".
///
/// Frames are re-resolved from `frame_ids` so the backend can't be tricked by a
/// stale client-supplied `CandidateFrame` payload. Emits `auto-merge-complete`
/// through `emitter` when the merge added frames.
pub fn auto_merge_new_frames_for_set(
    ctx: &ServiceContext,
    frames_set_id: i64,
    frame_ids: &[i64],
    source: &str,
    emitter: &dyn ProgressEmitter,
) -> Result<MergeReport, ApiError> {
    let db = crate::api::db(ctx)?;

    // Rebuild CandidateFrame structs from frame_ids so clients can't inject
    // fabricated coordinates past the validation loop.
    let candidates = {
        let conn = db.conn();
        let threshold_deg = ctx
            .settings
            .get_grouping_threshold_deg(&conn)
            .map_err(failed("read threshold"))?;
        let all = crate::auto_merge::find_candidates_for_set(&conn, frames_set_id, threshold_deg)
            .map_err(failed("find candidates"))?;
        let wanted: std::collections::HashSet<i64> = frame_ids.iter().copied().collect();
        all.into_iter()
            .filter(|c| wanted.contains(&c.frame_id))
            .collect::<Vec<_>>()
    };

    let threshold_arcmin = {
        let conn = db.conn();
        ctx.settings
            .get_grouping_threshold_arcsec(&conn)
            .map_err(failed("read threshold"))?
            / 60.0
    };
    let gap_hours = {
        let conn = db.conn();
        ctx.settings
            .get_session_gap_threshold_hours(&conn)
            .unwrap_or(6.0)
    };

    // Look up the set's name (best effort) to make the event payload useful
    // for the frontend toast/notification.
    let frames_set_name: Option<String> = {
        let conn = db.conn();
        conn.query_row(
            "SELECT name FROM frames_set WHERE id = ?1",
            rusqlite::params![frames_set_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    };

    // merge_candidates opens its own transaction on the connection.
    let mut conn = db.conn();
    let report = crate::auto_merge::merge_candidates(
        &mut conn,
        frames_set_id,
        candidates,
        source,
        threshold_arcmin,
        gap_hours,
    )
    .map_err(|e| ApiError::Internal(format!("Auto-merge failed: {e}")))?;

    // Fire the event if the merge actually added frames. The frontend
    // dedupes by (frames_set_id, timestamp) so duplicate emits from
    // rapid-fire merges are harmless.
    if report.added_count > 0 {
        let payload = crate::monitor::orchestrator::AutoMergeCompleteEvent {
            frames_set_id,
            frames_set_name,
            source: report.source.clone(),
            added_count: report.added_count,
            skipped_count: report.skipped_count,
            threshold_arcmin,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        crate::events::emit_event(emitter, "auto-merge-complete", &payload);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
    use crate::services::ServiceContext;
    use rusqlite::{params, Connection};

    fn seed_light_at(conn: &Connection, id: i64, date_obs: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-01-01T00:00:00Z', 'FITS')",
            params![id, format!("/t/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, date_obs, exptime)
             VALUES (?1, ?1, 'Light', 'CamA', ?2, 60.0)",
            params![id, date_obs],
        )
        .unwrap();
    }

    fn seed_set_with_night(conn: &Connection, set_id: i64, start: &str, end: &str, ids: &[i64]) {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![set_id, format!("Set {set_id}")],
        )
        .unwrap();
        let night_id = db::create_imaging_night(conn, set_id, start, end).unwrap();
        let session_id =
            db::create_session(conn, night_id, "CamA", ids.len() as i32, None).unwrap();
        db::insert_session_members(conn, session_id, ids).unwrap();
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The two halves of one night (a post-flip cluster merged back) become
    /// ONE night row on the target; the source set is gone, the target is
    /// custom, and no member is lost.
    #[test]
    fn merge_frame_sets_rederives_the_nights_and_deletes_the_source() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            seed_light_at(&conn, 10, "2025-09-13T21:55:00Z");
            seed_light_at(&conn, 11, "2025-09-13T23:30:00Z");
            seed_light_at(&conn, 12, "2025-09-13T22:36:00Z");
            seed_light_at(&conn, 13, "2025-09-14T01:59:00Z");
            seed_set_with_night(
                &conn,
                1,
                "2025-09-13T21:55:00Z",
                "2025-09-13T23:30:00Z",
                &[10, 11],
            );
            seed_set_with_night(
                &conn,
                2,
                "2025-09-13T22:36:00Z",
                "2025-09-14T01:59:00Z",
                &[12, 13],
            );
        }

        merge_frame_sets(&ctx, 2, 1).unwrap();

        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = 1"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM session_members m JOIN sessions s ON s.id = m.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id WHERE n.frames_set_id = 1"
            ),
            4
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM frames_set WHERE id = 2"),
            0
        );
        assert_eq!(
            count(&conn, "SELECT is_custom FROM frames_set WHERE id = 1"),
            1
        );
        assert!(
            merge_frame_sets(&ctx, 1, 1).is_err(),
            "self-merge is refused"
        );
        assert!(
            merge_frame_sets(&ctx, 99, 1).is_err(),
            "a missing source is refused"
        );
    }

    /// Splitting one frame out of a three-frame set makes a new custom set
    /// holding that frame, leaves the other two in the source (custom now),
    /// and refuses a split that would empty the source.
    #[test]
    fn split_frame_set_moves_the_selected_frames_into_a_new_custom_set() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            seed_light_at(&conn, 10, "2025-09-13T21:55:00Z");
            seed_light_at(&conn, 11, "2025-09-13T22:10:00Z");
            seed_light_at(&conn, 12, "2025-09-13T22:30:00Z");
            seed_set_with_night(
                &conn,
                1,
                "2025-09-13T21:55:00Z",
                "2025-09-13T22:30:00Z",
                &[10, 11, 12],
            );
        }

        let new_id = split_frame_set(&ctx, 1, SplitSelection::Frames { ids: vec![12] }, "Cut")
            .expect("split");

        let conn = ctx.db.get().unwrap().conn();
        let members_of = |set_id: i64| {
            count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM session_members m JOIN sessions s ON s.id = m.session_id
                     JOIN imaging_nights n ON n.id = s.imaging_night_id WHERE n.frames_set_id = {set_id}"
                ),
            )
        };
        assert_eq!(members_of(new_id), 1);
        assert_eq!(members_of(1), 2);
        assert_eq!(
            count(
                &conn,
                &format!("SELECT is_custom FROM frames_set WHERE id = {new_id}")
            ),
            1
        );
        assert_eq!(count(&conn, "SELECT is_custom FROM frames_set WHERE id = 1"), 1);
        drop(conn);

        assert!(matches!(
            split_frame_set(&ctx, 1, SplitSelection::Frames { ids: vec![10, 11] }, "All"),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            split_frame_set(&ctx, 1, SplitSelection::Frames { ids: vec![] }, "None"),
            Err(ApiError::Invalid(_))
        ));
    }

    /// A custom set built from a frame selection holds exactly those frames in
    /// detected nights; an empty selection is refused.
    #[test]
    fn create_frame_set_from_selection_builds_a_custom_set_with_nights() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            seed_light_at(&conn, 10, "2025-09-13T21:55:00Z");
            seed_light_at(&conn, 11, "2025-09-13T22:10:00Z");
        }

        let set_id = create_frame_set_from_selection(&ctx, "Mine", &[10, 11]).expect("create");

        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(
            count(
                &conn,
                &format!("SELECT is_custom FROM frames_set WHERE id = {set_id}")
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                &format!(
                    "SELECT COUNT(*) FROM session_members m JOIN sessions s ON s.id = m.session_id
                     JOIN imaging_nights n ON n.id = s.imaging_night_id WHERE n.frames_set_id = {set_id}"
                )
            ),
            2
        );
        drop(conn);
        assert!(create_frame_set_from_selection(&ctx, "Empty", &[]).is_err());
    }

    /// (d) An archived set is skipped outright: `Unchanged`, and its
    /// membership is never even read — no night row is created for a set
    /// that has no stored nights to begin with.
    #[test]
    fn reconcile_frame_set_nights_skips_an_archived_set() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO frames_set (id, name, archived_at) VALUES (1, 'X', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            // Not wired into any session — an archived-set reconcile must
            // never read this far.
            seed_light_at(&conn, 10, "2025-10-17T22:04:00Z");
        }

        let summary = reconcile_frame_set_nights(&ctx, 1).unwrap();
        assert!(!summary.changed);
        assert_eq!((summary.nights, summary.sessions), (0, 0));

        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = 1"
            ),
            0
        );
    }

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite, no keychain
    /// involved anywhere). Copied verbatim from `api::collab` / `api::sync`
    /// tests: a TEMPDIR-FILE-backed `Database` (not `:memory:`) so the pool can
    /// hand out multiple connections that all see one database.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;
        use std::sync::{Arc, Mutex, OnceLock};

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            zip_cache: std::sync::Arc::new(crate::zip_store::cache::ZipCache::for_tests()),
        };
        (tmp, ctx)
    }

    /// Seed one cataloged LIGHT frame (files row + frames row). `ra`/`dec` are
    /// `None` for the coordless frame the clustering filter must exclude.
    fn insert_light(
        conn: &Connection,
        id: i64,
        object: &str,
        date_obs: &str,
        ra: Option<f64>,
        dec: Option<f64>,
    ) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2026-01-01T00:00:00Z', 'FITS')",
            params![
                id,
                format!("/tmp/frame_{id}.fits"),
                format!("frame_{id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, object, date_obs, imagetyp, exptime, filter, ra, dec,
              instrume, telescop, focallen, xbinning, ybinning, rotation)
             VALUES (?1, ?1, ?2, ?3, 'Light', 300.0, 'L', ?4, ?5,
                     'TestCam', 'TestScope', 200.0, 1, 1, 0.0)",
            params![id, object, date_obs, ra, dec],
        )
        .unwrap();
    }

    /// Three LIGHTs within the default 3° grouping threshold cluster into ONE
    /// set; a fourth LIGHT with no usable coordinates lands in `excluded_frames`
    /// with the clustering filter's "Invalid coordinates" reason.
    #[test]
    fn auto_generate_clusters_close_lights_and_excludes_coordless() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            insert_light(
                &conn,
                1,
                "M31",
                "2026-03-05T22:10:00+00:00",
                Some(10.00),
                Some(41.00),
            );
            insert_light(
                &conn,
                2,
                "M31",
                "2026-03-05T22:20:00+00:00",
                Some(10.01),
                Some(41.01),
            );
            insert_light(
                &conn,
                3,
                "M31",
                "2026-03-05T22:30:00+00:00",
                Some(10.02),
                Some(41.02),
            );
            insert_light(
                &conn,
                4,
                "NoCoords",
                "2026-03-05T22:40:00+00:00",
                None,
                None,
            );
        }

        let result =
            auto_generate_frame_sets(&ctx, Some(1), None, &NullEmitter).expect("auto-generate");

        assert_eq!(result.sets_created, 1, "three close LIGHTs = one set");
        assert_eq!(result.frames_clustered, 3);
        assert_eq!(result.frames_excluded, 1, "the coordless frame is excluded");
        assert_eq!(result.frames_already_in_sets, 0);
        assert_eq!(result.exclusion_reasons.len(), 1);
        assert!(
            result.exclusion_reasons[0].contains("Invalid coordinates"),
            "unexpected exclusion reason: {}",
            result.exclusion_reasons[0]
        );

        // The excluded frame is persisted, and the three clustered frames really
        // landed in session_members under one frames_set.
        let conn = ctx.db.get().unwrap().conn();
        let excluded: i64 = crate::db::get_excluded_frames_count(&conn).unwrap();
        assert_eq!(excluded, 1);
        let sets: i64 = conn
            .query_row("SELECT COUNT(*) FROM frames_set", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sets, 1);
        let members: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_members", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            members, 3,
            "session detection put all three lights in sessions"
        );
    }

    /// A second run finds every LIGHT already in a set: no new sets, and the
    /// already-in-sets counter reports them.
    #[test]
    fn auto_generate_skips_frames_already_in_sets() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            insert_light(
                &conn,
                1,
                "M31",
                "2026-03-05T22:10:00+00:00",
                Some(10.00),
                Some(41.00),
            );
            insert_light(
                &conn,
                2,
                "M31",
                "2026-03-05T22:20:00+00:00",
                Some(10.01),
                Some(41.01),
            );
        }

        let first = auto_generate_frame_sets(&ctx, Some(1), None, &NullEmitter).expect("first run");
        assert_eq!(first.sets_created, 1);

        let second =
            auto_generate_frame_sets(&ctx, Some(1), None, &NullEmitter).expect("second run");
        assert_eq!(second.sets_created, 0);
        assert_eq!(second.frames_clustered, 0);
        assert_eq!(second.frames_already_in_sets, 2);
    }

    /// Spec §6.1 (F5): flips `calibrated_externally`/`attested_at`, refuses a
    /// zipped set, and refuses a set that does not exist.
    #[test]
    fn attestation_command_flips_the_flag_marks_dirty_and_refuses_a_zipped_set() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            conn.execute("INSERT INTO frames_set (name) VALUES ('S')", [])
                .unwrap();
            conn.last_insert_rowid()
        };
        crate::api::frame_sets::set_frame_set_attestation(&ctx, set_id, true).unwrap();
        #[cfg(all(feature = "render", feature = "solver"))]
        assert!(
            crate::api::collab_autopublish::is_set_dirty_for_test(set_id),
            "set_frame_set_attestation must mark the set dirty for auto-publish"
        );
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert!(crate::db::collab::frames_set_attested(&conn, set_id).unwrap());
            conn.execute(
                "UPDATE frames_set SET is_archived = 1, archived_at = '2026-09-01T00:00:00Z' WHERE id = ?1",
                [set_id],
            )
            .unwrap();
        }
        assert!(matches!(
            crate::api::frame_sets::set_frame_set_attestation(&ctx, set_id, false),
            Err(crate::api::ApiError::Invalid(_))
        ));
        assert!(matches!(
            crate::api::frame_sets::set_frame_set_attestation(&ctx, 4242, true),
            Err(crate::api::ApiError::NotFound(_))
        ));
    }
}
