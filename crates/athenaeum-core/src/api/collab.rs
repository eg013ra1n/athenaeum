//! Stage-II collaboration orchestration (slice 3, Task 4): the DB-wiring layer
//! between the catalog and the pure pieces built in Tasks 1–3.
//!
//! - **Linking** — locally attach/detach a frame set to a cached project
//!   (`project_links`; never sent to the hub, spec §7).
//! - **Suggestions** — rank every non-archived frame set by angular distance to
//!   a project's target, flagging within-radius and already-linked sets.
//! - **Gate report** — assemble each linked LIGHT frame's gate inputs from
//!   `frames`/`plate_solves`/`frame_analysis`/the project's cached filter
//!   dictionary, resolve the per-frame calibrated verdict (P7,
//!   [`frame_cal_verdict`]) and run the pure [`crate::collab::gate`] engine
//!   over the union.
//! - **Portal deep-link intent** — record a "publish as project" intent for a
//!   set and build the portal `/new` URL prefilled from its target.
//! - **Match** — cached projects whose target radius contains a point and that
//!   aren't already linked to a set (the Task-6 auto-link hook).
//!
//! - **Publish** — per frame (wave 2 Task 7): calibrate each passing light
//!   once into my own folder under the Collaboration root, seed it into the
//!   collab store by reference, announce it, record my own row.
//!
//! Render+solver-gated (`api/mod.rs`, bumped in wave 2 Task 6): the P7
//! calibrated verdict calls the render-gated `api::lights::check_mode_ready`,
//! and [`publish_collab_frames`] drives the render-gated calibrated-light
//! generator; `crate::collab::frame_meta` (the manifest `meta` builder) and
//! the P4 WCS swap read plate-solve records, which need `solver` too. The
//! `crate::collab` core module and `db::collab` stay ungated so the
//! headless/perseus `--no-default-features` build compiles.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::lights::check_mode_ready;
use crate::api::{db, ApiError};
use crate::collab::filters::{match_filter, DictionaryEntry};
use crate::collab::gate::{
    evaluate_frame, header_pixel_scale_arcsec, FrameGateRow, GateFrameInput, ProjectTarget,
    ThresholdRuleView,
};
use crate::collab::hub_client::CollabClient;
use crate::collab::snapshot::{member_node_ids, SnapshotMember};
use crate::coordinates::{angular_distance, parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::db::analysis::get_frame_analyses_by_ids;
use crate::db::collab::CollabProjectRow;
use crate::db::collab_exchange::{
    contributions_for_package, delete_package, get_package_by_announcement, list_packages,
};
use crate::events::ProgressEmitter;
use crate::export::models::{CalibratedLightOptions, ExportMode};
use crate::fits_writer::{Card, CardValue};
use crate::models::FrameAnalysis;
use crate::package::xxh3_full_file;
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
    pub pending_frames: i64,
    pub project_status: String,
    pub target_name: String,
    pub target_ra_deg: f64,
    pub target_dec_deg: f64,
    pub target_radius_deg: f64,
    pub membership_version: i64,
    pub linked_sets: i64,
    pub candidates: i64,
    pub publishable: i64,
    /// D3 §3.3: this device auto-downloads the project's published contributions
    /// (default ON). Local preference — `set_project_auto_replicate` writes it.
    pub auto_replicate: bool,
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
            tracing::warn!(
                frames_set_id,
                "frame set center did not parse — treating as no center"
            );
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

/// The union of LIGHT `(frame_id, filename)` across many frame sets, de-duped
/// by frame id — `api/lights.rs::load_light_members` generalized to
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

/// One `frames_set` id per LIGHT frame id, over the SAME set ids
/// [`union_light_frames`] unions — [`frame_gate_inputs`]'s per-frame
/// calibrated verdict (P7) needs the set a frame belongs to, not just its id,
/// because `api::lights::compute_export_readiness_for_frames` reads its
/// calibration links through `calibration_set_to_frames`, which is keyed by
/// frame, not by set — but the readiness WALK it drives
/// (`export::collect_export_data`) still wants a `set_id` to scope by.
/// `MIN(frames_set_id)` when a frame is (rarely) linked into the project
/// through more than one linked set, so the choice is deterministic — the
/// union's own dedup-by-frame-id (one [`GateFrameInput`] per frame,
/// regardless of how many linked sets share it) is untouched by this map.
fn frame_set_ids(
    conn: &rusqlite::Connection,
    set_ids: &[i64],
) -> anyhow::Result<HashMap<i64, i64>> {
    if set_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; set_ids.len()].join(",");
    let sql = format!(
        "SELECT sm.frame_id, MIN(ino.frames_set_id) \
         FROM session_members sm \
         JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
         JOIN frames f ON f.id = sm.frame_id \
         WHERE ino.frames_set_id IN ({placeholders}) AND f.imagetyp = 'Light' \
         GROUP BY sm.frame_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(set_ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (frame_id, set_id) = row?;
        out.insert(frame_id, set_id);
    }
    Ok(out)
}

/// Raw `frames` columns needed for the gate's center/scale precedence, plus
/// P3's filter and P18's uuid.
struct FrameRow {
    ra: Option<f64>,
    dec: Option<f64>,
    objctra: Option<String>,
    objctdec: Option<String>,
    xpixsz: Option<f64>,
    focallen: Option<f64>,
    filter: Option<String>,
    uuid: Option<String>,
}

/// The per-frame "calibrated" verdict (P7): `Ok` when this frame, run alone
/// through the calibrated-lights export readiness gate, is ready —
/// `Err(sentence)` otherwise, where `sentence` is the exact text
/// `check_mode_ready` would show the Export tab under a disabled Calibrated
/// Lights mode for a frame set containing only this frame. Replaces decision
/// C's `LightCalStatus::NotCalibrated` constant (spec 2026-08-31 §8a).
///
/// Takes the set's `ExportData` BY REFERENCE, already collected by the caller
/// (fix round 1, ruling R7) — `api::lights::readiness_from_data` filters it
/// down to just `frame_id` itself, so this never re-walks `set_id`'s whole
/// export tree. The caller (`frame_gate_inputs`) collects one `ExportData`
/// per linked set and calls this once per LIGHT of that set.
pub(crate) fn frame_cal_verdict(
    conn: &Connection,
    set_id: i64,
    frame_id: i64,
    data: &crate::export::models::ExportData,
) -> Result<(), String> {
    let readiness =
        crate::api::lights::readiness_from_data(conn, set_id, &[frame_id], data).map_err(|e| {
            tracing::warn!(set_id, frame_id, error = %e, "export readiness check failed for the collab gate; treating as not calibrated");
            format!("could not verify calibration: {e}")
        })?;
    check_mode_ready(&readiness, ExportMode::CalibratedLights)
}

/// P6: the fixed calibration options a wave-2 publish generates its
/// calibrated pixels with — OSC ships as a CFA float FITS (`debayer_osc =
/// false`, R21 `splitOsc = false`), never split into per-channel outputs.
/// `format` is `CalibratedLightOptions::default`'s own default, spelled out
/// here for the reader.
///
/// Not yet called outside tests — Task 7 wires it into the pixel-generation
/// phase of `publish_collab_frames`.
#[allow(dead_code)]
pub(crate) fn publish_options() -> CalibratedLightOptions {
    CalibratedLightOptions {
        debayer_osc: false,
        format: crate::fits_writer::OutputFormat::Fits,
        ..Default::default()
    }
}

/// Batch-assemble one [`GateFrameInput`] per `(frame_id, filename)` —
/// conn-only, no settings needed. Reads `plate_solves`, `frames`, and the
/// analyses in three batched queries, then resolves each frame's center
/// (crval → ra/dec → parsed objctra/objctdec), pixel scale (plate-solve →
/// header `atan(xpixsz/focallen)`, no binning multiply), P7's per-frame
/// calibrated verdict (via `frame_set_id_by_frame`, one `collect_export_data`
/// per DISTINCT set — fix round 1, ruling R7), and P3's dictionary filter
/// match.
fn frame_gate_inputs(
    conn: &rusqlite::Connection,
    frames: &[(i64, String)],
    dictionary: &[DictionaryEntry],
    frame_set_id_by_frame: &HashMap<i64, i64>,
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
            "SELECT id, ra, dec, objctra, objctdec, xpixsz, focallen, filter, uuid \
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
                    filter: r.get(7)?,
                    uuid: r.get(8)?,
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

    // Fix round 1 (ruling R7): one `collect_export_data` per DISTINCT linked
    // set, cached here and reused by [`frame_cal_verdict`] for every LIGHT of
    // that set — never once per frame, which was O(frames²) DB round trips
    // per set (`collect_export_data` walks the whole set every call). A
    // collect failure is cached too (as the blocker text every frame of that
    // set will carry), so a broken set is only logged once, not once per
    // frame.
    let mut export_data_cache: HashMap<i64, Result<crate::export::models::ExportData, String>> =
        HashMap::new();

    let mut out = Vec::with_capacity(frames.len());
    for (frame_id, filename) in frames {
        let solve = solves.get(frame_id);
        let frow = rows_by_id.get(frame_id);

        // Scale precedence: plate-solve pixel scale, else the header fallback
        // (fix round 1: the formula itself now lives ONCE, in
        // `collab::gate::header_pixel_scale_arcsec`, shared with
        // `collab::frame_meta::build_frame_meta`).
        let pixel_scale_arcsec = solve
            .map(|(s, _, _)| *s)
            .or_else(|| frow.and_then(|f| header_pixel_scale_arcsec(f.xpixsz, f.focallen)));

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

        let filter_raw = frow
            .and_then(|f| f.filter.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        let filter_canonical = match_filter(&filter_raw, dictionary);
        let uuid = frow
            .and_then(|f| f.uuid.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        // P7: the real gate, replacing decision C's constant.
        let cal_blocker = match frame_set_id_by_frame.get(frame_id) {
            Some(&set_id) => {
                let data = export_data_cache.entry(set_id).or_insert_with(|| {
                    crate::export::collect_export_data(conn, set_id).map_err(|e| {
                        tracing::warn!(set_id, error = %e, "collect export data failed for the collab gate; every frame in this set is treated as not calibrated");
                        format!("could not verify calibration: {e}")
                    })
                });
                match data {
                    Ok(data) => frame_cal_verdict(conn, set_id, *frame_id, data).err(),
                    Err(msg) => Some(msg.clone()),
                }
            }
            None => {
                // Every frame here came from `union_light_frames` over the
                // SAME set ids `frame_set_id_by_frame` was built from, so
                // this is unreachable in practice — logged rather than
                // panicking, in case a future caller ever hands mismatched
                // inputs.
                tracing::warn!(
                    frame_id,
                    "frame has no resolvable frames_set for the collab gate"
                );
                Some("frame set unresolved".to_string())
            }
        };

        out.push(GateFrameInput {
            frame_id: *frame_id,
            filename: filename.clone(),
            center,
            pixel_scale_arcsec,
            cal_blocker,
            analysis: analyses.get(frame_id).cloned(),
            filter_raw,
            filter_canonical,
            uuid,
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
    tracing::info!(
        project_id,
        frames_set_id,
        removed,
        "unlinked frame set from project"
    );
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
        .query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })
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
            ApiError::NotFound(format!(
                "project {project_id} is not cached — refresh first"
            ))
        })?;
    let gated = project_gate(&conn, &project)?;
    let rows: Vec<FrameGateRow> = gated.into_iter().map(|(_, row)| row).collect();
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

/// What publish needs from a gate input beyond the verdict row: the frame's
/// uuid (P18) and its dictionary filter match (P3).
struct GateIdentity {
    uuid: String,
    filter_canonical: Option<String>,
}

/// The gate over the union of LIGHT frames across `project`'s linked sets —
/// the one body [`evaluate_project_gate`] and [`publish_collab_frames`] share,
/// so the report the user reads and the set a publish acts on can never
/// disagree. Each verdict row is paired with the identity fields publish
/// stamps and announces.
fn project_gate(
    conn: &Connection,
    project: &CollabProjectRow,
) -> Result<Vec<(GateIdentity, FrameGateRow)>, ApiError> {
    let project_id = project.project_id.as_str();
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
    // P3: a NULL or unparsed dictionary maps to an empty list, so every frame
    // fails the filter precondition with its own raw name in the reason —
    // never a silent pass. The dictionary is filled by the version poll
    // (Task 8); until a project has one, nothing here can pass this
    // precondition, which is the correct fail-closed default for a hub field
    // this build hasn't fetched yet.
    let dictionary: Vec<DictionaryEntry> = match &project.dictionary_json {
        Some(json) => serde_json::from_str(json)
            .map_err(|e| {
                tracing::warn!(project_id, error = %e, "cached filter dictionary does not parse — every frame will fail the filter check");
                e
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };

    let set_ids = crate::db::collab::linked_set_ids(conn, project_id).map_err(internal)?;
    let frames = union_light_frames(conn, &set_ids).map_err(internal)?;
    let frame_sets = frame_set_ids(conn, &set_ids).map_err(internal)?;
    let inputs = frame_gate_inputs(conn, &frames, &dictionary, &frame_sets).map_err(internal)?;

    Ok(inputs
        .into_iter()
        .map(|i| {
            let row = evaluate_frame(&i, &target, &rules);
            (
                GateIdentity {
                    uuid: i.uuid,
                    filter_canonical: i.filter_canonical,
                },
                row,
            )
        })
        .collect())
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

/// User-facing message for a hub `409 collab_api_outdated` refusal — this
/// client speaks a stale collab api and the hub has moved past it. Re-exports
/// the canonical string from `account::client` (ungated) so a headless build
/// that never compiles this render-gated module still shares one message.
pub const COLLAB_API_OUTDATED_MSG: &str = crate::account::client::COLLAB_API_OUTDATED_MSG;

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
        E::Forbidden => {
            ApiError::Forbidden("The account's role may not perform this action.".into())
        }
        E::CollabApiOutdated => {
            crate::account::client::warn_collab_api_outdated_once();
            ApiError::Conflict(COLLAB_API_OUTDATED_MSG.into())
        }
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
        pending_frames: row.pending_frames,
        project_status: row.project_status,
        target_name: row.target_name,
        target_ra_deg: row.target_ra_deg,
        target_dec_deg: row.target_dec_deg,
        target_radius_deg: row.target_radius_deg,
        membership_version: row.membership_version,
        linked_sets,
        candidates: gate.total,
        publishable: gate.publishable,
        auto_replicate: row.auto_replicate,
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
    rows.into_iter()
        .map(|row| card_from_row(ctx, row))
        .collect()
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
        let distance_deg = center
            .map(|(ra, dec)| angular_distance(ra, dec, row.target_ra_deg, row.target_dec_deg));
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
                ApiError::NotFound(format!(
                    "project {project_id} is not cached — refresh first"
                ))
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
    prev: Option<&CollabProjectRow>,
) -> Result<FetchedProject, FetchError> {
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

    // The dictionary is fetched when it is unknown or the project version
    // moved since the last manifest sync (an older hub without a version:
    // every refresh). A dictionary change always bumps the version.
    let dictionary_due = prev.is_none_or(|r| {
        r.dictionary_version.is_none() || page.project.version != Some(r.hub_version)
    });
    let dictionary = if dictionary_due {
        let wire = client
            .dictionary(token, &p.id)
            .await
            .map_err(|e| FetchError::Transport(e.into()))?;
        Some(match wire.current {
            Some(set) => {
                let entries: Vec<crate::collab::filters::DictionaryEntry> = set
                    .entries
                    .into_iter()
                    .map(|e| crate::collab::filters::DictionaryEntry {
                        canonical: e.canonical,
                        aliases: e.aliases,
                        kind: e.kind,
                    })
                    .collect();
                let json =
                    serde_json::to_string(&entries).map_err(|e| FetchError::Transport(e.into()))?;
                (Some(set.version), Some(json))
            }
            None => (None, None),
        })
    } else {
        None
    };

    let members_json =
        serde_json::to_string(&verified.members).map_err(|e| FetchError::Transport(e.into()))?;
    let (thresholds_version, thresholds_rules_json) = match thresholds.current {
        Some(set) => {
            let rules =
                serde_json::to_string(&set.rules).map_err(|e| FetchError::Transport(e.into()))?;
            (Some(set.version), Some(rules))
        }
        None => (None, None),
    };

    let row = CollabProjectRow {
        project_id: p.id.clone(),
        slug: p.slug.clone(),
        title: p.title.clone(),
        data_role: p.data_role.clone(),
        is_coordinator: p.coordinator,
        // The signed snapshot is the single source of truth for both membership
        // fields (they travel together in the signed payload; slice-4 enforces
        // the signed `require_approval`).
        require_approval: verified.require_approval,
        // v3: the wire field is `pendingFrames`, and the DB row field is
        // renamed to match (Task 2).
        pending_frames: p.pending_frames,
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
        // The caps rule (P9) compares these against `synced_caps_json`.
        gov_caps_json: gov_caps_json(&p.gov_caps, p.coordinator),
        // all ignored on write (local preference / sync-state / dictionary) —
        // upsert_project leaves these six alone entirely.
        auto_replicate: true,
        synced_caps_json: "[]".into(),
        hub_version: 0,
        manifest_cursor: 0,
        dictionary_version: None,
        dictionary_json: None,
        policy_json: r#"{"mode":"all"}"#.into(),
        replication_paused: false,
        auto_publish: true,
        fetched_at: String::new(), // filled by SQL
    };
    Ok(FetchedProject { row, dictionary })
}

/// One project as a refresh fetched it: the cache row, plus the dictionary
/// when it was due (`Some((version, entries JSON))`, both `None` when the
/// hub has no dictionary).
struct FetchedProject {
    row: CollabProjectRow,
    dictionary: Option<(Option<i32>, Option<String>)>,
}

/// This member's caps as the caps rule (P9) compares them: the hub's
/// `govCaps` plus a `"coordinator"` element for a coordinator, sorted and
/// deduplicated so an order change on the hub is not a caps change.
fn gov_caps_json(caps: &[String], coordinator: bool) -> String {
    let mut all: Vec<&str> = caps.iter().map(String::as_str).collect();
    if coordinator {
        all.push("coordinator");
    }
    all.sort_unstable();
    all.dedup();
    serde_json::to_string(&all).expect("a list of strings serializes")
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
/// - Marks lost every cached project the hub no longer returns (a failed
///   fetch still counts as "still mine" and is kept); see
///   [`refresh_projects_reporting`].
/// - Auto-links any pending portal deep-link intent whose target matches a
///   project that appeared for the FIRST time this refresh (≤ 0.1°).
/// - Fires [`on_thresholds_or_dictionary_moved`] for every project whose
///   thresholds or dictionary version this refresh changed.
///
/// Returns the refreshed cards (== [`list_projects`]).
pub async fn refresh_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError> {
    let report = refresh_projects_reporting(ctx, None).await?;
    for project_id in &report.gate_moved {
        on_thresholds_or_dictionary_moved(ctx, project_id);
    }
    list_projects(ctx)
}

/// What one [`refresh_projects_reporting`] did.
#[derive(Debug, Default)]
pub(crate) struct RefreshReport {
    /// Ids whose cache row this refresh rewrote — a project whose fetch
    /// failed keeps its stale row and is not in the set.
    pub refreshed: std::collections::HashSet<String>,
    /// Ids whose thresholds or dictionary version this refresh changed
    /// (a first sight counts: nothing → something). The caller fires
    /// [`on_thresholds_or_dictionary_moved`] for each.
    pub gate_moved: Vec<String>,
    /// Ids marked lost by this refresh.
    pub lost: Vec<String>,
}

/// THE one entry point for "a project's thresholds or dictionary moved"
/// (ruling R11), called by every refresh path — the project list refresh and
/// the version poll — so a change is acted on once, whichever path absorbed
/// it. A no-op until auto-publish (Task 10) replaces the body.
pub(crate) fn on_thresholds_or_dictionary_moved(_ctx: &ServiceContext, project_id: &str) {
    tracing::debug!(project_id, "thresholds or dictionary moved");
    #[cfg(test)]
    GATE_MOVES_SEEN.with(|seen| seen.borrow_mut().push(project_id.to_string()));
}

#[cfg(test)]
thread_local! {
    /// Every [`on_thresholds_or_dictionary_moved`] call on this thread (a
    /// `#[tokio::test]` runs its whole body on one thread).
    static GATE_MOVES_SEEN: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The project ids [`on_thresholds_or_dictionary_moved`] saw on this thread,
/// drained.
#[cfg(test)]
pub(crate) fn take_gate_moves_seen() -> Vec<String> {
    GATE_MOVES_SEEN.with(|seen| std::mem::take(&mut *seen.borrow_mut()))
}

/// [`refresh_projects`] without the hook, reporting what it did.
///
/// `only` limits the per-project fetch to those ids (the version poll passes
/// the projects that moved or appeared); every project `/me/projects` lists
/// still counts as mine, so a loss is detected from the list either way.
///
/// A lost project (cached, no longer listed) is marked lost, never deleted
/// (ruling R14): its replica rows are deleted, its collab-store tags are
/// dropped, and my own rows and files stay. A later refresh that finds it
/// listed again is a re-join and clears the mark.
pub(crate) async fn refresh_projects_reporting(
    ctx: &ServiceContext,
    only: Option<&std::collections::HashSet<String>>,
) -> Result<RefreshReport, ApiError> {
    let mut report = RefreshReport::default();
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to use collaboration projects.".into(),
        ));
    };
    let client = crate::collab::hub_client::CollabClient::new(&hub_url).map_err(client_err)?;

    let pinned = pinned_pubkey(ctx, &hub_url, &client).await?;
    let mine = client.my_projects(&token).await.map_err(client_err)?;

    // Live projects only: a project marked lost that the hub lists again is a
    // re-join, i.e. new.
    let previous: std::collections::HashMap<String, CollabProjectRow> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn)
            .map_err(internal)?
            .into_iter()
            .map(|p| (p.project_id.clone(), p))
            .collect()
    };
    let previous_ids: std::collections::HashSet<String> = previous.keys().cloned().collect();

    // `keep` = every project the hub still lists (whether or not its fetch
    // succeeded); `new_targets` = only those that appeared this refresh, for the
    // intent auto-link below.
    let mut keep: Vec<String> = Vec::with_capacity(mine.len());
    let mut new_targets: Vec<(String, f64, f64)> = Vec::new();
    for p in &mine {
        keep.push(p.id.clone());
        if only.is_some_and(|ids| !ids.contains(&p.id)) {
            continue;
        }
        let prev = previous.get(&p.id);
        match fetch_one_project(&client, &token, &pinned, p, prev).await {
            Ok(FetchedProject { row, dictionary }) => {
                if !previous_ids.contains(&row.project_id) {
                    new_targets.push((
                        row.project_id.clone(),
                        row.target_ra_deg,
                        row.target_dec_deg,
                    ));
                }
                let (thresholds_before, dictionary_before) = prev.map_or((None, None), |r| {
                    (r.thresholds_version, r.dictionary_version)
                });
                let dictionary_now = dictionary
                    .as_ref()
                    .map_or(dictionary_before, |(version, _)| *version);
                if thresholds_before != row.thresholds_version
                    || dictionary_before != dictionary_now
                {
                    report.gate_moved.push(row.project_id.clone());
                }
                let db = db(ctx)?;
                let conn = db.conn();
                crate::db::collab::upsert_project(&conn, &row).map_err(internal)?;
                if let Some((version, entries)) = dictionary {
                    crate::db::collab::set_dictionary(
                        &conn,
                        &row.project_id,
                        version,
                        entries.as_deref(),
                    )
                    .map_err(internal)?;
                }
                report.refreshed.insert(row.project_id.clone());
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

    report.lost = previous_ids
        .iter()
        .filter(|id| !keep.contains(id))
        .cloned()
        .collect();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        // A lost project is marked, never deleted (R14): its replica rows go,
        // my own rows stay with the row they hang off (a delete would cascade
        // them away), and my own files are never touched.
        for lost in &report.lost {
            let removed = crate::db::collab_frames::delete_not_in(
                &conn,
                lost,
                &std::collections::HashSet::new(),
            )
            .map_err(internal)?;
            crate::db::collab::mark_lost(&conn, lost).map_err(internal)?;
            tracing::info!(project_id = %lost, count = removed, "project lost: marked, replica frame rows deleted");
        }
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

    // Stop seeding every project the hub no longer lists (D3 T4). This is the
    // ONE place local project membership ends — a project I left, was removed
    // from, or that was archived — and a device that is not a member has no
    // business advertising or serving that project's blobs. Only a SUCCESSFUL
    // `my_projects` reaches here (a failed list returns above) and a per-project
    // fetch failure still counts as "still mine", so this can never fire on a hub
    // blip. Scoped per project id, never a prefix sweep; it covers both stores
    // and the collab store's in-flight tags. Re-joining re-seeds.
    for lost in &report.lost {
        crate::api::collab_exchange::unseed_project_local_data(ctx, lost).await;
    }

    Ok(report)
}

// ── Publish (Task 7): per frame — write once, seed by reference, announce ────

/// The outcome of one publish run over a project (collab v3 wave 2, §5.2).
/// An empty run (nothing publishable, nothing changed) is an outcome, never
/// an error.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    /// Frames announced to the hub for the first time (content version 1).
    pub announced: usize,
    /// Own frames re-published as a new content version (P19).
    pub updated: usize,
    /// The hub's verdict on the announced frames: `published`, or `pending`
    /// when the project requires approval. `None` when nothing was announced.
    pub state: Option<String>,
    /// Frames that were not sent this run, each with why: the gate's own
    /// failure sentences, or the step (calibrate, seed, announce) that failed.
    pub held_back: Vec<HeldBackFrame>,
    /// Own frames already published whose inputs did not change, or whose
    /// regenerated bytes came out identical (P19).
    pub unchanged: usize,
}

/// One frame a publish run did not send, and every reason why.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HeldBackFrame {
    pub frame_id: i64,
    pub filename: String,
    pub reasons: Vec<String>,
}

/// The hub caps one `POST /projects/{id}/frames` batch at 500 frames.
const ANNOUNCE_BATCH: usize = 500;

/// The one outcome event of a publish run, for the frontend's notification.
pub const COLLAB_PUBLISHED_EVENT: &str = "collab-published";

/// The inputs of an own frame's recipe (P19): what the USER changed. The
/// light-calibration engine version is carried only so its exclusion is
/// explicit and testable — [`recipe_hash_from`] never reads it, because an
/// app release that bumps the engine must not re-version every published
/// frame of every member (re-publishing then is the manual
/// [`republish_collab_frames`]).
pub(crate) struct RecipeParts {
    // Deliberately never read (P19) — present so the exclusion is a tested
    // fact, not an omission.
    #[allow(dead_code)]
    pub engine_version: i64,
    /// `(master path, identity)` for every master the generation reads, in
    /// path order; identity is the file's strong hash when the catalog has
    /// one, else `size:mtime`.
    pub masters: Vec<(String, String)>,
    /// The source light's `size:mtime`.
    pub source: String,
}

/// xxh3 over a recipe's parts, `engine_version` excluded (P19).
pub(crate) fn recipe_hash_from(parts: &RecipeParts) -> String {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for (path, identity) in &parts.masters {
        h.update(b"m\0");
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(identity.as_bytes());
        h.update(b"\n");
    }
    h.update(b"s\0");
    h.update(parts.source.as_bytes());
    format!("{:016x}", h.digest())
}

/// `(size, mtime in nanoseconds since the epoch, mtime in seconds)` of a file.
fn size_and_mtime(path: &Path) -> anyhow::Result<(u64, u128, i64)> {
    let meta =
        std::fs::metadata(path).map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?;
    let since = meta
        .modified()
        .map_err(|e| anyhow::anyhow!("mtime of {}: {e}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok((meta.len(), since.as_nanos(), since.as_secs() as i64))
}

/// The recipe hash of one own frame (P19): the resolved masters a generation
/// of `spec` reads — the same dark-gated set
/// [`crate::export::resolved_master_paths`] preflights — each identified by
/// its catalog strong hash, else its `size:mtime`, plus the source light's
/// `size:mtime`.
pub(crate) fn recipe_hash(
    conn: &Connection,
    spec: &crate::export::GenerationSpec,
    source_path: &Path,
) -> anyhow::Result<String> {
    recipe_hash_for(conn, crate::export::spec_master_paths(spec), source_path)
}

/// [`recipe_hash`] from the catalog links alone (no flat read, no compute
/// permit) — what the pre-permit split (ruling R9) decides with. Same master
/// rule ([`crate::export::master_paths_read`]), so the two always agree for
/// the same links.
fn recipe_hash_of_inputs(
    conn: &Connection,
    resolved: &crate::calibration_library::light_resolve::ResolvedFrameInputs,
) -> anyhow::Result<String> {
    let masters = crate::export::master_paths_read(
        resolved.dark.as_ref().map(|m| Path::new(m.path.as_str())),
        resolved.flat.as_ref().map(|m| Path::new(m.path.as_str())),
        resolved.bias.as_ref().map(|m| Path::new(m.path.as_str())),
    );
    recipe_hash_for(conn, masters, &resolved.light_path)
}

fn recipe_hash_for(
    conn: &Connection,
    master_paths: std::collections::BTreeSet<std::path::PathBuf>,
    source_path: &Path,
) -> anyhow::Result<String> {
    let mut masters = Vec::new();
    for path in master_paths {
        let path_str = path.to_string_lossy().to_string();
        let strong: Option<String> = conn
            .query_row(
                "SELECT strong_hash FROM files WHERE path = ?1 AND strong_hash IS NOT NULL LIMIT 1",
                [&path_str],
                |r| r.get(0),
            )
            .optional()?;
        let identity = match strong {
            Some(h) => format!("xxh3:{h}"),
            None => {
                let (size, mtime_ns, _) = size_and_mtime(&path)?;
                format!("{size}:{mtime_ns}")
            }
        };
        masters.push((path_str, identity));
    }
    let (size, mtime_ns, _) = size_and_mtime(source_path)?;
    Ok(recipe_hash_from(&RecipeParts {
        engine_version: crate::models::LIGHT_CAL_ENGINE_VERSION,
        masters,
        source: format!("{size}:{mtime_ns}"),
    }))
}

/// The streamed BLAKE3 of a file, lowercase hex — the same value the collab
/// store computes as a raw blob's hash on import.
fn blake3_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| anyhow::anyhow!("read {} for hashing: {e}", path.display()))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Header WCS keywords P4 strips before the plate solve's own WCS goes in:
/// every keyword `wcs_cards` can emit (incl. `CDELTi`/`CROTAi`), the pole
/// keywords, and the `PCi_j` matrix — so the file never carries both a CD and
/// a PC matrix.
fn is_header_wcs_keyword(keyword: &str) -> bool {
    crate::stacking::master_cards::is_wcs_keyword(keyword)
        || matches!(keyword, "LONPOLE" | "LATPOLE")
        || is_pc_matrix_keyword(keyword)
}

/// `^PC\d+_\d+$` — one element of a FITS WCS `PCi_j` matrix.
fn is_pc_matrix_keyword(keyword: &str) -> bool {
    let Some(rest) = keyword.strip_prefix("PC") else {
        return false;
    };
    let mut parts = rest.split('_');
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    matches!((parts.next(), parts.next(), parts.next()), (Some(i), Some(j), None) if digits(i) && digits(j))
}

/// The hub's `fileName` rule (hub contract): non-empty, at most 255 bytes, no
/// surrounding whitespace, not `.`/`..`, none of `/ \ :`, NUL or control
/// characters. A name that breaks it would refuse the whole atomic batch, so
/// the frame is held back up front instead.
fn hub_file_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty file name");
    }
    if name.len() > 255 {
        return Some("file name longer than 255 bytes");
    }
    if name.trim() != name {
        return Some("file name has surrounding whitespace");
    }
    if name == "." || name == ".." {
        return Some("file name is . or ..");
    }
    if name
        .chars()
        .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
    {
        return Some("file name contains / \\ : or a control character");
    }
    None
}

/// The publish stamps on one frame's header: `ATH_PRJ` and `ATH_FILT`, and
/// P4's WCS swap — with a `plate_solves` row the header WCS is replaced by the
/// solve's; without one it is copied through as is.
fn stamp_publish_cards(
    conn: &Connection,
    spec: &mut crate::export::GenerationSpec,
    project_id: &str,
    frame_id: i64,
    filter_canonical: &str,
) -> anyhow::Result<()> {
    if let Some(solve) = crate::plate_solve::storage::get_plate_solve(conn, frame_id)? {
        let wcs = crate::fits_writer::wcs::wcs_cards(&solve)
            .map_err(|e| anyhow::anyhow!("build WCS cards from the plate solve: {e}"))?;
        spec.cards.retain(|c| !is_header_wcs_keyword(&c.keyword));
        spec.cards.extend(wcs);
    }
    spec.cards.push(
        Card::new("ATH_PRJ", CardValue::Str(project_id.to_string()))
            .map_err(|e| anyhow::anyhow!("build ATH_PRJ card: {e}"))?,
    );
    spec.cards.push(
        Card::new("ATH_FILT", CardValue::Str(filter_canonical.to_string()))
            .map_err(|e| anyhow::anyhow!("build ATH_FILT card: {e}"))?,
    );
    Ok(())
}

/// The landing path of a NEW own frame: `<dir>/<name>`, else `<stem>_2`, … —
/// the first spelling that is neither claimed earlier in this run nor any
/// cached frame's `landed_path`. A file already there that no row references
/// is the leftover of an earlier run whose announce failed (own rows are
/// recorded only after a successful announce), and it is written over, so a
/// retry re-announces the same file instead of piling up copies.
fn new_frame_target(
    conn: &Connection,
    dir: &Path,
    name: &str,
    claimed: &mut HashSet<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    let base = dir.join(name);
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame")
        .to_string();
    let ext = base
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string);
    for n in 1..10_000 {
        let candidate = if n == 1 {
            base.clone()
        } else {
            match &ext {
                Some(ext) => dir.join(format!("{stem}_{n}.{ext}")),
                None => dir.join(format!("{stem}_{n}")),
            }
        };
        if claimed.contains(&candidate) {
            continue;
        }
        let referenced =
            crate::db::collab_frames::find_by_landed_path(conn, &candidate.to_string_lossy())?
                .is_some();
        if !referenced {
            claimed.insert(candidate.clone());
            return Ok(candidate);
        }
    }
    anyhow::bail!("no free file name for {} in {}", name, dir.display())
}

/// The sibling temp an `update` regenerates into before its BLAKE3 decides
/// whether it replaces the landed file (P19).
fn update_temp_path(target: &Path) -> std::path::PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".athtmp");
    std::path::PathBuf::from(name)
}

