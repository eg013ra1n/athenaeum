//! Stage-II collaboration orchestration (slice 3, Task 4): the DB-wiring layer
//! between the catalog and the pure pieces built in Tasks 1–3.
//!
//! - **Linking** — locally attach/detach a frame set to a cached project
//!   (`project_links`; never sent to the hub, spec §7).
//! - **Suggestions** — rank every non-archived frame set by angular distance to
//!   a project's target, flagging within-radius and already-linked sets.
//! - **Gate report** — assemble each linked LIGHT frame's gate inputs from
//!   `frames`/`plate_solves`/`frame_analysis`/`light_calibrations` and run the
//!   pure [`crate::collab::gate`] engine over the union.
//! - **Portal deep-link intent** — record a "publish as project" intent for a
//!   set and build the portal `/new` URL prefilled from its target.
//! - **Match** — cached projects whose target radius contains a point and that
//!   aren't already linked to a set (the Task-6 auto-link hook).
//!
//! Render-gated (`api/mod.rs`) because [`frame_cal_status`] lives in the
//! render-only `api::lights`; the `crate::collab` core module and `db::collab`
//! stay ungated so the headless/perseus `--no-default-features` build compiles.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::api::lights::frame_cal_status;
use crate::api::{db, ApiError};
use crate::collab::gate::{
    evaluate_frame, FrameGateRow, GateFrameInput, ProjectTarget, ThresholdRuleView,
};
use crate::coordinates::{angular_distance, parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::db::analysis::get_frame_analyses_by_ids;
use crate::models::FrameAnalysis;
use crate::services::ServiceContext;

/// Module-local `anyhow::Error → ApiError::Internal` mapper (house style —
/// mirrors the blanket `From<anyhow::Error>` conversion so `.map_err(internal)`
/// reads cleanly at every DB call site).
fn internal(e: anyhow::Error) -> ApiError {
    ApiError::from(e)
}

// ── Response DTOs (BINDING for Tasks 5–6) ────────────────────────────────────

/// One ranked frame-set candidate for linking to a project.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LinkSuggestion {
    pub frames_set_id: i64,
    pub name: Option<String>,
    pub light_count: i64,
    /// Angular distance (deg) from the set center to the project target; `None`
    /// when the set has no parseable center.
    pub distance_deg: Option<f64>,
    pub within_radius: bool,
    pub already_linked: bool,
}

/// A frame set currently linked to a project (Task 5/6 surface).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LinkedSetView {
    pub frames_set_id: i64,
    pub name: Option<String>,
    pub light_count: i64,
    pub distance_deg: Option<f64>,
    pub within_radius: bool,
}

/// The per-frame gate verdict for a project's linked LIGHT frames.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GateReport {
    pub project_id: String,
    pub total: i64,
    pub publishable: i64,
    pub rows: Vec<FrameGateRow>,
}

/// A cached project whose target field contains a queried point (auto-link hook).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSetMatch {
    pub project_id: String,
    pub project_title: String,
    pub project_slug: String,
}

/// The portal `/new` deep link prefilled from a frame set's target.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PortalNewProjectLink {
    pub url: String,
}

// ── Shared internal helpers (also used by Task 5) ────────────────────────────

/// The parsed `(ra_deg, dec_deg)` center of a frame set from its
/// `objctra`/`objctdec` strings, or `None` when either is absent or fails to
/// parse (warn-and-None — an unparseable center never fails a whole listing).
fn set_center(conn: &Connection, frames_set_id: i64) -> Option<(f64, f64)> {
    let coords: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT objctra, objctdec FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((Some(ra_str), Some(dec_str))) = coords else {
        return None;
    };
    match (
        parse_ra_sexagesimal(&ra_str),
        parse_dec_sexagesimal(&dec_str),
    ) {
        (Ok(ra), Ok(dec)) => Some((ra, dec)),
        _ => {
            tracing::warn!(frames_set_id, "frame set center did not parse — treating as no center");
            None
        }
    }
}

