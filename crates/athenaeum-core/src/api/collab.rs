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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::collab_exchange::ensure_collab_sender_engine;
use crate::api::lights::frame_cal_status;
use crate::api::{db, ApiError};
use crate::collab::gate::{
    evaluate_frame, FrameGateRow, GateFrameInput, ProjectTarget, ThresholdRuleView,
};
use crate::collab::hub_client::{AnnounceRequest, CollabClient};
use crate::collab::snapshot::SnapshotMember;
use crate::coordinates::{angular_distance, parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::db::analysis::get_frame_analyses_by_ids;
use crate::db::collab::CollabProjectRow;
use crate::db::collab_exchange::{
    contributions_for_package, delete_package, get_package_by_announcement, list_packages,
    mark_superseded, own_active_announcement_ids_for_uuids, set_local_status, upsert_package,
    PackageRow,
};
use crate::db::light_calibrations::get_light_calibration_for_frame;
use crate::events::ProgressEmitter;
use crate::fits_writer::{stamp_extra_card, Card, CardValue};
use crate::models::FrameAnalysis;
use crate::package::{
    write_package, xxh3_full_file, ManifestRecord, PayloadKind, ProjectStamp, MANIFEST_FILENAME,
    MANIFEST_VERSION,
};
use crate::services::ServiceContext;
use crate::sharing::types::NodeId;

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

/// The `project-set-match` event payload: a newly-generated frame set whose
/// center falls inside one or more of my cached projects' target radii (Task-6
/// set-match hook; a suggestion only — never auto-links).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSetMatchEvent {
    pub frames_set_id: i64,
    pub set_name: Option<String>,
    pub matches: Vec<ProjectSetMatch>,
}

/// The portal `/new` deep link prefilled from a frame set's target.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PortalNewProjectLink {
    pub url: String,
}

/// A cached project rendered as a list card (`list_projects` / `refresh_projects`).
/// The three trailing counts are live: `linked_sets` from `project_links`,
/// `candidates`/`publishable` from the gate over the linked sets' LIGHT frames.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCard {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub data_role: String,
    pub coordinator: bool,
    pub require_approval: bool,
    pub pending_announcements: i64,
    pub project_status: String,
    pub target_name: String,
    pub target_ra_deg: f64,
    pub target_dec_deg: f64,
    pub target_radius_deg: f64,
    pub membership_version: i64,
    pub linked_sets: i64,
    pub candidates: i64,
    pub publishable: i64,
    pub fetched_at: String,
}

/// One verified snapshot member, projected for the project-detail view. Also
/// `Deserialize`: it is parsed back out of the cache row's `members_json` (a
/// serialized `Vec<SnapshotMember>`), whose extra `accountId`/`nodes` fields
/// serde ignores.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMemberView {
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
}

/// The full project-detail payload: the card plus the verified member list,
/// threshold registry, currently-linked sets, and the portal base URL (for the
/// frontend to build project links).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub card: ProjectCard,
    pub members: Vec<ProjectMemberView>,
    pub thresholds_version: Option<i32>,
    pub thresholds: Vec<ThresholdRuleView>,
    pub links: Vec<LinkedSetView>,
    pub portal_base: String,
}

// ── Shared internal helpers (also used by Task 5) ────────────────────────────