/// One gate-passing frame a publish run considers.
struct PublishCandidate {
    frame_id: i64,
    filename: String,
    uuid: String,
    filter_canonical: String,
}

/// What a publish run does with one frame, decided by the pre-permit split.
enum PublishKind {
    /// First publication.
    New,
    /// An own frame whose recipe moved (or a forced republish, P19).
    Update(crate::db::collab_frames::LocalFrameRow),
    /// A frame the hub already knows as mine but that has no local binding
    /// (an own row the manifest delivered, `source_frame_id` NULL — ruling
    /// R8b): regenerated, checked against the hub's BLAKE3, then bound —
    /// never announced again.
    Adopt(crate::db::collab_frames::LocalFrameRow),
}

impl PublishKind {
    fn row(&self) -> Option<&crate::db::collab_frames::LocalFrameRow> {
        match self {
            PublishKind::New => None,
            PublishKind::Update(r) | PublishKind::Adopt(r) => Some(r),
        }
    }
}

/// One frame the split sends to generation, with its landing path.
struct PlannedFrame {
    cand: PublishCandidate,
    kind: PublishKind,
    target: std::path::PathBuf,
}

/// A generated frame on disk, ready to seed.
struct WrittenFrame {
    frame_id: i64,
    filename: String,
    uuid: String,
    filter_canonical: String,
    kind: PublishKind,
    /// Where the frame lives once published.
    target: std::path::PathBuf,
    /// Where the generator wrote it: `target` for a new frame, the sibling
    /// temp for an update or an adoption.
    staged: std::path::PathBuf,
    recipe: String,
    xxh3: String,
    byte_size: u64,
    /// Manifest fields for a new frame's announce; `None` otherwise.
    meta: Option<crate::collab::frame_meta::FrameMeta>,
    /// An adoption whose regenerated bytes equal the hub's current content:
    /// bound and seeded at the hub's version, no new version.
    identical: bool,
}

