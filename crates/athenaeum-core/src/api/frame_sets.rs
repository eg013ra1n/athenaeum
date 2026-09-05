//! Frame-set workflows shared by both transports.
//!
//! Single source of truth for `auto_generate_frame_sets`: the Tauri command
//! (`commands/frame_sets.rs`) and the Axum route (`routes/frame_sets.rs`) are
//! thin delegations onto the function below. The body is carried over verbatim
//! from the pre-move Tauri copy — only the error adaptation
//! (`map_err(|e| e.to_string())` → `?` on `anyhow::Result`) and the now-injected
//! `emitter` differ.

use anyhow::{anyhow, Result};

use crate::db::{self};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::sessions::RederiveSummary;
use rusqlite::{Connection, OptionalExtension};

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
fn refresh_frame_set_metadata(conn: &Connection, frames_set_id: i64, is_custom: bool) -> Result<()> {
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

/// Re-derive one frame set's nights and sessions from its member frames and
/// refresh its aggregate metadata — the manual "Recalculate nights" action,
/// which repairs a set an older merge left with one night stored as two rows.
pub fn recalculate_frame_set_nights(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<RederiveSummary> {
    let db = ctx.db.get().ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let gap_hours = ctx.settings.get_session_gap_threshold_hours(&conn).unwrap_or(6.0);
    let tx = conn.transaction()?;
    let is_custom = frame_set_is_custom(&tx, frames_set_id)?;
    let summary = crate::sessions::rederive_for_frame_set(&tx, frames_set_id, &[], gap_hours)?;
    refresh_frame_set_metadata(&tx, frames_set_id, is_custom)?;
    tx.commit()?;
    Ok(summary)
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
    let db = ctx.db.get().ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let gap_hours = ctx.settings.get_session_gap_threshold_hours(&conn).unwrap_or(6.0);
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
        // auto-link — the notification is a suggestion).
        if let (Some(ra_str), Some(dec_str)) = (&metadata.objctra, &metadata.objctdec) {
            if let (Ok(ra), Ok(dec)) = (
                crate::coordinates::parse_ra_sexagesimal(ra_str),
                crate::coordinates::parse_dec_sexagesimal(dec_str),
            ) {
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
        let session_id = db::create_session(conn, night_id, "CamA", ids.len() as i32, None).unwrap();
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
            seed_set_with_night(&conn, 1, "2025-09-13T21:55:00Z", "2025-09-13T23:30:00Z", &[10, 11]);
            seed_set_with_night(&conn, 2, "2025-09-13T22:36:00Z", "2025-09-14T01:59:00Z", &[12, 13]);
        }

        merge_frame_sets(&ctx, 2, 1).unwrap();

        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM imaging_nights WHERE frames_set_id = 1"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM session_members m JOIN sessions s ON s.id = m.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id WHERE n.frames_set_id = 1"
            ),
            4
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames_set WHERE id = 2"), 0);
        assert_eq!(count(&conn, "SELECT is_custom FROM frames_set WHERE id = 1"), 1);
        assert!(merge_frame_sets(&ctx, 1, 1).is_err(), "self-merge is refused");
        assert!(merge_frame_sets(&ctx, 99, 1).is_err(), "a missing source is refused");
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
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
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
}