/// The parsed `(ra_deg, dec_deg)` center of a frame set from its
/// `objctra`/`objctdec` strings, or `None` when either is absent or fails to
/// parse (warn-and-None — an unparseable center never fails a whole listing).
fn set_center(conn: &Connection, frames_set_id: i64) -> Option<(f64, f64)> {
    let coords: Option<(Option<String>, Option<String>)> = match conn
        .query_row(
            "SELECT objctra, objctdec FROM frames_set WHERE id = ?1",
            [frames_set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
    {
        Ok(coords) => coords,
        Err(e) => {
            // Listing flows keep degrading gracefully (None), but a genuine DB
            // error must never be silent.
            tracing::warn!(frames_set_id, error = %e, "querying frame set center failed — treating as no center");
            return None;
        }
    };
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
        // objctra/objctdec strings. A real solve's crval is authoritative and
        // never sentinel-checked; but a header (0.0, 0.0) in frames.ra/dec is
        // the FITS "not actually set" placeholder (see plate_solve/hints.rs
        // is_sentinel_position) — treat it as unset and fall through to the
        // objctra/objctdec parse.
        let center = solve
            .map(|(_, crval1, crval2)| (*crval1, *crval2))
            .or_else(|| {
                frow.and_then(|f| match (f.ra, f.dec) {
                    (Some(ra), Some(dec)) if ra.abs() >= 1e-6 || dec.abs() >= 1e-6 => {
                        Some((ra, dec))
                    }
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

// ── Hub poll: cards, detail, refresh ─────────────────────────────────────────

/// `AccountClientError → ApiError`, a local copy of the private
/// `api::account::map_client_err` (keep the two in sync). A `401` surfaces as
/// [`ApiError::SignedOut`] so the frontend re-shows the sign-in flow.
fn client_err(e: crate::account::AccountClientError) -> ApiError {
    use crate::account::AccountClientError as E;
    match e {
        E::RateLimited => {
            ApiError::Invalid("Too many requests — wait a minute and try again.".into())
        }
        E::Unauthorized => {
            ApiError::SignedOut("Signed out or device revoked — sign in again.".into())
        }
        E::SecondPrimary(m) | E::DeviceConflict(m) => ApiError::Conflict(m),
        E::PeerValidation(m) | E::BadRequest(m) => ApiError::Invalid(m),
        E::DuplicateName => ApiError::Invalid("name already in use".into()),
        E::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// Host of a hub URL, for scoping the per-hub TOFU pin setting key. Mirrors the
/// private `api::account::hub_host`; falls back to the raw string if it does not
/// parse.
fn hub_host_of(hub_url: &str) -> String {
    reqwest::Url::parse(hub_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| hub_url.to_string())
}

/// Build a [`ProjectCard`] from a cached row, computing the live counts.
/// `card_from_row` never holds a DB connection across the gate call.
fn card_from_row(ctx: &ServiceContext, row: CollabProjectRow) -> Result<ProjectCard, ApiError> {
    let linked_sets = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::linked_set_ids(&conn, &row.project_id)
            .map_err(internal)?
            .len() as i64
    };
    let gate = evaluate_project_gate(ctx, &row.project_id)?;
    Ok(ProjectCard {
        project_id: row.project_id,
        slug: row.slug,
        title: row.title,
        data_role: row.data_role,
        coordinator: row.is_coordinator,
        require_approval: row.require_approval,
        pending_announcements: row.pending_announcements,
        project_status: row.project_status,
        target_name: row.target_name,
        target_ra_deg: row.target_ra_deg,
        target_dec_deg: row.target_dec_deg,
        target_radius_deg: row.target_radius_deg,
        membership_version: row.membership_version,
        linked_sets,
        candidates: gate.total,
        publishable: gate.publishable,
        fetched_at: row.fetched_at,
    })
}

/// Every cached project as a card (instant — cache only, no hub I/O).
pub fn list_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError> {
    let rows = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn).map_err(internal)?
    };
    rows.into_iter().map(|row| card_from_row(ctx, row)).collect()
}

/// A [`LinkedSetView`] per frame set currently linked to the project.
fn linked_set_views(
    conn: &Connection,
    project_id: &str,
    row: &CollabProjectRow,
) -> Result<Vec<LinkedSetView>, ApiError> {
    let mut out = Vec::new();
    for set_id in crate::db::collab::linked_set_ids(conn, project_id).map_err(internal)? {
        let name: Option<String> = conn
            .query_row("SELECT name FROM frames_set WHERE id = ?1", [set_id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| internal(e.into()))?
            .flatten();
        let center = set_center(conn, set_id);
        let distance_deg =
            center.map(|(ra, dec)| angular_distance(ra, dec, row.target_ra_deg, row.target_dec_deg));
        out.push(LinkedSetView {
            frames_set_id: set_id,
            name,
            light_count: light_count(conn, set_id).map_err(internal)?,
            distance_deg,
            within_radius: distance_deg
                .map(|d| d <= row.target_radius_deg)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// The full detail view for one cached project. `NotFound` when the project
/// isn't cached — the caller must [`refresh_projects`] first.
pub fn get_project_detail(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<ProjectDetail, ApiError> {
    let (row, links, portal_base) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let row = crate::db::collab::get_project(&conn, project_id)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("project {project_id} is not cached — refresh first"))
            })?;
        let links = linked_set_views(&conn, project_id, &row)?;
        let portal_base = ctx.settings.get_with_precedence(
            &conn,
            crate::settings::keys::ACCOUNT_HUB_URL,
            crate::settings::defaults::ACCOUNT_HUB_URL,
        )?;
        (row, links, portal_base)
    };

    let members: Vec<ProjectMemberView> = serde_json::from_str(&row.members_json)
        .map_err(|e| {
            tracing::warn!(project_id, error = %e, "cached members_json did not parse — showing an empty member list");
            e
        })
        .unwrap_or_default();
    let thresholds: Vec<ThresholdRuleView> = match &row.thresholds_rules_json {
        Some(json) => serde_json::from_str(json)
            .map_err(|e| {
                tracing::warn!(project_id, error = %e, "cached threshold rules do not parse — showing no rules");
                e
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let thresholds_version = row.thresholds_version;
    let card = card_from_row(ctx, row)?;

    Ok(ProjectDetail {
        card,
        members,
        thresholds_version,
        thresholds,
        links,
        portal_base,
    })
}

/// A per-project fetch failure, split so the refresh loop can log a snapshot
/// verification failure (a security-relevant event) at `error!` while every
/// other transport/decode failure logs at `warn!`. Either way the stale cache
/// row is kept (never swallowed — the caller always logs).
enum FetchError {
    /// The signed membership snapshot did not verify against the pinned hub key
    /// (pin mismatch, bad signature, tampered/unparseable payload).
    Verify(anyhow::Error),
    /// Any other per-project failure (network, unexpected status, decode).
    Transport(anyhow::Error),
}

/// Assemble one fresh cache row from `project_page` + `membership_snapshot`
/// (verified against the pinned key) + `thresholds`. The row stores BOTH the raw
/// transported snapshot `payload`/`signature` (so slice-4's `PeerAuthorizer` can
/// re-verify offline) AND the re-serialized verified member list.
///
/// The verified snapshot's `projectId` is cross-checked against `p.id`: a
/// correctly-signed snapshot for a DIFFERENT project is a binding violation and
/// is rejected via [`FetchError::Verify`] (the stale cache row is kept), so a
/// snapshot can never be cached under the wrong project.
async fn fetch_one_project(
    client: &crate::collab::hub_client::CollabClient,
    token: &str,
    pinned: &str,
    p: &crate::collab::hub_client::MyProjectWire,
) -> Result<CollabProjectRow, FetchError> {
    let page = client
        .project_page(&p.id)
        .await
        .map_err(|e| FetchError::Transport(e.into()))?;
    let snapshot_wire = client
        .membership_snapshot(token, &p.id)
        .await
        .map_err(|e| FetchError::Transport(e.into()))?;
    let verified = crate::collab::snapshot::verify_and_parse(&snapshot_wire, pinned)
        .map_err(FetchError::Verify)?;
    if verified.project_id != p.id {
        return Err(FetchError::Verify(anyhow::anyhow!(
            "snapshot is signed for project {} but was fetched for project {}",
            verified.project_id,
            p.id
        )));
    }
    let thresholds = client
        .thresholds(token, &p.id)
        .await
        .map_err(|e| FetchError::Transport(e.into()))?;

    let members_json =
        serde_json::to_string(&verified.members).map_err(|e| FetchError::Transport(e.into()))?;
    let (thresholds_version, thresholds_rules_json) = match thresholds.current {
        Some(set) => {
            let rules = serde_json::to_string(&set.rules).map_err(|e| FetchError::Transport(e.into()))?;
            (Some(set.version), Some(rules))
        }
        None => (None, None),
    };

    Ok(CollabProjectRow {
        project_id: p.id.clone(),
        slug: p.slug.clone(),
        title: p.title.clone(),
        data_role: p.data_role.clone(),
        is_coordinator: p.coordinator,
        // The signed snapshot is the single source of truth for both membership
        // fields (they travel together in the signed payload; slice-4 enforces
        // the signed `require_approval`).
        require_approval: verified.require_approval,
        pending_announcements: p.pending_announcements,
        project_status: page.project.status,
        target_name: page.project.target.name,
        target_ra_deg: page.project.target.ra_deg,
        target_dec_deg: page.project.target.dec_deg,
        target_radius_deg: page.project.target.radius_deg,
        membership_version: verified.membership_version,
        snapshot_payload_b64: snapshot_wire.payload,
        snapshot_signature_b64: snapshot_wire.signature,
        members_json,
        thresholds_version,
        thresholds_rules_json,
        fetched_at: String::new(), // filled by SQL
    })
}

/// TOFU: return the pinned snapshot pubkey for this hub host. First call fetches
/// the hub's key and stores it under `collab.snapshot_pubkey.<host>` (`info!`);
/// later calls return the stored pin unchanged. A subsequent `verify_and_parse`
/// mismatch against this pin is handled per-project by the refresh loop.
async fn pinned_pubkey(
    ctx: &ServiceContext,
    hub_url: &str,
    client: &crate::collab::hub_client::CollabClient,
) -> Result<String, ApiError> {
    let host = hub_host_of(hub_url);
    let key = format!("collab.snapshot_pubkey.{host}");

    let existing = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::get_setting(&conn, &key).map_err(|e| internal(e.into()))?
    };
    if let Some(pin) = existing.filter(|s| !s.is_empty()) {
        return Ok(pin);
    }

    let fetched = client.collab_pubkey().await.map_err(client_err)?;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::set_setting(&conn, &key, &fetched).map_err(|e| internal(e.into()))?;
    }
    tracing::info!(hub_host = %host, "pinned hub snapshot pubkey (TOFU)");
    Ok(fetched)
}

/// Poll the hub for every project I'm a member of and refresh the local cache.
///
/// - Signed out → [`ApiError::SignedOut`].
/// - TOFU-pins the hub's snapshot pubkey per host (see [`pinned_pubkey`]).
/// - Per-project isolation: a failed fetch keeps the stale cache row and
///   continues (`warn!`, or `error!` for a snapshot verification failure).
/// - Prunes cache rows for projects no longer returned by the hub (a failed
///   fetch still counts as "still mine" and is kept).
/// - Auto-links any pending portal deep-link intent whose target matches a
///   project that appeared for the FIRST time this refresh (≤ 0.1°).
///
/// Returns the refreshed cards (== [`list_projects`]).
pub async fn refresh_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to use collaboration projects.".into(),
        ));
    };
    let client = crate::collab::hub_client::CollabClient::new(&hub_url).map_err(client_err)?;

    let pinned = pinned_pubkey(ctx, &hub_url, &client).await?;
    let mine = client.my_projects(&token).await.map_err(client_err)?;

    let previous_ids: std::collections::HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn)
            .map_err(internal)?
            .into_iter()
            .map(|p| p.project_id)
            .collect()
    };

    // `keep` = every project the hub still lists (whether or not its fetch
    // succeeded); `new_targets` = only those that appeared this refresh, for the
    // intent auto-link below.
    let mut keep: Vec<String> = Vec::with_capacity(mine.len());
    let mut new_targets: Vec<(String, f64, f64)> = Vec::new();
    for p in &mine {
        keep.push(p.id.clone());
        match fetch_one_project(&client, &token, &pinned, p).await {
            Ok(row) => {
                if !previous_ids.contains(&row.project_id) {
                    new_targets.push((row.project_id.clone(), row.target_ra_deg, row.target_dec_deg));
                }
                let db = db(ctx)?;
                let conn = db.conn();
                crate::db::collab::upsert_project(&conn, &row).map_err(internal)?;
            }
            Err(FetchError::Verify(err)) => {
                tracing::error!(
                    project_id = %p.id,
                    error = %format!("{err:#}"),
                    "membership snapshot failed verification against the pinned hub key — keeping cached state"
                );
            }
            Err(FetchError::Transport(err)) => {
                tracing::warn!(
                    project_id = %p.id,
                    error = %format!("{err:#}"),
                    "project refresh failed — keeping cached state"
                );
            }
        }
    }

    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::prune_projects_not_in(&conn, &keep).map_err(internal)?;
        // Expire stale intents first: a "publish as project" intent that never
        // matched a new project must not silently auto-link an unrelated project
        // that appears weeks later.
        let expired = crate::db::collab::delete_intents_older_than(&conn, 7).map_err(internal)?;
        if expired > 0 {
            tracing::info!(count = expired, "expired stale portal link intents");
        }
        // Auto-link deep-link intents against projects that appeared this refresh.
        for (intent_id, set_id, ra, dec) in
            crate::db::collab::list_link_intents(&conn).map_err(internal)?
        {
            if let Some((project_id, ..)) = new_targets
                .iter()
                .find(|(_, tra, tdec)| angular_distance(ra, dec, *tra, *tdec) <= 0.1)
            {
                crate::db::collab::link_set(&conn, project_id, set_id).map_err(internal)?;
                crate::db::collab::delete_link_intent(&conn, intent_id).map_err(internal)?;
                tracing::info!(%project_id, frames_set_id = set_id, "auto-linked source set from portal deep-link intent");
            }
        }
    }

    list_projects(ctx)
}

// ── Publish (Task 7): stamped build, hub announce, push-seed ─────────────────

/// The outcome of publishing a project's gate-passing calibrated lights
/// (BINDING for Tasks 11–12).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    /// The HUB package uuid (minted first, stamped into every record, announced).
    pub package_id: String,
    /// The hub's announcement id (`{id}` from the announce reply).
    pub announcement_id: String,
    /// `pending` (require_approval) | `published`.
    pub state: String,
    pub frame_count: i64,
    pub byte_size: i64,
    /// Announcement ids this publish superseded (Д9); also flipped `superseded=1`
    /// locally.
    pub superseded_announcements: Vec<String>,
    /// Display name of the chosen first-replica member; `None` when no
    /// receive-capable member is available to seed (still announced).
    pub seed_target: Option<String>,
}

/// One publishable frame carried through the stamping/manifest build.
struct PublishFrame {
    frame_id: i64,
    /// The calibrated-light artifact on disk (`light_calibrations.output_path`).
    output_path: String,
    /// The light-calibration engine version stamped into the record's project stamp.
    engine_version: i64,
    frame: crate::models::Frame,
    analysis: Option<serde_json::Value>,
    fwhm_arcsec: Option<f64>,
    eccentricity: Option<f64>,
}

/// Linear-interpolated percentile of a pre-sorted slice (`p` in 0..=100). Matches
/// the median for `p == 50` (average of the two middle values on even n).
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let rank = (p / 100.0) * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * frac)
}