/// What the generation phase hands back to the async publish phase.
struct GenerationOutcome {
    written: Vec<WrittenFrame>,
    unchanged: usize,
    held_back: Vec<HeldBackFrame>,
}

/// Everything the blocking generation phase owns.
struct GenerationJob {
    db: crate::db::Database,
    queue: crate::services::compute_queue::ComputeQueue,
    pool: Arc<rayon::ThreadPool>,
    project_id: String,
    label: String,
    plans: Vec<PlannedFrame>,
}

fn held(frame_id: i64, filename: &str, reason: String) -> HeldBackFrame {
    HeldBackFrame {
        frame_id,
        filename: filename.to_string(),
        reasons: vec![reason],
    }
}

/// Remove a regeneration temp; a failure is logged, never silent (a missing
/// file is not a failure).
fn remove_temp(project_id: &str, path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(project_id, path = %path.display(), error = %e, "publish: removing a regeneration temp failed");
        }
    }
}

/// The generation phase of a publish run, on a blocking thread under ONE
/// `ComputeQueue` permit (the `sync_prepare::open_generation` pattern). Only
/// entered when the split found something to generate (ruling R9). Resolves
/// every planned frame in one catalog borrow, stats the masters it reads,
/// then calibrates each exactly once — a new frame straight into its landing
/// path, an update or adoption into a sibling temp whose BLAKE3 decides
/// whether anything changed. A failing frame is held back with its reason;
/// the run goes on.
fn run_publish_generation(job: GenerationJob) -> Result<GenerationOutcome, ApiError> {
    use crate::services::compute_queue::ComputeJobKind;

    let pid = job.project_id.as_str();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (permit, _job_id) = job
        .queue
        .acquire(
            ComputeJobKind::LightCalibration,
            &job.label,
            Arc::clone(&cancel),
        )
        .map_err(|_| {
            tracing::error!(project_id = pid, "publish: compute slot wait cancelled");
            ApiError::Internal("publish: the compute slot wait was cancelled".into())
        })?;

    let opts = publish_options();
    let scratch_dir = std::env::temp_dir();
    let mut held_back: Vec<HeldBackFrame> = Vec::new();
    let mut unchanged = 0usize;
    let mut prepared: Vec<(
        PlannedFrame,
        crate::export::GenerationSpec,
        String,
        Option<crate::collab::frame_meta::FrameMeta>,
    )> = Vec::new();
    {
        let conn = job.db.conn();
        let mut divisors = crate::export::DivisorCache::new();
        let mut master_ok: HashMap<std::path::PathBuf, bool> = HashMap::new();
        for plan in job.plans {
            let fid = plan.cand.frame_id;
            let name = plan.cand.filename.clone();
            let mut spec = match crate::export::resolve_generation_cached(
                &conn,
                fid,
                &opts,
                &scratch_dir,
                &mut divisors,
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(project_id = pid, frame_id = fid, error = %format!("{e:#}"), "publish: cannot calibrate this light");
                    held_back.push(held(fid, &name, format!("cannot calibrate: {e:#}")));
                    continue;
                }
            };
            // The recipe of what is actually generated (equal to the split's
            // by construction; stored with the frame).
            let recipe = match recipe_hash(&conn, &spec, &spec.inputs.light_path) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(project_id = pid, frame_id = fid, error = %format!("{e:#}"), "publish: recipe hash failed");
                    held_back.push(held(
                        fid,
                        &name,
                        format!("cannot read the calibration inputs: {e:#}"),
                    ));
                    continue;
                }
            };
            let missing: Vec<std::path::PathBuf> = crate::export::spec_master_paths(&spec)
                .into_iter()
                .filter(|p| !*master_ok.entry(p.clone()).or_insert_with(|| p.exists()))
                .collect();
            if let Some(path) = missing.first() {
                tracing::error!(project_id = pid, frame_id = fid, path = %path.display(), "publish: master file missing");
                held_back.push(held(
                    fid,
                    &name,
                    format!(
                        "master file missing on disk: {} (archived or moved — restore it, then publish again)",
                        path.display()
                    ),
                ));
                continue;
            }
            if let Err(e) =
                stamp_publish_cards(&conn, &mut spec, pid, fid, &plan.cand.filter_canonical)
            {
                tracing::error!(project_id = pid, frame_id = fid, error = %format!("{e:#}"), "publish: header stamping failed");
                held_back.push(held(fid, &name, format!("cannot build the header: {e:#}")));
                continue;
            }
            let meta = match plan.kind {
                PublishKind::New => match crate::collab::frame_meta::build_frame_meta(&conn, fid) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::error!(project_id = pid, frame_id = fid, error = %format!("{e:#}"), "publish: frame meta failed");
                        held_back.push(held(
                            fid,
                            &name,
                            format!("cannot read frame metadata: {e:#}"),
                        ));
                        continue;
                    }
                },
                _ => None,
            };
            prepared.push((plan, spec, recipe, meta));
        }
    }
    tracing::info!(
        project_id = pid,
        count = prepared.len(),
        "publish: calibrated-light generation planned"
    );

    // Pixel phase: no catalog connection held.
    let mut hot_maps = HashMap::new();
    let mut written: Vec<WrittenFrame> = Vec::new();
    let mut identical: Vec<(String, String)> = Vec::new();
    for (plan, spec, recipe, meta) in prepared {
        let PlannedFrame { cand, kind, target } = plan;
        let fid = cand.frame_id;
        let is_temp = kind.row().is_some();
        let staged = if is_temp {
            update_temp_path(&target)
        } else {
            target.clone()
        };
        let generated = match crate::export::execute_generation(
            &spec,
            &staged,
            None,
            &scratch_dir,
            &opts,
            &mut hot_maps,
            None,
            Some(&job.pool),
            &cancel,
        ) {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(project_id = pid, frame_id = fid, dest = %staged.display(), error = %format!("{e:#}"), "publish: calibration failed");
                held_back.push(held(
                    fid,
                    &cand.filename,
                    format!("calibration failed: {e:#}"),
                ));
                continue;
            }
        };
        for note in &generated.warnings {
            tracing::warn!(project_id = pid, frame_id = fid, note = %note, "publish: calibrated light written with a warning");
        }
        let xxh3 = match xxh3_full_file(&staged) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(project_id = pid, frame_id = fid, path = %staged.display(), error = %format!("{e:#}"), "publish: hashing the calibrated light failed");
                if is_temp {
                    remove_temp(pid, &staged);
                }
                held_back.push(held(
                    fid,
                    &cand.filename,
                    format!("cannot hash the calibrated light: {e:#}"),
                ));
                continue;
            }
        };
        let mut same_as_hub = false;
        if let Some(row) = kind.row() {
            match blake3_file(&staged) {
                Ok(b) => same_as_hub = b == row.blake3,
                Err(e) => {
                    tracing::error!(project_id = pid, frame_id = fid, path = %staged.display(), error = %format!("{e:#}"), "publish: hashing the regenerated light failed");
                    remove_temp(pid, &staged);
                    held_back.push(held(
                        fid,
                        &cand.filename,
                        format!("cannot hash the calibrated light: {e:#}"),
                    ));
                    continue;
                }
            }
        }
        if same_as_hub {
            if let PublishKind::Update(row) = &kind {
                // P19: identical pixels — only the recipe moves. No version,
                // no re-seed, no holder change.
                remove_temp(pid, &staged);
                identical.push((row.frame_uuid.clone(), recipe));
                continue;
            }
        }
        tracing::debug!(
            project_id = pid,
            frame_id = fid,
            dest = %staged.display(),
            bytes = generated.byte_size,
            "publish: calibrated light written"
        );
        written.push(WrittenFrame {
            frame_id: fid,
            filename: cand.filename,
            // An update or adoption keeps the uuid the hub knows the frame by.
            uuid: match kind.row() {
                Some(row) => row.frame_uuid.clone(),
                None => cand.uuid,
            },
            filter_canonical: cand.filter_canonical,
            kind,
            target,
            staged,
            recipe,
            xxh3,
            byte_size: generated.byte_size,
            meta,
            identical: same_as_hub,
        });
    }
    drop(permit);

    if !identical.is_empty() {
        let conn = job.db.conn();
        for (frame_uuid, recipe) in identical {
            if let Err(e) =
                crate::db::collab_frames::set_recipe_hash(&conn, pid, &frame_uuid, &recipe)
            {
                tracing::error!(project_id = pid, frame_uuid = %frame_uuid, error = %format!("{e:#}"), "publish: storing the new recipe failed");
            }
            unchanged += 1;
        }
    }

    Ok(GenerationOutcome {
        written,
        unchanged,
        held_back,
    })
}

/// A frame seeded and ready to announce, version or bind.
struct SeededFrame {
    written: WrittenFrame,
    blake3: String,
    content_version: i32,
}

/// Build the announce wire row of a new frame.
fn frame_in_wire(f: &SeededFrame, gate_version: i32) -> crate::collab::hub_client::FrameInWire {
    let meta = f.written.meta.as_ref();
    crate::collab::hub_client::FrameInWire {
        frame_uuid: f.written.uuid.clone(),
        file_name: f
            .written
            .target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        blake3: f.blake3.clone(),
        byte_size: f.written.byte_size as i64,
        xxh3: f.written.xxh3.clone(),
        filter_raw: meta.map(|m| m.filter_raw.clone()).unwrap_or_default(),
        filter_canonical: f.written.filter_canonical.clone(),
        channel: meta.map(|m| m.channel.clone()).unwrap_or_default(),
        exptime_sec: meta.map(|m| m.exptime_sec).unwrap_or_default(),
        date_obs: meta.and_then(|m| m.date_obs.clone()),
        gate_version,
        meta: meta
            .map(|m| m.meta.clone())
            .unwrap_or_else(|| serde_json::json!({})),
    }
}

/// `"size:mtime_secs"` of a landed file, for `size_mtime_seen`.
fn size_mtime_seen(path: &Path) -> Option<String> {
    match size_and_mtime(path) {
        Ok((size, _, secs)) => Some(format!("{size}:{secs}")),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "publish: stat of the landed frame failed");
            None
        }
    }
}

/// Is this hub refusal the stale-gate-version 409 (`gate version X is stale,
/// current is Y`)?
fn is_stale_gate_refusal(e: &crate::account::AccountClientError) -> bool {
    matches!(e, crate::account::AccountClientError::Network(m)
        if m.contains("gate version") && m.contains("is stale"))
}

/// Indices of the batch frames a 409 `frame {uuid} already announced; use
/// /version …` names (ruling R8a). Empty for any other refusal.
fn already_announced_in(
    e: &crate::account::AccountClientError,
    batch: &[SeededFrame],
) -> Vec<usize> {
    let crate::account::AccountClientError::Network(m) = e else {
        return Vec::new();
    };
    batch
        .iter()
        .enumerate()
        .filter(|(_, f)| m.contains(&format!("frame {} already announced", f.written.uuid)))
        .map(|(i, _)| i)
        .collect()
}

/// Split announce-ready frames into hub batches of at most [`ANNOUNCE_BATCH`].
fn announce_batches<T>(mut rest: Vec<T>) -> std::collections::VecDeque<Vec<T>> {
    let mut batches = std::collections::VecDeque::new();
    while !rest.is_empty() {
        let tail = rest.split_off(rest.len().min(ANNOUNCE_BATCH));
        batches.push_back(rest);
        rest = tail;
    }
    batches
}

/// Re-fetch a project's thresholds and write them into the cache (the
/// stale-gate retry). Returns the refreshed row.
async fn refresh_project_thresholds(
    ctx: &ServiceContext,
    client: &CollabClient,
    token: &str,
    project_id: &str,
) -> Result<CollabProjectRow, ApiError> {
    let wire = client.thresholds(token, project_id).await.map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: thresholds refresh failed");
        client_err(e)
    })?;
    let db = db(ctx)?;
    let conn = db.conn();
    let mut row = crate::db::collab::get_project(&conn, project_id)
        .map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the project failed");
            internal(e)
        })?
        .ok_or_else(|| {
            tracing::error!(project_id, "publish: project vanished from the cache");
            ApiError::NotFound(format!(
                "project {project_id} is not cached — refresh first"
            ))
        })?;
    match wire.current {
        Some(set) => {
            row.thresholds_version = Some(set.version);
            row.thresholds_rules_json = Some(serde_json::to_string(&set.rules).map_err(|e| {
                tracing::error!(project_id, error = %e, "publish: encoding threshold rules failed");
                ApiError::Internal(format!("encode threshold rules: {e}"))
            })?);
        }
        None => {
            row.thresholds_version = None;
            row.thresholds_rules_json = None;
        }
    }
    crate::db::collab::upsert_project(&conn, &row).map_err(|e| {
        tracing::error!(project_id, error = %format!("{e:#}"), "publish: caching refreshed thresholds failed");
        internal(e)
    })?;
    tracing::info!(
        project_id,
        version = row.thresholds_version,
        "publish: thresholds refreshed"
    );
    Ok(row)
}

/// Re-run the gate for the stale-gate retry: frame id → verdict.
fn regate(
    ctx: &ServiceContext,
    project: &CollabProjectRow,
) -> Result<HashMap<i64, FrameGateRow>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(project_gate(&conn, project)
        .map_err(|e| {
            tracing::error!(project_id = %project.project_id, error = %e, "publish: re-running the gate failed");
            e
        })?
        .into_iter()
        .map(|(_, r)| (r.frame_id, r))
        .collect())
}

/// Drop every seed tag of these frames.
async fn unseed_all(
    node: &crate::sharing::iroh::node::SharedIrohNode,
    project_id: &str,
    frames: &[&SeededFrame],
) {
    for f in frames {
        if let Err(e) = node.unseed_project_frame(project_id, &f.written.uuid).await {
            tracing::warn!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: unseed failed");
        }
    }
}

/// Unseed and hold back every frame of a failed batch (F5).
async fn fail_batch(
    node: &crate::sharing::iroh::node::SharedIrohNode,
    project_id: &str,
    batch: &[SeededFrame],
    reason: &str,
    held_back: &mut Vec<HeldBackFrame>,
) {
    let refs: Vec<&SeededFrame> = batch.iter().collect();
    unseed_all(node, project_id, &refs).await;
    for f in batch {
        held_back.push(held(
            f.written.frame_id,
            &f.written.filename,
            reason.to_string(),
        ));
    }
}

