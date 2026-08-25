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
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
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