/// Sort a value list ascending (NaN-safe: NaNs sink to the end but never panic).
fn sorted_f64(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// The base64 `nodes[]` of one snapshot member decoded to raw 32-byte node ids
/// (malformed entries skipped). Mirrors [`crate::collab::authz`]'s decode.
fn member_node_ids(member: &SnapshotMember) -> Vec<NodeId> {
    member
        .nodes
        .iter()
        .filter_map(|entry| {
            let bytes = B64.decode(entry).ok()?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        })
        .collect()
}

/// The publisher's own display name from the cached snapshot (the member owning
/// `own_node`), or empty when it can't be resolved.
fn own_display_name(members: &[SnapshotMember], own_node: &NodeId) -> String {
    members
        .iter()
        .find(|m| member_node_ids(m).iter().any(|n| n == own_node))
        .map(|m| m.display_name.clone())
        .unwrap_or_default()
}

/// Push-seed target selection (Д8): the FIRST eligible member's first node,
/// excluding our own. Candidate roles are the coordinator when the announcement
/// is `pending` (only they can decide it), else every `send_receive` member.
fn select_seed_target(
    members: &[SnapshotMember],
    state: &str,
    own_node: &NodeId,
) -> Option<(NodeId, String)> {
    let pending = state == "pending";
    for m in members {
        let eligible = if pending {
            m.coordinator
        } else {
            m.data_role == "send_receive"
        };
        if !eligible {
            continue;
        }
        for node in member_node_ids(m) {
            if &node != own_node {
                return Some((node, m.display_name.clone()));
            }
        }
    }
    None
}

/// Publish a project's gate-passing calibrated lights: build a stamped package,
/// announce it to the hub (anchored on the pre-minted hub uuid), record it
/// locally with Д9 supersedes, and push-seed the first receive-capable member.
///
/// Render-gated (it consumes [`evaluate_project_gate`] + `db::light_calibrations`),
/// so the headless `--no-default-features` build never compiles it.
pub async fn publish_collab_frames(
    ctx: &ServiceContext,
    collab_sender: &crate::sync::SyncSenderRuntime,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    // ── 1. Gate → publishable frames, resolve each artifact on disk ──────────
    let gate = evaluate_project_gate(ctx, project_id)?;
    let publishable: Vec<&FrameGateRow> = gate.rows.iter().filter(|r| r.publishable).collect();
    if publishable.is_empty() {
        return Err(ApiError::Invalid("no publishable frames".into()));
    }

    let (mut frames, project, skipped_missing) = {
        let db = db(ctx)?;
        let conn = db.conn();

        let project = crate::db::collab::get_project(&conn, project_id)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("project {project_id} is not cached — refresh first"))
            })?;

        let ids: Vec<i64> = publishable.iter().map(|r| r.frame_id).collect();
        let rows = crate::db::get_frames_with_files_by_ids(&conn, &ids)
            .map_err(|e| internal(e.into()))?;
        let frame_by_id: HashMap<i64, crate::models::Frame> =
            rows.into_iter().map(|(_, _, fr)| (fr.id.unwrap_or_default(), fr)).collect();
        let analysis_by_id: HashMap<i64, FrameAnalysis> = get_frame_analyses_by_ids(&conn, &ids)
            .map_err(|e| internal(e.into()))?
            .into_iter()
            .map(|a| (a.frame_id, a))
            .collect();

        let mut frames: Vec<PublishFrame> = Vec::with_capacity(publishable.len());
        let mut skipped: Vec<i64> = Vec::new();
        for row in &publishable {
            let Some(cal) = get_light_calibration_for_frame(&conn, row.frame_id).map_err(internal)?
            else {
                tracing::warn!(frame_id = row.frame_id, "publish: no light-calibration row — skipping");
                skipped.push(row.frame_id);
                continue;
            };
            if !Path::new(&cal.output_path).exists() {
                tracing::warn!(
                    frame_id = row.frame_id,
                    path = %cal.output_path,
                    "publish: calibrated artifact missing on disk — skipping"
                );
                skipped.push(row.frame_id);
                continue;
            }
            let Some(frame) = frame_by_id.get(&row.frame_id).cloned() else {
                tracing::warn!(frame_id = row.frame_id, "publish: frame row vanished — skipping");
                skipped.push(row.frame_id);
                continue;
            };
            let analysis = analysis_by_id
                .get(&row.frame_id)
                .and_then(|a| serde_json::to_value(a).ok());
            frames.push(PublishFrame {
                frame_id: row.frame_id,
                output_path: cal.output_path,
                engine_version: cal.engine_version,
                frame,
                analysis,
                fwhm_arcsec: row.fwhm_arcsec,
                eccentricity: row.eccentricity,
            });
        }
        (frames, project, skipped)
    };

    if frames.is_empty() {
        return Err(ApiError::Invalid(
            "no publishable frames — every calibrated artifact is missing on disk".into(),
        ));
    }
    if !skipped_missing.is_empty() {
        tracing::warn!(count = skipped_missing.len(), "publish: skipped frames with missing artifacts");
    }

    // ── 2. Mint the HUB package uuid FIRST; stamp each artifact into staging ──
    let (sync_dir, _db_path) = crate::api::sync::sync_paths(ctx)?;
    let own_node = DeviceKey::load_or_create(&device_key_path(&sync_dir))
        .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
        .node_id();
    let own_device = crate::sync::node_id_hex(&own_node);

    let hub_package_id = uuid::Uuid::new_v4().to_string();
    let pub_dir = sync_dir.join("collab_pub").join(&hub_package_id);
    let staging = sync_dir.join("collab_pub").join(format!("{hub_package_id}.staging"));
    std::fs::create_dir_all(&staging)
        .map_err(|e| ApiError::Internal(format!("create publish staging {}: {e}", staging.display())))?;

    let stamp_card = Card::new("ATH_PRJ", CardValue::Str(project_id.to_string()))
        .map_err(|e| ApiError::Internal(format!("build ATH_PRJ card: {e}")))?;
    let thresholds_version = project.thresholds_version.map(|v| v as i64);

    let mut records: Vec<(std::path::PathBuf, ManifestRecord)> = Vec::with_capacity(frames.len());
    let mut published_uuids: Vec<String> = Vec::with_capacity(frames.len());
    let mut used_rel_paths: HashSet<String> = HashSet::new();
    // Retain the surviving frames so the aggregate stats read the published subset.
    let mut kept: Vec<PublishFrame> = Vec::with_capacity(frames.len());

    for pf in frames.drain(..) {
        let out = Path::new(&pf.output_path);
        let base = out
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("frame_{}.fits", pf.frame_id));
        let rel_path = crate::api::sync::unique_rel_path(&base, pf.frame_id, &mut used_rel_paths);
        let stamped = staging.join(&rel_path);

        if let Err(e) = stamp_extra_card(out, &stamped, &stamp_card) {
            tracing::warn!(frame_id = pf.frame_id, error = %e, "publish: stamping failed — skipping frame");
            continue;
        }
        let byte_size = match std::fs::metadata(&stamped) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::warn!(frame_id = pf.frame_id, error = %e, "publish: cannot stat stamped copy — skipping");
                continue;
            }
        };
        let xxh3 = match xxh3_full_file(&stamped) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(frame_id = pf.frame_id, error = %format!("{e:#}"), "publish: cannot hash stamped copy — skipping");
                continue;
            }
        };
        let frame_uuid = pf
            .frame
            .uuid
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let frame_meta = match serde_json::to_value(&pf.frame) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(frame_id = pf.frame_id, error = %e, "publish: cannot serialize frame_meta — skipping");
                continue;
            }
        };
        published_uuids.push(frame_uuid.clone());
        records.push((
            stamped,
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: frame_uuid.clone(),
                origin_catalog_uuid: frame_uuid,
                origin_device: own_device.clone(),
                payload_kind: PayloadKind::CalibratedLight,
                rel_path,
                byte_size,
                xxh3,
                frame_meta,
                analysis: pf.analysis.clone(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                project: Some(ProjectStamp {
                    project_id: project_id.to_string(),
                    package_id: hub_package_id.clone(),
                    thresholds_version,
                    cal_engine_version: Some(pf.engine_version),
                }),
            },
        ));
        kept.push(pf);
    }

    if records.is_empty() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ApiError::Invalid(
            "no publishable frames — every calibrated artifact failed to stamp".into(),
        ));
    }

    // ── 3. write_package into the pub dir ────────────────────────────────────
    let announce = match write_package(&pub_dir, records) {
        Ok(a) => a,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            let _ = std::fs::remove_dir_all(&pub_dir);
            return Err(ApiError::Internal(format!("write publish package: {e:#}")));
        }
    };
    // Staging is only a copy source for `write_package`; the pub dir now holds
    // the authoritative payloads + manifest.
    let _ = std::fs::remove_dir_all(&staging);

    let manifest_path = pub_dir.join(MANIFEST_FILENAME);
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| ApiError::Internal(format!("read written manifest: {e}")))?;
    let manifest_xxh3 = xxh3_full_file(&manifest_path)
        .map_err(|e| ApiError::Internal(format!("hash written manifest: {e:#}")))?;
    // Hub rootHash: the hub REQUIRES exactly 64 hex chars. `write_package`'s own
    // `root_hash` is a 16-hex xxh3 placeholder, so compute the BLAKE3 collection
    // anchor over the manifest bytes (`iroh_blobs::Hash` — blake3 is not a direct
    // dep). This is an IDENTIFIER only; the wire transfer substitutes the real
    // iroh collection hash per serve.
    let root_hash = iroh_blobs::Hash::new(&manifest_bytes).to_hex();

    let frame_count = kept.len() as i64;
    let byte_size = announce.byte_size as i64;

    // ── 4. Aggregate stats from the published subset ─────────────────────────
    let aggregate_stats = {
        let mut stats = serde_json::Map::new();
        stats.insert("manifestXxh3".into(), serde_json::json!(manifest_xxh3));
        stats.insert("frameCount".into(), serde_json::json!(frame_count));

        let fwhms = sorted_f64(kept.iter().filter_map(|f| f.fwhm_arcsec).collect());
        if !fwhms.is_empty() {
            let mut fwhm_obj = serde_json::Map::new();
            if let Some(m) = percentile(&fwhms, 50.0) {
                fwhm_obj.insert("median".into(), serde_json::json!(m));
            }
            if let Some(p) = percentile(&fwhms, 10.0) {
                fwhm_obj.insert("p10".into(), serde_json::json!(p));
            }
            if let Some(p) = percentile(&fwhms, 90.0) {
                fwhm_obj.insert("p90".into(), serde_json::json!(p));
            }
            stats.insert("fwhmArcsec".into(), serde_json::Value::Object(fwhm_obj));
        }

        let eccs = sorted_f64(kept.iter().filter_map(|f| f.eccentricity).collect());
        if let Some(m) = percentile(&eccs, 50.0) {
            stats.insert("eccentricityMedian".into(), serde_json::json!(m));
        }

        let mut integ: BTreeMap<String, f64> = BTreeMap::new();
        for f in &kept {
            if let (Some(filter), Some(exp)) = (f.frame.filter.clone(), f.frame.exptime) {
                *integ.entry(filter).or_default() += exp;
            }
        }
        if !integ.is_empty() {
            stats.insert("integrationSecondsByFilter".into(), serde_json::json!(integ));
        }
        serde_json::Value::Object(stats)
    };

    // ── 5. Д9 supersedes: my still-active announcements touching these uuids ──
    let supersedes = {
        let db = db(ctx)?;
        let conn = db.conn();
        own_active_announcement_ids_for_uuids(&conn, project_id, &published_uuids).map_err(internal)?
    };

    // ── 6. Hub announce (anchored on the pre-minted hub uuid) ────────────────
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        let _ = std::fs::remove_dir_all(&pub_dir);
        return Err(ApiError::SignedOut("Sign in to publish to a project.".into()));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let req = AnnounceRequest {
        package_id: hub_package_id.clone(),
        root_hash: root_hash.clone(),
        byte_size,
        frame_count: frame_count as i32,
        aggregate_stats: aggregate_stats.clone(),
        supersedes: supersedes.clone(),
    };
    let resp = match client.announce(&token, project_id, &req).await {
        Ok(r) => r,
        Err(e) => {
            // A closed/duplicate (409), 403, or network failure leaves nothing
            // published — drop the retained dir so a retry starts clean.
            let _ = std::fs::remove_dir_all(&pub_dir);
            return Err(client_err(e));
        }
    };
    tracing::info!(
        project_id,
        package_id = %hub_package_id,
        announcement_id = %resp.id,
        state = %resp.state,
        frame_count,
        superseded = supersedes.len(),
        "collab package announced"
    );

    // ── 7. Local rows: upsert (origin=mine), mark supersedes, drop stale dirs ─
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let publisher_display = {
            let members: Vec<SnapshotMember> =
                serde_json::from_str(&project.members_json).unwrap_or_default();
            own_display_name(&members, &own_node)
        };
        upsert_package(
            &conn,
            &PackageRow {
                package_id: hub_package_id.clone(),
                project_id: project_id.to_string(),
                announcement_id: resp.id.clone(),
                publisher_display,
                own: true,
                root_hash: root_hash.clone(),
                byte_size,
                frame_count,
                manifest_xxh3: Some(manifest_xxh3.clone()),
                aggregate_stats: aggregate_stats.to_string(),
                supersedes: serde_json::to_string(&supersedes).unwrap_or_else(|_| "[]".into()),
                state: resp.state.clone(),
                reject_reason: None,
                superseded: false,
                origin: "mine".to_string(),
                local_dir: Some(pub_dir.to_string_lossy().to_string()),
                manifest_ndjson: Some(manifest_bytes.clone()),
                local_status: "complete".to_string(),
                holder_count: 0,
                online_count: 0,
                created_at: crate::sync::now_iso(),
                decided_at: None,
                fetched_at: String::new(),
            },
        )
        .map_err(internal)?;
        // T3 hazard: `upsert_package` ignores `local_status` on the UPDATE branch,
        // so an earlier poll INSERT could strand it at 'none'. Set it explicitly.
        set_local_status(&conn, &hub_package_id, "complete").map_err(internal)?;

        if !supersedes.is_empty() {
            mark_superseded(&conn, &supersedes).map_err(internal)?;
            // Reclaim the retained publication dir of each superseded package I own
            // (best-effort — a failure only leaves a stale dir on disk).
            for ann_id in &supersedes {
                if let Ok(Some(old)) = get_package_by_announcement(&conn, ann_id) {
                    if old.origin == "mine" {
                        if let Some(dir) = old.local_dir.as_deref() {
                            if let Err(e) = std::fs::remove_dir_all(dir) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    tracing::warn!(path = %dir, error = %e, "publish: superseded pub dir cleanup failed");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── 8. Push-seed the first receive-capable member (Д8) ───────────────────
    let seed_target = {
        let members: Vec<SnapshotMember> =
            serde_json::from_str(&project.members_json).unwrap_or_else(|e| {
                tracing::warn!(project_id, error = %e, "publish push-seed: members_json parse failed; no seed target");
                Vec::new()
            });
        match select_seed_target(&members, &resp.state, &own_node) {
            None => {
                tracing::info!(project_id, package_id = %hub_package_id, "publish: no receive-capable member to seed");
                None
            }
            Some((node, target_name)) => {
                // Best-effort: the package is already announced + recorded, so a
                // transient sender-build/enqueue failure must not undo the publish.
                match ensure_collab_sender_engine(ctx, collab_sender, node, emitter.clone()).await {
                    Ok((engine, _origin)) => match engine.enqueue_package(&pub_dir).await {
                        Ok(_) => {
                            tracing::info!(
                                project_id,
                                package_id = %hub_package_id,
                                seed_target = %target_name,
                                "publish: push-seed enqueued"
                            );
                            Some(target_name)
                        }
                        Err(e) => {
                            tracing::warn!(project_id, error = %format!("{e:#}"), "publish: push-seed enqueue failed");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!(project_id, error = %e, "publish: collab sender engine build failed; not seeded");
                        None
                    }
                }
            }
        }
    };

    Ok(PublishResult {
        package_id: hub_package_id,
        announcement_id: resp.id,
        state: resp.state,
        frame_count,
        byte_size,
        superseded_announcements: supersedes,
        seed_target,
    })
}

// ── Moderation (Task 9): review queue + approve/reject (render-gated) ─────────

/// One frame in a pending package's review copy, projected for the moderation
/// UI. Metrics are parsed from the contribution's stored `analysis` JSON (the
/// publisher's `serde_json::to_value(&FrameAnalysis)`, snake_case keys); an
/// absent metric stays `None` — never invented.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModerationFrame {
    pub frame_uuid: String,
    pub rel_path: String,
    /// Absolute on-disk path of the landed review copy; `None` when the
    /// contribution row carries no landed path.
    pub landed_path: Option<String>,
    pub byte_size: i64,
    pub fwhm: Option<f64>,
    pub eccentricity: Option<f64>,
    pub stars: Option<i64>,
    pub snr: Option<f64>,
}

/// One pending announcement awaiting a coordinator decision.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModerationItem {
    pub announcement_id: String,
    pub package_id: String,
    pub publisher: String,
    pub frame_count: i64,
    pub byte_size: i64,
    pub created_at: String,
    /// The push-seed review copy has fully landed (`local_status == "complete"`);
    /// `false` while the coordinator is still receiving it.
    pub review_copy_complete: bool,
    pub frames: Vec<ModerationFrame>,
}

/// Parse the four review metrics out of a contribution's stored `analysis` JSON
/// (`median_fwhm`, `median_eccentricity`, `stars_detected`, `median_snr`). A
/// missing field or malformed JSON leaves that metric `None` — never invented.
fn moderation_metrics(
    analysis: Option<&str>,
) -> (Option<f64>, Option<f64>, Option<i64>, Option<f64>) {
    let Some(json) = analysis else {
        return (None, None, None, None);
    };
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "moderation: contribution analysis JSON did not parse — metrics omitted");
            return (None, None, None, None);
        }
    };
    (
        v.get("median_fwhm").and_then(serde_json::Value::as_f64),
        v.get("median_eccentricity")
            .and_then(serde_json::Value::as_f64),
        v.get("stars_detected").and_then(serde_json::Value::as_i64),
        v.get("median_snr").and_then(serde_json::Value::as_f64),
    )
}