/// Publish a project's gate-passing calibrated lights, one frame at a time
/// (collab v3 wave 2, §5.2): calibrate each light ONCE straight into my own
/// folder under the Collaboration root, seed it into the collab store by
/// reference, announce new frames in batches of at most 500, post a new
/// content version for an own frame whose recipe changed (P19), and record
/// my own rows. Refuses without a Collaboration root (P25).
pub async fn publish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    run_publish(ctx, project_id, emitter, false, None).await
}

/// The manual re-publish (P19): every own frame is regenerated as an
/// `update`, then the same-hash rule applies — only frames whose bytes
/// changed get a new content version. The remedy after an app release that
/// changed the calibration engine or its defaults.
pub async fn republish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    run_publish(ctx, project_id, emitter, true, None).await
}

/// A test seam run right after the split, with the catalog connection — how
/// a test stands in for a manifest sync landing between split and write-back.
type AfterSplit<'a> = Option<&'a (dyn Fn(&Connection) + Sync)>;

async fn run_publish(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    force: bool,
    after_split: AfterSplit<'_>,
) -> Result<PublishResult, ApiError> {
    use crate::account::AccountClientError as E;
    use crate::db::collab_frames::{self as frames_db, FrameOrigin};

    // ── 1. Collaboration root, then the gate ─────────────────────────────────
    let collab_root = require_collaboration_root(ctx)?;
    let (project, gated) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let project = crate::db::collab::get_project(&conn, project_id)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!(
                    "project {project_id} is not cached — refresh first"
                ))
            })?;
        let gated = project_gate(&conn, &project)?;
        (project, gated)
    };
    let mut held_back: Vec<HeldBackFrame> = Vec::new();
    let mut candidates: Vec<PublishCandidate> = Vec::new();
    for (id, row) in gated {
        if row.publishable {
            candidates.push(PublishCandidate {
                frame_id: row.frame_id,
                filename: row.filename,
                uuid: id.uuid,
                // `publishable` implies a dictionary match (P3).
                filter_canonical: id.filter_canonical.unwrap_or_default(),
            });
        } else {
            held_back.push(HeldBackFrame {
                frame_id: row.frame_id,
                filename: row.filename,
                reasons: row.failures,
            });
        }
    }
    if candidates.is_empty() {
        tracing::info!(
            project_id,
            held_back = held_back.len(),
            "publish: nothing publishable"
        );
        return Ok(PublishResult {
            announced: 0,
            updated: 0,
            state: None,
            held_back,
            unchanged: 0,
        });
    }

    // ── 2. Hub, node, and my own identity in the project ─────────────────────
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::warn!(project_id, "publish refused: signed out");
        return Err(ApiError::SignedOut(
            "Sign in to publish to a project.".into(),
        ));
    };
    let client = CollabClient::new(&hub_url).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: hub client failed");
        client_err(e)
    })?;
    let node = crate::api::sync::ensure_iroh_node(ctx).await.map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: iroh node unavailable");
        e
    })?;
    let dirs = crate::api::sync::sync_dirs(ctx).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: sync dirs unavailable");
        e
    })?;
    let own_node = DeviceKey::load_or_create(&device_key_path(&dirs.identity_dir))
        .map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: device key unavailable");
            ApiError::Internal(format!("device key: {e:#}"))
        })?
        .node_id();
    let (account_id, display) = {
        let members: Vec<SnapshotMember> = serde_json::from_str(&project.members_json)
            .unwrap_or_else(|e| {
                tracing::warn!(project_id, error = %e, "publish: members_json does not parse");
                Vec::new()
            });
        match members
            .iter()
            .find(|m| member_node_ids(m).iter().any(|n| n == &own_node))
        {
            Some(m) => (m.account_id.clone(), m.display_name.clone()),
            None => {
                tracing::warn!(
                    project_id,
                    "publish: this device is not in the project snapshot; publishing under \"own\""
                );
                (String::new(), String::new())
            }
        }
    };

    // ── 3. The split (ruling R9: before any compute permit) ─────────────────
    let opts = publish_options();
    let mut unchanged = 0usize;
    let mut plans: Vec<PlannedFrame> = Vec::new();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let own_dir = publisher_folder(&conn, &collab_root, &project, &account_id, &display, "own")
            .map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: own folder failed");
                internal(e)
            })?;
        let own = frames_db::own_by_source_frame(&conn, project_id).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: read own frames failed");
            internal(e)
        })?;
        let mut claimed: HashSet<std::path::PathBuf> = HashSet::new();
        for cand in candidates {
            let fid = cand.frame_id;
            let resolved = match crate::calibration_library::light_resolve::resolve_frame_inputs(
                &conn,
                fid,
                opts.flat_norm,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: cannot resolve this light");
                    held_back.push(held(
                        fid,
                        &cand.filename,
                        format!("cannot calibrate: {e:#}"),
                    ));
                    continue;
                }
            };
            let recipe = match recipe_hash_of_inputs(&conn, &resolved) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: recipe hash failed");
                    held_back.push(held(
                        fid,
                        &cand.filename,
                        format!("cannot read the calibration inputs: {e:#}"),
                    ));
                    continue;
                }
            };
            let kind = match own.get(&fid) {
                Some(row) if force || row.recipe_hash.as_deref() != Some(recipe.as_str()) => {
                    PublishKind::Update(row.clone())
                }
                Some(_) => {
                    unchanged += 1;
                    continue;
                }
                None => match frames_db::get(&conn, project_id, &cand.uuid) {
                    Ok(Some(row))
                        if row.origin == FrameOrigin::Own && row.source_frame_id.is_none() =>
                    {
                        PublishKind::Adopt(row)
                    }
                    Ok(_) => PublishKind::New,
                    Err(e) => {
                        tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: read frame row failed");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!("cannot read the frame's project row: {e:#}"),
                        ));
                        continue;
                    }
                },
            };
            let target = match &kind {
                PublishKind::New => {
                    let name = crate::export::calibrated_output_filename(
                        &cand.filename,
                        opts.debayer_osc && resolved.cfa_geometry.is_some(),
                        opts.format,
                    );
                    let target = match new_frame_target(&conn, &own_dir, &name, &mut claimed) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: no landing path");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!("no landing path: {e:#}"),
                            ));
                            continue;
                        }
                    };
                    let file_name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if let Some(problem) = hub_file_name_problem(&file_name) {
                        tracing::warn!(project_id, frame_id = fid, path = %target.display(), reason = problem, "publish: file name breaks the hub rule");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!("the hub refuses this file name ({problem}): {file_name}"),
                        ));
                        continue;
                    }
                    target
                }
                PublishKind::Update(row) => match &row.landed_path {
                    Some(p) => std::path::PathBuf::from(p),
                    None => {
                        tracing::error!(project_id, frame_id = fid, frame_uuid = %row.frame_uuid, "publish: own frame has no landed path");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            "own frame has no file path".into(),
                        ));
                        continue;
                    }
                },
                PublishKind::Adopt(row) => row
                    .landed_path
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| own_dir.join(&row.file_name)),
            };
            plans.push(PlannedFrame { cand, kind, target });
        }
        if let Some(hook) = after_split {
            hook(&conn);
        }
    }

    // ── 4. Generation: one compute permit, only when there is work ───────────
    let outcome = if plans.is_empty() {
        GenerationOutcome {
            written: Vec::new(),
            unchanged: 0,
            held_back: Vec::new(),
        }
    } else {
        let job = GenerationJob {
            db: db(ctx)?.clone(),
            queue: ctx.compute_queue.clone(),
            pool: Arc::clone(&ctx.image_pool),
            project_id: project_id.to_string(),
            label: format!("collab publish {}", project.title),
            plans,
        };
        tokio::task::spawn_blocking(move || run_publish_generation(job))
            .await
            .map_err(|e| {
                tracing::error!(project_id, error = %e, "publish: generation task failed");
                ApiError::Internal(format!("publish generation task: {e}"))
            })??
    };
    held_back.extend(outcome.held_back);
    unchanged += outcome.unchanged;

    // ── Seed by reference ────────────────────────────────────────────────────
    let mut new_frames: Vec<SeededFrame> = Vec::new();
    let mut updates: Vec<SeededFrame> = Vec::new();
    let mut bound: Vec<SeededFrame> = Vec::new();
    for w in outcome.written {
        let prior_version = w.kind.row().map(|r| r.content_version);
        let Some(prior_version) = prior_version else {
            match node
                .seed_project_frame(project_id, &w.uuid, 1, &w.target)
                .await
            {
                Ok(hash) => new_frames.push(SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: 1,
                    written: w,
                }),
                Err(e) => {
                    tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: seeding a new frame failed; not announced");
                    held_back.push(held(
                        w.frame_id,
                        &w.filename,
                        format!("seeding failed: {e:#}"),
                    ));
                }
            }
            continue;
        };
        let version = if w.identical {
            prior_version
        } else {
            // The old tag must not pin the old content once the landed file
            // is replaced.
            if let Err(e) = node.unseed_project_frame(project_id, &w.uuid).await {
                tracing::warn!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: unseeding the previous version failed");
            }
            prior_version + 1
        };
        if let Err(e) = std::fs::rename(&w.staged, &w.target) {
            tracing::error!(project_id, frame_uuid = %w.uuid, src = %w.staged.display(), dest = %w.target.display(), error = %e, "publish: replacing the landed frame failed");
            remove_temp(project_id, &w.staged);
            held_back.push(held(
                w.frame_id,
                &w.filename,
                format!("cannot replace the published file: {e}"),
            ));
            continue;
        }
        match node
            .seed_project_frame(project_id, &w.uuid, version, &w.target)
            .await
        {
            Ok(hash) => {
                let f = SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: version,
                    written: w,
                };
                if f.written.identical {
                    bound.push(f);
                } else {
                    updates.push(f);
                }
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: seeding failed");
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    format!("seeding failed: {e:#}"),
                ));
            }
        }
    }

    // ── 5. Announce new frames, ≤ 500 per batch ──────────────────────────────
    let mut gate_version: i32 = project.thresholds_version.unwrap_or(0);
    let mut state: Option<String> = None;
    let mut announced: Vec<(SeededFrame, String, i32)> = Vec::new();
    let mut hub_adopted: Vec<(SeededFrame, i32)> = Vec::new();
    let mut first_err: Option<ApiError> = None;
    let mut stale_retried = false;
    let mut outdated = false;
    let mut batches = announce_batches(new_frames);
    'batches: while let Some(mut batch) = batches.pop_front() {
        loop {
            if batch.is_empty() {
                break;
            }
            let wire: Vec<_> = batch
                .iter()
                .map(|f| frame_in_wire(f, gate_version))
                .collect();
            match client.announce_frames(&token, project_id, &wire).await {
                Ok(resp) => {
                    state = Some(resp.state.clone());
                    tracing::info!(project_id, count = batch.len(), state = %resp.state, "publish: frames announced");
                    announced.extend(
                        batch
                            .drain(..)
                            .map(|f| (f, resp.state.clone(), gate_version)),
                    );
                    break;
                }
                Err(E::CollabApiOutdated) => {
                    // Nothing more goes out: roll back every seed that is not
                    // yet announced; what the hub already took is recorded
                    // below so it never becomes an orphan.
                    tracing::error!(
                        project_id,
                        outcome = "collab_api_outdated",
                        "publish: hub refused an outdated collab api; rolling back unsent seeds"
                    );
                    outdated = true;
                    let mut unsent: Vec<&SeededFrame> = batch.iter().collect();
                    unsent.extend(batches.iter().flatten());
                    unsent.extend(updates.iter());
                    unseed_all(&node, project_id, &unsent).await;
                    break 'batches;
                }
                Err(e) if !already_announced_in(&e, &batch).is_empty() => {
                    // R8a: the hub already holds these uuids as mine (an
                    // announce whose reply was lost, or whose row was never
                    // recorded). Adopt them and retry the rest — each retry
                    // drops at least one frame, so the loop is bounded by the
                    // batch size.
                    let named = already_announced_in(&e, &batch);
                    tracing::warn!(project_id, count = named.len(), error = %e, "publish: frames already announced; adopting them and retrying the rest");
                    let mut i = 0usize;
                    let mut keep = Vec::with_capacity(batch.len());
                    for f in batch.drain(..) {
                        if named.contains(&i) {
                            hub_adopted.push((f, gate_version));
                        } else {
                            keep.push(f);
                        }
                        i += 1;
                    }
                    batch = keep;
                }
                Err(e) if !stale_retried && is_stale_gate_refusal(&e) => {
                    stale_retried = true;
                    tracing::warn!(project_id, version = gate_version, error = %e, "publish: stale gate version; refreshing thresholds and retrying once");
                    let verdicts =
                        match refresh_project_thresholds(ctx, &client, &token, project_id).await {
                            Ok(refreshed) => {
                                gate_version = refreshed.thresholds_version.unwrap_or(0);
                                regate(ctx, &refreshed)
                            }
                            Err(err) => Err(err),
                        };
                    let verdicts = match verdicts {
                        Ok(v) => v,
                        Err(err) => {
                            fail_batch(
                                &node,
                                project_id,
                                &batch,
                                &format!("announce failed: {err}"),
                                &mut held_back,
                            )
                            .await;
                            first_err.get_or_insert(err);
                            break;
                        }
                    };
                    // The refreshed verdicts apply to this batch AND every
                    // queued one: a frame that no longer passes leaves.
                    let passes = |f: &SeededFrame| {
                        verdicts
                            .get(&f.written.frame_id)
                            .is_some_and(|r| r.publishable)
                    };
                    let mut dropped: Vec<SeededFrame> = Vec::new();
                    let (keep, drop_now): (Vec<_>, Vec<_>) =
                        batch.into_iter().partition(|f| passes(f));
                    batch = keep;
                    dropped.extend(drop_now);
                    for queued in batches.iter_mut() {
                        let (keep, drop_now): (Vec<_>, Vec<_>) =
                            std::mem::take(queued).into_iter().partition(|f| passes(f));
                        *queued = keep;
                        dropped.extend(drop_now);
                    }
                    let refs: Vec<&SeededFrame> = dropped.iter().collect();
                    unseed_all(&node, project_id, &refs).await;
                    for f in &dropped {
                        let reasons = verdicts
                            .get(&f.written.frame_id)
                            .map(|r| r.failures.clone())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| {
                                vec!["no longer in the project's linked sets".into()]
                            });
                        held_back.push(HeldBackFrame {
                            frame_id: f.written.frame_id,
                            filename: f.written.filename.clone(),
                            reasons,
                        });
                    }
                }
                Err(e) => {
                    // F5: nothing of this batch is announced — stop seeding
                    // it. The files stay; own rows are recorded only after a
                    // successful announce, so the next run re-announces them.
                    tracing::error!(project_id, count = batch.len(), error = %e, "publish: announce failed");
                    fail_batch(
                        &node,
                        project_id,
                        &batch,
                        &format!("announce failed: {e}"),
                        &mut held_back,
                    )
                    .await;
                    first_err.get_or_insert(client_err(e));
                    break;
                }
            }
        }
    }

    // ── 6. New content versions ──────────────────────────────────────────────
    let mut versioned: Vec<SeededFrame> = Vec::new();
    if !outdated {
        let mut pending = updates.into_iter();
        while let Some(mut f) = pending.next() {
            match client
                .new_frame_version(
                    &token,
                    project_id,
                    &f.written.uuid,
                    &f.blake3,
                    f.written.byte_size as i64,
                    &f.written.xxh3,
                )
                .await
            {
                Ok(v) => {
                    if v.content_version != f.content_version {
                        tracing::warn!(project_id, frame_uuid = %f.written.uuid, content_version = v.content_version, expected = f.content_version, "publish: hub assigned a different content version; re-tagging");
                        if let Err(e) = node
                            .seed_project_frame(
                                project_id,
                                &f.written.uuid,
                                v.content_version,
                                &f.written.target,
                            )
                            .await
                        {
                            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: re-tagging under the hub's version failed");
                        }
                        f.content_version = v.content_version;
                    }
                    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = f.content_version, "publish: new frame version");
                    versioned.push(f);
                }
                Err(e) => {
                    tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %e, "publish: new frame version failed");
                    unseed_all(&node, project_id, &[&f]).await;
                    held_back.push(held(
                        f.written.frame_id,
                        &f.written.filename,
                        format!("new version failed: {e}"),
                    ));
                    if matches!(e, E::CollabApiOutdated) {
                        outdated = true;
                        let rest: Vec<SeededFrame> = pending.by_ref().collect();
                        let refs: Vec<&SeededFrame> = rest.iter().collect();
                        unseed_all(&node, project_id, &refs).await;
                    }
                    first_err.get_or_insert(client_err(e));
                }
            }
        }
    }

    // ── 7. Own rows, per frame (never aborts the run), then holders ──────────
    let mut holders: Vec<crate::collab::hub_client::HolderRefWire> = Vec::new();
    let mut announced_n = 0usize;
    let mut updated_n = 0usize;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let mut record_failed = |f: &SeededFrame, what: &str, e: &anyhow::Error| {
            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the own frame failed");
            held_back.push(held(
                f.written.frame_id,
                &f.written.filename,
                format!("{what}, but recording it locally failed: {e:#}"),
            ));
        };
        let new_row = |f: &SeededFrame, frame_state: &str, gv: i32, accepted: bool| {
            let w = &f.written;
            let wire = frame_in_wire(f, gv);
            let mut manifest = serde_json::to_value(&wire).unwrap_or_else(|e| {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %e, "publish: encoding the manifest row failed");
                serde_json::json!({})
            });
            if let Some(obj) = manifest.as_object_mut() {
                obj.insert("publisherAccountId".into(), serde_json::json!(account_id));
                obj.insert("publisherDisplayName".into(), serde_json::json!(display));
                obj.insert("own".into(), serde_json::json!(true));
                obj.insert(
                    "contentVersion".into(),
                    serde_json::json!(f.content_version),
                );
                obj.insert("accepted".into(), serde_json::json!(accepted));
                obj.insert("state".into(), serde_json::json!(frame_state));
                obj.insert("manifestVersion".into(), serde_json::json!(0));
                obj.insert(
                    "createdAt".into(),
                    serde_json::json!(crate::sync::now_iso()),
                );
                obj.insert("holderCount".into(), serde_json::json!(1));
            }
            frames_db::LocalFrameRow {
                project_id: project_id.to_string(),
                frame_uuid: w.uuid.clone(),
                content_version: f.content_version,
                origin: FrameOrigin::Own,
                publisher_account_id: account_id.clone(),
                publisher_display: display.clone(),
                file_name: wire.file_name.clone(),
                filter_canonical: w.filter_canonical.clone(),
                state: frame_state.to_string(),
                accepted,
                byte_size: w.byte_size as i64,
                xxh3: w.xxh3.clone(),
                blake3: f.blake3.clone(),
                holder_count: 1,
                manifest_version: 0,
                manifest_json: manifest.to_string(),
                landed_path: Some(w.target.to_string_lossy().to_string()),
                size_mtime_seen: size_mtime_seen(&w.target),
                on_disk: true,
                locally_declined: false,
                awaiting_gc: false,
                source_frame_id: Some(w.frame_id),
                recipe_hash: Some(w.recipe.clone()),
                last_error: None,
                updated_at: String::new(),
            }
        };
        let adopt = |f: &SeededFrame| -> anyhow::Result<()> {
            let w = &f.written;
            let n = frames_db::adopt_own(
                &conn,
                project_id,
                &w.uuid,
                w.frame_id,
                &w.target.to_string_lossy(),
                &w.recipe,
                size_mtime_seen(&w.target).as_deref(),
            )?;
            if n == 0 {
                anyhow::bail!("no own row for frame {} to bind", w.uuid);
            }
            Ok(())
        };
        let holder = |f: &SeededFrame| crate::collab::hub_client::HolderRefWire {
            frame_uuid: f.written.uuid.clone(),
            content_version: f.content_version,
        };

        for (f, frame_state, gv) in &announced {
            match frames_db::record_own(&conn, &new_row(f, frame_state, *gv, true)) {
                Ok(()) => {
                    holders.push(holder(f));
                    announced_n += 1;
                }
                Err(e) => record_failed(f, "announced", &e),
            }
        }
        // R8a: the hub already had these as mine. Bind an existing own row
        // (a manifest sync got there first), else record one from the local
        // plan with the state unknown — the manifest sync sets the hub truth.
        for (f, gv) in &hub_adopted {
            let result = match frames_db::get(&conn, project_id, &f.written.uuid) {
                Ok(Some(row)) if row.origin == FrameOrigin::Own => adopt(f),
                Ok(Some(_)) => Err(anyhow::anyhow!(
                    "frame {} is cached as another publisher's",
                    f.written.uuid
                )),
                Ok(None) => frames_db::record_own(&conn, &new_row(f, "unknown", *gv, false)),
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => {
                    holders.push(holder(f));
                    announced_n += 1;
                }
                Err(e) => record_failed(f, "already announced", &e),
            }
        }
        // R8b, identical bytes: bound and seeded at the hub's version.
        for f in &bound {
            match adopt(f) {
                Ok(()) => {
                    holders.push(holder(f));
                    unchanged += 1;
                }
                Err(e) => record_failed(f, "already published", &e),
            }
        }
        for f in &versioned {
            let w = &f.written;
            if matches!(w.kind, PublishKind::Adopt(_)) {
                if let Err(e) = adopt(f) {
                    record_failed(f, "versioned", &e);
                    continue;
                }
            }
            match frames_db::set_own_version(
                &conn,
                project_id,
                &w.uuid,
                f.content_version,
                &f.blake3,
                &w.xxh3,
                w.byte_size as i64,
                &w.recipe,
                size_mtime_seen(&w.target).as_deref(),
            ) {
                Ok(_) => {
                    holders.push(holder(f));
                    updated_n += 1;
                }
                Err(e) => record_failed(f, "versioned", &e),
            }
        }
    }

    if !holders.is_empty() {
        // Folded after logging: the 20-minute full holder report repairs a
        // missed delta (P8).
        if let Err(e) = client
            .put_holders(&token, project_id, false, &holders, &[])
            .await
        {
            tracing::warn!(project_id, count = holders.len(), error = %e, "publish: holder delta failed");
        }
    }

    // ── 8. One outcome event + log ───────────────────────────────────────────
    if let Some(em) = emitter.as_ref() {
        em.emit_json(
            COLLAB_PUBLISHED_EVENT,
            serde_json::json!({
                "projectId": project_id,
                "announced": announced_n,
                "updated": updated_n,
                "heldBack": held_back.len(),
            }),
        );
    }
    tracing::info!(
        project_id,
        count = announced_n,
        updated = updated_n,
        held_back = held_back.len(),
        unchanged,
        "frames published"
    );
    if outdated {
        return Err(client_err(E::CollabApiOutdated));
    }
    if announced_n == 0 && updated_n == 0 {
        if let Some(err) = first_err {
            return Err(err);
        }
    }
    Ok(PublishResult {
        announced: announced_n,
        updated: updated_n,
        state,
        held_back,
        unchanged,
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
        #[allow(deprecated)] // collab v3: `approve_announcement` removed in wave 2 Task 11
        let resp = client
            .approve_announcement(&token, announcement_id)
            .await
            .map_err(decide_err)?;
        {
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
        }
        // Seed the copy the approval just published (D3 §3.4 / F2). A review copy
        // lands while the announcement is still pending, so its post-ingest seed
        // was skipped by the state gate and NOTHING else would ever seed it — the
        // need diff skips locally-complete packages. Awaited rather than spawned:
        // this boundary holds a `&ServiceContext` (no `'static` handle to hand a
        // task), and the work is local — hard-link the seed dir, import it. Never
        // fatal: `seed_approved_announcement` logs and returns, so a seed failure
        // cannot turn a successful decision into a reported failure.
        crate::api::collab_exchange::seed_approved_announcement(ctx, announcement_id).await;
    } else {
        let reason = reason.expect("the reject path validates a reason above");
        #[allow(deprecated)] // collab v3: `reject_announcement` removed in wave 2 Task 11
        let resp = client
            .reject_announcement(&token, announcement_id, &reason)
            .await
            .map_err(decide_err)?;
        // The package the reject tore down, if any — unseeded right after the DB
        // borrow closes (the unseed awaits).
        let deleted: Option<(String, String)> = {
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
                    Some((pkg.project_id, pkg.package_id))
                }
                None => {
                    tracing::info!(announcement_id, hub_state = %resp.state, "rejected announcement; no local review copy to delete");
                    None
                }
            }
        };
        // The review copy's landed files are gone, so the seed that REFERENCED
        // them must go with them (D3 T4) — and a rejected package must not stay
        // servable to anyone in any case.
        if let Some((project_id, package_id)) = deleted {
            crate::api::collab_exchange::unseed_package_local_data(ctx, &project_id, &package_id)
                .await;
        }
    }
    Ok(())
}