/// Count of a frame set's LIGHT members (same membership join as
/// [`union_light_frames`]).
fn light_count(conn: &Connection, frames_set_id: i64) -> anyhow::Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT sm.frame_id) \
         FROM session_members sm \
         JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
         JOIN frames f ON f.id = sm.frame_id \
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'",
        [frames_set_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// The union of LIGHT `(frame_id, filename)` across many frame sets, de-duped by
/// frame id — `api/lights.rs::load_light_members` generalized to
/// `ino.frames_set_id IN (…)` with `SELECT DISTINCT`.
fn union_light_frames(
    conn: &rusqlite::Connection,
    set_ids: &[i64],
) -> anyhow::Result<Vec<(i64, String)>> {
    if set_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; set_ids.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT sm.frame_id, fi.filename \
         FROM session_members sm \
         JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
         JOIN frames f ON f.id = sm.frame_id \
         JOIN files fi ON fi.id = f.file_id \
         WHERE ino.frames_set_id IN ({placeholders}) AND f.imagetyp = 'Light' \
         ORDER BY sm.frame_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(set_ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Raw `frames` columns needed for the gate's center/scale precedence.
struct FrameRow {
    ra: Option<f64>,
    dec: Option<f64>,
    objctra: Option<String>,
    objctdec: Option<String>,
    xpixsz: Option<f64>,
    focallen: Option<f64>,
}

/// Batch-assemble one [`GateFrameInput`] per `(frame_id, filename)` — conn-only,
/// so the self-consistency cal-status policy needs no settings. Reads
/// `plate_solves`, `frames`, and the analyses in three batched queries, then
/// resolves each frame's center (crval → ra/dec → parsed objctra/objctdec) and
/// pixel scale (plate-solve → header `atan(xpixsz/focallen)`, no binning
/// multiply) and its self-consistency cal status.
fn frame_gate_inputs(
    conn: &rusqlite::Connection,
    frames: &[(i64, String)],
) -> anyhow::Result<Vec<GateFrameInput>> {
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = frames.iter().map(|(id, _)| *id).collect();
    let placeholders = vec!["?"; ids.len()].join(",");

    // plate_solves: frame_id → (pixel_scale_arcsec, crval1, crval2)
    let mut solves: HashMap<i64, (f64, f64, f64)> = HashMap::new();
    {
        let sql = format!(
            "SELECT frame_id, pixel_scale_arcsec, crval1, crval2 \
             FROM plate_solves WHERE frame_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        for row in rows {
            let (fid, scale, crval1, crval2) = row?;
            solves.insert(fid, (scale, crval1, crval2));
        }
    }

    // frames: id → FrameRow
    let mut rows_by_id: HashMap<i64, FrameRow> = HashMap::new();
    {
        let sql = format!(
            "SELECT id, ra, dec, objctra, objctdec, xpixsz, focallen \
             FROM frames WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                FrameRow {
                    ra: r.get(1)?,
                    dec: r.get(2)?,
                    objctra: r.get(3)?,
                    objctdec: r.get(4)?,
                    xpixsz: r.get(5)?,
                    focallen: r.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (id, fr) = row?;
            rows_by_id.insert(id, fr);
        }
    }

    // analyses: frame_id → FrameAnalysis (frames without a row simply absent)
    let mut analyses: HashMap<i64, FrameAnalysis> = HashMap::new();
    for a in get_frame_analyses_by_ids(conn, &ids)? {
        analyses.insert(a.frame_id, a);
    }

    let mut out = Vec::with_capacity(frames.len());
    for (frame_id, filename) in frames {
        let solve = solves.get(frame_id);
        let frow = rows_by_id.get(frame_id);

        // Scale precedence: plate-solve pixel scale, else the header fallback
        // atan(xpixsz_mm / focallen_mm) when both present and positive
        // (mirrors plate_solve/hints.rs — a 0.0 xpixsz is a placeholder, not
        // a real pixel size, and must yield "unknown scale", not 0.0).
        let pixel_scale_arcsec = solve.map(|(s, _, _)| *s).or_else(|| {
            frow.and_then(|f| match (f.xpixsz, f.focallen) {
                (Some(xpixsz), Some(focallen)) if focallen > 0.0 && xpixsz > 0.0 => {
                    Some(((xpixsz / 1000.0) / focallen).atan().to_degrees() * 3600.0)
                }
                _ => None,
            })
        });

        // Center precedence: plate-solve crval → frames ra/dec → parsed
        // objctra/objctdec strings.
        let center = solve
            .map(|(_, crval1, crval2)| (*crval1, *crval2))
            .or_else(|| {
                frow.and_then(|f| match (f.ra, f.dec) {
                    (Some(ra), Some(dec)) => Some((ra, dec)),
                    _ => None,
                })
            })
            .or_else(|| {
                frow.and_then(|f| match (&f.objctra, &f.objctdec) {
                    (Some(ra_str), Some(dec_str)) => {
                        match (parse_ra_sexagesimal(ra_str), parse_dec_sexagesimal(dec_str)) {
                            (Ok(ra), Ok(dec)) => Some((ra, dec)),
                            _ => None,
                        }
                    }
                    _ => None,
                })
            });

        let cal_status = frame_cal_status(conn, *frame_id)?;

        out.push(GateFrameInput {
            frame_id: *frame_id,
            filename: filename.clone(),
            center,
            pixel_scale_arcsec,
            cal_status,
            analysis: analyses.get(frame_id).cloned(),
        });
    }
    Ok(out)
}

// ── Linking ──────────────────────────────────────────────────────────────────

/// Link a frame set to a cached project (idempotent). `NotFound` when the
/// project isn't cached or the set doesn't exist.
pub fn link_frame_set(
    ctx: &ServiceContext,
    project_id: &str,
    frames_set_id: i64,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    if crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .is_none()
    {
        return Err(ApiError::NotFound(format!(
            "project {project_id} is not cached — refresh first"
        )));
    }
    let set_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM frames_set WHERE id = ?1)",
            [frames_set_id],
            |r| r.get(0),
        )
        .map_err(|e| internal(e.into()))?;
    if !set_exists {
        return Err(ApiError::NotFound(format!(
            "frame set {frames_set_id} not found"
        )));
    }

    crate::db::collab::link_set(&conn, project_id, frames_set_id).map_err(internal)?;
    tracing::info!(project_id, frames_set_id, "linked frame set to project");
    Ok(())
}

/// Unlink a frame set from a project (idempotent — removing an absent link is a
/// no-op).
pub fn unlink_frame_set(
    ctx: &ServiceContext,
    project_id: &str,
    frames_set_id: i64,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let removed =
        crate::db::collab::unlink_set(&conn, project_id, frames_set_id).map_err(internal)?;
    tracing::info!(project_id, frames_set_id, removed, "unlinked frame set from project");
    Ok(())
}

// ── Suggestions ──────────────────────────────────────────────────────────────

/// Every non-archived frame set ranked for linking to a project: within-radius
/// first, then ascending distance, unparseable-center sets last.
pub fn list_link_suggestions(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<LinkSuggestion>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id} is not cached")))?;

    let mut out = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, name FROM frames_set WHERE is_archived = 0 ORDER BY id DESC")
        .map_err(|e| internal(e.into()))?;
    let sets = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| internal(e.into()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| internal(e.into()))?;

    for (set_id, name) in sets {
        let center = set_center(&conn, set_id);
        let distance_deg = center.map(|(ra, dec)| {
            angular_distance(ra, dec, project.target_ra_deg, project.target_dec_deg)
        });
        out.push(LinkSuggestion {
            frames_set_id: set_id,
            name,
            light_count: light_count(&conn, set_id).map_err(internal)?,
            within_radius: distance_deg
                .map(|d| d <= project.target_radius_deg)
                .unwrap_or(false),
            already_linked: crate::db::collab::is_set_linked(&conn, project_id, set_id)
                .map_err(internal)?,
            distance_deg,
        });
    }
    out.sort_by(|a, b| {
        b.within_radius.cmp(&a.within_radius).then_with(|| {
            a.distance_deg
                .unwrap_or(f64::MAX)
                .partial_cmp(&b.distance_deg.unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(out)
}

// ── Gate report ──────────────────────────────────────────────────────────────

/// Run the quality gate over the union of LIGHT frames across a project's linked
/// sets (dedup by frame id). `NotFound` when the project isn't cached — the
/// caller must refresh first.
pub fn evaluate_project_gate(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<GateReport, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .ok_or_else(|| {
            ApiError::NotFound(format!("project {project_id} is not cached — refresh first"))
        })?;

    let target = ProjectTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let rules: Vec<ThresholdRuleView> = match &project.thresholds_rules_json {
        Some(json) => serde_json::from_str(json)
            .map_err(|e| {
                tracing::warn!(project_id, error = %e, "cached threshold rules do not parse — gating on preconditions only");
                e
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };

    let set_ids = crate::db::collab::linked_set_ids(&conn, project_id).map_err(internal)?;
    let frames = union_light_frames(&conn, &set_ids).map_err(internal)?;
    let inputs = frame_gate_inputs(&conn, &frames).map_err(internal)?;

    let rows: Vec<_> = inputs
        .iter()
        .map(|i| evaluate_frame(i, &target, &rules))
        .collect();
    let publishable = rows.iter().filter(|r| r.publishable).count() as i64;
    tracing::info!(
        project_id,
        total = rows.len() as i64,
        publishable,
        "evaluated project gate"
    );
    Ok(GateReport {
        project_id: project_id.to_string(),
        total: rows.len() as i64,
        publishable,
        rows,
    })
}

// ── Portal deep-link intent ──────────────────────────────────────────────────

/// Record a "publish as project" intent for a frame set and build the portal
/// `/new` deep link prefilled from the set's target. `Invalid` when the set has
/// no usable center coordinates; `NotFound` when the set doesn't exist.
pub fn record_project_link_intent(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<PortalNewProjectLink, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let (name, objctra, objctdec): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT name, objctra, objctdec FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| internal(e.into()))?
        .ok_or_else(|| ApiError::NotFound(format!("frame set {frames_set_id} not found")))?;
    let (Some(ra_str), Some(dec_str)) = (objctra, objctdec) else {
        return Err(ApiError::Invalid(
            "the set has no usable center coordinates".into(),
        ));
    };
    let (Ok(ra), Ok(dec)) = (
        parse_ra_sexagesimal(&ra_str),
        parse_dec_sexagesimal(&dec_str),
    ) else {
        return Err(ApiError::Invalid(
            "the set has no usable center coordinates".into(),
        ));
    };

    crate::db::collab::add_link_intent(&conn, frames_set_id, ra, dec).map_err(internal)?;

    let hub_url = ctx.settings.get_with_precedence(
        &conn,
        crate::settings::keys::ACCOUNT_HUB_URL,
        crate::settings::defaults::ACCOUNT_HUB_URL,
    )?;
    let name = name.unwrap_or_default();
    let mut url = reqwest::Url::parse(&hub_url)
        .map_err(|e| ApiError::Internal(format!("invalid hub url {hub_url}: {e}")))?;
    url.set_path("/new");
    url.query_pairs_mut()
        .append_pair("object", &name)
        .append_pair("ra", &format!("{ra:.4}"))
        .append_pair("dec", &format!("{dec:.4}"))
        .append_pair("radius", "1.5");

    tracing::info!(frames_set_id, "recorded project link intent");
    Ok(PortalNewProjectLink {
        url: url.to_string(),
    })
}

// ── Match (Task-6 auto-link hook) ────────────────────────────────────────────

/// Cached projects whose target radius contains `(ra_deg, dec_deg)` AND that
/// aren't already linked to `frames_set_id`. Plain `anyhow` + a bare `conn` so
/// both thin transport layers can call it cheaply.
pub fn find_matching_projects(
    conn: &rusqlite::Connection,
    ra_deg: f64,
    dec_deg: f64,
    frames_set_id: i64,
) -> anyhow::Result<Vec<ProjectSetMatch>> {
    let mut out = Vec::new();
    for p in crate::db::collab::list_projects(conn)? {
        let d = angular_distance(ra_deg, dec_deg, p.target_ra_deg, p.target_dec_deg);
        if d <= p.target_radius_deg
            && !crate::db::collab::is_set_linked(conn, &p.project_id, frames_set_id)?
        {
            out.push(ProjectSetMatch {
                project_id: p.project_id,
                project_title: p.title,
                project_slug: p.slug,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::CollabProjectRow;

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite, no keychain
    /// involved anywhere). Copied verbatim from `api::sync` / `api::masters`
    /// tests: a TEMPDIR-FILE-backed `Database` (not `:memory:`) so the pool can
    /// hand out multiple connections that all see one database.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

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
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
        };
        (tmp, ctx)
    }

    /// Cached project fixture: target M101 (210.8, +54.35), radius 1.5°, one
    /// threshold rule (reject trailed frames).
    fn cached_project(conn: &rusqlite::Connection) {
        crate::db::collab::upsert_project(
            conn,
            &CollabProjectRow {
                project_id: "p-1".into(),
                slug: "m101".into(),
                title: "M 101".into(),
                data_role: "send_receive".into(),
                is_coordinator: true,
                require_approval: false,
                pending_announcements: 0,
                project_status: "active".into(),
                target_name: "M101".into(),
                target_ra_deg: 210.8,
                target_dec_deg: 54.35,
                target_radius_deg: 1.5,
                membership_version: 1,
                snapshot_payload_b64: "e30=".into(),
                snapshot_signature_b64: "e30=".into(),
                members_json: "[]".into(),
                thresholds_version: Some(1),
                thresholds_rules_json: Some(
                    r#"[{"metricKey":"not_trailed","op":"reject_if","value":true}]"#.into(),
                ),
                fetched_at: String::new(), // filled by SQL
            },
        )
        .unwrap();
    }

    /// A frames_set whose center is (`objctra`, `objctdec`), holding `lights`
    /// LIGHT frames through the imaging_nights → sessions → session_members
    /// chain. Every frame gets a frame_analysis row; frame index 1 (the second)
    /// is flagged trailed. Returns (set_id, frame_ids).
    fn seed_set(
        conn: &rusqlite::Connection,
        name: &str,
        objctra: &str,
        objctdec: &str,
        ra_deg: f64,
        dec_deg: f64,
        lights: usize,
    ) -> (i64, Vec<i64>) {
        conn.execute(
            "INSERT INTO frames_set (name, objctra, objctdec) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, objctra, objctdec],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
             VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
            [set_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'ASI2600MM')",
            [night_id],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        let mut frame_ids = Vec::new();
        for i in 0..lights {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS')",
                rusqlite::params![
                    format!("/data/{name}/L_{i:04}.fits"),
                    format!("L_{i:04}.fits")
                ],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();

            // xpixsz (µm, already binned) + focallen (mm) give the header
            // pixel-scale fallback: (3.76/1000 / 1000).atan() ≈ 0.776″/px.
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, exptime, filter) \
                 VALUES (?1, 'Light', 'M101', 'ASI2600MM', ?2, ?3, 3.76, 1000.0, 300.0, 'L')",
                rusqlite::params![file_id, ra_deg, dec_deg],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, frame_id],
            )
            .unwrap();

            // Every NOT NULL column of frame_analysis must be provided.
            conn.execute(
                "INSERT INTO frame_analysis \
                 (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity, median_snr, \
                  median_hfr, frame_snr, snr_weight, psf_signal, background, noise, \
                  detection_threshold, width, height, source_channels, trail_r_squared, possibly_trailed) \
                 VALUES (?1, ?2, 400, 2.0, 0.4, 10.0, 2.0, 10.0, 1.0, 100.0, 10.0, 1.0, 5.0, \
                         6248, 4176, 1, 0.0, ?3)",
                rusqlite::params![frame_id, file_id, if i == 1 { 1 } else { 0 }],
            )
            .unwrap();

            frame_ids.push(frame_id);
        }
        (set_id, frame_ids)
    }

    #[test]
    fn gate_report_covers_union_of_linked_sets() {
        let (_tmp, ctx) = test_ctx(); // (TempDir, ServiceContext) — see the note below
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 2)
        };

        // An uncached project id is a NotFound, not a panic.
        assert!(matches!(
            evaluate_project_gate(&ctx, "nope"),
            Err(crate::api::ApiError::NotFound(_))
        ));

        // Nothing linked yet → no candidates.
        assert_eq!(evaluate_project_gate(&ctx, "p-1").unwrap().total, 0);

        link_frame_set(&ctx, "p-1", set_id).unwrap();
        link_frame_set(&ctx, "p-1", set_id).unwrap(); // idempotent

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 2, "both LIGHT frames are candidates");
        assert_eq!(report.rows.len(), 2);
        assert_eq!(report.publishable, 0, "no light_calibrations rows → not calibrated");

        // Frame 0: blocked only by calibration. Frame 1: calibration + trailed.
        let row0 = report.rows.iter().find(|r| r.frame_id == frames[0]).unwrap();
        assert!(row0.failures.iter().any(|f| f.contains("not calibrated")), "{:?}", row0.failures);
        assert!(!row0.failures.iter().any(|f| f.contains("trailed")));
        assert_eq!(row0.stars_detected, Some(400));
        // 2.0 px × ~0.776 ″/px (header fallback, no binning multiply).
        let scale = ((3.76f64 / 1000.0) / 1000.0).atan().to_degrees() * 3600.0;
        assert!((row0.fwhm_arcsec.unwrap() - 2.0 * scale).abs() < 1e-6);

        let row1 = report.rows.iter().find(|r| r.frame_id == frames[1]).unwrap();
        assert!(row1.failures.iter().any(|f| f.contains("trailed")), "{:?}", row1.failures);

        unlink_frame_set(&ctx, "p-1", set_id).unwrap();
        assert_eq!(evaluate_project_gate(&ctx, "p-1").unwrap().total, 0);
    }

    #[test]
    fn suggestions_rank_by_distance_and_flag_linked() {
        let (_tmp, ctx) = test_ctx();
        let (near, far) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let (near, _) = seed_set(&conn, "On target", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            // ~5° south of the target — outside the 1.5° radius.
            let (far, _) = seed_set(&conn, "Far away", "14:03:12", "+49:21:00", 210.8, 49.35, 1);
            (near, far)
        };

        let suggestions = list_link_suggestions(&ctx, "p-1").unwrap();
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].frames_set_id, near, "within-radius set ranks first");
        assert!(suggestions[0].within_radius);
        assert_eq!(suggestions[0].light_count, 1);
        assert!(!suggestions[0].already_linked);

        assert_eq!(suggestions[1].frames_set_id, far);
        assert!(!suggestions[1].within_radius);
        assert!(suggestions[1].distance_deg.unwrap() > 4.0);

        link_frame_set(&ctx, "p-1", near).unwrap();
        let suggestions = list_link_suggestions(&ctx, "p-1").unwrap();
        assert!(suggestions[0].already_linked);
    }

    #[test]
    fn intent_builds_portal_url_and_persists() {
        let (_tmp, ctx) = test_ctx();
        let (with_center, no_center) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let (with_center, _) =
                seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            conn.execute("INSERT INTO frames_set (name) VALUES ('No center')", [])
                .unwrap();
            (with_center, conn.last_insert_rowid())
        };

        let link = record_project_link_intent(&ctx, with_center).unwrap();
        assert!(link.url.contains("/new?"), "portal deep link: {}", link.url);
        assert!(link.url.contains("object=M101+Set") || link.url.contains("object=M101%20Set"));
        assert!(link.url.contains("ra=210.8"));
        assert!(link.url.starts_with("http"), "must be a plain web URL");

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let intents = crate::db::collab::list_link_intents(&conn).unwrap();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].1, with_center);
        }

        assert!(matches!(
            record_project_link_intent(&ctx, no_center),
            Err(crate::api::ApiError::Invalid(_))
        ));
    }

    #[test]
    fn find_matching_projects_excludes_linked() {
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        cached_project(&conn);
        let (set_id, _) = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);

        let matches = find_matching_projects(&conn, 210.8, 54.35, set_id).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].project_id, "p-1");

        // A point far outside the radius matches nothing.
        assert!(find_matching_projects(&conn, 10.0, 10.0, set_id).unwrap().is_empty());

        // Once linked, the project stops being suggested for that set.
        crate::db::collab::link_set(&conn, "p-1", set_id).unwrap();
        assert!(find_matching_projects(&conn, 210.8, 54.35, set_id).unwrap().is_empty());
    }
}