/// The coordinator's review queue: every PENDING package for the project, each
/// with its landed review frames + parsed metrics. Cache-only (no hub I/O) — the
/// poll (Task 8) keeps the pending set current and lands the review copies.
pub fn list_moderation_queue(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ModerationItem>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut out = Vec::new();
    for pkg in list_packages(&conn, project_id).map_err(internal)? {
        if pkg.state != "pending" {
            continue;
        }
        let frames = contributions_for_package(&conn, &pkg.package_id)
            .map_err(internal)?
            .into_iter()
            .map(|c| {
                let (fwhm, eccentricity, stars, snr) = moderation_metrics(c.analysis.as_deref());
                ModerationFrame {
                    frame_uuid: c.frame_uuid,
                    rel_path: c.rel_path,
                    landed_path: (!c.landed_path.is_empty()).then_some(c.landed_path),
                    byte_size: c.byte_size,
                    fwhm,
                    eccentricity,
                    stars,
                    snr,
                }
            })
            .collect();
        out.push(ModerationItem {
            review_copy_complete: pkg.local_status == "complete",
            announcement_id: pkg.announcement_id,
            package_id: pkg.package_id,
            publisher: pkg.publisher_display,
            frame_count: pkg.frame_count,
            byte_size: pkg.byte_size,
            created_at: pkg.created_at,
            frames,
        });
    }
    Ok(out)
}