// The Collaboration-root guard (P25) lives beside the replication pass, which
// compiles headless; publish uses it through this re-export.
#[cfg(test)]
pub(crate) use crate::api::collab_exchange::COLLABORATION_ROOT_REQUIRED;
pub(crate) use crate::api::collab_exchange::{publisher_folder, require_collaboration_root};

#[cfg(test)]
mod tests {
    use super::*;

    /// P25: without a Collaboration root, the collab paths refuse with the
    /// one actionable message; with one, they get its path.
    #[tokio::test]
    async fn require_collaboration_root_refuses_until_one_is_set() {
        let (_tmp, ctx) = test_ctx();
        match require_collaboration_root(&ctx) {
            Err(ApiError::Invalid(m)) => assert_eq!(m, COLLABORATION_ROOT_REQUIRED),
            other => panic!("expected Invalid(P25 message), got {other:?}"),
        }
        let root = tempfile::tempdir().unwrap();
        let stored = crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.path().to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap();
        assert_eq!(
            require_collaboration_root(&ctx).unwrap(),
            std::path::PathBuf::from(stored)
        );
    }
    // The production module no longer imports base64 (the `member_node_ids`
    // helper that used it moved to `crate::collab::snapshot`); the tests still
    // encode node ids into snapshot fixtures.
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    use crate::db::collab_exchange::{upsert_package, PackageRow};
    use crate::sharing::types::NodeId;

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
        };
        (tmp, ctx)
    }

    /// Cached project fixture: target M101 (210.8, +54.35), radius 1.5°, one
    /// threshold rule (reject trailed frames), a dictionary that recognizes
    /// `seed_set`'s `L` filter (P3) so the gate tests below aren't ALSO
    /// blocked by an unmapped filter.
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
                pending_frames: 0,
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
                gov_caps_json: "[]".into(),
                // all ignored on write (local preference / sync-state / dictionary)
                auto_replicate: true,
                synced_caps_json: "[]".into(),
                hub_version: 0,
                manifest_cursor: 0,
                dictionary_version: Some(1),
                dictionary_json: Some(
                    r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#.into(),
                ),
                policy_json: r#"{"mode":"all"}"#.into(),
                replication_paused: false,
                auto_publish: true,
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
                "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, exptime, filter, uuid) \
                 VALUES (?1, 'Light', 'M101', 'ASI2600MM', ?2, ?3, 3.76, 1000.0, 300.0, 'L', ?4)",
                rusqlite::params![file_id, ra_deg, dec_deg, format!("uuid-seed-{name}-{i}")],
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
        assert_eq!(
            report.publishable, 0,
            "P7: neither frame has a calibration link, so both fail the real \
             readiness gate"
        );

        // Frame 0: blocked only by calibration. Frame 1: calibration + trailed.
        let row0 = report
            .rows
            .iter()
            .find(|r| r.frame_id == frames[0])
            .unwrap();
        assert!(
            row0.failures
                .iter()
                .any(|f| f.contains("calibration links")),
            "{:?}",
            row0.failures
        );
        assert!(!row0.failures.iter().any(|f| f.contains("trailed")));
        assert_eq!(row0.stars_detected, Some(400));
        // 2.0 px × ~0.776 ″/px (header fallback, no binning multiply).
        let scale = ((3.76f64 / 1000.0) / 1000.0).atan().to_degrees() * 3600.0;
        assert!((row0.fwhm_arcsec.unwrap() - 2.0 * scale).abs() < 1e-6);

        let row1 = report
            .rows
            .iter()
            .find(|r| r.frame_id == frames[1])
            .unwrap();
        assert!(
            row1.failures.iter().any(|f| f.contains("trailed")),
            "{:?}",
            row1.failures
        );

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
            let (set_id, frames) = seed_set(
                &conn,
                "Off-header set",
                "06:40:00",
                "+10:00:00",
                100.0,
                10.0,
                1,
            );
            // Solve: scale 1.5″/px (≠ the ~0.776″/px header fallback), center
            // on target (210.8, +54.35).
            seed_plate_solve(&conn, frames[0], 1.5, 210.8, 54.35);
            (set_id, frames)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 1);
        let row = report
            .rows
            .iter()
            .find(|r| r.frame_id == frames[0])
            .unwrap();

        // Scale from the solve: 2.0 px × 1.5 ″/px = 3.0″ (NOT 2.0 × ~0.776).
        assert!(
            (row.fwhm_arcsec.unwrap() - 3.0).abs() < 1e-6,
            "fwhm should use the solve pixel scale: {:?}",
            row.fwhm_arcsec
        );
        // Center from crval (on target) → no radius failure, even though
        // frames.ra/dec (100, 10) is far outside the 1.5° radius.
        assert!(
            !row.failures
                .iter()
                .any(|f| f.contains("outside target radius")),
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
        assert_eq!(
            report.total, 1,
            "the shared frame is counted once (union dedup)"
        );
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
        assert_eq!(
            suggestions[0].frames_set_id, near,
            "within-radius set ranks first"
        );
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
        assert!(find_matching_projects(&conn, 10.0, 10.0, set_id)
            .unwrap()
            .is_empty());

        // Once linked, the project stops being suggested for that set.
        crate::db::collab::link_set(&conn, "p-1", set_id).unwrap();
        assert!(find_matching_projects(&conn, 210.8, 54.35, set_id)
            .unwrap()
            .is_empty());
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "p-1", "slug": "m101", "title": "M 101", "dataRole": "send_receive",
                    "coordinator": true, "requireApproval": false, "pendingAnnouncements": 0
                }])),
            )
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
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/dictionary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current": {"version": 3,
                            "entries": [{"canonical": "L", "aliases": ["Lum"], "kind": "broadband"}],
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
        assert_eq!(
            cards[0].linked_sets, 1,
            "the intent auto-linked the source set"
        );

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            let row = crate::db::collab::get_project(&conn, "p-1")
                .unwrap()
                .unwrap();
            assert_eq!(row.membership_version, 7);
            // Task 8: caps (+ the coordinator element) and the dictionary.
            assert_eq!(row.gov_caps_json, r#"["coordinator"]"#);
            assert_eq!(row.dictionary_version, Some(3));
            let dict: Vec<crate::collab::filters::DictionaryEntry> =
                serde_json::from_str(row.dictionary_json.as_deref().unwrap()).unwrap();
            assert_eq!(dict[0].canonical, "L");
            assert_eq!(dict[0].aliases, vec!["Lum".to_string()]);
            let members: Vec<ProjectMemberView> = serde_json::from_str(&row.members_json).unwrap();
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
            assert!(crate::db::collab::list_link_intents(&conn)
                .unwrap()
                .is_empty());
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
            let row = crate::db::collab::get_project(&conn, "p-1")
                .unwrap()
                .unwrap();
            assert_eq!(
                row.membership_version, 7,
                "stale row kept after pin mismatch"
            );
        }
    }

    // ── Publish (Task 7) ─────────────────────────────────────────────────────

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
                pending_frames: 0,
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
                gov_caps_json: "[]".into(),
                // all ignored on write (local preference / sync-state / dictionary)
                auto_replicate: true,
                synced_caps_json: "[]".into(),
                hub_version: 0,
                manifest_cursor: 0,
                dictionary_version: None,
                dictionary_json: None,
                policy_json: r#"{"mode":"all"}"#.into(),
                replication_paused: false,
                auto_publish: true,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// One member entry as slice-3 caches it (camelCase `SnapshotMember`).
    fn member_json(
        display: &str,
        data_role: &str,
        coordinator: bool,
        node: &NodeId,
    ) -> serde_json::Value {
        serde_json::json!({
            "accountId": format!("acc-{display}"),
            "displayName": display,
            "dataRole": data_role,
            "coordinator": coordinator,
            "nodes": [B64.encode(node)],
        })
    }

    /// A frame set of on-target, analyzed, not-trailed LIGHT frames (filter
    /// `L`, each with a uuid) for the GATE tests. Returns the set id.
    ///
    /// Gate-passing only once the caller adds calibration links
    /// ([`link_shared_master_dark`]) and a dictionary carrying `L`. The tiny
    /// FITS written under `out_dir` are not the frames' source files (those
    /// paths are fictitious), so this fixture cannot drive a real publish —
    /// the publish tests use `publish::fixture` instead.
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

            // A real single-HDU FITS the stamper can copy. There is no tracking
            // row to seed any more (spec 2026-08-31 §8a).
            let out_path = out_dir.join(format!("c_{uuid}.fits"));
            crate::fits_writer::write_fits_f32(&out_path, 4, 4, 1, &vec![0.5f32; 16], &[]).unwrap();
        }
        set_id
    }

    /// One shared `MasterDark` calibration set — no on-disk master file needed
    /// (mirrors `api::lights::tests::seed_masters` + `add_link`): the export
    /// readiness gate only counts a master path as "missing" once
    /// `resolve_master` actually resolves one, and it resolves `None` for a
    /// master shell with no `calibration_set_frames` member, so an unresolved
    /// link is never counted as missing — it just makes `check_mode_ready`'s
    /// `unlinked_lights`/`raw_sets_without_master` counts both zero, which is
    /// exactly what P7's calibrated verdict needs to pass.
    fn link_shared_master_dark(conn: &rusqlite::Connection, set_id: i64, frame_ids: &[i64]) {
        let master_set_id = 9_000_000 + set_id;
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library) \
             VALUES (?1, 'MasterDark', '2026-07-01', 1)",
            [master_set_id],
        )
        .unwrap();
        for frame_id in frame_ids {
            conn.execute(
                "INSERT INTO calibration_set_to_frames \
                 (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
                 VALUES (?1, 'frame', ?2, 'Dark', '2026-07-01T00:00:00Z')",
                rusqlite::params![frame_id, master_set_id],
            )
            .unwrap();
        }
    }

    /// A `MasterDark` calibration set WITH an on-disk file that is never
    /// actually written — the "archived or moved" shape
    /// (`api::lights::tests::seed_master_with_file` + `add_link`) — linked to
    /// ONE frame's Dark. Unlike [`link_shared_master_dark`], `resolve_master`
    /// DOES resolve this one (it has a `calibration_set_frames` member), so
    /// its missing file counts in `ExportReadiness::missing_master_files` and
    /// blocks P7's calibrated verdict with the "restore from archive"
    /// sentence — the fix-round-1 equivalence test's third calibration state.
    fn link_missing_master_file(
        conn: &rusqlite::Connection,
        set_id: i64,
        frame_id: i64,
        path: &std::path::Path,
    ) {
        let master_set_id = 9_500_000 + set_id * 1000 + frame_id;
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library) \
             VALUES (?1, 'MasterDark', '2026-07-01', 1)",
            [master_set_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) \
             VALUES (?1, 'master_dark_missing.fits', 0, '2026-07-01T00:00:00Z', 'FITS')",
            rusqlite::params![path.to_string_lossy()],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
            [file_id],
        )
        .unwrap();
        let master_frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![master_set_id, master_frame_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames \
             (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
             VALUES (?1, 'frame', ?2, 'Dark', '2026-07-01T00:00:00Z')",
            rusqlite::params![frame_id, master_set_id],
        )
        .unwrap();
    }

    /// Every LIGHT frame id of a set, in `id` order — for wiring up
    /// [`link_shared_master_dark`] after [`seed_publishable_set`].
    fn light_frame_ids_of(conn: &rusqlite::Connection, set_id: i64) -> Vec<i64> {
        conn.prepare(
            "SELECT DISTINCT sm.frame_id FROM session_members sm \
             JOIN sessions s ON s.id = sm.session_id \
             JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
             JOIN frames f ON f.id = sm.frame_id \
             WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light' \
             ORDER BY sm.frame_id",
        )
        .unwrap()
        .query_map([set_id], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<i64>, _>>()
        .unwrap()
    }

    /// Point `ctx`'s account hub at `uri` and store a device token.
    ///
    /// Also caches a bogus relay map: publishing binds the shared iroh node (D3
    /// T2 seeds the collection before announcing), and a signed-in ctx with no
    /// relay map at all refuses to bind rather than ride iroh's public relays.
    /// `.invalid` is guaranteed non-resolvable (RFC 2606) and no role is ever
    /// started here, so the endpoint binds locally and the test stays hermetic —
    /// the same trick `api::sync`'s node tests use.
    fn wire_hub(ctx: &ServiceContext, uri: &str) {
        {
            let conn = crate::api::db(ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::SYNC_CACHED_RELAYS,
                "https://relay.invalid",
            )
            .unwrap();
        }
        crate::api::account::store_token_for_test(ctx, "tok").unwrap();
    }

    /// P7: the per-frame calibrated verdict is the real export-readiness
    /// gate, not a constant. A set whose lights are linked to a usable master
    /// passes the calibration precondition — and, with the project's
    /// dictionary carrying the frames' `L` filter (P3) and every frame
    /// carrying a uuid (P18), every other precondition too.
    #[tokio::test]
    async fn gate_passes_a_frame_whose_set_has_usable_masters() {
        let (_tmp, ctx) = test_ctx();
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(1),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#),
            )
            .unwrap();
            let set_id =
                seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-pass-1", "uuid-pass-2"]);
            let frame_ids = light_frame_ids_of(&conn, set_id);
            link_shared_master_dark(&conn, set_id, &frame_ids);
            set_id
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 2, "both LIGHT frames are candidates");
        assert_eq!(report.publishable, 2, "{:?}", report.rows);
        for row in &report.rows {
            assert!(
                row.publishable,
                "frame {}: {:?}",
                row.frame_id, row.failures
            );
            assert!(row.failures.is_empty(), "{:?}", row.failures);
        }
    }

    /// P7: a set whose lights have no calibration links at all fails with
    /// EXACTLY the sentence `check_mode_ready(CalibratedLights)` would show
    /// the Export tab for that one frame — never a constant enum
    /// debug-print, and never some OTHER precondition quietly doing the
    /// work (the dictionary and every uuid are seeded so this is isolated).
    #[tokio::test]
    async fn gate_blocks_a_frame_whose_set_lacks_masters_with_the_readiness_sentence() {
        let (_tmp, ctx) = test_ctx();
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(1),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#),
            )
            .unwrap();
            seed_publishable_set(&conn, &out_dir, "M101 Set", &["uuid-fail-1", "uuid-fail-2"])
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        // 1. The frames are gate-eligible in every respect but calibration.
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 2, "both LIGHT frames are candidates");
        assert_eq!(report.publishable, 0, "no calibration links anywhere");
        for row in &report.rows {
            assert_eq!(
                row.failures,
                vec!["1 light has no calibration links".to_string()],
                "the readiness sentence must be the SOLE failure — otherwise \
                 this test is pinning some other precondition; frame {} got {:?}",
                row.frame_id,
                row.failures
            );
        }

        // 2. Publish holds both back at the gate, above any hub call (no hub
        // is wired): an empty run is an outcome carrying the gate's sentence.
        let collab = _tmp.path().join("Collab");
        std::fs::create_dir_all(&collab).unwrap();
        crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            collab.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap();
        let res = publish_collab_frames(&ctx, "p-1", None)
            .await
            .expect("an unlinked set is held back, not an error");
        assert_eq!((res.announced, res.updated, res.unchanged), (0, 0, 0));
        assert_eq!(res.state, None);
        assert_eq!(res.held_back.len(), 2);
        for h in &res.held_back {
            assert_eq!(
                h.reasons,
                vec!["1 light has no calibration links".to_string()]
            );
        }
    }

    /// Fix round 1 (ruling R7) equivalence: the gate's per-frame verdict —
    /// now routed through one `ExportData` collected per LINKED SET and
    /// reused for every frame of it — must match EXACTLY what calling the
    /// old, un-cached per-frame `compute_export_readiness_for_frames` would
    /// have given, across a set mixing all three calibration states: no
    /// links, a usable (unresolved-on-disk) master, and a master whose file
    /// is missing.
    #[tokio::test]
    async fn gate_verdicts_match_the_old_per_frame_readiness_call() {
        let (_tmp, ctx) = test_ctx();
        let out_dir = _tmp.path().join("cal_out");
        let missing_master_path = _tmp.path().join("missing_master_dark.fits");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(1),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#),
            )
            .unwrap();
            let set_id = seed_publishable_set(
                &conn,
                &out_dir,
                "Mixed Set",
                &[
                    "uuid-mix-1",
                    "uuid-mix-2",
                    "uuid-mix-3",
                    "uuid-mix-4",
                    "uuid-mix-5",
                ],
            );
            let frame_ids = light_frame_ids_of(&conn, set_id);
            assert_eq!(frame_ids.len(), 5);
            // frame_ids[0]: left completely unlinked.
            // frame_ids[1], [3], [4]: linked to a usable (unresolved-on-disk) master.
            link_shared_master_dark(&conn, set_id, &[frame_ids[1], frame_ids[3], frame_ids[4]]);
            // frame_ids[2]: linked to a master whose file is missing on disk.
            link_missing_master_file(&conn, set_id, frame_ids[2], &missing_master_path);
            set_id
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert_eq!(report.total, 5);
        // Sanity: the mix is real — not all three states collapsed into one.
        assert_eq!(report.publishable, 3, "{:?}", report.rows);

        let conn = crate::api::db(&ctx).unwrap().conn();
        for row in &report.rows {
            let old = crate::api::lights::compute_export_readiness_for_frames(
                &conn,
                set_id,
                &[row.frame_id],
            )
            .unwrap();
            match check_mode_ready(&old, ExportMode::CalibratedLights) {
                Ok(()) => assert!(
                    row.failures.is_empty(),
                    "frame {}: gate said {:?}, old per-frame call said Ok",
                    row.frame_id,
                    row.failures
                ),
                Err(msg) => assert_eq!(
                    row.failures,
                    vec![msg],
                    "frame {} verdict mismatch between the gate and the old per-frame call",
                    row.frame_id
                ),
            }
        }
    }

    /// Fix round 1 (ruling R7): evaluating a project's gate collects each
    /// linked set's `ExportData` EXACTLY ONCE no matter how many lights it
    /// has — never once per frame (which was the O(frames²) DB-round-trip
    /// bug this fix closes).
    #[tokio::test]
    async fn gate_collects_export_data_once_per_linked_set() {
        let (_tmp, ctx) = test_ctx();
        let out_dir = _tmp.path().join("cal_out");
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(1),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#),
            )
            .unwrap();
            seed_publishable_set(
                &conn,
                &out_dir,
                "Count Set",
                &[
                    "uuid-cnt-1",
                    "uuid-cnt-2",
                    "uuid-cnt-3",
                    "uuid-cnt-4",
                    "uuid-cnt-5",
                ],
            )
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        let before = crate::export::data_collector::collect_export_data_calls_on_this_thread();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        let after = crate::export::data_collector::collect_export_data_calls_on_this_thread();

        assert_eq!(report.total, 5);
        assert_eq!(
            after - before,
            1,
            "one linked set of 5 frames must collect its export tree exactly once"
        );
    }

    /// P6: OSC never gets debayered on a wave-2 publish.
    #[test]
    fn publish_options_ships_osc_as_cfa_never_debayered() {
        let opts = publish_options();
        assert!(!opts.debayer_osc);
        assert_eq!(opts.format, crate::fits_writer::OutputFormat::Fits);
    }

    /// An empty gate (no publishable frames) is an outcome, never an error
    /// (wave 2 Task 7 replaced the old `Invalid("no publishable frames")`).
    #[tokio::test]
    async fn publish_with_no_publishable_frames_is_an_empty_outcome() {
        let (tmp, ctx) = test_ctx();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
        }
        let collab = tmp.path().join("Collab");
        std::fs::create_dir_all(&collab).unwrap();
        crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            collab.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap();
        let res = publish_collab_frames(&ctx, "p-1", None).await.unwrap();
        assert_eq!((res.announced, res.updated, res.unchanged), (0, 0, 0));
        assert!(res.held_back.is_empty());
        assert_eq!(res.state, None);
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
        seed_moderation_package(
            &conn, "p-1", "pkg-pend", "ann-pend", "pending", "complete", 2,
        );
        let analysis = serde_json::json!({
            "stars_detected": 400, "median_fwhm": 2.0,
            "median_eccentricity": 0.4, "median_snr": 10.0
        })
        .to_string();
        add_contribution(
            &conn,
            "p-1",
            "pkg-pend",
            "u-1",
            "/land/u-1.fits",
            Some(analysis),
        );
        add_contribution(&conn, "p-1", "pkg-pend", "u-2", "/land/u-2.fits", None);

        // Pending but still downloading (review copy incomplete).
        seed_moderation_package(
            &conn,
            "p-1",
            "pkg-dl",
            "ann-dl",
            "pending",
            "downloading",
            1,
        );
        add_contribution(&conn, "p-1", "pkg-dl", "u-3", "/land/u-3.fits", None);

        // Published — never in the moderation queue.
        seed_moderation_package(
            &conn,
            "p-1",
            "pkg-pub",
            "ann-pub",
            "published",
            "complete",
            1,
        );
        // Pending, but a DIFFERENT project — excluded by scope.
        seed_moderation_package(
            &conn,
            "p-2",
            "pkg-other",
            "ann-other",
            "pending",
            "complete",
            1,
        );

        let queue = list_moderation_queue(&ctx, "p-1").unwrap();
        assert_eq!(queue.len(), 2, "only p-1's pending packages");

        let complete = queue.iter().find(|m| m.package_id == "pkg-pend").unwrap();
        assert!(complete.review_copy_complete);
        assert_eq!(complete.announcement_id, "ann-pend");
        assert_eq!(complete.publisher, "Alice");
        assert_eq!(complete.byte_size, 4096);
        assert_eq!(complete.frames.len(), 2);

        let f1 = complete
            .frames
            .iter()
            .find(|f| f.frame_uuid == "u-1")
            .unwrap();
        assert_eq!(f1.fwhm, Some(2.0));
        assert_eq!(f1.eccentricity, Some(0.4));
        assert_eq!(f1.stars, Some(400));
        assert_eq!(f1.snr, Some(10.0));
        assert_eq!(f1.landed_path.as_deref(), Some("/land/u-1.fits"));
        assert_eq!(f1.byte_size, 2048);

        let f2 = complete
            .frames
            .iter()
            .find(|f| f.frame_uuid == "u-2")
            .unwrap();
        assert_eq!(f2.fwhm, None, "absent analysis ⇒ metrics stay None");
        assert_eq!(f2.eccentricity, None);
        assert_eq!(f2.stars, None);
        assert_eq!(f2.snr, None);

        let incomplete = queue.iter().find(|m| m.package_id == "pkg-dl").unwrap();
        assert!(
            !incomplete.review_copy_complete,
            "still downloading ⇒ not complete"
        );
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

        decide_announcement(&ctx, "ann-a", true, None)
            .await
            .unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = get_package_by_announcement(&conn, "ann-a")
            .unwrap()
            .unwrap();
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
            get_package_by_announcement(&conn, "ann-r")
                .unwrap()
                .is_none(),
            "package row deleted"
        );
        assert!(
            crate::db::collab_exchange::contributions_for_package(&conn, "pkg-r")
                .unwrap()
                .is_empty(),
            "contributions cascaded away"
        );
    }

    // ── D3 T4: unseeding at every project-data deletion site ─────────────────

    /// Seed `package_id` for `project_id` on `ctx`'s node from a throwaway one-file
    /// package dir, binding the node if the test has not already. Returns the node.
    ///
    /// The seed's CONTENT is irrelevant to a deletion test — what is asserted is
    /// that the tag lives and dies with the project data — so this skips the
    /// (heavier) real reconstruct and imports a minimal dir directly.
    async fn seed_package_on_node(
        ctx: &ServiceContext,
        dir_root: &std::path::Path,
        project_id: &str,
        package_id: &str,
    ) -> std::sync::Arc<crate::sharing::iroh::node::SharedIrohNode> {
        let node = crate::api::sync::ensure_iroh_node(ctx).await.unwrap();
        let pkg_dir = dir_root.join(format!("seed-{project_id}-{package_id}"));
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join(crate::package::MANIFEST_FILENAME),
            format!("{project_id}/{package_id}\n").as_bytes(),
        )
        .unwrap();
        node.seed_project_collection(project_id, package_id, &pkg_dir)
            .await
            .unwrap();
        node
    }

    async fn seed_tag_present(
        node: &crate::sharing::iroh::node::SharedIrohNode,
        project_id: &str,
        package_id: &str,
    ) -> bool {
        node.store()
            .tags()
            .get(format!("project/{project_id}/{package_id}").as_bytes())
            .await
            .unwrap()
            .is_some()
    }

    /// Deletion site 2 (D3 T4): rejecting an announcement deletes the review
    /// copy's landed files, so it must also stop seeding that package.
    #[tokio::test]
    async fn reject_unseeds_the_review_copy() {
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
        let land = _tmp.path().join("land");
        std::fs::create_dir_all(&land).unwrap();
        let f1 = land.join("u-1.fits");
        std::fs::write(&f1, b"aaa").unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_moderation_package(&conn, "p-1", "pkg-r", "ann-r", "pending", "complete", 1);
            add_contribution(&conn, "p-1", "pkg-r", "u-1", f1.to_str().unwrap(), None);
        }
        let node = seed_package_on_node(&ctx, _tmp.path(), "p-1", "pkg-r").await;
        seed_package_on_node(&ctx, _tmp.path(), "p-2", "pkg-keep").await;

        decide_announcement(&ctx, "ann-r", false, Some("FWHM too high".into()))
            .await
            .unwrap();

        assert!(!f1.exists(), "the review copy's landed file is deleted");
        assert!(
            !seed_tag_present(&node, "p-1", "pkg-r").await,
            "a rejected package stops being seeded in the same operation"
        );
        assert!(
            seed_tag_present(&node, "p-2", "pkg-keep").await,
            "another project's seed is untouched"
        );
        node.shutdown().await;
    }

    /// Deletion site 3 (D3 T4): a project the hub no longer lists (left, removed,
    /// archived) is marked lost (R14) — this device is not a member any more,
    /// so it must stop seeding EVERY package of that project. The `p-stays` seed
    /// is the scope control: unseeding is per project id, never a `project/`
    /// prefix sweep, so a project this prune did not name keeps every seed.
    #[tokio::test]
    async fn pruning_a_lost_project_unseeds_all_its_packages() {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        use ed25519_dalek::SigningKey;

        let key = SigningKey::from_bytes(&[7u8; 32]);
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/collab/pubkey"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pubkey": B64.encode(key.verifying_key().to_bytes())
            })))
            .mount(&server)
            .await;
        // The hub lists NOTHING: every cached project is gone from my membership.
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/me/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-gone", "[]");
            seed_moderation_package(
                &conn,
                "p-gone",
                "pkg-a",
                "ann-a",
                "published",
                "complete",
                1,
            );
            seed_moderation_package(
                &conn,
                "p-gone",
                "pkg-b",
                "ann-b",
                "published",
                "complete",
                1,
            );
        }
        let node = seed_package_on_node(&ctx, _tmp.path(), "p-gone", "pkg-a").await;
        seed_package_on_node(&ctx, _tmp.path(), "p-gone", "pkg-b").await;
        seed_package_on_node(&ctx, _tmp.path(), "p-stays", "pkg-c").await;

        refresh_projects(&ctx).await.unwrap();

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert!(
                crate::db::collab::lost_at(&conn, "p-gone")
                    .unwrap()
                    .is_some(),
                "the lost project is marked lost (R14: kept, never deleted)"
            );
            assert!(
                crate::db::collab::list_projects(&conn).unwrap().is_empty(),
                "and hidden from the project list"
            );
        }
        assert!(
            !seed_tag_present(&node, "p-gone", "pkg-a").await
                && !seed_tag_present(&node, "p-gone", "pkg-b").await,
            "every package of a project I am no longer in stops seeding"
        );
        assert!(
            seed_tag_present(&node, "p-stays", "pkg-c").await,
            "a project I am still in keeps seeding"
        );
        node.shutdown().await;
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
        assert!(
            matches!(err, ApiError::Conflict(_)),
            "409 ⇒ Conflict, got {err:?}"
        );

        // Local review copy untouched.
        assert!(f1.exists(), "landed file NOT removed on 409");
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = get_package_by_announcement(&conn, "ann-x")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "pending", "local row untouched on 409");
        assert_eq!(
            crate::db::collab_exchange::contributions_for_package(&conn, "pkg-x")
                .unwrap()
                .len(),
            1
        );
    }

    // ── Publish per frame (wave 2 Task 7) ───────────────────────────────────

    mod publish {
        use super::*;
        use std::path::PathBuf;
        use wiremock::matchers::path_regex as wm_path_regex;

        const PID: &str = "p1";
        /// Big enough that every calibrated frame is an EXTERNAL reference in
        /// the collab store, never data inlined into its database (the store
        /// inlines blobs up to 16 KiB) — the only shape in which "the store
        /// grew by less than 1 %" proves anything.
        const W: usize = 512;
        const H: usize = 512;

        pub(super) struct PubFx {
            pub tmp: tempfile::TempDir,
            pub ctx: ServiceContext,
            pub server: MockServer,
            pub node: Arc<crate::sharing::iroh::node::SharedIrohNode>,
            pub collab: PathBuf,
            pub frame_ids: Vec<i64>,
            pub lights: Vec<PathBuf>,
            pub master: PathBuf,
            pub uuids: Vec<String>,
        }

        /// A real relay-disabled node installed on `ctx` — where
        /// `ensure_iroh_node` leaves it in production.
        async fn bind_node_into(
            ctx: &ServiceContext,
        ) -> Arc<crate::sharing::iroh::node::SharedIrohNode> {
            let dirs = crate::api::sync::sync_dirs(ctx).unwrap();
            std::fs::create_dir_all(&dirs.identity_dir).unwrap();
            std::fs::create_dir_all(&dirs.working_dir).unwrap();
            let node = crate::sharing::iroh::node::SharedIrohNode::bind_with(
                &dirs.identity_dir,
                &dirs.working_dir,
                iroh::RelayMode::Disabled,
                crate::sharing::iroh::node::NodeOptions::default(),
            )
            .await
            .expect("bind relay-disabled node");
            *ctx.iroh_node.lock().await = Some(Arc::clone(&node));
            node
        }

        fn write_plane(path: &Path, fill: impl Fn(usize, usize) -> f32) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut data = vec![0f32; W * H];
            for y in 0..H {
                for x in 0..W {
                    data[y * W + x] = fill(x, y);
                }
            }
            crate::fits_writer::write_fits_f32(path, W, H, 1, &data, &[]).unwrap();
        }

        /// A master dark with a spread and two spikes, offset by `level`.
        fn write_dark(path: &Path, level: f32) {
            write_plane(path, |x, y| {
                if (x, y) == (5, 5) || (x, y) == (9, 9) {
                    5000.0
                } else if (x + y) % 2 == 0 {
                    level
                } else {
                    level + 2.0
                }
            });
        }

        /// Hub routes every run touches: announce, holder delta, version.
        async fn mount_hub(server: &MockServer, state: &str) {
            Mock::given(wm_method("POST"))
                .and(wm_path(format!("/api/v1/projects/{PID}/frames")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "state": state, "projectVersion": 5, "announced": 2
                })))
                .mount(server)
                .await;
            mount_holders_and_version(server).await;
        }

        async fn mount_holders_and_version(server: &MockServer) {
            Mock::given(wm_method("PUT"))
                .and(wm_path(format!("/api/v1/projects/{PID}/holders/self")))
                .respond_with(ResponseTemplate::new(204))
                .mount(server)
                .await;
            Mock::given(wm_method("POST"))
                .and(wm_path_regex(format!(
                    r"^/api/v1/projects/{PID}/frames/[^/]+/version$"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "contentVersion": 2, "projectVersion": 6
                })))
                .mount(server)
                .await;
        }

        /// `(method, path, body)` of every request the hub received.
        async fn requests(server: &MockServer) -> Vec<(String, String, serde_json::Value)> {
            server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .map(|r| {
                    let body = serde_json::from_slice(&r.body).unwrap_or(serde_json::Value::Null);
                    (r.method.to_string(), r.url.path().to_string(), body)
                })
                .collect()
        }

        async fn announce_bodies(server: &MockServer) -> Vec<serde_json::Value> {
            requests(server)
                .await
                .into_iter()
                .filter(|(m, p, _)| m == "POST" && p == &format!("/api/v1/projects/{PID}/frames"))
                .map(|(_, _, b)| b)
                .collect()
        }

        async fn version_calls(server: &MockServer) -> Vec<String> {
            requests(server)
                .await
                .into_iter()
                .filter(|(m, p, _)| m == "POST" && p.ends_with("/version"))
                .map(|(_, p, _)| p)
                .collect()
        }

        async fn tag_present(fx: &PubFx, uuid: &str, version: i32) -> bool {
            fx.node
                .collab_store()
                .expect("collab store mounted")
                .tags()
                .get(crate::sharing::iroh::node::project_frame_tag(PID, uuid, version).as_bytes())
                .await
                .unwrap()
                .is_some()
        }

        async fn project_tag_count(fx: &PubFx) -> usize {
            use n0_future::StreamExt as _;
            let store = fx.node.collab_store().expect("collab store mounted");
            let prefix = format!("project/{PID}/");
            let mut stream = store.tags().list_prefix(prefix.as_bytes()).await.unwrap();
            let mut n = 0;
            while let Some(item) = stream.next().await {
                item.unwrap();
                n += 1;
            }
            n
        }

        fn own_row(fx: &PubFx, uuid: &str) -> Option<crate::db::collab_frames::LocalFrameRow> {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            crate::db::collab_frames::get(&conn, PID, uuid).unwrap()
        }

        fn dir_bytes(dir: &Path) -> u64 {
            let mut total = 0;
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        total += dir_bytes(&p);
                    } else if let Ok(m) = p.metadata() {
                        total += m.len();
                    }
                }
            }
            total
        }

        fn own_dir(fx: &PubFx) -> PathBuf {
            fx.collab.join("m31").join("me-myself")
        }

        fn set_mtime(path: &Path, ahead_secs: u64) {
            let t = std::time::SystemTime::now() + std::time::Duration::from_secs(ahead_secs);
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }

        fn mtime(path: &Path) -> std::time::SystemTime {
            std::fs::metadata(path).unwrap().modified().unwrap()
        }

        /// A signed-in context with a bound node, a mounted Collaboration
        /// root, a cached project `p1` (slug `m31`, this device a
        /// `send_receive` member "Me Myself", dictionary `L`, no thresholds)
        /// and one linked set of `n` gate-passing LIGHT frames — each a real
        /// FITS on disk, all linked to one real master dark.
        pub(super) async fn fixture(n: usize) -> PubFx {
            let (tmp, ctx) = test_ctx();
            let server = MockServer::start().await;
            wire_hub(&ctx, &server.uri());
            let node = bind_node_into(&ctx).await;
            let requested = tmp.path().join("Collab");
            std::fs::create_dir_all(&requested).unwrap();
            // The stored spelling (canonicalized) is the one publish lands under.
            let collab = PathBuf::from(
                crate::api::scan_roots::set_collaboration_dir(
                    &ctx,
                    requested.to_string_lossy().to_string(),
                    &crate::api::PathPolicy::AllowAll,
                )
                .await
                .unwrap(),
            );
            assert!(node.collab_store().is_some(), "the collab store is mounted");
            let me = DeviceKey::load_or_create(&device_key_path(
                &crate::api::sync::sync_dirs(&ctx).unwrap().identity_dir,
            ))
            .unwrap()
            .node_id();

            let master = tmp.path().join("masters").join("master_dark.fits");
            write_dark(&master, 300.0);
            let mut lights = Vec::new();
            let mut uuids = Vec::new();
            let mut frame_ids = Vec::new();
            let set_id = {
                let conn = crate::api::db(&ctx).unwrap().conn();
                let members =
                    serde_json::json!([member_json("Me Myself", "send_receive", false, &me)]);
                crate::db::collab::upsert_project(
                    &conn,
                    &CollabProjectRow {
                        project_id: PID.into(),
                        slug: "m31".into(),
                        title: "M 31".into(),
                        data_role: "send_receive".into(),
                        is_coordinator: false,
                        require_approval: false,
                        pending_frames: 0,
                        project_status: "active".into(),
                        target_name: "M31".into(),
                        target_ra_deg: 10.68,
                        target_dec_deg: 41.27,
                        target_radius_deg: 1.5,
                        membership_version: 1,
                        snapshot_payload_b64: "e30=".into(),
                        snapshot_signature_b64: "e30=".into(),
                        members_json: members.to_string(),
                        thresholds_version: None,
                        thresholds_rules_json: None,
                        gov_caps_json: "[]".into(),
                        auto_replicate: true,
                        synced_caps_json: "[]".into(),
                        hub_version: 0,
                        manifest_cursor: 0,
                        dictionary_version: None,
                        dictionary_json: None,
                        policy_json: r#"{"mode":"all"}"#.into(),
                        replication_paused: false,
                        auto_publish: true,
                        fetched_at: String::new(),
                    },
                )
                .unwrap();
                crate::db::collab::set_dictionary(
                    &conn,
                    PID,
                    Some(1),
                    Some(r#"[{"canonical":"L","aliases":["Lum"],"kind":"broadband"}]"#),
                )
                .unwrap();

                conn.execute(
                    "INSERT INTO frames_set (name, objctra, objctdec) VALUES ('M31 Set', '00:42:44', '+41:16:09')",
                    [],
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

                // The built master dark, with a real member file.
                conn.execute(
                    "INSERT INTO calibration_set (id, imagetyp, date, is_master_library) \
                     VALUES (700, 'MasterDark', '2026-07-01', 1)",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format) \
                     VALUES (?1, 'master_dark.fits', 0, '2026-07-01T00:00:00Z', 'FITS')",
                    [master.to_string_lossy()],
                )
                .unwrap();
                let mfile = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
                    [mfile],
                )
                .unwrap();
                let mframe = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (700, ?1)",
                    [mframe],
                )
                .unwrap();

                for i in 0..n {
                    let name = format!("L_{i:04}.fits");
                    let light = tmp.path().join("src").join(&name);
                    // Distinct pixels per frame, so no two outputs share a hash.
                    write_plane(&light, |x, y| {
                        1000.0 + (i * 10) as f32 + ((x * 7 + y) % 13) as f32
                    });
                    let uuid = format!("uuid-pub-{i}");
                    conn.execute(
                        "INSERT INTO files (path, filename, size, modified_at, format) \
                         VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS')",
                        rusqlite::params![light.to_string_lossy(), name],
                    )
                    .unwrap();
                    let file_id = conn.last_insert_rowid();
                    conn.execute(
                        "INSERT INTO frames (file_id, imagetyp, object, instrume, ra, dec, xpixsz, focallen, \
                                             exptime, filter, uuid, date_obs) \
                         VALUES (?1, 'Light', 'M31', 'ASI2600MM', 10.68, 41.27, 3.76, 1000.0, 300.0, 'L', ?2, \
                                 '2026-07-01T21:00:00Z')",
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
                                 512, 512, 1, 0.0, 0)",
                        rusqlite::params![frame_id, file_id],
                    )
                    .unwrap();
                    conn.execute(
                        "INSERT INTO calibration_set_to_frames \
                         (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
                         VALUES (?1, 'frame', 700, 'Dark', '2026-07-01T00:00:00Z')",
                        [frame_id],
                    )
                    .unwrap();
                    lights.push(light);
                    uuids.push(uuid);
                    frame_ids.push(frame_id);
                }
                set_id
            };
            link_frame_set(&ctx, PID, set_id).unwrap();
            PubFx {
                tmp,
                ctx,
                server,
                node,
                collab,
                frame_ids,
                lights,
                master,
                uuids,
            }
        }

        /// §5.2 / disk-copy ledger: ONE calibrated copy per frame, written
        /// straight into `<Collab>/<project>/<me>/`, referenced (not copied)
        /// by the collab store, tagged per frame, announced with the dictionary
        /// filter + gate version + manifest meta, and recorded as own rows.
        #[tokio::test]
        async fn publish_writes_once_into_the_collab_folder_and_seeds_by_reference() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            let store_dir = fx.collab.join(".athenaeum").join("blobs");
            let store_before = dir_bytes(&store_dir);

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (2, 0, 0),
                "{res:?}"
            );
            assert_eq!(res.state.as_deref(), Some("published"));
            assert!(res.held_back.is_empty(), "{:?}", res.held_back);

            // Exactly the two calibrated files, named `c_<stem>.fits`.
            let mut names: Vec<String> = std::fs::read_dir(own_dir(&fx))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            assert_eq!(names, vec!["c_L_0000.fits", "c_L_0001.fits"]);
            let files_bytes: u64 = names
                .iter()
                .map(|n| std::fs::metadata(own_dir(&fx).join(n)).unwrap().len())
                .sum();

            // No package staging anywhere.
            let dirs = crate::api::sync::sync_dirs(&fx.ctx).unwrap();
            assert!(!dirs.working_dir.join("collab_pub").exists());

            // Reference import: the store holds outboards, never the bytes.
            let grown = dir_bytes(&store_dir).saturating_sub(store_before);
            assert!(
                grown * 100 < files_bytes,
                "the collab store grew {grown} B for {files_bytes} B of frames"
            );

            for uuid in &fx.uuids {
                assert!(tag_present(&fx, uuid, 1).await, "tag for {uuid}");
            }

            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 1, "one batch");
            let frames = bodies[0]["frames"].as_array().unwrap();
            assert_eq!(frames.len(), 2);
            for f in frames {
                assert_eq!(f["filterCanonical"], "L");
                assert_eq!(f["filterRaw"], "L");
                assert_eq!(f["gateVersion"], 0);
                assert_eq!(f["channel"], "mono");
                assert!(f["meta"]["fwhmArcsec"].is_number(), "{f}");
                assert_eq!(f["blake3"].as_str().unwrap().len(), 64);
                assert_eq!(f["xxh3"].as_str().unwrap().len(), 16);
            }

            for (i, uuid) in fx.uuids.iter().enumerate() {
                let row = own_row(&fx, uuid).expect("own row recorded");
                assert_eq!(row.origin, crate::db::collab_frames::FrameOrigin::Own);
                assert!(row.on_disk);
                assert_eq!(row.content_version, 1);
                assert_eq!(row.state, "published");
                assert_eq!(row.source_frame_id, Some(fx.frame_ids[i]));
                assert!(row.recipe_hash.is_some());
                assert_eq!(row.publisher_display, "Me Myself");
                let landed = PathBuf::from(row.landed_path.unwrap());
                assert_eq!(landed.parent().unwrap(), own_dir(&fx));
                assert_eq!(
                    crate::package::xxh3_full_file(&landed).unwrap(),
                    row.xxh3,
                    "the recorded xxh3 is the landed file's"
                );
                // The written header carries the publish stamps.
                let header =
                    crate::fits_parser::FitsHeader::from_path(&landed).expect("read header");
                assert_eq!(header.get_str("ATH_PRJ").as_deref(), Some(PID));
                assert_eq!(header.get_str("ATH_FILT").as_deref(), Some("L"));
            }

            // One holder delta carrying both frames at version 1.
            let holder_puts: Vec<_> = requests(&fx.server)
                .await
                .into_iter()
                .filter(|(m, _, _)| m == "PUT")
                .collect();
            assert_eq!(holder_puts.len(), 1);
            assert_eq!(holder_puts[0].2["full"], false);
            assert_eq!(holder_puts[0].2["add"].as_array().unwrap().len(), 2);
            drop(fx.tmp);
        }

        /// The hub's `pending` (require-approval project) is reported as is.
        #[tokio::test]
        async fn pending_state_is_returned_as_is() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "pending").await;
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.state.as_deref(), Some("pending"));
            assert_eq!(own_row(&fx, &fx.uuids[0]).unwrap().state, "pending");
        }

        /// F5: a failed announce leaves no seed tag and no own row; the file
        /// stays on disk, and the next run re-announces THAT file.
        #[tokio::test]
        async fn failed_announce_unseeds_and_records_nothing() {
            let fx = fixture(2).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(format!("/api/v1/projects/{PID}/frames")))
                .respond_with(
                    ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "boom"})),
                )
                .mount(&fx.server)
                .await;
            let err = publish_collab_frames(&fx.ctx, PID, None)
                .await
                .expect_err("nothing was announced");
            assert!(matches!(err, ApiError::Internal(_)), "{err:?}");
            assert_eq!(project_tag_count(&fx).await, 0, "every seed tag dropped");
            for uuid in &fx.uuids {
                assert!(own_row(&fx, uuid).is_none(), "no own row for {uuid}");
            }
            let on_disk = std::fs::read_dir(own_dir(&fx)).unwrap().count();
            assert_eq!(on_disk, 2, "the written files stay");

            // The retry reuses the same files instead of piling up copies.
            fx.server.reset().await;
            mount_hub(&fx.server, "published").await;
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 2);
            assert_eq!(std::fs::read_dir(own_dir(&fx)).unwrap().count(), 2);
        }

        /// A 409 "gate version … stale" refreshes the thresholds once and
        /// retries the batch with the new version.
        #[tokio::test]
        async fn stale_gate_version_refreshes_thresholds_and_retries_once() {
            let fx = fixture(1).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(format!("/api/v1/projects/{PID}/frames")))
                .respond_with(ResponseTemplate::new(409).set_body_json(
                    serde_json::json!({"error": "gate version 0 is stale, current is 1"}),
                ))
                .up_to_n_times(1)
                .mount(&fx.server)
                .await;
            Mock::given(wm_method("GET"))
                .and(wm_path(format!("/api/v1/projects/{PID}/thresholds")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "current": {"version": 1, "rules": [], "createdAt": "2026-09-24T00:00:00Z"},
                    "history": []
                })))
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 2, "one refused, one retried");
            assert_eq!(bodies[0]["frames"][0]["gateVersion"], 0);
            assert_eq!(bodies[1]["frames"][0]["gateVersion"], 1);
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            let row = crate::db::collab::get_project(&conn, PID).unwrap().unwrap();
            assert_eq!(row.thresholds_version, Some(1), "the refresh is cached");
        }

        /// P19: an mtime-only touch of the source moves the recipe, the frame
        /// is regenerated, its bytes come out identical — so no version is
        /// posted, the v1 tag stays, and only the recipe is stored.
        #[tokio::test]
        async fn touched_source_with_identical_output_sends_no_version() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let before = own_row(&fx, &fx.uuids[0]).unwrap();

            set_mtime(&fx.lights[0], 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 0, 1),
                "{res:?}"
            );
            assert!(version_calls(&fx.server).await.is_empty(), "no …/version");
            assert!(tag_present(&fx, &fx.uuids[0], 1).await);
            let after = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_ne!(
                after.recipe_hash, before.recipe_hash,
                "the new recipe is stored"
            );
            assert_eq!(after.content_version, 1);
            assert_eq!(after.blake3, before.blake3);
            assert!(
                !update_temp_path(Path::new(after.landed_path.as_deref().unwrap())).exists(),
                "the identical regeneration is cleaned up"
            );
        }

        /// P19: the engine version is not an input of the recipe.
        #[test]
        fn engine_version_is_not_part_of_the_recipe() {
            let parts = |engine_version| RecipeParts {
                engine_version,
                masters: vec![("/m/dark.fits".into(), "100:5".into())],
                source: "200:7".into(),
            };
            assert_eq!(recipe_hash_from(&parts(3)), recipe_hash_from(&parts(4)));
            // …while what the user changed IS.
            let mut moved = parts(3);
            moved.source = "200:8".into();
            assert_ne!(recipe_hash_from(&parts(3)), recipe_hash_from(&moved));
        }

        /// P19 manual: republish regenerates every own frame and posts a
        /// version only where the bytes changed (here: a plate solve arrived
        /// for frame 0 — P4 swaps its WCS in — which the recipe ignores).
        #[tokio::test]
        async fn republish_forces_regeneration_but_respects_identical_output() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                seed_plate_solve(&conn, fx.frame_ids[0], 0.776, 10.68, 41.27);
            }
            let plain = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(plain.unchanged, 2, "a plate solve is not in the recipe");
            assert!(version_calls(&fx.server).await.is_empty());

            let res = republish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 1, 1),
                "{res:?}"
            );
            let calls = version_calls(&fx.server).await;
            assert_eq!(
                calls,
                vec![format!(
                    "/api/v1/projects/{PID}/frames/{}/version",
                    fx.uuids[0]
                )]
            );
            assert_eq!(own_row(&fx, &fx.uuids[0]).unwrap().content_version, 2);
            assert_eq!(own_row(&fx, &fx.uuids[1]).unwrap().content_version, 1);
            let landed = own_row(&fx, &fx.uuids[0]).unwrap().landed_path.unwrap();
            let header = crate::fits_parser::FitsHeader::from_path(Path::new(&landed)).unwrap();
            assert_eq!(header.get_str("CTYPE1").as_deref(), Some("RA---TAN"));
        }

        /// A rewritten master moves the recipe AND the pixels: one
        /// `…/version`, the row at content version 2, the tag moved from
        /// `…/1` to `…/2`, the file path unchanged.
        #[tokio::test]
        async fn changed_master_publishes_a_new_version() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let before = own_row(&fx, &fx.uuids[0]).unwrap();

            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 1, 0),
                "{res:?}"
            );
            assert_eq!(version_calls(&fx.server).await.len(), 1);
            let after = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(after.content_version, 2);
            assert_ne!(after.blake3, before.blake3);
            assert_eq!(after.landed_path, before.landed_path, "same path");
            assert_eq!(
                crate::package::xxh3_full_file(Path::new(after.landed_path.as_deref().unwrap()))
                    .unwrap(),
                after.xxh3
            );
            assert!(tag_present(&fx, &fx.uuids[0], 2).await, "…/2 present");
            assert!(!tag_present(&fx, &fx.uuids[0], 1).await, "…/1 gone");
            assert_eq!(
                std::fs::read_dir(own_dir(&fx)).unwrap().count(),
                1,
                "no temp left"
            );
        }

        /// Nothing changed ⇒ nothing regenerated, no hub call at all.
        #[tokio::test]
        async fn unchanged_frames_are_not_regenerated() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let landed: Vec<PathBuf> = fx
                .uuids
                .iter()
                .map(|u| PathBuf::from(own_row(&fx, u).unwrap().landed_path.unwrap()))
                .collect();
            let mtimes: Vec<_> = landed.iter().map(|p| mtime(p)).collect();
            let hub_calls = requests(&fx.server).await.len();

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 0, 2),
                "{res:?}"
            );
            assert_eq!(res.state, None);
            assert_eq!(landed.iter().map(|p| mtime(p)).collect::<Vec<_>>(), mtimes);
            assert_eq!(requests(&fx.server).await.len(), hub_calls, "no hub call");
        }

        /// P25: no Collaboration root ⇒ the one actionable refusal, before
        /// the gate, the hub or the node.
        #[tokio::test]
        async fn no_collab_root_refuses_before_any_work() {
            let (_tmp, ctx) = test_ctx();
            {
                let conn = crate::api::db(&ctx).unwrap().conn();
                seed_publish_project(&conn, "p-1", "[]");
            }
            match publish_collab_frames(&ctx, "p-1", None).await {
                Err(ApiError::Invalid(m)) => assert_eq!(m, COLLABORATION_ROOT_REQUIRED),
                other => panic!("expected the P25 refusal, got {other:?}"),
            }
            assert!(ctx.iroh_node.lock().await.is_none(), "no node was bound");
        }

        /// R8a: a hub 409 "frame … already announced" adopts that frame from
        /// the local plan (state unknown until the manifest sync) and retries
        /// the rest of the batch.
        #[tokio::test]
        async fn hub_already_announced_frame_is_adopted_and_the_rest_retried() {
            let fx = fixture(2).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(format!("/api/v1/projects/{PID}/frames")))
                .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                    "error": format!("frame {} already announced; use /version", fx.uuids[0])
                })))
                .up_to_n_times(1)
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 2, "{res:?}");
            assert!(res.held_back.is_empty(), "{:?}", res.held_back);
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 2);
            assert_eq!(bodies[1]["frames"].as_array().unwrap().len(), 1);
            assert_eq!(bodies[1]["frames"][0]["frameUuid"], fx.uuids[1].as_str());

            let adopted = own_row(&fx, &fx.uuids[0]).expect("adopted own row");
            assert_eq!(adopted.state, "unknown");
            assert_eq!(adopted.content_version, 1);
            assert_eq!(adopted.source_frame_id, Some(fx.frame_ids[0]));
            assert!(adopted.on_disk && adopted.recipe_hash.is_some());
            assert!(tag_present(&fx, &fx.uuids[0], 1).await);
            assert_eq!(own_row(&fx, &fx.uuids[1]).unwrap().state, "published");

            // The adopted frame is now an ordinary own frame: nothing to do.
            let again = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!((again.announced, again.updated, again.unchanged), (0, 0, 2));
        }

        /// R8c: a row write that fails for one frame is held back with its
        /// reason; the other frame is recorded and its holder delta sent.
        #[tokio::test]
        async fn a_failed_record_does_not_abort_the_run() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute_batch(&format!(
                    "CREATE TRIGGER fail_one BEFORE INSERT ON project_frames_local \
                     WHEN NEW.frame_uuid = '{}' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
                    fx.uuids[1]
                ))
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            assert_eq!(res.held_back.len(), 1);
            assert_eq!(res.held_back[0].frame_id, fx.frame_ids[1]);
            assert!(
                res.held_back[0].reasons[0].contains("recording it locally failed"),
                "{:?}",
                res.held_back[0].reasons
            );
            assert!(own_row(&fx, &fx.uuids[0]).is_some());
            assert!(own_row(&fx, &fx.uuids[1]).is_none());
            let puts: Vec<_> = requests(&fx.server)
                .await
                .into_iter()
                .filter(|(m, _, _)| m == "PUT")
                .collect();
            assert_eq!(puts.len(), 1);
            assert_eq!(puts[0].2["add"].as_array().unwrap().len(), 1);
        }

        /// R9: a run with nothing to generate never asks for the compute
        /// slot, so it cannot queue behind a stacking run holding it.
        #[tokio::test]
        async fn unchanged_run_takes_no_compute_permit() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();

            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (permit, _) = fx
                .ctx
                .compute_queue
                .acquire(
                    crate::services::compute_queue::ComputeJobKind::LightCalibration,
                    "a long stacking run",
                    flag,
                )
                .unwrap();
            let run = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                publish_collab_frames(&fx.ctx, PID, None),
            )
            .await;
            drop(permit);
            let res = run
                .expect("an unchanged run must not wait for the compute slot")
                .unwrap();
            assert_eq!(res.unchanged, 2);
        }

        /// R10: the identical-output and the new-version write-backs are
        /// column-targeted — a hub state a manifest sync wrote between the
        /// split and the write-back survives both.
        #[tokio::test]
        async fn write_backs_keep_hub_state_written_mid_run() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let hook = |conn: &Connection| {
                conn.execute(
                    "UPDATE project_frames_local SET state = 'hub-truth', manifest_version = 42",
                    [],
                )
                .unwrap();
            };

            // Identical output: only the recipe moves.
            set_mtime(&fx.lights[0], 120);
            let res = run_publish(&fx.ctx, PID, None, false, Some(&hook))
                .await
                .unwrap();
            assert_eq!(res.unchanged, 1, "{res:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                (row.state.as_str(), row.manifest_version),
                ("hub-truth", 42)
            );

            // New version.
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute("UPDATE project_frames_local SET state = 'published'", [])
                    .unwrap();
            }
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 240);
            let res = run_publish(&fx.ctx, PID, None, false, Some(&hook))
                .await
                .unwrap();
            assert_eq!(res.updated, 1, "{res:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert_eq!(
                (row.state.as_str(), row.manifest_version),
                ("hub-truth", 42)
            );
            let m: serde_json::Value = serde_json::from_str(&row.manifest_json).unwrap();
            assert_eq!(m["contentVersion"], 2);
            assert_eq!(m["blake3"], row.blake3.as_str());
        }

        /// R8b: an own row the manifest delivered (no local binding) is bound
        /// to its source frame after a verifying regeneration — never
        /// announced again, no version when the bytes match.
        #[tokio::test]
        async fn manifest_delivered_own_row_is_bound_not_reannounced() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let before = own_row(&fx, &fx.uuids[0]).unwrap();
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE project_frames_local SET source_frame_id = NULL, recipe_hash = NULL, \
                     landed_path = NULL, on_disk = 0",
                    [],
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 0, 1),
                "{res:?}"
            );
            assert_eq!(
                announce_bodies(&fx.server).await.len(),
                1,
                "no second announce"
            );
            assert!(version_calls(&fx.server).await.is_empty());
            let after = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(after.source_frame_id, Some(fx.frame_ids[0]));
            assert_eq!(after.landed_path, before.landed_path);
            assert!(after.on_disk && after.recipe_hash.is_some());
            assert_eq!(after.content_version, 1);
            assert!(tag_present(&fx, &fx.uuids[0], 1).await);
            assert_eq!(
                std::fs::read_dir(own_dir(&fx)).unwrap().count(),
                1,
                "no temp left"
            );
        }

        /// A frame that fails to calibrate is held back; the others go out.
        #[tokio::test]
        async fn partial_run_holds_back_the_failed_frame_and_announces_the_rest() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            std::fs::write(&fx.lights[1], b"not a FITS file at all").unwrap();
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            assert_eq!(res.held_back.len(), 1);
            assert_eq!(res.held_back[0].frame_id, fx.frame_ids[1]);
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies[0]["frames"].as_array().unwrap().len(), 1);
            assert!(own_row(&fx, &fx.uuids[1]).is_none());
        }

        /// The hub caps a batch at 500 frames.
        #[test]
        fn announce_batches_split_at_500() {
            let sizes: Vec<usize> = announce_batches((0..1001).collect::<Vec<_>>())
                .iter()
                .map(Vec::len)
                .collect();
            assert_eq!(sizes, vec![500, 500, 1]);
            assert!(announce_batches(Vec::<u8>::new()).is_empty());
        }

        /// The hub's fileName rule, checked before a name can refuse a batch.
        #[test]
        fn hub_file_name_rule() {
            assert_eq!(hub_file_name_problem("c_L_0001.fits"), None);
            for bad in [
                "", " c.fits", ".", "..", "a:b.fits", "a\\b", "a/b", "a\u{1}b",
            ] {
                assert!(hub_file_name_problem(bad).is_some(), "{bad:?}");
            }
            assert!(hub_file_name_problem(&"a".repeat(256)).is_some());
        }

        /// P4: a PC matrix is stripped with the rest of the header WCS.
        #[test]
        fn header_wcs_strip_covers_pc_cdelt_crota() {
            for k in [
                "PC1_1",
                "PC2_1",
                "PC001_002",
                "CDELT1",
                "CROTA2",
                "CD1_1",
                "LONPOLE",
            ] {
                assert!(is_header_wcs_keyword(k), "{k}");
            }
            for k in ["PC", "PC1", "PCX_1", "PC1_1_1", "EXPTIME", "PCOUNT"] {
                assert!(!is_header_wcs_keyword(k), "{k}");
            }
        }

        /// P17: `409 collab_api_outdated` is a Conflict with the stable
        /// prefix, and every seed of the run is rolled back.
        #[tokio::test]
        async fn outdated_hub_is_a_conflict_with_the_stable_prefix() {
            let fx = fixture(2).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(format!("/api/v1/projects/{PID}/frames")))
                .respond_with(
                    ResponseTemplate::new(409)
                        .set_body_json(serde_json::json!({"error": "collab_api_outdated"})),
                )
                .mount(&fx.server)
                .await;
            match publish_collab_frames(&fx.ctx, PID, None).await {
                Err(ApiError::Conflict(m)) => {
                    assert!(m.starts_with("collab_api_outdated"), "{m}");
                    assert_eq!(m, COLLAB_API_OUTDATED_MSG);
                }
                other => panic!("expected Conflict, got {other:?}"),
            }
            assert_eq!(project_tag_count(&fx).await, 0);
            for uuid in &fx.uuids {
                assert!(own_row(&fx, uuid).is_none());
            }
        }
    }
}