/// Map a hub approve/reject error: a 409 (the announcement is no longer pending —
/// already decided by another coordinator, or superseded) surfaces as
/// [`ApiError::Conflict`] so the caller leaves the local row untouched (the next
/// poll re-syncs it). Everything else goes through [`client_err`]. The collab
/// client collapses a 409 into `Network("hub returned 409 Conflict…")`, so the
/// status is detected there.
fn decide_err(e: crate::account::AccountClientError) -> ApiError {
    if let crate::account::AccountClientError::Network(ref msg) = e {
        // Match the client's stable prefix, not a bare "409" — the message tail
        // can echo hub-controlled text (e.g. the typed reject reason), and a
        // literal "409" inside it must not relabel a non-conflict error.
        if msg.contains("hub returned 409") {
            return ApiError::Conflict(
                "This announcement was already decided — refresh the queue.".into(),
            );
        }
    }
    client_err(e)
}

/// Decide a pending announcement (coordinator only — enforced by the hub).
///
/// - `approve` ⇒ hub approve, then flip the local package state to `published`
///   (optimistic; the poll re-syncs the authoritative state).
/// - reject ⇒ `reason` is required, trimmed, and must be 1..=500 BYTES —
///   validated BEFORE any hub call. On a successful hub reject the local review
///   copy is removed: every contribution's landed file is deleted best-effort
///   (`warn!` per failure), then [`delete_package`] drops the row (its
///   contributions CASCADE). Nothing else — the poll won't resurrect the files.
///
/// A hub 409 (no longer pending) is a [`ApiError::Conflict`] and leaves the local
/// row untouched.
pub async fn decide_announcement(
    ctx: &ServiceContext,
    announcement_id: &str,
    approve: bool,
    reason: Option<String>,
) -> Result<(), ApiError> {
    // Validate the rejection reason BEFORE touching the hub (the hub also enforces
    // the 1..=500 BYTE bound, but bailing here avoids a wasted round trip).
    let reason = if approve {
        None
    } else {
        let trimmed = reason.unwrap_or_default().trim().to_string();
        if !(1..=500).contains(&trimmed.len()) {
            return Err(ApiError::Invalid(
                "a rejection reason of 1 to 500 bytes is required".into(),
            ));
        }
        Some(trimmed)
    };

    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to moderate announcements.".into(),
        ));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;

    if approve {
        let resp = client
            .approve_announcement(&token, announcement_id)
            .await
            .map_err(decide_err)?;
        let db = db(ctx)?;
        let conn = db.conn();
        let updated = conn
            .execute(
                "UPDATE project_packages SET state = 'published', decided_at = datetime('now') \
                 WHERE announcement_id = ?1",
                [announcement_id],
            )
            .map_err(|e| internal(e.into()))?;
        tracing::info!(announcement_id, hub_state = %resp.state, updated, "approved announcement");
    } else {
        let reason = reason.expect("the reject path validates a reason above");
        let resp = client
            .reject_announcement(&token, announcement_id, &reason)
            .await
            .map_err(decide_err)?;
        let db = db(ctx)?;
        let conn = db.conn();
        match get_package_by_announcement(&conn, announcement_id).map_err(internal)? {
            Some(pkg) => {
                let contributions =
                    contributions_for_package(&conn, &pkg.package_id).map_err(internal)?;
                for c in &contributions {
                    if c.landed_path.is_empty() {
                        continue;
                    }
                    if let Err(e) = std::fs::remove_file(&c.landed_path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(path = %c.landed_path, error = %e, "reject: removing review-copy file failed");
                        }
                    }
                }
                let removed = delete_package(&conn, &pkg.package_id).map_err(internal)?;
                tracing::info!(
                    announcement_id,
                    package_id = %pkg.package_id,
                    hub_state = %resp.state,
                    files = contributions.len(),
                    removed,
                    "rejected announcement; review copy deleted"
                );
            }
            None => {
                tracing::info!(announcement_id, hub_state = %resp.state, "rejected announcement; no local review copy to delete");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Insert a fully-populated `plate_solves` row (every NOT NULL column) for
    /// one frame, with an explicit pixel scale and crval center so the gate's
    /// precedence branches are observable.
    fn seed_plate_solve(
        conn: &rusqlite::Connection,
        frame_id: i64,
        pixel_scale_arcsec: f64,
        crval1: f64,
        crval2: f64,
    ) {
        conn.execute(
            "INSERT INTO plate_solves \
             (frame_id, crpix1, crpix2, crval1, crval2, cd1_1, cd1_2, cd2_1, cd2_2, \
              matched_stars, total_detected, rms_residual_px, rms_residual_arcsec, \
              pixel_scale_arcsec, field_rotation_deg, solve_time_ms, catalog_used, \
              algorithm_used, solved_at) \
             VALUES (?1, 100.0, 100.0, ?2, ?3, 1.0, 0.0, 0.0, 1.0, \
                     300, 400, 0.5, 0.4, ?4, 0.0, 1000, 'gaia', 'blind', '2026-07-13T00:00:00Z')",
            rusqlite::params![frame_id, crval1, crval2, pixel_scale_arcsec],
        )
        .unwrap();
    }

    #[test]
    fn gate_prefers_plate_solve_scale_and_center_over_header() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            // frames.ra/dec are deliberately FAR off target (100, 10) so a
            // center taken from the frame row would fail the radius check; the
            // solve's crval sits on target.
            let (set_id, frames) =
                seed_set(&conn, "Off-header set", "06:40:00", "+10:00:00", 100.0, 10.0, 1);
            // Solve: scale 1.5″/px (≠ the ~0.776″/px header fallback), center
            // on target (210.8, +54.35).
            seed_plate_solve(&conn, frames[0], 1.5, 210.8, 54.35);
            (set_id, frames)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 1);
        let row = report.rows.iter().find(|r| r.frame_id == frames[0]).unwrap();

        // Scale from the solve: 2.0 px × 1.5 ″/px = 3.0″ (NOT 2.0 × ~0.776).
        assert!(
            (row.fwhm_arcsec.unwrap() - 3.0).abs() < 1e-6,
            "fwhm should use the solve pixel scale: {:?}",
            row.fwhm_arcsec
        );
        // Center from crval (on target) → no radius failure, even though
        // frames.ra/dec (100, 10) is far outside the 1.5° radius.
        assert!(
            !row.failures.iter().any(|f| f.contains("outside target radius")),
            "center should come from crval, not frames.ra/dec: {:?}",
            row.failures
        );
    }

    #[test]
    fn gate_dedups_a_frame_shared_by_two_linked_sets() {
        let (_tmp, ctx) = test_ctx();
        let (set_a, set_b, frame_id) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let (set_a, frames) =
                seed_set(&conn, "Set A", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            let frame_id = frames[0];

            // A second set (own imaging_night + session) that shares the SAME
            // frame via an extra session_members row.
            conn.execute(
                "INSERT INTO frames_set (name, objctra, objctdec) VALUES ('Set B', '14:03:12', '+54:21:00')",
                [],
            )
            .unwrap();
            let set_b = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
                 VALUES (?1, '2026-07-03T20:00:00Z', '2026-07-04T03:00:00Z')",
                [set_b],
            )
            .unwrap();
            let night_b = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'ASI2600MM')",
                [night_b],
            )
            .unwrap();
            let session_b = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_b, frame_id],
            )
            .unwrap();
            (set_a, set_b, frame_id)
        };

        link_frame_set(&ctx, "p-1", set_a).unwrap();
        link_frame_set(&ctx, "p-1", set_b).unwrap();

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 1, "the shared frame is counted once (union dedup)");
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].frame_id, frame_id);
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

    /// End-to-end hub poll (wiremock): a fresh refresh fetches the page + a REAL
    /// ed25519-signed membership snapshot + thresholds, TOFU-pins the hub key,
    /// caches the verified row (raw payload/signature + parsed members), and
    /// auto-links the pending portal deep-link intent. A second refresh whose
    /// membership is signed by a DIFFERENT key fails verification against the
    /// pin and keeps the stale row (refresh still Ok).
    #[tokio::test]
    async fn refresh_populates_cache_verifies_snapshot_and_auto_links_intent() {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Signing fixtures (same technique as the Task-1 snapshot tests).
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let key_b64 = B64.encode(key.verifying_key().to_bytes());
        let snapshot_payload = serde_json::json!({
            "schema": 1, "projectId": "p-1", "membershipVersion": 7, "requireApproval": true,
            "issuedAt": "2026-07-13T00:00:00Z",
            "members": [{"accountId": "a-1", "displayName": "Vilen", "dataRole": "send_receive",
                         "coordinator": true, "nodes": [B64.encode([9u8; 32])]}]
        });
        let signed = |k: &SigningKey, payload: &serde_json::Value| -> serde_json::Value {
            let bytes = serde_json::to_vec(payload).unwrap();
            serde_json::json!({
                "payload": B64.encode(&bytes),
                "signature": B64.encode(k.sign(&bytes).to_bytes()),
                "pubkey": B64.encode(k.verifying_key().to_bytes()),
            })
        };

        // Mock hub.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/collab/pubkey"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "pubkey": key_b64 })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "p-1", "slug": "m101", "title": "M 101", "dataRole": "send_receive",
                "coordinator": true, "requireApproval": false, "pendingAnnouncements": 0
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": {"id": "p-1", "slug": "m101", "title": "M 101", "status": "active",
                            "requireApproval": false,
                            "target": {"name": "M101", "raDeg": 210.8, "decDeg": 54.35, "radiusDeg": 1.5}},
                "members": [{"displayName": "Vilen", "dataRole": "send_receive", "coordinator": true}],
                "packages": [], "progress": {"totalFrames": 0, "integrationSecondsByFilter": {}, "perMember": []}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/thresholds"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current": {"version": 2,
                            "rules": [{"metricKey": "not_trailed", "op": "reject_if", "value": true}],
                            "createdAt": "2026-07-13T00:00:00Z"},
                "history": []
            })))
            .mount(&server)
            .await;
        // Membership: K-signed, served ONCE so the second refresh falls through
        // to the K2-signed mock mounted later.
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/membership"))
            .respond_with(ResponseTemplate::new(200).set_body_json(signed(&key, &snapshot_payload)))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // ctx: point the account hub at the mock, sign in, seed a set + intent.
        let (_tmp, ctx) = test_ctx();
        let host = reqwest::Url::parse(&server.uri())
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, &server.uri())
                .unwrap();
        }
        crate::api::account::store_token_for_test(&ctx, "tok").unwrap();
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let (set_id, _) = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            set_id
        };
        // Record the "publish as project" intent from the set's own target.
        record_project_link_intent(&ctx, set_id).unwrap();

        // First refresh.
        let cards = refresh_projects(&ctx).await.unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].project_id, "p-1");
        assert_eq!(cards[0].linked_sets, 1, "the intent auto-linked the source set");

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let row = crate::db::collab::get_project(&conn, "p-1").unwrap().unwrap();
            assert_eq!(row.membership_version, 7);
            let members: Vec<ProjectMemberView> =
                serde_json::from_str(&row.members_json).unwrap();
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].display_name, "Vilen");
            assert!(members[0].coordinator);

            // The raw snapshot payload/signature are cached (slice-4 re-verify).
            assert!(!row.snapshot_payload_b64.is_empty());
            assert!(!row.snapshot_signature_b64.is_empty());

            // The TOFU pin now stores K under the per-host key.
            let pin = crate::db::get_setting(&conn, &format!("collab.snapshot_pubkey.{host}"))
                .unwrap()
                .unwrap();
            assert_eq!(pin, key_b64);

            // The intent auto-linked and was consumed.
            assert_eq!(
                crate::db::collab::linked_set_ids(&conn, "p-1").unwrap(),
                vec![set_id]
            );
            assert!(crate::db::collab::list_link_intents(&conn).unwrap().is_empty());
        }

        // Second refresh: membership now signed by a DIFFERENT key → the pinned
        // key rejects it and the stale row is kept.
        let other = SigningKey::from_bytes(&[2u8; 32]);
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/membership"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(signed(&other, &snapshot_payload)),
            )
            .mount(&server)
            .await;

        let cards2 = refresh_projects(&ctx)
            .await
            .expect("pin mismatch keeps the stale row, refresh still Ok");
        assert_eq!(cards2.len(), 1);
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let row = crate::db::collab::get_project(&conn, "p-1").unwrap().unwrap();
            assert_eq!(
                row.membership_version, 7,
                "stale row kept after pin mismatch"
            );
        }
    }

    // ── Publish (Task 7) ─────────────────────────────────────────────────────

    use crate::db::light_calibrations::{
        upsert_light_calibration, FlatNormMode, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Cache a project directly (no snapshot verification) with the given
    /// `members_json`; target M101, no threshold rules (precondition-only gate),
    /// thresholds_version 4.
    fn seed_publish_project(conn: &rusqlite::Connection, project_id: &str, members_json: &str) {
        crate::db::collab::upsert_project(
            conn,
            &CollabProjectRow {
                project_id: project_id.into(),
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
                members_json: members_json.into(),
                thresholds_version: Some(4),
                thresholds_rules_json: None,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// One member entry as slice-3 caches it (camelCase `SnapshotMember`).
    fn member_json(display: &str, data_role: &str, coordinator: bool, node: &NodeId) -> serde_json::Value {
        serde_json::json!({
            "accountId": format!("acc-{display}"),
            "displayName": display,
            "dataRole": data_role,
            "coordinator": coordinator,
            "nodes": [B64.encode(node)],
        })
    }

    /// A frame set of gate-passing, calibrated LIGHT frames: on-target, known
    /// pixel scale, not trailed, each with a real tiny calibrated FITS artifact on
    /// disk + a `Calibrated` `light_calibrations` row. Returns the set id.
    fn seed_publishable_set(
        conn: &rusqlite::Connection,
        out_dir: &std::path::Path,
        name: &str,
        uuids: &[&str],
    ) -> i64 {
        std::fs::create_dir_all(out_dir).unwrap();
        conn.execute(
            "INSERT INTO frames_set (name, objctra, objctdec) VALUES (?1, '14:03:12', '+54:21:00')",
            [name],
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

        for (i, uuid) in uuids.iter().enumerate() {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at) \
                 VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS', '2026-07-01T21:00:00Z')",
                rusqlite::params![
                    format!("/data/{name}/L_{i:04}.fits"),
                    format!("L_{i:04}.fits")
                ],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, exptime, filter, uuid) \
                 VALUES (?1, 'Light', 'M101', 'ASI2600MM', 210.8, 54.35, 3.76, 1000.0, 300.0, 'L', ?2)",
                rusqlite::params![file_id, uuid],
            )
            .unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![session_id, frame_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frame_analysis \
                 (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity, median_snr, \
                  median_hfr, frame_snr, snr_weight, psf_signal, background, noise, \
                  detection_threshold, width, height, source_channels, trail_r_squared, possibly_trailed) \
                 VALUES (?1, ?2, 400, 2.0, 0.4, 10.0, 2.0, 10.0, 1.0, 100.0, 10.0, 1.0, 5.0, \
                         6248, 4176, 1, 0.0, 0)",
                rusqlite::params![frame_id, file_id],
            )
            .unwrap();

            // A real single-HDU FITS the stamper can copy + a Calibrated tracking
            // row (no set-ids, current engine, no calibration links ⇒ Calibrated).
            let out_path = out_dir.join(format!("c_{uuid}.fits"));
            crate::fits_writer::write_fits_f32(&out_path, 4, 4, 1, &vec![0.5f32; 16], &[]).unwrap();
            upsert_light_calibration(
                conn,
                &LightCalRow {
                    id: 0,
                    frame_id: Some(frame_id),
                    source_uuid: Some(uuid.to_string()),
                    source_filename: Some(format!("L_{i:04}.fits")),
                    output_path: out_path.to_string_lossy().to_string(),
                    dark_set_id: None,
                    flat_set_id: None,
                    bias_set_id: None,
                    calstat: "B".to_string(),
                    flat_norm_applied: false,
                    flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
                    output_hash: "deadbeef".to_string(),
                    engine_version: LIGHT_CAL_ENGINE_VERSION,
                    created_at: "2026-07-10T00:00:00Z".to_string(),
                    cal_params: "{}".to_string(),
                },
            )
            .unwrap();
        }
        set_id
    }

    /// Point `ctx`'s account hub at `uri` and store a device token.
    fn wire_hub(ctx: &ServiceContext, uri: &str) {
        {
            let conn = crate::api::db(ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
        }
        crate::api::account::store_token_for_test(ctx, "tok").unwrap();
    }

    /// Mount a single announce responder returning `{id, state}`.
    async fn mount_announce(server: &MockServer, project_id: &str, id: &str, state: &str) {
        Mock::given(wm_method("POST"))
            .and(wm_path(format!("/api/v1/projects/{project_id}/announcements")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id, "state": state
            })))
            .mount(server)
            .await;
    }

    /// Insert a loopback-backed collab sender engine keyed by `peer` into `sender`
    /// (so `ensure_collab_sender_engine` short-circuits to it instead of building a
    /// real iroh transport). Returns the engine's outbound store.
    async fn insert_loopback_engine(
        sender: &crate::sync::SyncSenderRuntime,
        db_dir: &std::path::Path,
        tag: &str,
        peer: NodeId,
    ) -> std::sync::Arc<crate::sync::StandaloneSyncStore> {
        use crate::sharing::SharingTransport;
        use crate::sync::{
            node_id_hex, StandaloneSyncStore, StartedSender, SyncEngine, SyncStore,
        };

        let net = crate::sharing::loopback::LoopbackNetwork::new();
        let a_ep = net.endpoint();
        let a_node = a_ep.node_id();
        let store = std::sync::Arc::new(
            StandaloneSyncStore::open(db_dir.join(format!("{tag}_a_sync.db"))).unwrap(),
        );
        let engine = std::sync::Arc::new(SyncEngine::spawn_with_sink_and_emitter(
            std::sync::Arc::clone(&store) as std::sync::Arc<dyn SyncStore>,
            std::sync::Arc::new(a_ep) as std::sync::Arc<dyn SharingTransport>,
            peer,
            std::sync::Arc::new(crate::api::collab_exchange::CollabCleanupSink),
            None,
        ));
        sender.lock_inner().await.insert(
            peer,
            StartedSender {
                engine,
                origin_device: node_id_hex(&a_node),
                peer,
            },
        );
        store
    }

    /// Pull the announce request bodies (in order) the mock hub received.
    async fn announce_bodies(server: &MockServer) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path().ends_with("/announcements"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    /// (a) Full publish over a wiremock hub + a loopback seed target: state
    /// published, retained stamped pub dir (ATH_PRJ in the payload header), an
    /// origin=mine row with manifest bytes, the hub saw the manifest anchor +
    /// `supersedes: []`, and the send_receive member was seeded.
    #[tokio::test]
    async fn publish_announces_stamps_and_seeds_the_member() {
        const BOB: NodeId = [0x11; 32];
        let server = MockServer::start().await;
        mount_announce(&server, "p-1", "ann-a", "published").await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(
                &conn,
                "p-1",
                &serde_json::json!([member_json("Bob", "send_receive", false, &BOB)]).to_string(),
            );
            seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-a-1", "uuid-a-2"])
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let sender = crate::sync::SyncSenderRuntime::new();
        let store = insert_loopback_engine(&sender, _tmp.path(), "a", BOB).await;

        let result = publish_collab_frames(&ctx, &sender, "p-1", None).await.unwrap();

        assert_eq!(result.state, "published");
        assert_eq!(result.announcement_id, "ann-a");
        assert_eq!(result.frame_count, 2);
        assert!(result.byte_size > 0);
        assert!(result.superseded_announcements.is_empty());
        assert_eq!(result.seed_target.as_deref(), Some("Bob"), "the send_receive member is seeded");

        // Retained pub dir with a stamped payload (ATH_PRJ in the header).
        let pub_dir = _tmp.path().join("sync").join("collab_pub").join(&result.package_id);
        assert!(pub_dir.join(crate::package::MANIFEST_FILENAME).exists());
        let payload = pub_dir.join("c_uuid-a-1.fits");
        assert!(payload.exists(), "stamped payload retained under its rel_path");
        let head = String::from_utf8_lossy(&std::fs::read(&payload).unwrap()).to_string();
        assert!(head.contains("ATH_PRJ"), "payload header carries the project stamp card");
        // Staging scratch dir cleaned.
        assert!(!_tmp.path().join("sync").join("collab_pub").join(format!("{}.staging", result.package_id)).exists());

        // Local package row: origin=mine, complete, retained manifest bytes.
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let row = crate::db::collab_exchange::get_package(&conn, &result.package_id)
                .unwrap()
                .unwrap();
            assert_eq!(row.origin, "mine");
            assert!(row.own);
            assert_eq!(row.state, "published");
            assert_eq!(row.local_status, "complete");
            assert_eq!(row.frame_count, 2);
            assert!(row.manifest_ndjson.as_ref().is_some_and(|b| !b.is_empty()));
            assert_eq!(row.root_hash.len(), 64, "rootHash rendered to 64 hex chars");
        }

        // Hub saw the manifest anchor + an empty supersedes.
        let bodies = announce_bodies(&server).await;
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            bodies[0]["aggregateStats"]["manifestXxh3"].as_str().unwrap().len(),
            16
        );
        assert_eq!(bodies[0]["supersedes"], serde_json::json!([]));
        assert_eq!(bodies[0]["rootHash"].as_str().unwrap().len(), 64);

        // The seed reached the loopback engine (outbound row created on enqueue).
        assert!(store.get_outbound(1).unwrap().is_some(), "push-seed enqueued to the member engine");
    }

    /// (b) Re-publishing the same source uuids supersedes the first announcement:
    /// the second announce body carries the first announcement id, the first
    /// package row flips `superseded=1`, and the result echoes it.
    #[tokio::test]
    async fn republish_supersedes_the_prior_announcement() {
        let server = MockServer::start().await;
        // First POST → ann-1 (once), later POSTs → ann-2.
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-1", "state": "published"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_announce(&server, "p-1", "ann-2", "published").await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            // Send-only member ⇒ no seed target (keeps this test focused on Д9).
            seed_publish_project(
                &conn,
                "p-1",
                &serde_json::json!([member_json("Solo", "send", false, &[0x33; 32])]).to_string(),
            );
            seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-b-1", "uuid-b-2"])
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let sender = crate::sync::SyncSenderRuntime::new();
        let first = publish_collab_frames(&ctx, &sender, "p-1", None).await.unwrap();
        assert_eq!(first.announcement_id, "ann-1");
        assert!(first.superseded_announcements.is_empty());
        assert!(first.seed_target.is_none(), "a send-only member is not a seed target");

        let second = publish_collab_frames(&ctx, &sender, "p-1", None).await.unwrap();
        assert_eq!(second.announcement_id, "ann-2");
        assert_eq!(
            second.superseded_announcements,
            vec!["ann-1".to_string()],
            "the re-publish supersedes the first announcement"
        );

        // The first package row is now flagged superseded.
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let first_row = crate::db::collab_exchange::get_package(&conn, &first.package_id)
                .unwrap()
                .unwrap();
            assert!(first_row.superseded, "first package row flipped superseded=1");
        }

        // The second announce body carried the first announcement id in supersedes.
        let bodies = announce_bodies(&server).await;
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["supersedes"], serde_json::json!([]));
        assert_eq!(bodies[1]["supersedes"], serde_json::json!(["ann-1"]));
    }

    /// (c) A `pending` announcement (require_approval) seeds the COORDINATOR, not
    /// a non-coordinator send_receive member.
    #[tokio::test]
    async fn pending_publish_seeds_the_coordinator() {
        const COORD: NodeId = [0x22; 32];
        const BOB: NodeId = [0x11; 32];
        let server = MockServer::start().await;
        mount_announce(&server, "p-1", "ann-c", "pending").await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(
                &conn,
                "p-1",
                &serde_json::json!([
                    member_json("Coord", "send_receive", true, &COORD),
                    member_json("Bob", "send_receive", false, &BOB),
                ])
                .to_string(),
            );
            seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-c-1"])
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let sender = crate::sync::SyncSenderRuntime::new();
        // Pre-insert engines for BOTH candidates so whichever is chosen enqueues
        // cleanly (no real transport) — the seed_target string reveals the choice.
        insert_loopback_engine(&sender, _tmp.path(), "c_coord", COORD).await;
        insert_loopback_engine(&sender, _tmp.path(), "c_bob", BOB).await;

        let result = publish_collab_frames(&ctx, &sender, "p-1", None).await.unwrap();
        assert_eq!(result.state, "pending");
        assert_eq!(
            result.seed_target.as_deref(),
            Some("Coord"),
            "a pending package seeds only the coordinator"
        );
    }

    /// (d) No receive-capable member online ⇒ `seed_target` is None but the
    /// package is still announced + recorded.
    #[tokio::test]
    async fn publish_with_no_eligible_target_still_announces() {
        let server = MockServer::start().await;
        mount_announce(&server, "p-1", "ann-d", "published").await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            // Only a send-only contributor ⇒ no published-state seed candidate.
            seed_publish_project(
                &conn,
                "p-1",
                &serde_json::json!([member_json("Solo", "send", false, &[0x44; 32])]).to_string(),
            );
            seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-d-1"])
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let sender = crate::sync::SyncSenderRuntime::new();
        let result = publish_collab_frames(&ctx, &sender, "p-1", None).await.unwrap();

        assert_eq!(result.state, "published");
        assert!(result.seed_target.is_none(), "no receive-capable member ⇒ no seed target");
        assert!(!sender.is_started().await, "nothing was enqueued");
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert!(crate::db::collab_exchange::get_package(&conn, &result.package_id)
                .unwrap()
                .is_some());
        }
    }

    /// An empty gate (no publishable frames) is an `Invalid`, never a panic.
    #[tokio::test]
    async fn publish_with_no_publishable_frames_is_invalid() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
        }
        let sender = crate::sync::SyncSenderRuntime::new();
        assert!(matches!(
            publish_collab_frames(&ctx, &sender, "p-1", None).await,
            Err(ApiError::Invalid(_))
        ));
    }

    // ── Moderation (Task 9) ──────────────────────────────────────────────────

    /// Insert a package row with a given decision `state` + local fetch status
    /// (fresh `package_id` ⇒ an INSERT, so `local_status` is honored).
    fn seed_moderation_package(
        conn: &rusqlite::Connection,
        project_id: &str,
        package_id: &str,
        announcement_id: &str,
        state: &str,
        local_status: &str,
        frame_count: i64,
    ) {
        upsert_package(
            conn,
            &PackageRow {
                package_id: package_id.into(),
                project_id: project_id.into(),
                announcement_id: announcement_id.into(),
                publisher_display: "Alice".into(),
                own: false,
                root_hash: "r".into(),
                byte_size: 4096,
                frame_count,
                manifest_xxh3: None,
                aggregate_stats: "{}".into(),
                supersedes: "[]".into(),
                state: state.into(),
                reject_reason: None,
                superseded: false,
                origin: "remote".into(),
                local_dir: None,
                manifest_ndjson: None,
                local_status: local_status.into(),
                holder_count: 0,
                online_count: 0,
                created_at: "2026-07-13T00:00:00Z".into(),
                decided_at: None,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Insert one landed contribution (received frame) for a package.
    fn add_contribution(
        conn: &rusqlite::Connection,
        project_id: &str,
        package_id: &str,
        uuid: &str,
        landed_path: &str,
        analysis: Option<String>,
    ) {
        crate::db::collab_exchange::insert_contribution(
            conn,
            &crate::db::collab_exchange::ContributionRow {
                id: 0,
                project_id: project_id.into(),
                package_id: package_id.into(),
                frame_uuid: uuid.into(),
                publisher_display: "Alice".into(),
                rel_path: format!("Alice/{uuid}.fits"),
                landed_path: landed_path.into(),
                byte_size: 2048,
                xxh3: "deadbeef".into(),
                frame_meta: "{}".into(),
                analysis,
                superseded: false,
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    /// The queue lists PENDING packages only (scoped to the project), each frame's
    /// metrics parsed from the stored `analysis` JSON (absent ⇒ None), and
    /// `review_copy_complete` reflects `local_status == "complete"` both ways.
    #[test]
    fn moderation_queue_lists_pending_with_parsed_metrics() {
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();

        // Pending + fully landed: one frame with analysis, one without.
        seed_moderation_package(&conn, "p-1", "pkg-pend", "ann-pend", "pending", "complete", 2);
        let analysis = serde_json::json!({
            "stars_detected": 400, "median_fwhm": 2.0,
            "median_eccentricity": 0.4, "median_snr": 10.0
        })
        .to_string();
        add_contribution(&conn, "p-1", "pkg-pend", "u-1", "/land/u-1.fits", Some(analysis));
        add_contribution(&conn, "p-1", "pkg-pend", "u-2", "/land/u-2.fits", None);

        // Pending but still downloading (review copy incomplete).
        seed_moderation_package(&conn, "p-1", "pkg-dl", "ann-dl", "pending", "downloading", 1);
        add_contribution(&conn, "p-1", "pkg-dl", "u-3", "/land/u-3.fits", None);

        // Published — never in the moderation queue.
        seed_moderation_package(&conn, "p-1", "pkg-pub", "ann-pub", "published", "complete", 1);
        // Pending, but a DIFFERENT project — excluded by scope.
        seed_moderation_package(&conn, "p-2", "pkg-other", "ann-other", "pending", "complete", 1);

        let queue = list_moderation_queue(&ctx, "p-1").unwrap();
        assert_eq!(queue.len(), 2, "only p-1's pending packages");

        let complete = queue.iter().find(|m| m.package_id == "pkg-pend").unwrap();
        assert!(complete.review_copy_complete);
        assert_eq!(complete.announcement_id, "ann-pend");
        assert_eq!(complete.publisher, "Alice");
        assert_eq!(complete.byte_size, 4096);
        assert_eq!(complete.frames.len(), 2);

        let f1 = complete.frames.iter().find(|f| f.frame_uuid == "u-1").unwrap();
        assert_eq!(f1.fwhm, Some(2.0));
        assert_eq!(f1.eccentricity, Some(0.4));
        assert_eq!(f1.stars, Some(400));
        assert_eq!(f1.snr, Some(10.0));
        assert_eq!(f1.landed_path.as_deref(), Some("/land/u-1.fits"));
        assert_eq!(f1.byte_size, 2048);

        let f2 = complete.frames.iter().find(|f| f.frame_uuid == "u-2").unwrap();
        assert_eq!(f2.fwhm, None, "absent analysis ⇒ metrics stay None");
        assert_eq!(f2.eccentricity, None);
        assert_eq!(f2.stars, None);
        assert_eq!(f2.snr, None);

        let incomplete = queue.iter().find(|m| m.package_id == "pkg-dl").unwrap();
        assert!(!incomplete.review_copy_complete, "still downloading ⇒ not complete");
    }

    /// Approve → hub approve (200) then the local package flips to `published`.
    #[tokio::test]
    async fn approve_flips_local_state_to_published() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/announcements/ann-a/approve"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-a", "state": "published"
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_moderation_package(&conn, "p-1", "pkg-a", "ann-a", "pending", "complete", 1);
        }

        decide_announcement(&ctx, "ann-a", true, None).await.unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = get_package_by_announcement(&conn, "ann-a").unwrap().unwrap();
        assert_eq!(row.state, "published", "approve flips local state");
        assert!(row.decided_at.is_some(), "decided_at stamped");
    }

    /// A reject with an empty / whitespace-only / over-long reason is `Invalid`
    /// BEFORE any hub call (the mock server sees zero requests).
    #[tokio::test]
    async fn reject_bad_reason_is_invalid_before_any_hub_call() {
        let server = MockServer::start().await;
        // No reject mock mounted on purpose.
        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_moderation_package(&conn, "p-1", "pkg-e", "ann-e", "pending", "complete", 1);
        }

        for reason in [
            None,
            Some(String::new()),
            Some("   ".to_string()),
            Some("x".repeat(501)),
        ] {
            assert!(
                matches!(
                    decide_announcement(&ctx, "ann-e", false, reason).await,
                    Err(ApiError::Invalid(_))
                ),
                "empty/whitespace/over-long reason ⇒ Invalid"
            );
        }
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "no hub request was made for an invalid reason"
        );
    }

    /// Reject happy path (hub 200): every landed file is removed and the package
    /// row + its contributions are deleted (CASCADE).
    #[tokio::test]
    async fn reject_deletes_the_review_copy() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/announcements/ann-r/reject"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "ann-r", "state": "rejected"
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());

        // Land two real files the reject must delete.
        let land = _tmp.path().join("land");
        std::fs::create_dir_all(&land).unwrap();
        let f1 = land.join("u-1.fits");
        let f2 = land.join("u-2.fits");
        std::fs::write(&f1, b"aaa").unwrap();
        std::fs::write(&f2, b"bbb").unwrap();

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_moderation_package(&conn, "p-1", "pkg-r", "ann-r", "pending", "complete", 2);
            add_contribution(&conn, "p-1", "pkg-r", "u-1", f1.to_str().unwrap(), None);
            add_contribution(&conn, "p-1", "pkg-r", "u-2", f2.to_str().unwrap(), None);
        }

        decide_announcement(&ctx, "ann-r", false, Some("FWHM too high".into()))
            .await
            .unwrap();

        assert!(!f1.exists(), "landed file removed");
        assert!(!f2.exists(), "landed file removed");
        let conn = crate::api::db(&ctx).unwrap().conn();
        assert!(
            get_package_by_announcement(&conn, "ann-r").unwrap().is_none(),
            "package row deleted"
        );
        assert!(
            crate::db::collab_exchange::contributions_for_package(&conn, "pkg-r")
                .unwrap()
                .is_empty(),
            "contributions cascaded away"
        );
    }

    /// A hub 409 (already decided) is a `Conflict` and leaves the local review
    /// copy entirely untouched — the next poll re-syncs it.
    #[tokio::test]
    async fn reject_hub_409_is_conflict_and_leaves_local_row() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/announcements/ann-x/reject"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "announcement already decided"
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());

        let land = _tmp.path().join("land");
        std::fs::create_dir_all(&land).unwrap();
        let f1 = land.join("u-1.fits");
        std::fs::write(&f1, b"keep").unwrap();

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_moderation_package(&conn, "p-1", "pkg-x", "ann-x", "pending", "complete", 1);
            add_contribution(&conn, "p-1", "pkg-x", "u-1", f1.to_str().unwrap(), None);
        }

        let err = decide_announcement(&ctx, "ann-x", false, Some("too soft".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "409 ⇒ Conflict, got {err:?}");

        // Local review copy untouched.
        assert!(f1.exists(), "landed file NOT removed on 409");
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = get_package_by_announcement(&conn, "ann-x").unwrap().unwrap();
        assert_eq!(row.state, "pending", "local row untouched on 409");
        assert_eq!(
            crate::db::collab_exchange::contributions_for_package(&conn, "pkg-x")
                .unwrap()
                .len(),
            1
        );
    }
}
