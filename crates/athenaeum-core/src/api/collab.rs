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
use crate::collab::filters::DictionaryEntry;
use crate::collab::gate::{
    evaluate_frame, header_pixel_scale_arcsec, FrameGateRow, GateFrameInput, ProjectTarget,
    ThresholdRuleView,
};
use crate::collab::hub_client::CollabClient;
use crate::collab::snapshot::{member_node_ids, SnapshotMember};
use crate::coordinates::{angular_distance, parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::db::analysis::get_frame_analyses_by_ids;
use crate::db::collab::CollabProjectRow;
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
    /// Spec §7.1 — the same rows, grouped into causes with a batch action.
    pub blockers: Vec<crate::collab::gate::GateBlocker>,
}

/// A cached project whose target field contains a queried point (auto-link hook).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSetMatch {
    pub project_id: String,
    pub project_title: String,
    pub project_slug: String,
    /// Angular distance (deg) from the queried point to the project target.
    pub distance_deg: f64,
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
    /// Collab v3 wave 2 Task 10 (R16, P13): this device coalesces and
    /// auto-publishes its own passing frames on scan/analysis/solve/link/
    /// threshold changes (default ON). Local preference —
    /// `set_project_auto_publish` writes it.
    pub auto_publish: bool,
    pub fetched_at: String,
    /// Amendment A6: the one device of this account that may announce new
    /// frames into the project, as the hub last reported it. `None` = no
    /// device is publishing yet (nothing bound, or the bound device was
    /// revoked/retired) — the next device that publishes becomes it.
    pub publishing_device: Option<PublishingDeviceView>,
    /// `true` only when `publishing_device` names THIS device. Never `true`
    /// for an unbound project.
    pub publishing_here: bool,
}

/// The account's publishing device of a project (amendment A6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PublishingDeviceView {
    /// Base64 of the device's public key.
    pub device_id: String,
    /// The device's name on the hub, when it has one.
    pub name: Option<String>,
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
    /// The hub's per-filter goals (`{canonical: seconds}`), strictly parsed
    /// by [`parse_goals`] — `None` when the hub sent nothing usable.
    pub goals: Option<std::collections::BTreeMap<String, f64>>,
}

/// Goals as the hub guarantees them since wave 0 (`{canonical: seconds}`,
/// every value > 0). Anything else is logged and read as "no goals".
pub fn parse_goals(
    project_id: &str,
    json: Option<&str>,
) -> Option<std::collections::BTreeMap<String, f64>> {
    let raw = json?;
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(project_id, error = %e, "project goals are not JSON; ignored");
            return None;
        }
    };
    let obj = value.as_object().filter(|o| !o.is_empty());
    let Some(obj) = obj else {
        tracing::warn!(
            project_id,
            "project goals are not a non-empty object; ignored"
        );
        return None;
    };
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in obj {
        match v.as_f64().filter(|s| s.is_finite() && *s > 0.0) {
            Some(s) => {
                out.insert(k.clone(), s);
            }
            None => {
                tracing::warn!(project_id, canonical = %k, "project goal is not a positive number; goals ignored");
                return None;
            }
        }
    }
    Some(out)
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
    instrume: Option<String>,
}

/// The signed-in e-mail, lower-cased — the scope of `collab_filter_mappings`
/// (spec 2026-09-28 §3.1). `None` while signed out.
pub(crate) fn current_account_email(conn: &Connection) -> Option<String> {
    match crate::db::get_setting(conn, crate::settings::keys::ACCOUNT_EMAIL) {
        Ok(v) => v.map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()),
        Err(e) => {
            tracing::warn!(error = %e, "reading the account e-mail failed; no filter mappings apply");
            None
        }
    }
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

/// Batch-assemble one `(`[`GateFrameInput`]`, instrume)` pair per
/// `(frame_id, filename)` — conn-only, no settings needed. Reads
/// `plate_solves`, `frames`, and the analyses in three batched queries, then
/// resolves each frame's center (crval → ra/dec → parsed objctra/objctdec),
/// pixel scale (plate-solve → header `atan(xpixsz/focallen)`, no binning
/// multiply), P7's per-frame calibrated verdict (via `frame_set_id_by_frame`,
/// one `collect_export_data` per DISTINCT set — fix round 1, ruling R7,
/// skipped entirely for an attested set — F5), and spec §3.2's filter
/// resolution (`resolve_filter`: the account's mappings first, then P3's
/// dictionary match). The `instrume` alongside each input is the frame's own
/// camera, trimmed — `project_gate` needs it for the blocker table (§7.1)
/// even though the gate engine itself never reads it.
fn frame_gate_inputs(
    conn: &rusqlite::Connection,
    frames: &[(i64, String)],
    dictionary: &[DictionaryEntry],
    frame_set_id_by_frame: &HashMap<i64, i64>,
    mappings: &[crate::db::collab::FilterMappingRow],
    attested_sets: &HashSet<i64>,
) -> anyhow::Result<Vec<(GateFrameInput, String)>> {
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
            "SELECT id, ra, dec, objctra, objctdec, xpixsz, focallen, filter, uuid, instrume \
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
                    instrume: r.get(9)?,
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
        let instrume = frow
            .and_then(|f| f.instrume.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        let filter =
            crate::collab::filters::resolve_filter(&filter_raw, &instrume, mappings, dictionary);
        let uuid = frow
            .and_then(|f| f.uuid.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        // P7: the real gate, replacing decision C's constant. F5: an attested
        // set's frames skip the export-readiness walk entirely — the operator
        // has already told the app "the pixels are handled elsewhere", so
        // nothing about calibration is checked for them.
        let cal_blocker = match frame_set_id_by_frame.get(frame_id) {
            Some(&set_id) if attested_sets.contains(&set_id) => None,
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

        out.push((
            GateFrameInput {
                frame_id: *frame_id,
                filename: filename.clone(),
                center,
                pixel_scale_arcsec,
                cal_blocker,
                analysis: analyses.get(frame_id).cloned(),
                filter_raw,
                filter,
                uuid,
            },
            instrume,
        ));
    }
    Ok(out)
}

// ── Linking ──────────────────────────────────────────────────────────────────

/// Link a frame set to a cached project (idempotent). `NotFound` when the
/// project isn't cached, is lost (R14), or the set doesn't exist.
pub fn link_frame_set(
    ctx: &ServiceContext,
    project_id: &str,
    frames_set_id: i64,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    crate::api::collab_exchange::live_project(&conn, project_id)?;
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
    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
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

/// Set one project's auto-publish preference (P13, R16): whether a coalesced
/// publish run fires for this project on scan/analysis/solve/link/master/
/// calibration-link triggers. Local-only, like `set_project_auto_replicate`
/// (`api::collab_exchange`) — the hub never learns of it, and `NotFound` when
/// the project isn't cached or lost (R14; a toggle for a project this device
/// doesn't know about, or was removed from, is a caller bug, not a silent
/// no-op).
pub async fn set_project_auto_publish(
    ctx: &ServiceContext,
    project_id: &str,
    on: bool,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::api::collab_exchange::live_project(&conn, project_id)?;
    crate::db::collab::set_auto_publish(&conn, project_id, on).map_err(internal)?;
    tracing::info!(project_id, on, "collab auto-publish toggled");
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
    let blocker_rows: Vec<crate::collab::gate::BlockerRow<'_>> = gated
        .iter()
        .map(|(id, row)| crate::collab::gate::BlockerRow {
            frame_id: row.frame_id,
            set_id: id.set_id,
            instrume: &id.instrume,
            filter_raw: &id.filter_raw,
            filter_unresolved: id.filter_unresolved,
            failures: &row.failures,
        })
        .collect();
    let blockers = crate::collab::gate::derive_blockers(&blocker_rows);
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
        blockers,
    })
}

/// What publish needs from a gate input beyond the verdict row: the frame's
/// uuid (P18), its dictionary filter match (P3), and the blocker-derivation
/// context (spec 2026-09-28 §7.1) — its linked set, whether that set is
/// attested, its camera and raw filter name, and whether the filter resolved.
struct GateIdentity {
    uuid: String,
    filter_canonical: Option<String>,
    set_id: Option<i64>,
    /// Spec §6 (F5): whether this frame's linked set has attested its own
    /// calibration. The publish split (`run_publish`) copies this onto
    /// [`PublishCandidate`] and branches on it BEFORE any calibration
    /// resolution — an attested light is never resolved or regenerated.
    attested: bool,
    instrume: String,
    filter_raw: String,
    filter_unresolved: bool,
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
    // never a silent pass. The dictionary is filled by the project refresh
    // the live feed runs; until a project has one, nothing here can pass this
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

    // Spec §3.2: an explicit mapping only ever applies within the signed-in
    // account's own scope; signed out (or a read failure — fail-closed, see
    // `current_account_email`) means no mappings at all, never a mix-up with
    // someone else's.
    let mappings = current_account_email(conn)
        .map(|account| crate::db::collab::filter_mappings_for_account(conn, &account))
        .transpose()
        .map_err(internal)?
        .unwrap_or_default();
    // F5 / a failed attestation read (fail-closed) is treated as not attested
    // — never a silent skip of the calibration precondition.
    let attested: HashSet<i64> = set_ids
        .iter()
        .copied()
        .filter(|s| {
            crate::db::collab::frames_set_attested(conn, *s).unwrap_or_else(|e| {
                tracing::warn!(set_id = s, error = %e, "attestation read failed; treated as not attested");
                false
            })
        })
        .collect();

    let inputs = frame_gate_inputs(
        conn,
        &frames,
        &dictionary,
        &frame_sets,
        &mappings,
        &attested,
    )
    .map_err(internal)?;

    Ok(inputs
        .into_iter()
        .map(|(i, instrume)| {
            let row = evaluate_frame(&i, &target, &rules);
            let set_id = frame_sets.get(&i.frame_id).copied();
            (
                GateIdentity {
                    uuid: i.uuid,
                    filter_canonical: i.filter.canonical().map(str::to_string),
                    attested: set_id.is_some_and(|s| attested.contains(&s)),
                    set_id,
                    instrume,
                    filter_unresolved: i.filter.is_unresolved(),
                    filter_raw: i.filter_raw,
                },
                row,
            )
        })
        .collect())
}

// ── Filter mapping sheet (spec 2026-09-28 §5.2) ─────────────────────────────

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingRowView {
    pub instrume: String,
    pub filter_raw: String,
    pub frames: i64,
    /// `mapped` | `mappedToMissing` | `matched` | `unmapped`
    pub resolution: String,
    pub canonical: Option<String>,
    pub proposal: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingSheet {
    pub project_id: String,
    pub dictionary: Vec<DictionaryEntry>,
    pub rows: Vec<FilterMappingRowView>,
}

#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingEdit {
    pub instrume: String,
    pub filter_raw: String,
    /// `None` = delete the row ("back to automatic").
    pub canonical: Option<String>,
}

fn project_dictionary(project: &CollabProjectRow) -> Result<Vec<DictionaryEntry>, ApiError> {
    match &project.dictionary_json {
        Some(json) => serde_json::from_str(json).map_err(|e| {
            tracing::error!(project_id = %project.project_id, error = %e, "cached filter dictionary does not parse");
            ApiError::Internal(format!("cached filter dictionary does not parse: {e}"))
        }),
        None => {
            tracing::warn!(project_id = %project.project_id, "filter mapping refused: no cached dictionary yet");
            Err(ApiError::Invalid("the project's filter dictionary has not been fetched yet".into()))
        }
    }
}

/// Every distinct (camera, raw FILTER) among the project's candidate frames
/// with its resolution and a proposal (F3): unresolved rows first, then by
/// frame count descending, camera, raw name.
pub fn get_filter_mapping_sheet(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<FilterMappingSheet, ApiError> {
    use crate::collab::filters::FilterResolution;
    let db = db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id)
        .map_err(internal)?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "project {project_id} is not cached — refresh first"
            ))
        })?;
    let dictionary = project_dictionary(&project)?;
    let mappings = current_account_email(&conn)
        .map(|a| crate::db::collab::filter_mappings_for_account(&conn, &a))
        .transpose()
        .map_err(internal)?
        .unwrap_or_default();
    let set_ids = crate::db::collab::linked_set_ids(&conn, project_id).map_err(internal)?;
    let frames = union_light_frames(&conn, &set_ids).map_err(internal)?;
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    if !frames.is_empty() {
        let ids: Vec<i64> = frames.iter().map(|(id, _)| *id).collect();
        let placeholders = vec!["?"; ids.len()].join(",");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT instrume, filter FROM frames WHERE id IN ({placeholders})"
            ))
            .map_err(|e| internal(e.into()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(|e| internal(e.into()))?;
        for row in rows {
            let (i, f) = row.map_err(|e| internal(e.into()))?;
            *counts
                .entry((
                    i.unwrap_or_default().trim().to_string(),
                    f.unwrap_or_default().trim().to_string(),
                ))
                .or_default() += 1;
        }
    }
    // Carry `is_unresolved()` alongside each view so the sort below reads it
    // straight off the `FilterResolution`, not by re-deriving it from the
    // `resolution` string the view already flattened it into.
    let mut rows: Vec<(bool, FilterMappingRowView)> = counts
        .into_iter()
        .map(|((instrume, filter_raw), frames)| {
            let res = crate::collab::filters::resolve_filter(
                &filter_raw,
                &instrume,
                &mappings,
                &dictionary,
            );
            let unresolved = res.is_unresolved();
            let (resolution, canonical) = match &res {
                FilterResolution::Mapped(c) => ("mapped", Some(c.clone())),
                FilterResolution::MappedToMissing(c) => ("mappedToMissing", Some(c.clone())),
                FilterResolution::Matched(c) => ("matched", Some(c.clone())),
                FilterResolution::Unmapped => ("unmapped", None),
            };
            let proposal = if unresolved {
                crate::collab::filters::propose_canonical(&filter_raw, &dictionary)
            } else {
                None
            };
            (
                unresolved,
                FilterMappingRowView {
                    instrume,
                    filter_raw,
                    frames,
                    resolution: resolution.into(),
                    canonical,
                    proposal,
                },
            )
        })
        .collect();
    rows.sort_by(|(ua, a), (ub, b)| {
        ub.cmp(ua)
            .then(b.frames.cmp(&a.frames))
            .then(a.instrume.cmp(&b.instrume))
            .then(a.filter_raw.cmp(&b.filter_raw))
    });
    let rows: Vec<FilterMappingRowView> = rows.into_iter().map(|(_, r)| r).collect();
    tracing::info!(project_id, rows = rows.len(), "filter mapping sheet built");
    Ok(FilterMappingSheet {
        project_id: project_id.to_string(),
        dictionary,
        rows,
    })
}

/// Upsert/delete the account's rows in one `BEGIN IMMEDIATE`, refuse a
/// canonical outside the project's dictionary before writing anything, mark
/// the project dirty for auto-publish, return the fresh gate report.
pub fn set_filter_mappings(
    ctx: &ServiceContext,
    project_id: &str,
    edits: Vec<FilterMappingEdit>,
) -> Result<GateReport, ApiError> {
    {
        let db = db(ctx)?;
        let mut conn = db.conn();
        let project = crate::db::collab::get_project(&conn, project_id)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!(
                    "project {project_id} is not cached — refresh first"
                ))
            })?;
        let dictionary = project_dictionary(&project)?;
        let account = current_account_email(&conn).ok_or_else(|| {
            tracing::warn!(project_id, "filter mappings refused: signed out");
            ApiError::SignedOut("Sign in to map filters.".into())
        })?;
        for e in &edits {
            if let Some(c) = &e.canonical {
                if !dictionary.iter().any(|d| &d.canonical == c) {
                    tracing::warn!(project_id, canonical = %c, "filter mapping refused: canonical not in the dictionary");
                    return Err(ApiError::Invalid(format!(
                        "{c:?} is not in the project dictionary"
                    )));
                }
            }
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| internal(e.into()))?;
        for e in &edits {
            let instrume = e.instrume.trim();
            let raw = e.filter_raw.trim();
            match &e.canonical {
                Some(c) => {
                    crate::db::collab::upsert_filter_mapping(&tx, &account, instrume, raw, c)
                        .map_err(internal)?
                }
                None => {
                    crate::db::collab::delete_filter_mapping(&tx, &account, instrume, raw)
                        .map_err(internal)?;
                }
            }
        }
        tx.commit().map_err(|e| internal(e.into()))?;
        tracing::info!(project_id, count = edits.len(), "filter mappings saved");
    }
    // An empty edit list changes nothing — no dirty mark, no auto-publish
    // wake-up for it.
    if !edits.is_empty() {
        crate::api::collab_autopublish::request_auto_publish(Some(project_id));
    }
    evaluate_project_gate(ctx, project_id)
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
                distance_deg: d,
            });
        }
    }
    Ok(out)
}

// ── Frame set's project status (Task 7, spec §8.2) ──────────────────────────

/// One linked project's per-frame contributor state over one frame set's
/// LIGHT frames.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetProjectLink {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    /// Amendment A6: this device is the account's publishing device for the
    /// project.
    pub publishing_here: bool,
    pub auto_publish: bool,
    pub counts: ContributorCounts,
    pub frames: Vec<FrameProjectState>,
}

/// One LIGHT frame's contributor state within a [`FrameSetProjectLink`].
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameProjectState {
    pub frame_id: i64,
    /// A [`crate::collab::contributor_state::ContributorState`] key.
    pub state: String,
    pub reason: Option<String>,
}

/// Per-state tally of a [`FrameSetProjectLink`]'s frames — one field per
/// [`crate::collab::contributor_state::ContributorState`] variant.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContributorCounts {
    pub not_published: i64,
    pub fails_gate: i64,
    pub pending_approval: i64,
    pub published: i64,
    pub update_pending: i64,
    pub rejected: i64,
    pub published_not_on_disk: i64,
    pub published_now_fails_gate: i64,
}

impl ContributorCounts {
    fn bump(&mut self, state: crate::collab::contributor_state::ContributorState) {
        use crate::collab::contributor_state::ContributorState as S;
        match state {
            S::NotPublished => self.not_published += 1,
            S::FailsGate => self.fails_gate += 1,
            S::PendingApproval => self.pending_approval += 1,
            S::Published => self.published += 1,
            S::UpdatePending => self.update_pending += 1,
            S::Rejected => self.rejected += 1,
            S::PublishedNotOnDisk => self.published_not_on_disk += 1,
            S::PublishedNowFailsGate => self.published_now_fails_gate += 1,
        }
    }
}

/// A cached project not yet linked to the set but whose target radius
/// contains it ([`find_matching_projects`], carried through with its
/// distance).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetProjectCandidate {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub distance_deg: f64,
}

/// What the frame set's page shows about projects: every project it is
/// linked to (with per-frame contributor state + counts), or, when unlinked,
/// nearby candidate projects.
#[derive(Debug, Clone, Default, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetProjectStatus {
    pub links: Vec<FrameSetProjectLink>,
    pub candidates: Vec<FrameSetProjectCandidate>,
}

/// Spec §8.2 — what the frame set's page shows about projects: every project
/// the set is linked to, with the same per-frame contributor state
/// [`crate::collab::contributor_state::derive`] computes everywhere else, or,
/// when the set isn't linked to anything yet, nearby candidate projects
/// ([`find_matching_projects`], reused here for a set the user is looking at
/// directly rather than one just auto-clustered). Signed out returns empty
/// links and candidates, never an error (spec §3.1's fail-closed default).
pub fn get_frame_set_project_status(
    ctx: &ServiceContext,
    frames_set_id: i64,
) -> Result<FrameSetProjectStatus, ApiError> {
    use crate::collab::contributor_state::{derive, OwnRowFacts};
    let db = db(ctx)?;
    let conn = db.conn();
    let mut links = Vec::new();
    let mut candidates = Vec::new();
    if current_account_email(&conn).is_none() {
        return Ok(FrameSetProjectStatus { links, candidates });
    }

    let set_frames = union_light_frames(&conn, &[frames_set_id]).map_err(internal)?;
    let set_frame_ids: HashSet<i64> = set_frames.iter().map(|(id, _)| *id).collect();
    let me = device_for_cards(ctx);

    for p in crate::db::collab::list_projects(&conn).map_err(internal)? {
        if !crate::db::collab::is_set_linked(&conn, &p.project_id, frames_set_id)
            .map_err(internal)?
        {
            continue;
        }
        let gated = project_gate(&conn, &p)?;
        let own = crate::db::collab_frames::own_by_source_frame(&conn, &p.project_id)
            .map_err(internal)?;

        let mut counts = ContributorCounts::default();
        let mut frames = Vec::new();
        for (identity, row) in gated
            .iter()
            .filter(|(_, r)| set_frame_ids.contains(&r.frame_id))
        {
            let own_row = own.get(&row.frame_id);
            // Finding 3: a recipe read stats the light's source file on disk
            // (`recipe_hash_for`) — only worth paying for a frame this
            // account has actually published; `derive` ignores it otherwise.
            let current_recipe = own_row
                .and_then(|_| current_recipe_for_frame(&conn, row.frame_id, identity.attested));
            let reject_reason = own_row.and_then(|o| {
                crate::api::collab_exchange::parse_manifest_wire(
                    &p.project_id,
                    &o.frame_uuid,
                    &o.manifest_json,
                    "get_frame_set_project_status",
                )
                .and_then(|w| w.reject_reason)
            });
            let facts = own_row.map(|o| OwnRowFacts {
                state: o.state.as_str(),
                content_version: o.content_version,
                recipe_hash: o.recipe_hash.as_deref(),
                on_disk: o.on_disk,
                reject_reason: reject_reason.as_deref(),
            });
            let (state, reason) = derive(
                facts,
                current_recipe.as_deref(),
                row.publishable,
                row.failures.first().map(String::as_str),
            );
            counts.bump(state);
            frames.push(FrameProjectState {
                frame_id: row.frame_id,
                state: state.key().to_string(),
                reason,
            });
        }

        let publishing =
            crate::db::collab::publishing_device(&conn, &p.project_id).map_err(internal)?;
        let publishing_here = match (&publishing, me.as_deref()) {
            (Some(publisher), Some(me)) => publisher.device_id == me,
            _ => false,
        };

        links.push(FrameSetProjectLink {
            project_id: p.project_id.clone(),
            slug: p.slug.clone(),
            title: p.title.clone(),
            publishing_here,
            auto_publish: p.auto_publish,
            counts,
            frames,
        });
    }

    if links.is_empty() {
        if let Some((ra, dec)) =
            crate::api::frame_sets::frame_set_center_deg(&conn, frames_set_id).map_err(internal)?
        {
            for m in find_matching_projects(&conn, ra, dec, frames_set_id).map_err(internal)? {
                candidates.push(FrameSetProjectCandidate {
                    project_id: m.project_id,
                    slug: m.project_slug,
                    title: m.project_title,
                    distance_deg: m.distance_deg,
                });
            }
        }
    }

    tracing::info!(
        frames_set_id,
        links = links.len(),
        candidates = candidates.len(),
        "frame set project status built"
    );
    Ok(FrameSetProjectStatus { links, candidates })
}

/// Every own row's contributor state, keyed by frame uuid (Task 7) — the same
/// [`crate::collab::contributor_state::derive`] [`get_frame_set_project_status`]
/// uses, so the project page's own-frames table
/// ([`crate::api::collab_live::surface::list_collab_frames`]) shows the
/// identical chip. A gate or own-row read failure is logged and leaves the
/// map empty — never a hard error out of a frames list.
pub(crate) fn own_contributor_states(
    conn: &Connection,
    project_id: &str,
    project: &CollabProjectRow,
) -> HashMap<String, (String, Option<String>)> {
    use crate::collab::contributor_state::{derive, OwnRowFacts};
    let gated = match project_gate(conn, project) {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(project_id, error = %e, "gate evaluation failed; own contributor state left blank");
            return HashMap::new();
        }
    };
    let own = match crate::db::collab_frames::own_by_source_frame(conn, project_id) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(project_id, error = %e, "own-frame read failed; own contributor state left blank");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    for (identity, row) in &gated {
        let Some(own_row) = own.get(&row.frame_id) else {
            continue;
        };
        let current_recipe = current_recipe_for_frame(conn, row.frame_id, identity.attested);
        let reject_reason = crate::api::collab_exchange::parse_manifest_wire(
            project_id,
            &own_row.frame_uuid,
            &own_row.manifest_json,
            "own_contributor_states",
        )
        .and_then(|w| w.reject_reason);
        let facts = OwnRowFacts {
            state: own_row.state.as_str(),
            content_version: own_row.content_version,
            recipe_hash: own_row.recipe_hash.as_deref(),
            on_disk: own_row.on_disk,
            reject_reason: reject_reason.as_deref(),
        };
        let (state, reason) = derive(
            Some(facts),
            current_recipe.as_deref(),
            row.publishable,
            row.failures.first().map(String::as_str),
        );
        out.insert(
            own_row.frame_uuid.clone(),
            (state.key().to_string(), reason),
        );
    }
    out
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
        E::NotFound(m) => ApiError::NotFound(m),
        E::DuplicateName => ApiError::Invalid("name already in use".into()),
        E::Forbidden => {
            ApiError::Forbidden("The account's role may not perform this action.".into())
        }
        E::CollabApiOutdated => {
            crate::account::client::warn_collab_api_outdated_once();
            ApiError::Conflict(COLLAB_API_OUTDATED_MSG.into())
        }
        E::Http { message, .. } | E::Gone(message) | E::Decode(message) => {
            ApiError::Internal(format!("Hub request failed: {message}"))
        }
        E::SessionGone => ApiError::Internal("hub session expired".into()),
        E::VersionConflict { content_version } => ApiError::Conflict(format!(
            "version_conflict: the hub has content version {content_version}"
        )),
        E::PublishingDevice { device_name, .. } => ApiError::Conflict(
            crate::account::client::publishing_device_msg(device_name.as_deref()),
        ),
        E::NotPublishingDevice { device_name, .. } => ApiError::Conflict(
            crate::account::client::not_publishing_device_msg(device_name.as_deref()),
        ),
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

/// This device's key for `publishing_here`, or `None` (logged) when it
/// cannot be read — a card then never claims "publishing here".
fn device_for_cards(ctx: &ServiceContext) -> Option<String> {
    match crate::api::account::own_device_id(ctx) {
        Ok(me) => Some(me),
        Err(e) => {
            tracing::warn!(error = %e, "project cards: this device's key is unavailable; no project shows as publishing here");
            None
        }
    }
}

/// Build a [`ProjectCard`] from a cached row, computing the live counts.
/// `card_from_row` never holds a DB connection across the gate call. `me`
/// is this device's key (`publishing_here`).
fn card_from_row(
    ctx: &ServiceContext,
    row: CollabProjectRow,
    me: Option<&str>,
) -> Result<ProjectCard, ApiError> {
    let (linked_sets, publishing) = {
        let db = db(ctx)?;
        let conn = db.conn();
        (
            crate::db::collab::linked_set_ids(&conn, &row.project_id)
                .map_err(internal)?
                .len() as i64,
            crate::db::collab::publishing_device(&conn, &row.project_id).map_err(internal)?,
        )
    };
    let publishing_here = match (&publishing, me) {
        (Some(p), Some(me)) => p.device_id == me,
        _ => false,
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
        auto_publish: row.auto_publish,
        fetched_at: row.fetched_at,
        publishing_device: publishing.map(|p| PublishingDeviceView {
            device_id: p.device_id,
            name: p.name,
        }),
        publishing_here,
    })
}

/// Every cached project as a card (instant — cache only, no hub I/O).
pub fn list_projects(ctx: &ServiceContext) -> Result<Vec<ProjectCard>, ApiError> {
    let rows = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn).map_err(internal)?
    };
    let me = device_for_cards(ctx);
    rows.into_iter()
        .map(|row| card_from_row(ctx, row, me.as_deref()))
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
    let (row, links, portal_base, goals_json) = {
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
        let (goals_json, _members_seen_json) =
            crate::db::collab::page_extras(&conn, project_id).map_err(internal)?;
        (row, links, portal_base, goals_json)
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
    let me = device_for_cards(ctx);
    let goals = parse_goals(project_id, goals_json.as_deref());
    let card = card_from_row(ctx, row, me.as_deref())?;

    Ok(ProjectDetail {
        card,
        members,
        thresholds_version,
        thresholds,
        links,
        portal_base,
        goals,
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
        .project_page(&p.id, Some(token))
        .await
        .map_err(|e| FetchError::Transport(e.into()))?;
    let (goals_json, members_seen_json) = page_extras_of(&page);
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
        feed_epoch: None,
        holder_seq: -1,
    };
    Ok(FetchedProject {
        row,
        dictionary,
        goals_json,
        members_seen_json,
    })
}

/// One project as a refresh fetched it: the cache row, plus the dictionary
/// when it was due (`Some((version, entries JSON))`, both `None` when the
/// hub has no dictionary), plus the page extras (spec 2026-09-29 §5.5/§5.6)
/// this refresh always fetches.
struct FetchedProject {
    row: CollabProjectRow,
    dictionary: Option<(Option<i32>, Option<String>)>,
    goals_json: Option<String>,
    members_seen_json: String,
}

/// One member's last-seen as the hub reported it on the project page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberSeen {
    pub display_name: String,
    pub last_seen_at: Option<String>,
}

/// The two project-page extras the cache keeps beside the row: the goals JSON
/// (`None` when the hub has none set, or it decoded to `null`) and the
/// members-seen JSON (always an array, `"[]"` when the page had no members).
pub fn page_extras_of(
    page: &crate::collab::hub_client::ProjectPageWire,
) -> (Option<String>, String) {
    let goals = page
        .project
        .goals
        .as_ref()
        .filter(|v| !v.is_null())
        .map(|v| v.to_string());
    let seen: Vec<MemberSeen> = page
        .members
        .iter()
        .map(|m| MemberSeen {
            display_name: m.display_name.clone(),
            last_seen_at: m.last_seen_at.clone(),
        })
        .collect();
    (
        goals,
        serde_json::to_string(&seen).unwrap_or_else(|_| "[]".into()),
    )
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
/// the live feed's — so a change is acted on once, whichever path absorbed
/// it. Dirties the project for auto-publish (Task 10, R16): a tightened
/// threshold or a dictionary change can turn a previously-refused frame
/// publishable. May fire twice for one change (a UI refresh and the live
/// feed overlapping) — the auto-publish worker's dirty-set + kick + debounce
/// design collapses that into one run (see
/// `collab_autopublish::two_gate_moves_for_the_same_project_drain_to_one_entry`).
pub(crate) fn on_thresholds_or_dictionary_moved(_ctx: &ServiceContext, project_id: &str) {
    tracing::debug!(project_id, "thresholds or dictionary moved");
    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
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
/// `only` limits the per-project fetch to those ids (the live feed passes
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
            Ok(FetchedProject {
                row,
                dictionary,
                goals_json,
                members_seen_json,
            }) => {
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
                crate::db::collab::set_page_extras(
                    &conn,
                    &row.project_id,
                    goals_json.as_deref(),
                    &members_seen_json,
                )
                .map_err(internal)?;
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

    // A6: the account's publishing device, straight from `/me/projects`
    // for every listed project (a project whose row this refresh could not
    // create is skipped by the write itself). A moved binding re-arms
    // auto-publish: a device that stopped on a refusal resumes, a device
    // that lost the binding stops on the next run's check.
    // (project, the device bound before) for every binding that moved HERE.
    let mut moved_here: Vec<(String, Option<String>)> = Vec::new();
    let mut me: Option<String> = None;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        for p in &mine {
            let device =
                p.publishing_device
                    .as_ref()
                    .map(|d| crate::db::collab::PublishingDevice {
                        device_id: d.device_id.clone(),
                        name: d.name.clone(),
                    });
            if let Some(previous) =
                crate::db::collab::replace_publishing_device(&conn, &p.id, device.as_ref())
                    .map_err(internal)?
            {
                tracing::info!(
                    project_id = %p.id,
                    device_id = ?device.as_ref().map(|d| d.device_id.as_str()),
                    "the account's publishing device moved"
                );
                crate::api::collab_autopublish::request_auto_publish(Some(&p.id));
                if let Some(d) = &device {
                    if me.is_none() {
                        me = device_for_cards(ctx);
                    }
                    if me.as_deref() == Some(d.device_id.as_str()) {
                        moved_here.push((p.id.clone(), previous));
                    }
                }
            }
        }
    }
    // Fix rounds 1+2: own frames the hub lost while another device was
    // bound are listed again now that this device is — only when another
    // device was bound before, or a re-announce was refused meanwhile.
    if let Some(me) = &me {
        for (project_id, previous) in &moved_here {
            crate::api::collab_live::feed::after_binding_moved_here(
                ctx,
                project_id,
                previous.as_deref(),
                me,
            )
            .await;
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
            // Task 15 R1: a lost project is never reported again — its claim
            // set, outbox and holder map go with it (a re-join reloads them).
            crate::db::collab_live::clear_project_live_state(&conn, lost).map_err(internal)?;
            // Its fetches stop at once (the runtime finds it lost).
            crate::api::collab_live::notify_local_change(ctx, lost);
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
    // I11 (Task 15 R4): every membership refresh rebuilt what the connect
    // gate admits — close the open collab connections it no longer does.
    if let Some(node) = crate::api::collab_exchange::bound_node(ctx).await {
        let closed = node.close_collab_connections_not_admitted();
        if closed > 0 {
            tracing::info!(
                count = closed,
                "collab connections of devices no longer admitted closed"
            );
        }
    }

    Ok(report)
}

// ── Publishing device (amendment A6) ────────────────────────────────────────

/// "Publish from this device" (amendment A6): make THIS device the one that
/// announces new frames of this account into the project (`PUT
/// /projects/{id}/publishing-device`). Idempotent. The previously bound
/// device can still post new versions of the frames it published, but no
/// new frames. Stores the binding, re-arms auto-publish, and returns the
/// refreshed card.
pub async fn set_collab_publishing_device(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<ProjectCard, ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::warn!(project_id, "publishing device switch refused: signed out");
        return Err(ApiError::SignedOut(
            "Sign in to use collaboration projects.".into(),
        ));
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::api::collab_exchange::live_project(&conn, project_id)?;
    }
    let client = CollabClient::new(&hub_url).map_err(|e| {
        tracing::error!(project_id, error = %e, "publishing device switch: hub client failed");
        client_err(e)
    })?;
    let reply = client
        .set_publishing_device(&token, project_id)
        .await
        .map_err(|e| {
            tracing::error!(project_id, error = %e, "publishing device switch failed");
            client_err(e)
        })?;
    let moved = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::replace_publishing_device(
            &conn,
            project_id,
            Some(&crate::db::collab::PublishingDevice {
                device_id: reply.device_id.clone(),
                name: reply.name.clone(),
            }),
        )
        .map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publishing device switch: storing the binding failed");
            internal(e)
        })?
    };
    // Fix rounds 1+2: own frames the hub lost while another device was
    // bound (an epoch-change re-announce refused then) are listed again —
    // when another device was bound before, or the refusal mark is set. An
    // unchanged cache (already this device) needs the mark.
    let previous = match moved {
        Some(previous) => previous,
        None => Some(reply.device_id.clone()),
    };
    crate::api::collab_live::feed::after_binding_moved_here(
        ctx,
        project_id,
        previous.as_deref(),
        &reply.device_id,
    )
    .await;
    tracing::info!(
        project_id,
        device_id = %reply.device_id,
        changed = reply.changed,
        "publishing device switched to this device"
    );
    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
    let row = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::get_project(&conn, project_id)
            .map_err(internal)?
            .ok_or_else(|| {
                ApiError::NotFound(format!(
                    "project {project_id} is not cached — refresh first"
                ))
            })?
    };
    let me = device_for_cards(ctx);
    card_from_row(ctx, row, me.as_deref())
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
    /// Amendment A6: set ONLY for a new frame held back because another
    /// device of this account is the project's publishing device — that
    /// device's name, or "another device of this account". `None` for every
    /// other reason. The UI keys on this field, never on the reason text.
    pub publishing_device: Option<String>,
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

/// Spec §6 (F5): the recipe of an attested (externally calibrated) light —
/// no masters, no engine version, just the catalog's own drift detector.
/// Any change to `files.size`/`files.modified_at` (the scanner's in-place
/// re-parse after the user overwrites the file with a new external pass)
/// moves the recipe, which the split reads as an `Update`.
pub(crate) fn external_recipe(size: i64, modified_at: &str) -> String {
    format!("external:{size}:{modified_at}")
}

/// Task 7 (spec §8.1/§8.2): the recipe a publish would compute for
/// `frame_id` right now — [`external_recipe`] for an attested light, else
/// [`recipe_hash_of_inputs`] over its resolved calibration links. `None` when
/// the light cannot be resolved (a deleted source frame, e.g., or no
/// calibration links at all) — the contributor-state derivation reads that as
/// "can't tell", never as an error.
pub(crate) fn current_recipe_for_frame(
    conn: &Connection,
    frame_id: i64,
    attested: bool,
) -> Option<String> {
    if attested {
        conn.query_row(
            "SELECT fi.size, fi.modified_at FROM frames f JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
            [frame_id],
            |r| Ok(external_recipe(r.get::<_, i64>(0)?, &r.get::<_, String>(1)?)),
        )
        .map_err(|e| {
            tracing::debug!(frame_id, error = %e, "current recipe: could not read the attested light's file row");
        })
        .ok()
    } else {
        let resolved = crate::calibration_library::light_resolve::resolve_frame_inputs(
            conn,
            frame_id,
            publish_options().flat_norm,
        )
        .map_err(|e| {
            tracing::debug!(frame_id, error = %e, "current recipe: could not resolve the light's calibration inputs");
        })
        .ok()?;
        recipe_hash_of_inputs(conn, &resolved)
            .map_err(|e| {
                tracing::debug!(frame_id, error = %e, "current recipe: could not hash the resolved calibration inputs");
            })
            .ok()
    }
}

/// Spec §6 (F8) — informational only, carried on every NEW frame's announce:
/// which masters a generation actually used, or `external: true` for an
/// attested light that was never calibrated by this app at all. Reads the
/// resolved `GenerationSpec`'s own typed master fields rather than a
/// path-name heuristic, so it can never disagree with what was applied.
pub(crate) fn calibration_meta(
    spec: Option<&crate::export::GenerationSpec>,
    external: bool,
) -> serde_json::Value {
    serde_json::json!({
        "dark": spec.is_some_and(|s| s.inputs.dark_path.is_some()),
        "flat": spec.is_some_and(|s| s.inputs.flat_path.is_some()),
        "bias": spec.is_some_and(|s| s.inputs.bias_path.is_some()),
        "external": external,
    })
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

/// The hub's per-frame `FrameIn` rules a new frame's manifest fields must
/// pass before it is announced (final review M1) — a batch is atomic, so one
/// frame the hub refuses would refuse every frame announced with it. `None`
/// when it passes; else the held-back reason.
fn hub_frame_rule_problem(meta: &crate::collab::frame_meta::FrameMeta) -> Option<String> {
    const META_MAX_BYTES: usize = 8192;
    if !(meta.exptime_sec > 0.0 && meta.exptime_sec <= 86_400.0) {
        return Some(format!(
            "the exposure time ({} s) is outside the hub's range of more than 0 and at most 86400 s — is EXPTIME missing from the header?",
            meta.exptime_sec
        ));
    }
    // Controller ruling 2026-09-28: the hub accepts an EMPTY filterRaw (an
    // unfiltered light with no dictionary mapping yet, or one mapped to the
    // dictionary's own `None` entry) — 0..=80 characters after trimming.
    let filter = meta.filter_raw.trim();
    if filter.chars().count() > 80 {
        return Some(format!(
            "the filter name must be at most 80 characters, is {} characters",
            filter.chars().count()
        ));
    }
    if !matches!(
        meta.channel.as_str(),
        "mono" | "osc" | "osc-r" | "osc-g" | "osc-b"
    ) {
        return Some(format!("unknown channel {:?}", meta.channel));
    }
    if !meta.meta.is_object() {
        return Some("the frame metadata is not a JSON object".into());
    }
    let size = serde_json::to_string(&meta.meta).map_or(usize::MAX, |m| m.len());
    if size > META_MAX_BYTES {
        return Some(format!(
            "the frame metadata is {size} bytes; the hub takes at most {META_MAX_BYTES}"
        ));
    }
    None
}

/// M4: `build_frame_meta` then `hub_frame_rule_problem`, in one place — both
/// branches of the publish split's pass 3 (a generated New and an
/// attested/external New) ran this identical two-step sequence separately
/// before this helper. `Err(reason)` covers both ways a NEW frame's manifest
/// fields can hold it back (an unreadable frame, or one the hub's per-frame
/// rules refuse) and has ALREADY logged its own `error!`/`warn!` — the caller
/// only needs to `held_back.push(held(fid, &cand.filename, reason))` and
/// `continue`.
fn new_frame_meta_or_hold_back(
    conn: &Connection,
    project_id: &str,
    frame_id: i64,
) -> Result<crate::collab::frame_meta::FrameMeta, String> {
    let m = crate::collab::frame_meta::build_frame_meta(conn, frame_id).map_err(|e| {
        tracing::error!(project_id, frame_id, error = %format!("{e:#}"), "publish: frame meta failed");
        format!("cannot read frame metadata: {e:#}")
    })?;
    if let Some(problem) = hub_frame_rule_problem(&m) {
        tracing::warn!(project_id, frame_id, reason = %problem, "publish: frame breaks a hub rule");
        return Err(problem);
    }
    Ok(m)
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

/// F5/C1: whether `path` is a landing this publisher's own generation chose
/// (`<own_dir>/<name>`, via [`new_frame_target`]) — the ONE shape a
/// generated Update/Adopt may safely reuse as its target. Anything else
/// (an attested original, a path from a previous own-dir spelling) is never
/// reused: a set that was attested and is now un-attested (or vice versa)
/// must never let a generation write over — or an attestation seed read as
/// though it owned — a file this publisher did not itself land there.
fn is_own_dir_landing(path: &Path, own_dir: &Path) -> bool {
    path.parent() == Some(own_dir)
}

/// Belt-and-braces alongside [`is_own_dir_landing`] (fix round 2): a row
/// whose stored recipe is external-shaped is never trusted as a generation
/// target either, independent of where its `landed_path` happens to sit —
/// two ways of catching the same crossing, since neither is airtight alone
/// (a mid-flight `stage_own_file` clears `recipe_hash` to `NULL`; a path
/// spelling drift could in principle fool the own-dir check).
fn was_external_recipe(row: &crate::db::collab_frames::LocalFrameRow) -> bool {
    row.recipe_hash
        .as_deref()
        .is_some_and(|r| r.starts_with("external:"))
}

/// The landing path of a NEW own frame: `<dir>/<name>`, else `<stem>_2`, … —
/// the first spelling that is neither claimed earlier in this run, nor any
/// cached frame's `landed_path`, nor (amendment A6) any `fileName` this
/// publisher already has in the project's manifest (`taken_names`). A file already there that no row references
/// is the leftover of an earlier run whose announce failed (own rows are
/// recorded only after a successful announce), and it is written over, so a
/// retry re-announces the same file instead of piling up copies.
fn new_frame_target(
    conn: &Connection,
    dir: &Path,
    name: &str,
    claimed: &mut HashSet<std::path::PathBuf>,
    taken_names: &HashSet<String>,
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
        // A6: never a name this publisher already uses anywhere in the
        // project's manifest — another device of this account (before a
        // switch) may have published it, from another folder.
        if candidate
            .file_name()
            .is_some_and(|n| taken_names.contains(n.to_string_lossy().as_ref()))
        {
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
/// whether it replaces the landed file (P19): `<target>.athpub`. Never the
/// landing temp's `.athtmp` (final fix B-M2): the live session removes
/// every `.athtmp` under the root when it starts (a crashed landing's), and
/// a publish runs regardless of the session — it must never lose its temp
/// to that sweep. The watcher ignores both; a crash's leftover `.athpub` is
/// overwritten by the frame's next regeneration.
fn update_temp_path(target: &Path) -> std::path::PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".");
    s.push(crate::collab::storage::watch::PUBLISH_TEMP_EXT);
    std::path::PathBuf::from(s)
}

/// One gate-passing frame a publish run considers.
struct PublishCandidate {
    frame_id: i64,
    filename: String,
    uuid: String,
    filter_canonical: String,
    /// Spec §6 (F5), copied from [`GateIdentity`]: whether the frame's linked
    /// set is attested. The split branches on this BEFORE any calibration
    /// resolution.
    attested: bool,
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
    /// Manifest fields of a NEW frame, built and checked against the hub's
    /// per-frame rules by the split (M1); `None` otherwise.
    meta: Option<crate::collab::frame_meta::FrameMeta>,
    /// Spec §6 (F5): an attested light, seeded in place with no generation —
    /// `target` is the frame's current catalog path, never a landing name
    /// the split picked.
    external: bool,
    /// The split's `external:<size>:<modified_at>` recipe (F5), carried
    /// through for an external plan only. A generated plan recomputes its
    /// own recipe from the resolved `GenerationSpec` (unchanged behavior),
    /// so this stays `None` there.
    recipe: Option<String>,
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
    /// BLAKE3 of the regenerated temp of an update or adoption — compared
    /// again with the row as it is at seeding time (final review C1c).
    staged_blake3: Option<String>,
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
        publishing_device: None,
    }
}

/// A new frame held back because another device of this account is the
/// project's publishing device (amendment A6): the reason text plus the
/// structured `publishing_device` the UI keys on.
fn held_bound_elsewhere(frame_id: i64, filename: &str, device_name: Option<&str>) -> HeldBackFrame {
    HeldBackFrame {
        publishing_device: Some(crate::account::client::publishing_device_label(device_name)),
        ..held(frame_id, filename, publishing_device_reason(device_name))
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

/// The generation phase of a publish run. F5: an attested (external) plan is
/// handled first and needs no compute at all — hashed and stat'd straight
/// from its catalog path. Whatever remains runs on a blocking thread under
/// ONE `ComputeQueue` permit (the `sync_prepare::open_generation` pattern,
/// taken only when there is something to generate), resolving every planned
/// frame in one catalog borrow, stating the masters it reads, then
/// calibrating each exactly once — a new frame straight into its landing
/// path, an update or adoption into a sibling temp whose BLAKE3 decides
/// whether anything changed. A failing frame is held back with its reason;
/// the run goes on.
fn run_publish_generation(job: GenerationJob) -> Result<GenerationOutcome, ApiError> {
    use crate::services::compute_queue::ComputeJobKind;

    let pid = job.project_id.as_str();
    let mut held_back: Vec<HeldBackFrame> = Vec::new();
    let mut written: Vec<WrittenFrame> = Vec::new();
    let mut unchanged = 0usize;

    // F5/A1: an attested light is never regenerated, renamed or copied — it
    // is hashed and stat'd IN PLACE, with no compute permit and no spec.
    let (external_plans, generated_plans): (Vec<PlannedFrame>, Vec<PlannedFrame>) =
        job.plans.into_iter().partition(|p| p.external);

    for plan in external_plans {
        let PlannedFrame {
            cand,
            kind,
            target,
            mut meta,
            recipe,
            ..
        } = plan;
        let fid = cand.frame_id;
        // M2: an external plan always carries `Some` recipe by construction
        // (pass 3) — a `None` here is a broken invariant, not a value to
        // paper over with an empty string (which would force a republish
        // every run).
        let recipe = match recipe {
            Some(r) => r,
            None => {
                tracing::error!(
                    project_id = pid,
                    frame_id = fid,
                    "publish: an attested plan carried no recipe (internal invariant violation)"
                );
                held_back.push(held(
                    fid,
                    &cand.filename,
                    "internal error: no recipe for an attested light".into(),
                ));
                continue;
            }
        };
        let xxh3 = match xxh3_full_file(&target) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(project_id = pid, frame_id = fid, path = %target.display(), error = %format!("{e:#}"), "publish: hashing the attested light failed");
                held_back.push(held(
                    fid,
                    &cand.filename,
                    format!("cannot read the attested light: {e:#}"),
                ));
                continue;
            }
        };
        let byte_size = match std::fs::metadata(&target) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::error!(project_id = pid, frame_id = fid, path = %target.display(), error = %e, "publish: stat of the attested light failed");
                held_back.push(held(
                    fid,
                    &cand.filename,
                    format!("cannot read the attested light: {e}"),
                ));
                continue;
            }
        };
        // I3: an Update/Adopt's blake3, over the ORIGINAL — so the same
        // `identical` shortcut the generated path uses (P19) also catches a
        // `touch`, an archive round-trip, or a copy that moved
        // `modified_at` without changing the bytes. A New frame has no hub
        // blake3 to compare against, so this stays `None` for it, exactly
        // as the generated loop does.
        let staged_blake3 = if kind.row().is_some() {
            match blake3_file(&target) {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::error!(project_id = pid, frame_id = fid, path = %target.display(), error = %format!("{e:#}"), "publish: hashing the attested light for the identity check failed");
                    held_back.push(held(
                        fid,
                        &cand.filename,
                        format!("cannot hash the attested light: {e:#}"),
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        if let Some(m) = meta.as_mut() {
            m.meta["calibration"] = calibration_meta(None, true);
        }
        tracing::debug!(
            project_id = pid,
            frame_id = fid,
            path = %target.display(),
            bytes = byte_size,
            "publish: attested light staged in place"
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
            target: target.clone(),
            staged: target,
            recipe,
            xxh3,
            byte_size,
            meta,
            identical: false,
            staged_blake3,
        });
    }

    if generated_plans.is_empty() {
        return Ok(GenerationOutcome {
            written,
            unchanged,
            held_back,
        });
    }

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
    let mut prepared: Vec<(PlannedFrame, crate::export::GenerationSpec, String)> = Vec::new();
    {
        let conn = job.db.conn();
        let mut divisors = crate::export::DivisorCache::new();
        let mut master_ok: HashMap<std::path::PathBuf, bool> = HashMap::new();
        for plan in generated_plans {
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
            prepared.push((plan, spec, recipe));
        }
    }
    tracing::info!(
        project_id = pid,
        count = prepared.len(),
        "publish: calibrated-light generation planned"
    );

    // Pixel phase: no catalog connection held.
    let mut hot_maps = HashMap::new();
    // (frame_uuid, recipe, verified blake3, regenerated xxh3)
    let mut identical: Vec<(String, String, String, String)> = Vec::new();
    for (plan, spec, recipe) in prepared {
        let PlannedFrame {
            cand,
            kind,
            target,
            mut meta,
            ..
        } = plan;
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
        let mut staged_blake3: Option<String> = None;
        if let Some(row) = kind.row() {
            match blake3_file(&staged) {
                Ok(b) => {
                    same_as_hub = b == row.blake3;
                    staged_blake3 = Some(b);
                }
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
                identical.push((
                    row.frame_uuid.clone(),
                    recipe,
                    row.blake3.clone(),
                    xxh3.clone(),
                ));
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
        // F8: which masters this generation actually used — a NEW frame
        // only (`meta` stays `None` for an update, which keeps the hub's
        // prior meta).
        if let Some(m) = meta.as_mut() {
            m.meta["calibration"] = calibration_meta(Some(&spec), false);
        }
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
            staged_blake3,
        });
    }
    drop(permit);

    if !identical.is_empty() {
        let conn = job.db.conn();
        for (frame_uuid, recipe, blake3, xxh3) in identical {
            if let Err(e) =
                crate::db::collab_frames::set_recipe_hash(&conn, pid, &frame_uuid, &recipe)
            {
                tracing::error!(project_id = pid, frame_uuid = %frame_uuid, error = %format!("{e:#}"), "publish: storing the new recipe failed");
            }
            // The regeneration matched the hub's version: a row staged with
            // exactly these bytes is confirmed (Task 10, C11).
            if let Err(e) =
                crate::db::collab_frames::clear_own_staged(&conn, pid, &frame_uuid, &blake3, &xxh3)
            {
                tracing::error!(project_id = pid, frame_uuid = %frame_uuid, error = %format!("{e:#}"), "publish: clearing the staged mark failed");
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
    /// An update staged on its own row (`stage_own_file`): the hub's `xxh3`
    /// and `byteSize` it replaced, put back when the version is refused.
    hub_prior: Option<(String, i64)>,
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
    e.hub_text()
        .is_some_and(|m| m.contains("gate version") && m.contains("is stale"))
}

/// Indices of the batch frames a 409 `frame {uuid} already announced; use
/// /version …` names (ruling R8a). Empty for any other refusal.
fn already_announced_in(
    e: &crate::account::AccountClientError,
    batch: &[SeededFrame],
) -> Vec<usize> {
    let Some(m) = e.hub_text() else {
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
pub(crate) fn announce_batches<T>(mut rest: Vec<T>) -> std::collections::VecDeque<Vec<T>> {
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

/// The held-back reason of a new frame this device may not announce
/// (amendment A6).
fn publishing_device_reason(device_name: Option<&str>) -> String {
    format!(
        "{} publishes new frames to this project — use \"Publish from this device\" to switch",
        crate::account::client::publishing_device_label(device_name)
    )
}

/// The `last_error` a `not_publishing_device` version refusal leaves on an
/// own row (amendment A6), tied to the manifest version it was refused at:
/// the frame is not versioned again until its manifest row changes.
fn not_publishing_mark(manifest_version: i64) -> String {
    format!(
        "not_publishing_device: another device of this account versions this frame (manifest {manifest_version})"
    )
}

/// Whether an own row's recorded publishing device (its manifest row's
/// `publisherDeviceId`) is another device than this one (or one it
/// replaced) — its versions are that device's to post (amendment A6).
/// `None`/unparseable → not another.
fn published_by_another_device(
    project_id: &str,
    row: &crate::db::collab_frames::LocalFrameRow,
    own: &crate::api::collab_exchange::OwnDevices,
) -> bool {
    crate::api::collab_exchange::parse_manifest_wire(
        project_id,
        &row.frame_uuid,
        &row.manifest_json,
        "publish",
    )
    .and_then(|v| {
        v.publisher_device_id
            .map(|d| !own.is_mine(&d, &v.publisher_account_id))
    })
    .unwrap_or(false)
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

/// Put the hub's content keys back on staged updates that never became a
/// version (the hub refused it, or the run stopped before posting): the
/// landed file holds bytes the hub does not have, so the row is marked not
/// on disk and the next run regenerates and posts again (final review I2).
/// Under the project's disk lock, per frame; failures are logged.
async fn unstage_updates(
    ctx: &ServiceContext,
    disk_lock: &tokio::sync::Mutex<()>,
    project_id: &str,
    frames: &[&SeededFrame],
) {
    for f in frames {
        let Some((xxh3, byte_size)) = &f.hub_prior else {
            continue;
        };
        let _disk = disk_lock.lock().await;
        let result = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
            crate::db::collab_frames::unstage_own_file(
                &db.conn(),
                project_id,
                &f.written.uuid,
                xxh3,
                *byte_size,
            )
        });
        match result {
            Ok(_) => {
                tracing::warn!(project_id, frame_uuid = %f.written.uuid, "publish: new version not taken; the regenerated file is not held")
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: restoring the own frame row failed")
            }
        }
    }
}

/// A version the hub took: re-tag the seed when the hub's number differs
/// from the one this run seeded, and queue the frame for its write-back.
async fn accept_version(
    node: &crate::sharing::iroh::node::SharedIrohNode,
    project_id: &str,
    mut f: SeededFrame,
    content_version: i32,
    versioned: &mut Vec<SeededFrame>,
) {
    if content_version != f.content_version {
        tracing::warn!(project_id, frame_uuid = %f.written.uuid, content_version, expected = f.content_version, "publish: hub assigned a different content version; re-tagging");
        if let Err(e) = node
            .seed_project_frame(
                project_id,
                &f.written.uuid,
                content_version,
                &f.written.target,
            )
            .await
        {
            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: re-tagging under the hub's version failed");
        }
        f.content_version = content_version;
    }
    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = f.content_version, "publish: new frame version");
    versioned.push(f);
}

/// A real version conflict: unseed, unstage, hold the frame back with the
/// hub's number, and remember it for the post-run manifest check.
#[allow(clippy::too_many_arguments)]
async fn refuse_version(
    ctx: &ServiceContext,
    node: &crate::sharing::iroh::node::SharedIrohNode,
    disk_lock: &tokio::sync::Mutex<()>,
    project_id: &str,
    f: SeededFrame,
    hub_version: i32,
    conflicts: &mut Vec<(String, i32)>,
    held_back: &mut Vec<HeldBackFrame>,
) {
    tracing::warn!(project_id, frame_uuid = %f.written.uuid, content_version = hub_version, "publish: version conflict; the hub has a newer version");
    unseed_all(node, project_id, &[&f]).await;
    unstage_updates(ctx, disk_lock, project_id, &[&f]).await;
    held_back.push(held(
        f.written.frame_id,
        &f.written.filename,
        format!("version conflict: the hub has content version {hub_version}"),
    ));
    conflicts.push((f.written.uuid.clone(), hub_version));
}

/// The hub's `(contentVersion, blake3)` for each of `uuids`, from a manifest
/// delta read starting at `since` (every changed frame carries a
/// `manifestVersion` above the row's last-seen one). Stops once every uuid is
/// found or the manifest ends.
async fn hub_frame_versions(
    client: &CollabClient,
    token: &str,
    project_id: &str,
    since: i64,
    uuids: &HashSet<String>,
) -> Result<HashMap<String, (i32, String)>, crate::account::AccountClientError> {
    let mut out = HashMap::new();
    let mut since = since;
    let mut after: Option<String> = None;
    loop {
        let page = client
            .manifest_page(token, project_id, since, after.as_deref(), 1000)
            .await?;
        for v in page.rows {
            if uuids.contains(&v.frame_uuid) {
                out.insert(v.frame_uuid, (v.content_version, v.blake3));
            }
        }
        if out.len() == uuids.len() || !page.has_more {
            return Ok(out);
        }
        match page.next {
            Some(n) => {
                since = n.since;
                after = Some(n.after);
            }
            None => {
                tracing::warn!(project_id, "manifest page says hasMore without a next cursor; stopping the version read here");
                return Ok(out);
            }
        }
    }
}

/// A `conflict` at `expected + 1` is our own version (a retried call whose
/// first reply was lost) only when the hub's bytes for it are ours.
fn is_own_lost_reply(expected: i32, ours_blake3: &str, hub: Option<&(i32, String)>) -> bool {
    hub.is_some_and(|(cv, b3)| *cv == expected + 1 && b3 == ours_blake3)
}

/// Re-tag an update's seed under the version the hub already holds for the
/// same bytes (C1c), dropping the tag of the version this run never posted.
async fn retag_under_hub_version(
    node: &crate::sharing::iroh::node::SharedIrohNode,
    project_id: &str,
    f: &mut SeededFrame,
    hub_version: i32,
) {
    if let Err(e) = node
        .seed_project_frame(project_id, &f.written.uuid, hub_version, &f.written.target)
        .await
    {
        tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: re-tagging under the hub's version failed");
    }
    let stale = crate::sharing::iroh::node::project_frame_tag(
        project_id,
        &f.written.uuid,
        f.content_version,
    );
    if let Some(store) = node.collab_store() {
        if let Err(e) = store.tags().delete(&stale).await {
            tracing::warn!(project_id, frame_uuid = %f.written.uuid, tag = %stale, error = %e, "publish: dropping the unposted version tag failed");
        }
    }
    f.content_version = hub_version;
}

/// Publish a project's gate-passing calibrated lights, one frame at a time
/// (collab v3 wave 2, §5.2): calibrate each light ONCE straight into my own
/// folder under the Collaboration root, seed it into the collab store by
/// reference, announce new frames in batches of at most 500, post a new
/// content version for an own frame whose recipe changed (P19), and record
/// my own rows. Refuses without a Collaboration root (P25).
///
/// One publish run per project at a time (final review C1, owner decision
/// 2026-09-24): while a run of the same project holds [`publish_lock`] —
/// manual, republish or auto — this command is REFUSED at once with
/// `Conflict(`[`PUBLISH_BUSY_MSG`]`)`, never queued or waited for. Two
/// overlapping runs would each compare against the row as it was before the
/// other posted, and post the same bytes as a second version.
pub async fn publish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    let lock = publish_lock(ctx, project_id)?;
    let _run = claim_publish(&lock, project_id)?;
    run_publish(ctx, project_id, emitter, false, None).await
}

/// The refusal a publish (manual, republish or auto) gets while another
/// publish run of the same project holds [`publish_lock`] (owner decision
/// 2026-09-24). The auto-publish worker matches it and re-dirties the project
/// instead of running in parallel.
pub(crate) const PUBLISH_BUSY_MSG: &str = "publication of this project is already running";

/// The auto-publish worker's entry point — [`publish_collab_frames`] under
/// the same lock and the same refusal; kept separate so the worker's call
/// site names what it is. On `Conflict(`[`PUBLISH_BUSY_MSG`]`)` the worker
/// marks the project dirty again, so it runs after the current run ends.
pub(crate) async fn auto_publish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    let lock = publish_lock(ctx, project_id)?;
    let _run = claim_publish(&lock, project_id)?;
    run_publish(ctx, project_id, emitter, false, None).await
}

/// Take the project's publish lock without waiting, or refuse with
/// [`PUBLISH_BUSY_MSG`] (logged).
fn claim_publish<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    project_id: &str,
) -> Result<tokio::sync::MutexGuard<'a, ()>, ApiError> {
    lock.try_lock().map_err(|_| {
        tracing::warn!(
            project_id,
            outcome = "publish_busy",
            "publish refused: a publication of this project is already running"
        );
        ApiError::Conflict(PUBLISH_BUSY_MSG.into())
    })
}

/// The per-project publish lock, keyed by catalog + project, shared by the
/// publish and republish commands and the auto-publish worker (final review
/// C1: two overlapping runs each compared against the row as it was before
/// the other posted, and posted the same bytes as a new version).
pub(crate) fn publish_lock(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, ApiError> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let key = format!("{}|{project_id}", db(ctx)?.path().display());
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(Arc::clone(locks.entry(key).or_default()))
}

/// The manual re-publish (P19): every own frame is regenerated as an
/// `update`, then the same-hash rule applies — only frames whose bytes
/// changed get a new content version. The remedy after an app release that
/// changed the calibration engine or its defaults. Refused while a publish
/// run of the same project is in progress, like [`publish_collab_frames`].
pub async fn republish_collab_frames(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError> {
    let lock = publish_lock(ctx, project_id)?;
    let _run = claim_publish(&lock, project_id)?;
    run_publish(ctx, project_id, emitter, true, None).await
}

/// The refusal a publish gets while the Collaboration folder's store cannot
/// be mounted (final review I3).
pub(crate) const COLLAB_STORE_UNMOUNTED: &str =
    "The Collaboration folder's frame store is not available — check that the folder is reachable, then publish again.";

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
    let (project, gated, binding) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        let gated = project_gate(&conn, &project)?;
        let binding =
            crate::db::collab::publishing_device(&conn, project_id).map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the publishing device failed");
                internal(e)
            })?;
        (project, gated, binding)
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
                attested: id.attested,
            });
        } else {
            held_back.push(HeldBackFrame {
                frame_id: row.frame_id,
                filename: row.filename,
                reasons: row.failures,
                publishing_device: None,
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
    // I3: nothing can be seeded without the collab store — refuse before any
    // calibration, never regenerate every frame only to hold it back.
    if crate::api::collab_exchange::ensure_collab_store(ctx)
        .await
        .is_none()
    {
        tracing::warn!(
            project_id,
            outcome = "collab_store_unmounted",
            "publish refused: the collab store is not mounted"
        );
        return Err(ApiError::Conflict(COLLAB_STORE_UNMOUNTED.into()));
    }
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

    // A6: one publishing device per (project, account). When the cached
    // binding names another device (in service — a revoked one reads as
    // unbound), no new frame is generated or announced here: the hub would
    // refuse it. Versions of this device's own frames still go out.
    let me = crate::api::account::own_device_id(ctx).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: this device's key is unavailable");
        e
    })?;
    // Fix round 2 (M3): a binding naming a device THIS device replaced is
    // not "elsewhere" (that device is retired; the hub reads it unbound).
    let replaced = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_live::replaced_devices(&conn).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the replaced devices failed");
            internal(e)
        })?
    };
    let bound_elsewhere: Option<Option<String>> = binding
        .filter(|b| b.device_id != me && !replaced.contains_key(&b.device_id))
        .map(|b| b.name);
    let mut refused_new = 0usize;

    // ── 3. The split (ruling R9: before any compute permit) ─────────────────
    let opts = publish_options();
    let mut unchanged = 0usize;
    let mut plans: Vec<PlannedFrame> = Vec::new();
    // Hoisted out of the split's block (below) so the seed-by-reference
    // section (F5/C1/I1) can also tell an own-dir landing apart from an
    // attested original.
    let own_dir = {
        let db = db(ctx)?;
        let conn = db.conn();
        publisher_folder(&conn, &collab_root, &project, &account_id, &display, "own").map_err(
            |e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: own folder failed");
                internal(e)
            },
        )?
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let own_devices = crate::api::collab_exchange::own_devices(ctx, &conn, &project)?;
        let own = frames_db::own_by_source_frame(&conn, project_id).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: read own frames failed");
            internal(e)
        })?;
        // Pass 1: what each candidate is (new / update / adopt). F5: an
        // attested candidate is branched here, BEFORE any calibration
        // resolution — an attested light is never resolved or calibrated,
        // only hashed and stat'd in place (the generation phase).
        let mut split: Vec<(
            PublishCandidate,
            PublishKind,
            bool,
            Option<(std::path::PathBuf, String)>,
        )> = Vec::new();
        for cand in candidates {
            let fid = cand.frame_id;
            let (recipe, osc, external): (String, bool, Option<(std::path::PathBuf, String)>) =
                if cand.attested {
                    let row: rusqlite::Result<(String, i64, String)> = conn.query_row(
                        "SELECT fi.path, fi.size, fi.modified_at FROM frames f \
                         JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
                        [fid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    );
                    match row {
                        Ok((path, size, modified)) => {
                            // M1: `osc` (`opts.debayer_osc && osc`) only ever
                            // feeds `calibrated_output_filename` — dead on
                            // this branch, which never calls it (an
                            // attested light's name is its own basename).
                            // Never queried, so there is nothing to swallow.
                            let recipe = external_recipe(size, &modified);
                            (
                                recipe.clone(),
                                false,
                                Some((std::path::PathBuf::from(path), recipe)),
                            )
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %e, "publish: reading the attested light failed");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!("cannot read the attested light: {e}"),
                            ));
                            continue;
                        }
                    }
                } else {
                    let resolved =
                        match crate::calibration_library::light_resolve::resolve_frame_inputs(
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
                    (recipe, resolved.cfa_geometry.is_some(), None)
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
            match &kind {
                PublishKind::New => {
                    if let Some(name) = &bound_elsewhere {
                        held_back.push(held_bound_elsewhere(fid, &cand.filename, name.as_deref()));
                        refused_new += 1;
                        continue;
                    }
                }
                PublishKind::Update(row) | PublishKind::Adopt(row) => {
                    if published_by_another_device(project_id, row, &own_devices) {
                        tracing::debug!(project_id, frame_uuid = %row.frame_uuid, "publish: frame published by another device of this account; its versions are that device's");
                        continue;
                    }
                    if !force
                        && row.last_error.as_deref()
                            == Some(not_publishing_mark(row.manifest_version).as_str())
                    {
                        tracing::debug!(project_id, frame_uuid = %row.frame_uuid, manifest_version = row.manifest_version, "publish: version refused at this manifest version; not retried until the manifest changes");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            "another device of this account versions this frame now".into(),
                        ));
                        continue;
                    }
                }
            }
            split.push((cand, kind, osc, external));
        }
        // Pass 2 (final review I1): every update and adoption target is
        // reserved BEFORE any new frame picks its name — an adoption without
        // a recorded path lands at `<own>/<fileName>`, which no row
        // references yet, so a new frame could otherwise pick the same file.
        // F5: an external candidate's target is always deferred to pass 3
        // (its own CURRENT catalog path, dedup-checked there) regardless of
        // kind — never the previous own row's `landed_path`, which for a
        // frame attested after an earlier generated publish would still name
        // the old calibrated file, not the original.
        let mut claimed: HashSet<std::path::PathBuf> = HashSet::new();
        let mut taken_names = if account_id.is_empty() {
            HashSet::new()
        } else {
            frames_db::file_names_of_publisher(&conn, project_id, &account_id).map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the publisher's file names failed");
                internal(e)
            })?
        };
        let mut targeted: Vec<(
            PublishCandidate,
            PublishKind,
            bool,
            Option<std::path::PathBuf>,
            Option<(std::path::PathBuf, String)>,
        )> = Vec::with_capacity(split.len());
        for (cand, kind, osc, external) in split {
            let target = if external.is_some() {
                None
            } else {
                match &kind {
                    PublishKind::New => None,
                    PublishKind::Update(row) => match &row.landed_path {
                        // C1: a row whose landed_path is not this
                        // publisher's own-dir landing — or whose recipe is
                        // external-shaped (fix round 2 belt-and-braces) —
                        // was an attested original (or is otherwise stale):
                        // never reuse it as a generation target; defer to
                        // pass 3's fresh-name picker exactly like a New
                        // frame.
                        Some(p)
                            if is_own_dir_landing(Path::new(p), &own_dir)
                                && !was_external_recipe(row) =>
                        {
                            Some(std::path::PathBuf::from(p))
                        }
                        Some(p) => {
                            tracing::info!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, path = %p, "publish: own frame's landed path is not this publisher's landing (un-attested or moved); picking a fresh one");
                            None
                        }
                        None => {
                            tracing::error!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, "publish: own frame has no landed path");
                            held_back.push(held(
                                cand.frame_id,
                                &cand.filename,
                                "own frame has no file path".into(),
                            ));
                            continue;
                        }
                    },
                    PublishKind::Adopt(row) => match &row.landed_path {
                        Some(p)
                            if is_own_dir_landing(Path::new(p), &own_dir)
                                && !was_external_recipe(row) =>
                        {
                            Some(std::path::PathBuf::from(p))
                        }
                        Some(p) => {
                            tracing::info!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, path = %p, "publish: own frame's landed path is not this publisher's landing (un-attested or moved); picking a fresh one");
                            None
                        }
                        None => Some(own_dir.join(&row.file_name)),
                    },
                }
            };
            if let Some(t) = &target {
                claimed.insert(t.clone());
            }
            targeted.push((cand, kind, osc, target, external));
        }
        // Pass 3: new frames — their manifest fields checked against the
        // hub's per-frame rules (M1: one bad frame refuses a whole atomic
        // batch), then a free landing name. F5: an external candidate never
        // picks a landing name — its target IS the frame's current catalog
        // path, and a basename collision within this publisher is held back,
        // never renamed on disk.
        for (cand, kind, osc, target, external) in targeted {
            let fid = cand.frame_id;
            if let Some((path, recipe)) = external {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let meta = if matches!(kind, PublishKind::New) {
                    // Controller ruling (fix round 2, finding 3):
                    // `landed_path` is `TEXT UNIQUE` table-wide, and a frame
                    // set is linked, not owned — the SAME set can be linked
                    // to more than one project. A NEW attested frame whose
                    // original already backs ANOTHER project's own row
                    // would hit that constraint only after the hub
                    // announce succeeded, orphaning it there. Caught here,
                    // before anything is planned; a read failure holds back
                    // rather than risk the same orphan (fail-closed).
                    match crate::db::collab_frames::find_by_landed_path(
                        &conn,
                        &path.to_string_lossy(),
                    ) {
                        Ok(Some(existing)) if existing.project_id != project_id => {
                            let title = crate::db::collab::get_project(&conn, &existing.project_id)
                                .ok()
                                .flatten()
                                .map(|p| p.title)
                                .unwrap_or_else(|| existing.project_id.clone());
                            tracing::warn!(project_id, frame_id = fid, other_project = %existing.project_id, path = %path.display(), "publish: attested light's original already backs another project's frame");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!(
                                    "this file already backs a frame in project \"{title}\" — a file can back one project frame"
                                ),
                            ));
                            continue;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: checking the attested light's landed path failed");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!("cannot verify this file is not already published elsewhere: {e:#}"),
                            ));
                            continue;
                        }
                    }
                    // Dedup (F5): a NEW frame only — an Update or Adopt
                    // reuses the SAME name it already legitimately holds
                    // (it is its own file, unchanged), which `taken_names`
                    // already carries from its own prior publish; checking
                    // it here would refuse a frame against itself.
                    if taken_names.contains(&file_name) || !claimed.insert(path.clone()) {
                        tracing::warn!(project_id, frame_id = fid, file_name = %file_name, "publish: attested basename already published by this publisher");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!(
                                "a frame named {file_name:?} is already published by you — rename the file"
                            ),
                        ));
                        continue;
                    }
                    // I2: the hub's per-file-name rule — an original's
                    // basename is not chosen by the app the way a generated
                    // `c_<stem>.fits` is, and can break it (`:`, surrounding
                    // whitespace, empty).
                    if let Some(problem) = hub_file_name_problem(&file_name) {
                        tracing::warn!(project_id, frame_id = fid, path = %path.display(), reason = problem, "publish: attested file name breaks the hub rule");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!("the hub refuses this file name ({problem}): {file_name}"),
                        ));
                        continue;
                    }
                    taken_names.insert(file_name);
                    match new_frame_meta_or_hold_back(&conn, project_id, fid) {
                        Ok(m) => Some(m),
                        Err(reason) => {
                            held_back.push(held(fid, &cand.filename, reason));
                            continue;
                        }
                    }
                } else {
                    claimed.insert(path.clone());
                    None
                };
                plans.push(PlannedFrame {
                    cand,
                    kind,
                    target: path,
                    meta,
                    external: true,
                    recipe: Some(recipe),
                });
                continue;
            }
            if let Some(target) = target {
                plans.push(PlannedFrame {
                    cand,
                    kind,
                    target,
                    meta: None,
                    external: false,
                    recipe: None,
                });
                continue;
            }
            // C1/I1: this fallback now also serves a generated Update/Adopt
            // whose PREVIOUS landed_path was not this publisher's own-dir
            // landing (pass 2, `is_own_dir_landing`) — an un-attested light,
            // or one whose landing folder otherwise changed. It picks a
            // fresh own-dir name exactly like a New frame, but keeps its
            // row (`kind` stays `Update`/`Adopt`) and carries no manifest
            // meta — a version never re-announces it; the hub keeps the
            // prior meta.
            let meta = if matches!(kind, PublishKind::New) {
                match new_frame_meta_or_hold_back(&conn, project_id, fid) {
                    Ok(m) => Some(m),
                    Err(reason) => {
                        held_back.push(held(fid, &cand.filename, reason));
                        continue;
                    }
                }
            } else {
                None
            };
            let name = crate::export::calibrated_output_filename(
                &cand.filename,
                opts.debayer_osc && osc,
                opts.format,
            );
            let target = match new_frame_target(&conn, &own_dir, &name, &mut claimed, &taken_names)
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: no landing path");
                    held_back.push(held(fid, &cand.filename, format!("no landing path: {e:#}")));
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
            // M3: without this, a generated New's picked basename never
            // enters `taken_names` — only the attested/external branch (and
            // `file_names_of_publisher`'s seed from a PRIOR run) did — so an
            // attested New later in this SAME run whose basename happens to
            // match could pass its own `taken_names.contains` check and both
            // would announce under one fileName.
            taken_names.insert(file_name);
            plans.push(PlannedFrame {
                cand,
                kind,
                target,
                meta,
                external: false,
                recipe: None,
            });
        }
        if let Some(hook) = after_split {
            hook(&conn);
        }
        if refused_new > 0 && plans.is_empty() {
            // Nothing else to do: refuse at once, before any permit or hub
            // call (auto-publish stays quiet on it until the binding moves).
            let name = bound_elsewhere.clone().flatten();
            tracing::debug!(
                project_id,
                count = refused_new,
                outcome = "publishing_device",
                "publish: another device of this account publishes into this project; nothing announced"
            );
            return Err(ApiError::Conflict(
                crate::account::client::publishing_device_msg(name.as_deref()),
            ));
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
    // An update or adoption replaces the landed file, so it runs under the
    // project's disk lock per frame (final review I2): the rename, the seed
    // and the local own-row write are one step disk truth never sees half
    // done. The row it writes describes the NEW file; the hub-confirmed
    // columns follow after `…/version` (`set_own_version`), or are put back
    // when the hub refuses it (`unstage_updates`).
    let disk_lock = crate::api::collab_exchange::project_disk_lock(ctx, project_id)?;
    let mut new_frames: Vec<SeededFrame> = Vec::new();
    let mut updates: Vec<SeededFrame> = Vec::new();
    let mut bound: Vec<SeededFrame> = Vec::new();
    for w in outcome.written {
        if w.kind.row().is_none() {
            match node
                .seed_project_frame(project_id, &w.uuid, 1, &w.target)
                .await
            {
                Ok(hash) => new_frames.push(SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: 1,
                    hub_prior: None,
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
        }
        let _disk = disk_lock.lock().await;
        // C1c: the row as it is NOW — not as the split saw it.
        let current = {
            let db = db(ctx)?;
            let conn = db.conn();
            frames_db::get(&conn, project_id, &w.uuid)
        };
        let current = match current {
            Ok(Some(row)) if row.origin == FrameOrigin::Own => row,
            Ok(_) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, "publish: the own frame row vanished before seeding");
                // F5/A1: `staged == target` for an external frame — its
                // original is never removed.
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    "the frame's project row vanished during the run".into(),
                ));
                continue;
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: re-reading the own frame failed");
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    format!("cannot read the frame's project row: {e:#}"),
                ));
                continue;
            }
        };
        let identical = w.identical || w.staged_blake3.as_deref() == Some(current.blake3.as_str());
        if identical && !w.identical {
            // C1c: another run already versioned exactly these bytes.
            tracing::info!(project_id, frame_uuid = %w.uuid, content_version = current.content_version, "publish: the hub already has these bytes; no new version");
            if let PublishKind::Update(_) = &w.kind {
                // F5/A1: never remove an external frame's original.
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                // I2: an attested original moved on disk (the app's own file
                // browser, or a forced Republish reading its CURRENT catalog
                // path) keeps identical bytes and size/mtime, so the recipe
                // is unchanged and this identical-bytes branch is the one
                // that runs — but `landed_path` still names the OLD location
                // unless moved here too, or the row reads "not on disk"
                // forever. Same `update_landed_path` + UNIQUE-failure
                // handling as the generated-file move above.
                let new_landed = w.target.to_string_lossy().to_string();
                if current.landed_path.as_deref() != Some(new_landed.as_str()) {
                    let moved = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                        frames_db::update_landed_path(&db.conn(), project_id, &w.uuid, &new_landed)
                    });
                    match moved {
                        Ok(n) if n > 0 => {
                            tracing::info!(project_id, frame_uuid = %w.uuid, from = current.landed_path.as_deref().unwrap_or(""), to = %new_landed, "publish: own frame's landed path moved (identical bytes, moved on disk)");
                        }
                        Ok(_) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, "publish: moving the own frame's landed path matched no row");
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, error = %format!("{e:#}"), "publish: moving the own frame's landed path failed");
                        }
                    }
                }
                let written = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    frames_db::set_recipe_hash(&conn, project_id, &w.uuid, &w.recipe).and_then(
                        |n| {
                            // The hub already versioned exactly these bytes: a
                            // row staged with them is confirmed (Task 10, C11).
                            frames_db::clear_own_staged(
                                &conn,
                                project_id,
                                &w.uuid,
                                &current.blake3,
                                &w.xxh3,
                            )?;
                            Ok(n)
                        },
                    )
                };
                if let Err(e) = written {
                    tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: storing the new recipe failed");
                }
                unchanged += 1;
                continue;
            }
        }
        let prior_version = current.content_version;
        let version = if identical {
            prior_version
        } else {
            // The old tag must not pin the old content once the landed file
            // is replaced.
            if let Err(e) = node.unseed_project_frame(project_id, &w.uuid).await {
                tracing::warn!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: unseeding the previous version failed");
            }
            prior_version + 1
        };
        // F5/A1: an external Update's `staged` IS `target` — the same
        // original path — so there is nothing to rename, and never a
        // `remove_temp` on it either.
        if w.staged != w.target {
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
        }
        match node
            .seed_project_frame(project_id, &w.uuid, version, &w.target)
            .await
        {
            Ok(hash) => {
                // C1/I1: the landing crossed between generated and attested
                // (or its folder otherwise changed) — move `landed_path` to
                // where this run actually put the frame's CURRENT bytes,
                // before any downstream write depends on it agreeing
                // (`stage_own_file`'s `WHERE landed_path = ?` needs the row
                // to already match). The stale file is removed ONLY when
                // the move itself actually succeeded AND it was this
                // publisher's own-dir landing — an app-generated artifact,
                // never an attested original (F5/A1: that file is never
                // removed, whatever the set's attestation is now).
                //
                // Fix round 2 (finding 2): `landed_path` is `TEXT UNIQUE`
                // table-wide, and a frame set can be linked to more than one
                // project — a move CAN fail with a real collision (another
                // project's own row already holds this exact path). A
                // failed move must never delete the row's OWN still-current
                // file: that would leave the row pointing at nothing.
                let new_landed = w.target.to_string_lossy().to_string();
                if current.landed_path.as_deref() != Some(new_landed.as_str()) {
                    let moved = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                        frames_db::update_landed_path(&db.conn(), project_id, &w.uuid, &new_landed)
                    });
                    let moved_ok = match moved {
                        Ok(n) if n > 0 => {
                            tracing::info!(project_id, frame_uuid = %w.uuid, from = current.landed_path.as_deref().unwrap_or(""), to = %new_landed, "publish: own frame's landed path moved");
                            true
                        }
                        Ok(_) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, "publish: moving the own frame's landed path matched no row");
                            false
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, error = %format!("{e:#}"), "publish: moving the own frame's landed path failed");
                            false
                        }
                    };
                    if moved_ok {
                        if let Some(old) = current.landed_path.as_deref() {
                            let old_path = Path::new(old);
                            if is_own_dir_landing(old_path, &own_dir) {
                                // C1 fix round 3, part 1: `is_own_dir_landing`
                                // only tells us the path SITS in our own
                                // folder shape — not that WE put it there. A
                                // user can move (or attest) their own light
                                // straight into that folder; deleting it
                                // would be real data loss. Two independent
                                // guards, belt-and-braces with the
                                // `publisher_dir` fix above:
                                //   - a `files` row still referencing this
                                //     exact path means it is catalogued —
                                //     generated files are never catalogued,
                                //     so this check is exact, no false
                                //     positive possible.
                                //   - `was_external_recipe(&current)`: the
                                //     row we are moving OFF of was itself an
                                //     attested (external) landing.
                                let catalogued = {
                                    let conn = match db(ctx) {
                                        Ok(db) => Some(db.conn()),
                                        Err(e) => {
                                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %e, "publish: checking whether the superseded path is catalogued failed; not removing it");
                                            None
                                        }
                                    };
                                    match conn {
                                        Some(conn) => {
                                            crate::db::file_exists(&conn, old)
                                                .unwrap_or_else(|e| {
                                                    tracing::error!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %format!("{e:#}"), "publish: checking whether the superseded path is catalogued failed; not removing it");
                                                    true
                                                })
                                        }
                                        None => true,
                                    }
                                };
                                if catalogued {
                                    tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: skipped removing the superseded path — a files row still references it");
                                } else if was_external_recipe(&current) {
                                    tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: skipped removing the superseded path — its recipe was external");
                                } else {
                                    match std::fs::remove_file(old_path) {
                                        Ok(()) => {
                                            tracing::info!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: removed the superseded generated file")
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                        Err(e) => {
                                            tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %e, "publish: removing the superseded generated file failed")
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let mut f = SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: version,
                    hub_prior: None,
                    written: w,
                };
                if identical {
                    f.written.identical = true;
                    bound.push(f);
                    continue;
                }
                f.hub_prior = Some((current.xxh3.clone(), current.byte_size));
                let staged = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    frames_db::stage_own_file(
                        &conn,
                        project_id,
                        &f.written.uuid,
                        &f.written.target.to_string_lossy(),
                        &f.written.xxh3,
                        f.written.byte_size as i64,
                        size_mtime_seen(&f.written.target).as_deref(),
                    )
                };
                match staged {
                    Ok(0) => {
                        // Fix round 2 (finding 1): a silent no-match here
                        // means `landed_path` in the row does not (yet, or
                        // any longer) equal `f.written.target` — most often
                        // because the move above failed (finding 2) — and
                        // disk truth is now silently stale until the next
                        // publish notices the drift.
                        tracing::error!(project_id, frame_uuid = %f.written.uuid, path = %f.written.target.display(), "publish: recording the regenerated file matched no row");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the regenerated file failed");
                    }
                }
                updates.push(f);
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
    // Final fix D-18: every frame this run announces or versions is "in
    // flight" until step 7 recorded its implicit claim (the guard lives to
    // the end of the run) — a full holder report never ends its claim
    // explicitly meanwhile, although the hub may hold it before the local
    // set does.
    let _claim_calls = crate::api::collab_live::holdings::ClaimCalls::begin(
        ctx,
        project_id,
        new_frames
            .iter()
            .chain(updates.iter())
            .map(|f| f.written.uuid.clone()),
    );
    let mut gate_version: i32 = project.thresholds_version.unwrap_or(0);
    let mut state: Option<String> = None;
    let mut announced: Vec<(SeededFrame, String, i32)> = Vec::new();
    let mut hub_adopted: Vec<(SeededFrame, i32)> = Vec::new();
    let mut first_err: Option<ApiError> = (refused_new > 0).then(|| {
        ApiError::Conflict(crate::account::client::publishing_device_msg(
            bound_elsewhere.clone().flatten().as_deref(),
        ))
    });
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
                    let staged: Vec<&SeededFrame> = updates.iter().collect();
                    unstage_updates(ctx, &disk_lock, project_id, &staged).await;
                    break 'batches;
                }
                Err(E::PublishingDevice {
                    device_id,
                    device_name,
                }) => {
                    // A6: another in-service device of this account is the
                    // project's publishing device. Nothing of this batch was
                    // written; nothing more is announced this run. Recorded
                    // so the next run (auto-publish included) stops before
                    // any generation, until the binding moves.
                    tracing::info!(
                        project_id,
                        count = batch.len(),
                        bound_device_id = %device_id,
                        outcome = "publishing_device",
                        "publish refused: another device of the account publishes into this project"
                    );
                    {
                        let recorded = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                            crate::db::collab::set_publishing_device(
                                &db.conn(),
                                project_id,
                                Some(&crate::db::collab::PublishingDevice {
                                    device_id: device_id.clone(),
                                    name: device_name.clone(),
                                }),
                            )
                        });
                        if let Err(e) = recorded {
                            tracing::error!(project_id, error = %format!("{e:#}"), "publish: recording the publishing device failed");
                        }
                    }
                    let rest: Vec<SeededFrame> = batches.drain(..).flatten().collect();
                    for group in [&batch, &rest] {
                        let refs: Vec<&SeededFrame> = group.iter().collect();
                        unseed_all(&node, project_id, &refs).await;
                        held_back.extend(group.iter().map(|f| {
                            held_bound_elsewhere(
                                f.written.frame_id,
                                &f.written.filename,
                                device_name.as_deref(),
                            )
                        }));
                    }
                    first_err = Some(ApiError::Conflict(
                        crate::account::client::publishing_device_msg(device_name.as_deref()),
                    ));
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
                            publishing_device: None,
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
    // Hub rule: any pending outbox entry for a frame is flushed BEFORE its
    // version call. A failed flush is logged and the versions still go out:
    // the hub keeps the highest `reportSeq` per frame, so a late flush is
    // harmless.
    let mut versioned: Vec<SeededFrame> = Vec::new();
    // Frames whose bytes the hub turned out to hold already (C1c): recorded
    // at the hub's version, counted unchanged, no `…/version`.
    let mut already_versioned: Vec<SeededFrame> = Vec::new();
    // Real version conflicts `(uuid, the hub's content version)`: the hub
    // has a newer version than this run based its update on — held back;
    // another run is asked for once the local manifest has caught up.
    let mut conflicts: Vec<(String, i32)> = Vec::new();
    // `conflict` answers at exactly `expectedVersion + 1`: our own version
    // whose first reply was lost — or another device of this account's.
    // Decided after the batches by the hub's BLAKE3 for that version.
    let mut maybe_ours: Vec<(SeededFrame, i32, i32)> = Vec::new();
    if !outdated && !updates.is_empty() {
        if let Err(e) = crate::api::collab_live::holdings::flush_project_now(ctx, project_id).await
        {
            tracing::warn!(project_id, error = %e, "publish: flushing holdings before the versions failed; posting the versions anyway");
        }
    }
    if !outdated {
        // C1c: never post a version whose bytes are the row's CURRENT
        // content — re-read right before the call; the re-read row's
        // version is what the new one supersedes (CAS `expectedVersion`).
        let mut to_post: Vec<(SeededFrame, i32)> = Vec::new();
        for mut f in updates {
            let current = {
                let db = db(ctx)?;
                let conn = db.conn();
                frames_db::get(&conn, project_id, &f.written.uuid)
            };
            let expected = match current {
                Ok(Some(row)) if row.blake3 == f.blake3 => {
                    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = row.content_version, "publish: the hub already has these bytes; no new version");
                    if row.content_version != f.content_version {
                        retag_under_hub_version(&node, project_id, &mut f, row.content_version)
                            .await;
                    }
                    already_versioned.push(f);
                    continue;
                }
                Ok(Some(row)) => row.content_version,
                Ok(None) => {
                    tracing::warn!(project_id, frame_uuid = %f.written.uuid, "publish: the own frame row vanished before its version; posting against the seeded version");
                    f.content_version - 1
                }
                Err(e) => {
                    // The hub is the arbiter: post, and let it refuse.
                    tracing::warn!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: re-reading the own frame before its version failed");
                    f.content_version - 1
                }
            };
            to_post.push((f, expected));
        }
        let mut batches = announce_batches(to_post);
        while let Some(batch) = batches.pop_front() {
            let wire: Vec<crate::collab::live::wire::VersionInWire> = batch
                .iter()
                .map(|(f, expected)| crate::collab::live::wire::VersionInWire {
                    uuid: f.written.uuid.clone(),
                    expected_version: *expected,
                    blake3: f.blake3.clone(),
                    byte_size: f.written.byte_size as i64,
                    xxh3: f.written.xxh3.clone(),
                })
                .collect();
            let reply = crate::collab::hub_client::with_retry(
                "publish versions",
                crate::collab::hub_client::RetryPolicy::Interactive,
                || client.frame_versions(&token, project_id, &wire),
            )
            .await;
            let reply = match reply {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(project_id, count = batch.len(), error = %e, "publish: new frame versions failed");
                    let frames: Vec<SeededFrame> = batch.into_iter().map(|(f, _)| f).collect();
                    let refs: Vec<&SeededFrame> = frames.iter().collect();
                    unseed_all(&node, project_id, &refs).await;
                    unstage_updates(ctx, &disk_lock, project_id, &refs).await;
                    for f in &frames {
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            format!("new version failed: {e}"),
                        ));
                    }
                    if matches!(e, E::CollabApiOutdated) {
                        outdated = true;
                        let rest: Vec<SeededFrame> =
                            batches.drain(..).flatten().map(|(f, _)| f).collect();
                        let refs: Vec<&SeededFrame> = rest.iter().collect();
                        unseed_all(&node, project_id, &refs).await;
                        unstage_updates(ctx, &disk_lock, project_id, &refs).await;
                    }
                    first_err.get_or_insert(client_err(e));
                    if outdated {
                        break;
                    }
                    continue;
                }
            };
            let mut results = reply.results.into_iter();
            for (f, expected) in batch {
                use crate::collab::live::wire::VersionStatus;
                let result = results.next().filter(|r| r.uuid == f.written.uuid);
                match &result {
                    Some(r) if r.status == VersionStatus::Ok => {
                        let cv = r.content_version;
                        accept_version(&node, project_id, f, cv, &mut versioned).await;
                    }
                    Some(r)
                        if r.status == VersionStatus::Conflict
                            && r.content_version == expected + 1 =>
                    {
                        maybe_ours.push((f, expected, r.content_version));
                    }
                    Some(r) if r.status == VersionStatus::Conflict => {
                        let n = r.content_version;
                        refuse_version(
                            ctx,
                            &node,
                            &disk_lock,
                            project_id,
                            f,
                            n,
                            &mut conflicts,
                            &mut held_back,
                        )
                        .await;
                    }
                    Some(r) if r.status == VersionStatus::NotPublishingDevice => {
                        // A6: another device of this account versions this
                        // frame. Not retried until its manifest row changes.
                        tracing::info!(project_id, frame_uuid = %f.written.uuid, outcome = "not_publishing_device", "publish: new frame version refused: another device of the account versions this frame");
                        unseed_all(&node, project_id, &[&f]).await;
                        unstage_updates(ctx, &disk_lock, project_id, &[&f]).await;
                        let marked = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                            let conn = db.conn();
                            let mv = frames_db::get(&conn, project_id, &f.written.uuid)?
                                .map_or(0, |r| r.manifest_version);
                            frames_db::set_error(
                                &conn,
                                project_id,
                                &f.written.uuid,
                                Some(&not_publishing_mark(mv)),
                            )
                        });
                        if let Err(e) = marked {
                            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the version refusal failed");
                        }
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            "new version refused: another device of this account versions this frame now".into(),
                        ));
                    }
                    other => {
                        let reason = match other {
                            Some(r) if r.status == VersionStatus::Unknown => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused with a status this build does not know");
                                "new version failed: the hub answered with a status this version of Athenaeum does not know"
                            }
                            Some(r) if r.status == VersionStatus::NotFound => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused: the hub does not know the frame");
                                "new version failed: the hub does not know this frame"
                            }
                            Some(r) if r.status == VersionStatus::Forbidden => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused: not the frame's publisher");
                                "new version failed: the hub says this device's account did not publish the frame"
                            }
                            _ => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: the versions reply has no matching result for this frame");
                                "new version failed: the hub's reply did not answer for this frame"
                            }
                        };
                        unseed_all(&node, project_id, &[&f]).await;
                        unstage_updates(ctx, &disk_lock, project_id, &[&f]).await;
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            reason.to_string(),
                        ));
                    }
                }
            }
        }
        // A conflict at `expected + 1` is our own lost reply ONLY when the
        // hub's BLAKE3 for that version is ours (controller ruling): another
        // device of this account may have bumped it with other bytes. One
        // manifest delta read decides; a failed read decides "not ours" (the
        // safe side: the frame is held back and regenerated next run).
        if !maybe_ours.is_empty() {
            let uuids: HashSet<String> = maybe_ours
                .iter()
                .map(|(f, _, _)| f.written.uuid.clone())
                .collect();
            let since = {
                let db = db(ctx)?;
                let conn = db.conn();
                let mut since = i64::MAX;
                for u in &uuids {
                    let mv = frames_db::get(&conn, project_id, u)
                        .ok()
                        .flatten()
                        .map_or(0, |r| r.manifest_version);
                    since = since.min(mv);
                }
                since.clamp(0, i64::MAX)
            };
            let hub = match hub_frame_versions(&client, &token, project_id, since, &uuids).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(project_id, error = %e, "publish: reading the hub's versions after a conflict failed; treating the conflicts as real");
                    HashMap::new()
                }
            };
            for (f, expected, n) in std::mem::take(&mut maybe_ours) {
                if is_own_lost_reply(expected, &f.blake3, hub.get(&f.written.uuid)) {
                    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = n, "publish: the hub already took this version (a retried call)");
                    accept_version(&node, project_id, f, n, &mut versioned).await;
                } else {
                    refuse_version(
                        ctx,
                        &node,
                        &disk_lock,
                        project_id,
                        f,
                        n,
                        &mut conflicts,
                        &mut held_back,
                    )
                    .await;
                }
            }
        }
    }

    // ── 7. Own rows and their claims, per frame (never aborts the run) ───────
    // Claims (spec §6.2, hub § "Implicit claims"): an announce made the hub
    // claim `(uuid, 1)` for this device and a version `(uuid, new)` — both
    // enter the claim set WITHOUT an outbox row, in the same transaction as
    // the own row. An adoption (R8a/R8b) may have no hub claim of this
    // device behind it: `ensure_claimed` adds it through the outbox.
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
                manifest_version: 0,
                manifest_json: manifest.to_string(),
                landed_path: Some(w.target.to_string_lossy().to_string()),
                size_mtime_seen: size_mtime_seen(&w.target),
                on_disk: true,
                awaiting_gc: false,
                source_frame_id: Some(w.frame_id),
                recipe_hash: Some(w.recipe.clone()),
                last_error: None,
                updated_at: String::new(),
                local_state: frames_db::LocalState::OwnHeld,
                frame_seq: None,
            }
        };
        let adopt = |conn: &Connection, f: &SeededFrame| -> anyhow::Result<()> {
            let w = &f.written;
            let n = frames_db::adopt_own(
                conn,
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
        // One transaction per frame: the own-row write(s) and the claim.
        // IMMEDIATE: a body may read before it writes (the "already
        // announced" bind reads the row first); a deferred read-to-write
        // upgrade under another writer fails at once with SQLITE_BUSY, never
        // waiting the busy timeout.
        let in_tx = |body: &dyn Fn(&Connection) -> anyhow::Result<()>| -> anyhow::Result<()> {
            let tx = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            body(&tx)?;
            tx.commit()?;
            Ok(())
        };

        for (f, frame_state, gv) in &announced {
            let row = new_row(f, frame_state, *gv, true);
            match in_tx(&|c| {
                frames_db::record_own(c, &row)?;
                crate::db::collab_live::add_implicit_claim(
                    c,
                    project_id,
                    &f.written.uuid,
                    f.content_version,
                )
            }) {
                Ok(()) => announced_n += 1,
                Err(e) => record_failed(f, "announced", &e),
            }
        }
        // R8a: the hub already had these as mine. Bind an existing own row
        // (a manifest sync got there first), else record one from the local
        // plan with the state unknown — the manifest sync sets the hub truth.
        for (f, gv) in &hub_adopted {
            let result = in_tx(&|c| {
                match frames_db::get(c, project_id, &f.written.uuid)? {
                    Some(row) if row.origin == FrameOrigin::Own => adopt(c, f)?,
                    Some(_) => {
                        anyhow::bail!("frame {} is cached as another publisher's", f.written.uuid)
                    }
                    None => frames_db::record_own(c, &new_row(f, "unknown", *gv, false))?,
                }
                frames_db::ensure_claimed(c, project_id, &f.written.uuid)?;
                Ok(())
            });
            match result {
                Ok(()) => announced_n += 1,
                Err(e) => record_failed(f, "already announced", &e),
            }
        }
        // R8b, identical bytes: bound and seeded at the hub's version.
        for f in &bound {
            match in_tx(&|c| {
                adopt(c, f)?;
                frames_db::ensure_claimed(c, project_id, &f.written.uuid)?;
                Ok(())
            }) {
                Ok(()) => unchanged += 1,
                Err(e) => record_failed(f, "already published", &e),
            }
        }
        for (f, already) in versioned
            .iter()
            .map(|f| (f, false))
            .chain(already_versioned.iter().map(|f| (f, true)))
        {
            let w = &f.written;
            let result = in_tx(&|c| {
                if matches!(w.kind, PublishKind::Adopt(_)) {
                    adopt(c, f)?;
                }
                if !already {
                    // The hub's implicit claim of the new version.
                    crate::db::collab_live::add_implicit_claim(
                        c,
                        project_id,
                        &w.uuid,
                        f.content_version,
                    )?;
                }
                frames_db::set_own_version(
                    c,
                    project_id,
                    &w.uuid,
                    f.content_version,
                    &f.blake3,
                    &w.xxh3,
                    w.byte_size as i64,
                    &w.recipe,
                    size_mtime_seen(&w.target).as_deref(),
                )?;
                if already {
                    frames_db::ensure_claimed(c, project_id, &w.uuid)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    if already {
                        unchanged += 1;
                    } else {
                        updated_n += 1;
                    }
                }
                Err(e) => record_failed(f, "versioned", &e),
            }
        }
    }
    if !conflicts.is_empty() {
        // The hub moved on under this run (another device of this account
        // versioned the frame). Asking for another run right away would
        // regenerate and conflict again until the manifest catches up, so
        // pull the manifest delta first, and ask only once the local rows
        // actually carry the hub's newer versions.
        match crate::api::collab_exchange::sync_manifest(ctx, project_id, None, None).await {
            Ok(_) => {
                let caught_up = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    conflicts.iter().all(|(u, n)| {
                        frames_db::get(&conn, project_id, u)
                            .ok()
                            .flatten()
                            .is_some_and(|r| r.content_version >= *n)
                    })
                };
                if caught_up {
                    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
                } else {
                    tracing::info!(project_id, count = conflicts.len(), "publish: the manifest does not show the hub's newer versions yet; no new run requested");
                }
            }
            Err(e) => {
                tracing::warn!(project_id, error = %e, "publish: manifest sync after a version conflict failed; no new run requested");
            }
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

// ── Moderation (wave 2 Task 11): per-frame review queue + approve/reject ──────

/// One pending frame awaiting a coordinator decision, projected from the
/// cached manifest row (cache-only — the manifest sync / replication pass
/// keep pending rows current, same as every other frame).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModerationFrameView {
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    pub publisher_account_id: String,
    pub filter: String,
    pub exptime_sec: f64,
    /// Parsed from the manifest row's `meta.fwhmArcsec` (`build_frame_meta`).
    pub fwhm_arcsec: Option<f64>,
    pub created_at: String,
}

/// The coordinator's review queue: every PENDING frame of the project. Cache-
/// only (no hub I/O) — the manifest sync (Task 8) keeps the pending set
/// current, and the replication pass (Task 9) lands a moderator's copy of
/// each one so it can be inspected before a decision.
pub fn list_moderation_queue(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ModerationFrameView>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let mut out = Vec::new();
    for row in crate::db::collab_frames::list_for_project(&conn, project_id).map_err(internal)? {
        if row.state != "pending" {
            continue;
        }
        let wire = crate::api::collab_exchange::parse_manifest_wire(
            project_id,
            &row.frame_uuid,
            &row.manifest_json,
            "list_moderation_queue",
        );
        let (created_at, exptime_sec, fwhm_arcsec) = match &wire {
            Some(w) => (
                w.created_at.clone(),
                w.exptime_sec,
                w.meta.get("fwhmArcsec").and_then(serde_json::Value::as_f64),
            ),
            None => (row.updated_at.clone(), 0.0, None),
        };
        out.push(ModerationFrameView {
            frame_uuid: row.frame_uuid,
            file_name: row.file_name,
            publisher: row.publisher_display,
            publisher_account_id: row.publisher_account_id,
            filter: row.filter_canonical,
            exptime_sec,
            fwhm_arcsec,
            created_at,
        });
    }
    Ok(out)
}

/// Map a hub approve/reject error: a 409 (the frame is no longer pending —
/// already decided by another moderator, or superseded) surfaces as
/// [`ApiError::Conflict`] so the caller leaves the local row untouched (the next
/// sync re-syncs it). Everything else goes through [`client_err`]. This 409 is
/// not one of the collab client's typed refusals, so it decodes as
/// `Http("hub returned 409 Conflict…")`, and the status is detected there via
/// [`crate::account::AccountClientError::hub_text`].
fn decide_err(e: crate::account::AccountClientError) -> ApiError {
    // Match the client's stable prefix, not a bare "409" — the message tail
    // can echo hub-controlled text (e.g. the typed reject reason), and a
    // literal "409" inside it must not relabel a non-conflict error.
    if e.hub_text().is_some_and(|m| m.contains("hub returned 409")) {
        return ApiError::Conflict("This frame was already decided — refresh the queue.".into());
    }
    client_err(e)
}

/// Approve a pending frame (coordinator only — enforced by the hub).
///
/// `trust` also marks the publisher trusted for future first-publications and
/// can retroactively publish every other pending frame from the same
/// publisher in the same hub call (the hub's `{"published": N}` reply, logged
/// here — the local cache does not try to guess which other rows moved). A
/// manifest sync immediately follows so the cache picks up the frame's (and,
/// under `trust`, every sibling's) new `published` state — `vouched_version:
/// None`, since an approval's `published` count is not a
/// `/me/project-versions` value to vouch for (R12 is about that endpoint, not
/// this reply). That sync is best-effort: once the hub took the decision the
/// command succeeds, and a failed sync is logged at `warn!` for the next
/// project event to repair (M9).
///
/// A hub 409 (no longer pending) is [`ApiError::Conflict`] and leaves the
/// local row untouched (a sync will pick up the true state later).
pub async fn approve_collab_frame(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuid: &str,
    trust: bool,
) -> Result<(), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut("Sign in to moderate frames.".into()));
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::api::collab_exchange::live_project(&conn, project_id)?;
    }
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let published = client
        .approve_frame(&token, project_id, frame_uuid, trust)
        .await
        .map_err(decide_err)?;
    tracing::info!(project_id, frame_uuid, trust, published, "approved frame");
    // M9: the decision is made; a failed follow-up sync is not the
    // command's failure — the hub's project event syncs the manifest.
    if let Err(e) = crate::api::collab_exchange::sync_manifest(ctx, project_id, None, None).await {
        tracing::warn!(project_id, frame_uuid, error = %e, "manifest sync after approval failed; the next project event syncs it");
    }
    Ok(())
}

/// Reject a pending frame (coordinator only — enforced by the hub). `reason`
/// is required, trimmed, and must be 1..=500 BYTES — validated BEFORE any hub
/// call. A best-effort manifest sync follows a successful reject, same as
/// approve.
///
/// A hub 409 (no longer pending) is [`ApiError::Conflict`] and leaves the
/// local row untouched.
pub async fn reject_collab_frame(
    ctx: &ServiceContext,
    project_id: &str,
    frame_uuid: &str,
    reason: String,
) -> Result<(), ApiError> {
    let trimmed = reason.trim().to_string();
    if !(1..=500).contains(&trimmed.len()) {
        return Err(ApiError::Invalid(
            "a rejection reason of 1 to 500 bytes is required".into(),
        ));
    }
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut("Sign in to moderate frames.".into()));
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::api::collab_exchange::live_project(&conn, project_id)?;
    }
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    client
        .reject_frame(&token, project_id, frame_uuid, &trimmed)
        .await
        .map_err(decide_err)?;
    tracing::info!(project_id, frame_uuid, "rejected frame");
    // M9: as for approval — the hub's project event syncs the manifest.
    if let Err(e) = crate::api::collab_exchange::sync_manifest(ctx, project_id, None, None).await {
        tracing::warn!(project_id, frame_uuid, error = %e, "manifest sync after rejection failed; the next project event syncs it");
    }
    Ok(())
}

// The Collaboration-root guard (P25) lives beside the replication pass, which
// compiles headless; publish uses it through this re-export.
#[cfg(test)]
pub(crate) use crate::api::collab_exchange::COLLABORATION_ROOT_REQUIRED;
pub(crate) use crate::api::collab_exchange::{publisher_folder, require_collaboration_root};

#[cfg(test)]
pub(crate) mod tests {
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
    /// blocked by an unmapped filter. `dictionary_version`/`dictionary_json`
    /// on the literal below are for the reader only — `upsert_project`
    /// deliberately leaves both columns untouched (only [`set_dictionary`]
    /// writes them, so a wholesale poll refresh can never clobber the hub's
    /// dictionary cursor) — so the explicit call below is what actually seeds
    /// the dictionary this fixture's doc comment promises.
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
                feed_epoch: None,
                holder_seq: -1,
            },
        )
        .unwrap();
        crate::db::collab::set_dictionary(
            conn,
            "p-1",
            Some(1),
            Some(r#"[{"canonical":"L","aliases":[],"kind":"broadband"}]"#),
        )
        .unwrap();
    }

    #[test]
    fn page_extras_carry_goals_and_member_last_seen() {
        let page: crate::collab::hub_client::ProjectPageWire = serde_json::from_value(serde_json::json!({
            "project": {"id": "p1", "slug": "m31", "title": "M31", "status": "active", "requireApproval": false,
                        "target": {"name": "M31", "raDeg": 1.0, "decDeg": 2.0, "radiusDeg": 1.0}, "goals": {"Ha": 3600}},
            "members": [{"displayName": "Anna", "dataRole": "send", "coordinator": false, "lastSeenAt": "2026-09-27T08:30:00Z"}]
        })).unwrap();
        let (goals, seen) = page_extras_of(&page);
        assert_eq!(goals.as_deref(), Some(r#"{"Ha":3600}"#));
        let seen: Vec<MemberSeen> = serde_json::from_str(&seen).unwrap();
        assert_eq!(
            seen,
            vec![MemberSeen {
                display_name: "Anna".into(),
                last_seen_at: Some("2026-09-27T08:30:00Z".into())
            }]
        );
    }

    #[test]
    fn page_extras_round_trip_through_the_catalog() {
        let (_d, ctx) = test_ctx();
        let db = crate::api::db(&ctx).unwrap();
        let conn = db.conn();
        cached_project(&conn);
        assert_eq!(
            crate::db::collab::page_extras(&conn, "p-1").unwrap(),
            (None, "[]".to_string())
        );
        crate::db::collab::set_page_extras(
            &conn,
            "p-1",
            Some(r#"{"L":7200}"#),
            r#"[{"displayName":"A","lastSeenAt":null}]"#,
        )
        .unwrap();
        assert_eq!(
            crate::db::collab::page_extras(&conn, "p-1").unwrap(),
            (
                Some(r#"{"L":7200}"#.to_string()),
                r#"[{"displayName":"A","lastSeenAt":null}]"#.to_string()
            )
        );
        // A wholesale refresh of the row must not wipe them.
        let row = crate::db::collab::get_project(&conn, "p-1")
            .unwrap()
            .unwrap();
        crate::db::collab::upsert_project(&conn, &row).unwrap();
        assert_eq!(
            crate::db::collab::page_extras(&conn, "p-1")
                .unwrap()
                .0
                .as_deref(),
            Some(r#"{"L":7200}"#)
        );
    }

    #[test]
    fn goals_parse_strictly() {
        assert_eq!(parse_goals("p", None), None);
        assert_eq!(
            parse_goals("p", Some(r#"{"Ha":3600,"L":7200.5}"#)),
            Some(
                [("Ha".to_string(), 3600.0), ("L".to_string(), 7200.5)]
                    .into_iter()
                    .collect()
            )
        );
        // Anything else is logged and treated as no goals — never guessed at.
        for bad in [
            r#"[1]"#,
            r#"{"Ha":"3600"}"#,
            r#"{"Ha":0}"#,
            r#"{"Ha":-1}"#,
            "not json",
            r#"{}"#,
        ] {
            assert_eq!(parse_goals("p", Some(bad)), None, "{bad}");
        }
    }

    #[test]
    fn project_detail_carries_goals() {
        let (_d, ctx) = test_ctx();
        {
            let db = crate::api::db(&ctx).unwrap();
            let conn = db.conn();
            cached_project(&conn);
            crate::db::collab::set_page_extras(&conn, "p-1", Some(r#"{"L":7200}"#), "[]").unwrap();
        }
        let detail = get_project_detail(&ctx, "p-1").unwrap();
        assert_eq!(
            detail.goals,
            Some([("L".to_string(), 7200.0)].into_iter().collect())
        );
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

    fn sign_in_as(conn: &rusqlite::Connection, email: &str) {
        crate::db::set_setting(conn, crate::settings::keys::ACCOUNT_EMAIL, email).unwrap();
    }

    fn row_of(report: &GateReport, id: i64) -> FrameGateRow {
        report
            .rows
            .iter()
            .find(|r| r.frame_id == id)
            .unwrap()
            .clone()
    }

    #[test]
    fn gate_resolves_through_the_account_mapping_and_reports_blockers() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "A@x.io");
            let (set_id, frames) =
                seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 3);
            // Frame 0 has no FILTER, frame 1 a slot name, frame 2 stays "L".
            conn.execute("UPDATE frames SET filter = NULL WHERE id = ?1", [frames[0]])
                .unwrap();
            conn.execute(
                "UPDATE frames SET filter = 'Slot 0' WHERE id = ?1",
                [frames[1]],
            )
            .unwrap();
            (set_id, frames)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        let row = |id: i64| {
            report
                .rows
                .iter()
                .find(|r| r.frame_id == id)
                .unwrap()
                .clone()
        };
        assert!(row(frames[0])
            .failures
            .contains(&"no FILTER header — needs a filter mapping".to_string()));
        assert!(row(frames[1])
            .failures
            .contains(&"filter \"Slot 0\" needs a filter mapping".to_string()));
        assert!(!row(frames[2]).failures.iter().any(|f| f.contains("filter")));
        let map_filter = report
            .blockers
            .iter()
            .find(|b| b.kind == "mapFilter")
            .expect("mapFilter blocker");
        assert_eq!(map_filter.frames, 2);
        assert_eq!(
            map_filter
                .names
                .iter()
                .map(|n| n.filter_raw.as_str())
                .collect::<Vec<_>>(),
            ["", "Slot 0"]
        );
        assert_eq!(map_filter.names[0].instrume, "ASI2600MM");
        assert!(report
            .blockers
            .iter()
            .any(|b| b.kind == "linkCalibration" && b.sets == vec![set_id]));
        assert!(report
            .blockers
            .iter()
            .any(|b| b.kind == "attest" && b.frames == 3));

        // Mapping rows of the signed-in account resolve both; another account's do not.
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "", "L")
                .unwrap();
            crate::db::collab::upsert_filter_mapping(&conn, "b@x.io", "ASI2600MM", "Slot 0", "L")
                .unwrap();
        }
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(!row_of(&report, frames[0])
            .failures
            .iter()
            .any(|f| f.contains("filter")));
        assert!(row_of(&report, frames[1])
            .failures
            .contains(&"filter \"Slot 0\" needs a filter mapping".to_string()));
    }

    #[test]
    fn mapped_to_missing_reason_names_both() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            conn.execute("UPDATE frames SET filter = 'H' WHERE id = ?1", [r.1[0]])
                .unwrap();
            crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "H", "Hb")
                .unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(row_of(&report, frames[0]).failures.contains(
            &"filter \"H\" is mapped to \"Hb\", which is not in this project's dictionary"
                .to_string()
        ));
        assert_eq!(
            report
                .blockers
                .iter()
                .find(|b| b.kind == "mapFilter")
                .unwrap()
                .names[0]
                .filter_raw,
            "H"
        );
    }

    #[test]
    fn an_attested_set_passes_the_calibration_precondition() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            crate::db::collab::set_frames_set_attestation(&conn, r.0, true).unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        let row = row_of(&report, frames[0]);
        assert!(
            !row.failures.iter().any(|f| f.contains("calibration")),
            "{:?}",
            row.failures
        );
        assert!(!report
            .blockers
            .iter()
            .any(|b| b.kind == "attest" || b.kind == "linkCalibration"));
    }

    /// Spec §3.2: a mapping row scopes to the SIGNED-IN account only. Signed
    /// out entirely (no `ACCOUNT_EMAIL` setting), even mapping rows that
    /// exist for some account must never apply — the frame stays unmapped,
    /// exactly as if no row existed at all.
    #[test]
    fn signed_out_with_mapping_rows_leaves_the_frame_unmapped() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            // Deliberately no `sign_in_as` call.
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            conn.execute(
                "UPDATE frames SET filter = 'Slot 0' WHERE id = ?1",
                [r.1[0]],
            )
            .unwrap();
            // A mapping row exists (from a previous sign-in on this device, or
            // simply seeded directly here) but must not apply while signed out.
            crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "Slot 0", "L")
                .unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(row_of(&report, frames[0])
            .failures
            .contains(&"filter \"Slot 0\" needs a filter mapping".to_string()));
    }

    /// Spec §7.1 / F5: C-2's "missing master file" sentence is normally a
    /// `buildMasters` blocker — attesting the set must clear it from
    /// `buildMasters` too, not only from `attest`/`linkCalibration` (the two
    /// kinds [`an_attested_set_passes_the_calibration_precondition`] already
    /// covers, which only ever exercises the "no calibration links" sentence).
    #[test]
    fn attested_set_is_also_absent_from_build_masters() {
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
            let set_id =
                seed_publishable_set(&conn, &out_dir, "Missing Master Set", &["uuid-bm-1"]);
            let frame_ids = light_frame_ids_of(&conn, set_id);
            link_missing_master_file(&conn, set_id, frame_ids[0], &missing_master_path);
            set_id
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();

        // Before attestation: the missing master FILE blocks under buildMasters.
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(
            report.blockers.iter().any(|b| b.kind == "buildMasters"),
            "{:?}",
            report.blockers
        );

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_frames_set_attestation(&conn, set_id, true).unwrap();
        }
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(
            !report.blockers.iter().any(|b| b.kind == "buildMasters"
                || b.kind == "attest"
                || b.kind == "linkCalibration"),
            "{:?}",
            report.blockers
        );
    }

    /// Two `warn!`+fallback paths, both reachable from ONE broken read:
    /// dropping `frames_set` breaks `frames_set_attested`'s query (fallback:
    /// treated as not attested), which sends the frame down the normal
    /// calibration path, where `collect_export_data`'s own `frames_set` read
    /// (via `get_frame_set_info`) THEN also fails (fallback: the frame is
    /// treated as not calibrated) — one dropped table exercises both
    /// `project_gate`/`frame_gate_inputs` fallbacks in a single call, and
    /// neither one panics or bubbles an `Err` up to the caller.
    #[test]
    fn broken_attestation_and_export_data_reads_fall_back_without_erroring() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            // `foreign_keys=ON` (this codebase's connection default) makes
            // `DROP TABLE` perform an implicit `DELETE FROM` first, which
            // would CASCADE through `imaging_nights`/`sessions`/
            // `session_members` and wipe the very rows this test needs to
            // survive — turned off on this ONE connection, for this ONE
            // statement, purely to drop the schema without deleting any data.
            conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
            conn.execute("DROP TABLE frames_set", []).unwrap();
        }
        let report = evaluate_project_gate(&ctx, "p-1").expect(
            "a broken attestation/export-data read falls back — it must never error the whole gate",
        );
        let row = row_of(&report, frames[0]);
        assert!(!row.publishable);
        assert!(
            row.failures.iter().any(|f| f.starts_with("could not verify calibration")),
            "the attestation-read fallback (treated as not attested) must have sent the frame down the \
             calibration path, where collect_export_data's own broken read then fails it too: {:?}",
            row.failures
        );
    }

    /// Insert a fully-populated `plate_solves` row (every NOT NULL column) for
    /// one frame, with an explicit pixel scale and crval center so the gate's
    /// precedence branches are observable.
    pub(crate) fn seed_plate_solve(
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

    #[test]
    fn frame_set_project_status_counts_states_and_lists_candidates() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, _frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 2)
        };
        let st = get_frame_set_project_status(&ctx, set_id).unwrap();
        assert!(st.links.is_empty());
        assert_eq!(st.candidates.len(), 1, "within radius, not linked");
        assert_eq!(st.candidates[0].project_id, "p-1");
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let st = get_frame_set_project_status(&ctx, set_id).unwrap();
        assert_eq!(st.links.len(), 1);
        assert!(st.candidates.is_empty());
        assert_eq!(st.links[0].counts.fails_gate, 2, "no calibration links");
        assert_eq!(
            st.links[0]
                .frames
                .iter()
                .filter(|f| f.state == "failsGate")
                .count(),
            2
        );
        assert!(st.links[0].frames[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("calibration"));
    }

    /// An own row's manifest carries two distinct texts — `acceptedReason`
    /// (the exclude/restore reason) and `rejectReason` (a moderator's
    /// reject). Review fix round 1, finding 1: the rejected chip must show
    /// the LATTER, never the former.
    fn seed_own_rejected_row(
        conn: &rusqlite::Connection,
        project_id: &str,
        frame_uuid: &str,
        source_frame_id: i64,
        reject_reason: &str,
        accepted_reason: &str,
    ) {
        let wire = crate::collab::hub_client::FrameViewWire {
            frame_uuid: frame_uuid.into(),
            frame_seq: 1,
            publisher_account_id: "acc-me".into(),
            publisher_display_name: "Me".into(),
            own: true,
            publisher_device_id: None,
            file_name: "c_L_0000.fits".into(),
            content_version: 1,
            blake3: "b".repeat(64),
            byte_size: 4096,
            xxh3: "0123456789abcdef".into(),
            filter_raw: "L".into(),
            filter_canonical: "L".into(),
            channel: "mono".into(),
            exptime_sec: 300.0,
            date_obs: None,
            meta: serde_json::json!({}),
            gate_version: 0,
            accepted: false,
            accepted_reason: Some(accepted_reason.to_string()),
            state: "rejected".into(),
            reject_reason: Some(reject_reason.to_string()),
            manifest_version: 1,
            created_at: "2026-07-13T00:00:00Z".into(),
        };
        let row = crate::db::collab_frames::LocalFrameRow {
            project_id: project_id.into(),
            frame_uuid: frame_uuid.into(),
            content_version: 1,
            origin: crate::db::collab_frames::FrameOrigin::Own,
            publisher_account_id: "acc-me".into(),
            publisher_display: "Me".into(),
            file_name: "c_L_0000.fits".into(),
            filter_canonical: "L".into(),
            state: "rejected".into(),
            accepted: false,
            byte_size: 4096,
            xxh3: "0123456789abcdef".into(),
            blake3: "b".repeat(64),
            manifest_version: 1,
            manifest_json: serde_json::to_string(&wire).unwrap(),
            landed_path: None,
            size_mtime_seen: None,
            on_disk: true,
            awaiting_gc: false,
            source_frame_id: Some(source_frame_id),
            recipe_hash: None,
            last_error: None,
            updated_at: String::new(),
            local_state: crate::db::collab_frames::LocalState::OwnHeld,
            frame_seq: Some(1),
        };
        crate::db::collab_frames::record_own(conn, &row).unwrap();
    }

    #[test]
    fn frame_set_project_status_rejected_own_row_shows_the_reject_reason() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_own_rejected_row(
                &conn,
                "p-1",
                "u-rej",
                frames[0],
                "FWHM too high",
                "excluded: not the reason under test",
            );
        }

        let st = get_frame_set_project_status(&ctx, set_id).unwrap();
        assert_eq!(st.links.len(), 1);
        let f = st.links[0]
            .frames
            .iter()
            .find(|f| f.frame_id == frames[0])
            .unwrap();
        assert_eq!(f.state, "rejected");
        assert_eq!(f.reason.as_deref(), Some("FWHM too high"));

        // `own_contributor_states` (the project page's own-frames table) must
        // agree — same fix, same source field.
        let conn = crate::api::db(&ctx).unwrap().conn();
        let project = crate::db::collab::get_project(&conn, "p-1")
            .unwrap()
            .unwrap();
        let contrib = own_contributor_states(&conn, "p-1", &project);
        let (state, reason) = contrib.get("u-rej").unwrap();
        assert_eq!(state, "rejected");
        assert_eq!(reason.as_deref(), Some("FWHM too high"));
    }

    /// A minimal own `LocalFrameRow`, `record_own`'d directly (no live
    /// network, no publish run) — a [`seed_own_rejected_row`] sibling that
    /// lets a test pick `state`/`recipe_hash`/`on_disk` freely, to reach every
    /// `derive` branch, not just `rejected`.
    fn seed_own_row(
        conn: &rusqlite::Connection,
        project_id: &str,
        frame_uuid: &str,
        source_frame_id: i64,
        state: &str,
        recipe_hash: Option<&str>,
        on_disk: bool,
    ) {
        let wire = crate::collab::hub_client::FrameViewWire {
            frame_uuid: frame_uuid.into(),
            frame_seq: 1,
            publisher_account_id: "acc-me".into(),
            publisher_display_name: "Me".into(),
            own: true,
            publisher_device_id: None,
            file_name: "c_L_0000.fits".into(),
            content_version: 1,
            blake3: "b".repeat(64),
            byte_size: 4096,
            xxh3: "0123456789abcdef".into(),
            filter_raw: "L".into(),
            filter_canonical: "L".into(),
            channel: "mono".into(),
            exptime_sec: 300.0,
            date_obs: None,
            meta: serde_json::json!({}),
            gate_version: 0,
            accepted: state == "published",
            accepted_reason: None,
            state: state.into(),
            reject_reason: None,
            manifest_version: 1,
            created_at: "2026-07-13T00:00:00Z".into(),
        };
        let row = crate::db::collab_frames::LocalFrameRow {
            project_id: project_id.into(),
            frame_uuid: frame_uuid.into(),
            content_version: 1,
            origin: crate::db::collab_frames::FrameOrigin::Own,
            publisher_account_id: "acc-me".into(),
            publisher_display: "Me".into(),
            file_name: "c_L_0000.fits".into(),
            filter_canonical: "L".into(),
            state: state.into(),
            accepted: state == "published",
            byte_size: 4096,
            xxh3: "0123456789abcdef".into(),
            blake3: "b".repeat(64),
            manifest_version: 1,
            manifest_json: serde_json::to_string(&wire).unwrap(),
            landed_path: None,
            size_mtime_seen: None,
            on_disk,
            awaiting_gc: false,
            source_frame_id: Some(source_frame_id),
            recipe_hash: recipe_hash.map(str::to_string),
            last_error: None,
            updated_at: String::new(),
            local_state: crate::db::collab_frames::LocalState::OwnHeld,
            frame_seq: Some(1),
        };
        crate::db::collab_frames::record_own(conn, &row).unwrap();
    }

    /// First announce wins: a re-publish of the same own row (`record_own`
    /// again, `INSERT OR REPLACE`) must not push `announced_at` forward.
    #[test]
    fn record_own_stamps_announced_at_once() {
        let (_d, ctx) = test_ctx();
        let db = crate::api::db(&ctx).unwrap();
        let conn = db.conn();
        cached_project(&conn);
        let (_set, ids) = seed_set(&conn, "S", "00 42 44", "+41 16 09", 10.68, 41.27, 2);
        seed_own_row(&conn, "p-1", "u-1", ids[0], "published", None, true);
        let first = crate::db::collab_frames::announced_at(&conn, "p-1", "u-1").unwrap();
        assert!(first.is_some());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        seed_own_row(&conn, "p-1", "u-1", ids[0], "published", None, true); // a re-publish
        assert_eq!(
            crate::db::collab_frames::announced_at(&conn, "p-1", "u-1").unwrap(),
            first
        );
    }

    /// Spec §8.1: `list_collab_frames`'s own-row `contributorState` chip
    /// (`api::collab_live::surface::list_collab_frames` →
    /// `own_contributor_states` → `derive`) for every reachable state, not
    /// just `rejected` (already pinned above). All three frames share one
    /// ATTESTED set, so `current_recipe_for_frame` resolves deterministically
    /// to `external_recipe(1000, "2026-07-01T21:00:00Z")` (`seed_set`'s fixed
    /// file size/mtime) — matching the stored `recipe_hash` reads as
    /// published, a stale one reads as an update pending.
    #[test]
    fn list_collab_frames_own_row_contributor_state_for_published_update_pending_and_rejected() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 3)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let current_recipe = external_recipe(1000, "2026-07-01T21:00:00Z");
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_frames_set_attestation(&conn, set_id, true).unwrap();
            seed_own_row(
                &conn,
                "p-1",
                "u-pub",
                frames[0],
                "published",
                Some(&current_recipe),
                true,
            );
            seed_own_row(
                &conn,
                "p-1",
                "u-upd",
                frames[1],
                "published",
                Some("external:999:2000-01-01T00:00:00Z"),
                true,
            );
            seed_own_row(&conn, "p-1", "u-rej", frames[2], "rejected", None, true);
        }
        let views =
            crate::api::collab_live::surface::list_collab_frames(&ctx, "p-1", true).unwrap();
        let state_of = |uuid: &str| {
            views
                .iter()
                .find(|v| v.frame_uuid == uuid)
                .unwrap_or_else(|| panic!("no view for {uuid}"))
                .contributor_state
                .clone()
        };
        assert_eq!(state_of("u-pub").as_deref(), Some("published"));
        assert_eq!(state_of("u-upd").as_deref(), Some("updatePending"));
        assert_eq!(state_of("u-rej").as_deref(), Some("rejected"));

        // I1: the default (`false`, the reload `ReceiveTab` fires on every
        // `collab-frames-landed` event) skips the gate + per-row recipe read
        // entirely — every own row's chip comes back `None`, not stale data.
        let unfilled =
            crate::api::collab_live::surface::list_collab_frames(&ctx, "p-1", false).unwrap();
        for uuid in ["u-pub", "u-upd", "u-rej"] {
            assert_eq!(
                unfilled
                    .iter()
                    .find(|v| v.frame_uuid == uuid)
                    .unwrap_or_else(|| panic!("no view for {uuid}"))
                    .contributor_state,
                None,
                "{uuid}: withContributorState=false must leave the chip unfilled"
            );
        }
    }

    /// `FrameSetProjectLink.publishing_here` (spec §8.2, amendment A6) is
    /// otherwise untested: `false` unbound, `true` once THIS device is the
    /// account's publishing device, `false` again for a DIFFERENT device.
    #[test]
    fn frame_set_project_status_reports_publishing_here() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1).0
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        assert!(!get_frame_set_project_status(&ctx, set_id).unwrap().links[0].publishing_here);

        let me = crate::api::account::own_device_id(&ctx).unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_publishing_device(
                &conn,
                "p-1",
                Some(&crate::db::collab::PublishingDevice {
                    device_id: me,
                    name: None,
                }),
            )
            .unwrap();
        }
        assert!(get_frame_set_project_status(&ctx, set_id).unwrap().links[0].publishing_here);

        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_publishing_device(
                &conn,
                "p-1",
                Some(&crate::db::collab::PublishingDevice {
                    device_id: "someone-else".into(),
                    name: None,
                }),
            )
            .unwrap();
        }
        assert!(!get_frame_set_project_status(&ctx, set_id).unwrap().links[0].publishing_here);
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

    /// Fix round 1: the page extras flow end to end through a refresh, driven
    /// by the fake hub rather than a hand-mocked `/projects/{id}` body — this
    /// is the only test that exercises the fake hub's D8 caller gate
    /// (`lastSeenAt` filled only for a fetch that is both authenticated AND a
    /// current member) via the real `fetch_one_project` call site.
    #[tokio::test]
    async fn refresh_caches_goals_and_member_last_seen_through_the_fake_hub() {
        let hub = crate::collab::fake_hub::FakeHub::start().await;
        hub.add_account("tok", "acc-1", "Anna", "dev-1-pubkey", None);
        hub.add_project("p-1", "m101", &[("acc-1", "send_receive", true)], false);
        hub.set_goals("p-1", serde_json::json!({"Ha": 7200}));
        hub.set_last_seen("p-1", "Anna", "2026-09-27T08:30:00Z");

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &hub.uri());

        refresh_projects(&ctx).await.unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        let (goals, seen) = crate::db::collab::page_extras(&conn, "p-1").unwrap();
        assert_eq!(goals.as_deref(), Some(r#"{"Ha":7200}"#));
        let seen: Vec<MemberSeen> = serde_json::from_str(&seen).unwrap();
        assert_eq!(
            seen,
            vec![MemberSeen {
                display_name: "Anna".into(),
                last_seen_at: Some("2026-09-27T08:30:00Z".into())
            }]
        );
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
                feed_epoch: None,
                holder_seq: -1,
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

    // ── Unseeding at the project-data deletion site ──────────────────────────

    /// Pin a throwaway blob under `project/<project_id>/<frame>/1` in the
    /// COLLAB store of `ctx`'s node — where per-frame seeds live — binding the
    /// node and mounting `collab_root` if the test has not already. The seed's
    /// CONTENT is irrelevant to a deletion test — what is asserted is that the
    /// tag lives and dies with the project data. Returns the node.
    async fn seed_tag_on_node(
        ctx: &ServiceContext,
        collab_root: &std::path::Path,
        project_id: &str,
        frame: &str,
    ) -> std::sync::Arc<crate::sharing::iroh::node::SharedIrohNode> {
        let node = crate::api::sync::ensure_iroh_node(ctx).await.unwrap();
        std::fs::create_dir_all(collab_root).unwrap();
        node.set_collab_root(Some(collab_root)).await.unwrap();
        let store = node.collab_store().expect("collab store mounted");
        let tt = store
            .blobs()
            .add_bytes(format!("{project_id}/{frame}").into_bytes())
            .temp_tag()
            .await
            .unwrap();
        store
            .tags()
            .set(
                crate::sharing::iroh::node::project_frame_tag(project_id, frame, 1),
                tt.hash_and_format(),
            )
            .await
            .unwrap();
        node
    }

    async fn seed_tag_present(
        node: &crate::sharing::iroh::node::SharedIrohNode,
        project_id: &str,
        frame: &str,
    ) -> bool {
        node.collab_store()
            .expect("collab store mounted")
            .tags()
            .get(crate::sharing::iroh::node::project_frame_tag(project_id, frame, 1).as_bytes())
            .await
            .unwrap()
            .is_some()
    }

    /// A project the hub no longer lists (left, removed, archived) is marked
    /// lost (R14) — this device is not a member any more, so it must stop
    /// seeding EVERY frame of that project. The `p-stays` seed is the scope
    /// control: unseeding is per project id, never a `project/` prefix sweep,
    /// so a project this prune did not name keeps every seed.
    #[tokio::test]
    async fn pruning_a_lost_project_unseeds_all_its_frames() {
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
            // Task 15 R1: a claim and an unsent report of the project.
            crate::db::collab_live::add_implicit_claim(&conn, "p-gone", "f-a", 1).unwrap();
            crate::db::collab_live::record_claim_change(
                &conn,
                "p-gone",
                "f-b",
                crate::db::collab_live::ClaimOp::Add { content_version: 1 },
            )
            .unwrap();
        }
        let collab_root = _tmp.path().join("Collab");
        let node = seed_tag_on_node(&ctx, &collab_root, "p-gone", "f-a").await;
        seed_tag_on_node(&ctx, &collab_root, "p-gone", "f-b").await;
        seed_tag_on_node(&ctx, &collab_root, "p-stays", "f-c").await;

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
            assert!(
                crate::db::collab_live::my_claims(&conn, "p-gone")
                    .unwrap()
                    .is_empty()
                    && crate::db::collab_live::outbox_len(&conn, "p-gone").unwrap() == 0,
                "a lost project's claims and unsent reports are dropped, never reported"
            );
        }
        assert!(
            !seed_tag_present(&node, "p-gone", "f-a").await
                && !seed_tag_present(&node, "p-gone", "f-b").await,
            "every frame of a project I am no longer in stops seeding"
        );
        assert!(
            seed_tag_present(&node, "p-stays", "f-c").await,
            "a project I am still in keeps seeding"
        );
        node.shutdown().await;
    }

    // ── Moderation (wave 2 Task 11): approve/reject per frame ────────────────

    /// A pending manifest row from another member, cached the way a manifest
    /// sync would have left it (`origin='replica'`).
    fn seed_pending_frame(conn: &rusqlite::Connection, project_id: &str, frame_uuid: &str) {
        let view: crate::collab::hub_client::FrameViewWire = serde_json::from_value(serde_json::json!({
            "frameUuid": frame_uuid, "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice",
            "own": false, "fileName": format!("c_{frame_uuid}.fits"), "contentVersion": 1,
            "blake3": "b".repeat(64), "byteSize": 4096, "xxh3": "0123456789abcdef",
            "filterRaw": "Red", "filterCanonical": "R", "channel": "mono", "exptimeSec": 300.0,
            "meta": {}, "gateVersion": 0, "accepted": true, "state": "pending", "manifestVersion": 1,
            "createdAt": "2026-07-13T00:00:00Z", "holderCount": 1
        }))
        .unwrap();
        crate::db::collab_frames::upsert_from_manifest(conn, project_id, &view).unwrap();
    }

    /// Approve → hub approve (200, `{"published": 1}`) → the manifest sync that
    /// follows picks up the hub's `published` state for the local cache (the
    /// approve call itself never writes `project_frames_local` — only the
    /// sync does), fed by the manifest page the sync fetches right after.
    #[tokio::test]
    async fn approve_then_sync_marks_published() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/frames/u1/approve"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "published": 1
            })))
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/projects/p-1/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projectVersion": 2,
                "rows": [{
                    "frameUuid": "u1", "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice",
                    "own": false, "fileName": "c_u1.fits", "contentVersion": 1,
                    "blake3": "b".repeat(64), "byteSize": 4096, "xxh3": "0123456789abcdef",
                    "filterRaw": "Red", "filterCanonical": "R", "channel": "mono", "exptimeSec": 300.0,
                    "meta": {}, "gateVersion": 0, "accepted": true, "state": "published",
                    "manifestVersion": 2, "createdAt": "2026-07-13T00:00:00Z", "holderCount": 1
                }],
                "hasMore": false,
                "next": null
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            seed_pending_frame(&conn, "p-1", "u1");
        }

        approve_collab_frame(&ctx, "p-1", "u1", false)
            .await
            .unwrap();

        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, "p-1", "u1")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state, "published",
            "the sync after approve marks it published"
        );
    }

    /// Reject sends the trimmed reason in the hub body and, on a 200, runs
    /// the same follow-up manifest sync as approve.
    #[tokio::test]
    async fn reject_carries_the_reason() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/frames/u1/reject"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "reason": "FWHM too high"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "state": "rejected"
            })))
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/projects/p-1/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projectVersion": 2,
                "rows": [{
                    "frameUuid": "u1", "publisherAccountId": "acc-alice", "publisherDisplayName": "Alice",
                    "own": false, "fileName": "c_u1.fits", "contentVersion": 1,
                    "blake3": "b".repeat(64), "byteSize": 4096, "xxh3": "0123456789abcdef",
                    "filterRaw": "Red", "filterCanonical": "R", "channel": "mono", "exptimeSec": 300.0,
                    "meta": {}, "gateVersion": 0, "accepted": true, "state": "rejected",
                    "rejectReason": "FWHM too high", "manifestVersion": 2,
                    "createdAt": "2026-07-13T00:00:00Z", "holderCount": 1
                }],
                "hasMore": false,
                "next": null
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            seed_pending_frame(&conn, "p-1", "u1");
        }

        reject_collab_frame(&ctx, "p-1", "u1", "  FWHM too high  ".into())
            .await
            .unwrap();

        // The mock only responds to the exact trimmed-reason body above — a
        // failed `.unwrap()` already proves the reason was carried; the local
        // row's post-sync state is the second half of the same contract as
        // approve.
        let conn = crate::api::db(&ctx).unwrap().conn();
        let row = crate::db::collab_frames::get(&conn, "p-1", "u1")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "rejected");
    }

    /// A reason that is empty, whitespace-only or over 500 bytes is `Invalid`
    /// BEFORE any hub call (the mock server sees zero requests).
    #[tokio::test]
    async fn reject_bad_reason_is_invalid_before_any_hub_call() {
        let server = MockServer::start().await;
        // No reject mock mounted on purpose.
        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            seed_pending_frame(&conn, "p-1", "u1");
        }

        for reason in ["", "   ", &"x".repeat(501)] {
            assert!(
                matches!(
                    reject_collab_frame(&ctx, "p-1", "u1", reason.to_string()).await,
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

    /// A hub 409 (already decided) is a `Conflict`.
    #[tokio::test]
    async fn approve_hub_409_is_conflict() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/frames/u1/approve"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "frame already decided"
            })))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            seed_pending_frame(&conn, "p-1", "u1");
        }

        let err = approve_collab_frame(&ctx, "p-1", "u1", false)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Conflict(_)),
            "409 ⇒ Conflict, got {err:?}"
        );
    }

    /// Final review M9: once the hub took the decision, a failed follow-up
    /// manifest sync does not fail the command (the next poll syncs) — for
    /// approve AND reject; the local row simply waits for that poll.
    #[tokio::test]
    async fn a_decision_the_hub_took_succeeds_even_if_the_follow_up_sync_fails() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/frames/u1/approve"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "published": 1
            })))
            .mount(&server)
            .await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/api/v1/projects/p-1/frames/u2/reject"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "state": "rejected"
            })))
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/api/v1/projects/p-1/manifest"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            seed_publish_project(&conn, "p-1", "[]");
            seed_pending_frame(&conn, "p-1", "u1");
            seed_pending_frame(&conn, "p-1", "u2");
        }

        approve_collab_frame(&ctx, "p-1", "u1", true)
            .await
            .expect("the approval stands");
        reject_collab_frame(&ctx, "p-1", "u2", "trailed".into())
            .await
            .expect("the rejection stands");
        let manifest_calls = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/api/v1/projects/p-1/manifest")
            .count();
        assert!(
            manifest_calls >= 2,
            "each decision tried its follow-up sync"
        );
        let conn = crate::api::db(&ctx).unwrap().conn();
        for uuid in ["u1", "u2"] {
            let row = crate::db::collab_frames::get(&conn, "p-1", uuid)
                .unwrap()
                .unwrap();
            assert_eq!(
                row.state, "pending",
                "{uuid}: unchanged until the next poll"
            );
        }
    }

    /// Fix round 1, item 10, against the fake hub: the manifest read finds
    /// the hub's `(contentVersion, blake3)`, and a `conflict` at
    /// `expected + 1` is ours only when the bytes match.
    #[tokio::test]
    async fn a_lost_reply_is_told_apart_from_another_devices_version_by_the_hubs_bytes() {
        use crate::collab::fake_hub::FakeHub;
        let hub = FakeHub::start().await;
        hub.add_account("tok", "acc-me", "Me", "AAA=", None);
        hub.add_project("p1", "m31", &[("acc-me", "send_receive", false)], false);
        hub.seed_frames("p1", "acc-me", &["a", "b"], "published");
        let ours = "c".repeat(64);
        let theirs = "d".repeat(64);
        let (o, t) = (ours.clone(), theirs.clone());
        hub.update_frame("p1", "a", move |f| {
            f.content_version = 2;
            f.blake3 = o;
        });
        hub.update_frame("p1", "b", move |f| {
            f.content_version = 2;
            f.blake3 = t;
        });
        let client = CollabClient::new(hub.uri()).unwrap();
        let uuids: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        let seen = hub_frame_versions(&client, "tok", "p1", 0, &uuids)
            .await
            .unwrap();
        assert_eq!(seen["a"], (2, ours.clone()));
        assert!(
            is_own_lost_reply(1, &ours, seen.get("a")),
            "our bytes → ours"
        );
        assert!(
            !is_own_lost_reply(1, &ours, seen.get("b")),
            "other bytes → another device's version"
        );
        assert!(
            !is_own_lost_reply(2, &ours, seen.get("a")),
            "not expected + 1"
        );
        assert!(!is_own_lost_reply(1, &ours, None), "unknown → not ours");
    }

    // ── Filter mapping sheet commands (spec 2026-09-28 §5.2) ────────────────

    #[test]
    fn mapping_sheet_lists_every_raw_name_with_its_resolution_and_proposal() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, _frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn); // dictionary [L]
            crate::db::collab::set_dictionary(&conn, "p-1", Some(2), Some(
                r#"[{"canonical":"L","aliases":["lum"],"kind":"luminance"},{"canonical":"Ha","aliases":[],"kind":"narrowband"},{"canonical":"None","aliases":["none"],"kind":"unfiltered"}]"#)).unwrap();
            sign_in_as(&conn, "a@x.io");
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 4);
            conn.execute(
                "UPDATE frames SET filter = NULL WHERE id IN (?1, ?2)",
                [r.1[0], r.1[1]],
            )
            .unwrap();
            conn.execute("UPDATE frames SET filter = 'H' WHERE id = ?1", [r.1[2]])
                .unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let sheet = get_filter_mapping_sheet(&ctx, "p-1").unwrap();
        assert_eq!(sheet.dictionary.len(), 3);
        let raws: Vec<(&str, &str)> = sheet
            .rows
            .iter()
            .map(|r| (r.filter_raw.as_str(), r.resolution.as_str()))
            .collect();
        assert_eq!(
            raws,
            [("", "unmapped"), ("H", "unmapped"), ("L", "matched")],
            "unresolved first, then by frames desc"
        );
        assert_eq!(sheet.rows[0].frames, 2);
        assert_eq!(sheet.rows[0].proposal.as_deref(), Some("None"));
        assert_eq!(sheet.rows[1].proposal.as_deref(), Some("Ha"));
        assert_eq!(sheet.rows[2].canonical.as_deref(), Some("L"));
        assert_eq!(
            sheet.rows[2].proposal, None,
            "a resolved row proposes nothing"
        );

        // Three separate calls: a lone invalid edit refuses outright and
        // writes nothing; a batch of two valid edits sets both; a later
        // null-canonical edit deletes its row (back to automatic).
        let bad = set_filter_mappings(
            &ctx,
            "p-1",
            vec![FilterMappingEdit {
                instrume: "ASI2600MM".into(),
                filter_raw: "H".into(),
                canonical: Some("Hb".into()),
            }],
        );
        assert!(
            matches!(bad, Err(crate::api::ApiError::Invalid(m)) if m.contains("\"Hb\" is not in the project dictionary"))
        );
        let report = set_filter_mappings(
            &ctx,
            "p-1",
            vec![
                FilterMappingEdit {
                    instrume: "ASI2600MM".into(),
                    filter_raw: "".into(),
                    canonical: Some("None".into()),
                },
                FilterMappingEdit {
                    instrume: "ASI2600MM".into(),
                    filter_raw: "H".into(),
                    canonical: Some("Ha".into()),
                },
            ],
        )
        .unwrap();
        assert!(!report.blockers.iter().any(|b| b.kind == "mapFilter"));
        let sheet = get_filter_mapping_sheet(&ctx, "p-1").unwrap();
        assert_eq!(
            sheet
                .rows
                .iter()
                .find(|r| r.filter_raw == "H")
                .unwrap()
                .resolution,
            "mapped"
        );
        // The auto-publish dirty mark was requested.
        assert!(crate::api::collab_autopublish::is_dirty_for_test("p-1"));
        // Back to automatic.
        set_filter_mappings(
            &ctx,
            "p-1",
            vec![FilterMappingEdit {
                instrume: "ASI2600MM".into(),
                filter_raw: "H".into(),
                canonical: None,
            }],
        )
        .unwrap();
        assert_eq!(
            get_filter_mapping_sheet(&ctx, "p-1")
                .unwrap()
                .rows
                .iter()
                .find(|r| r.filter_raw == "H")
                .unwrap()
                .resolution,
            "unmapped"
        );
    }

    /// A batch mixing a valid and an invalid canonical for the SAME
    /// (instrume, raw) key must refuse the WHOLE call and write nothing —
    /// validation (`set_filter_mappings`'s first loop) runs entirely ahead of
    /// the write transaction, so an earlier entry for the same key in the
    /// same refused batch must not be left half-applied.
    #[test]
    fn mixed_batch_with_one_invalid_canonical_writes_nothing() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(2),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"luminance"},{"canonical":"Ha","aliases":[],"kind":"narrowband"}]"#),
            )
            .unwrap();
            sign_in_as(&conn, "a@x.io");
        }
        let res = set_filter_mappings(
            &ctx,
            "p-1",
            vec![
                FilterMappingEdit {
                    instrume: "ASI2600MM".into(),
                    filter_raw: "H".into(),
                    canonical: Some("Ha".into()),
                },
                FilterMappingEdit {
                    instrume: "ASI2600MM".into(),
                    filter_raw: "H".into(),
                    canonical: Some("Hb".into()),
                },
            ],
        );
        assert!(
            matches!(&res, Err(crate::api::ApiError::Invalid(m)) if m.contains("\"Hb\" is not in the project dictionary")),
            "{res:?}"
        );
        let conn = crate::api::db(&ctx).unwrap().conn();
        let mappings = crate::db::collab::filter_mappings_for_account(&conn, "a@x.io").unwrap();
        assert!(
            mappings.is_empty(),
            "the earlier valid edit in the same refused batch must not have been written either: {mappings:?}"
        );
    }

    #[test]
    fn mapping_sheet_refuses_without_a_cached_dictionary_or_a_sign_in() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            crate::db::collab::set_dictionary(&conn, "p-1", None, None).unwrap();
        }
        assert!(
            matches!(get_filter_mapping_sheet(&ctx, "p-1"), Err(crate::api::ApiError::Invalid(m)) if m.contains("has not been fetched"))
        );
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_dictionary(
                &conn,
                "p-1",
                Some(1),
                Some(r#"[{"canonical":"L","aliases":[],"kind":"luminance"}]"#),
            )
            .unwrap();
        }
        assert!(matches!(
            set_filter_mappings(
                &ctx,
                "p-1",
                vec![FilterMappingEdit {
                    instrume: "X".into(),
                    filter_raw: "L".into(),
                    canonical: Some("L".into())
                }]
            ),
            Err(crate::api::ApiError::SignedOut(_))
        ));
    }

    // ── Publish per frame (wave 2 Task 7) ───────────────────────────────────

    pub(crate) mod publish {
        use super::*;
        use std::path::PathBuf;

        const PID: &str = "p1";
        /// Big enough that every calibrated frame is an EXTERNAL reference in
        /// the collab store, never data inlined into its database (the store
        /// inlines blobs up to 16 KiB) — the only shape in which "the store
        /// grew by less than 1 %" proves anything.
        const W: usize = 512;
        const H: usize = 512;

        pub(super) struct PubFx {
            pub tmp: tempfile::TempDir,
            pub ctx: Arc<ServiceContext>,
            pub server: MockServer,
            pub node: Arc<crate::sharing::iroh::node::SharedIrohNode>,
            pub collab: PathBuf,
            pub set_id: i64,
            pub frame_ids: Vec<i64>,
            pub lights: Vec<PathBuf>,
            pub master: PathBuf,
            pub uuids: Vec<String>,
        }

        impl PubFx {
            pub(super) fn conn(
                &self,
            ) -> r2d2::PooledConnection<crate::db::SqliteConnectionManager> {
                crate::api::db(&self.ctx).unwrap().conn()
            }
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
        pub(crate) fn write_dark(path: &Path, level: f32) {
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

        /// `POST …/frames/versions` answered per entry, in request order:
        /// `Ok` → `ok` at `expectedVersion + 1`; `Conflict(n)` → `conflict`
        /// at `n`; `ConflictNext` → `conflict` at `expectedVersion + 1` (a
        /// retried call whose first reply was lost). Optionally delayed.
        #[derive(Clone, Copy)]
        pub(super) enum VersionsMode {
            Ok,
            Conflict(i64),
            ConflictNext,
        }

        /// uuid → `(expectedVersion, blake3)` of every entry a
        /// [`VersionsReply`] answered — what [`ManifestEcho`] plays back.
        pub(super) type SeenVersions = Arc<std::sync::Mutex<HashMap<String, (i64, String)>>>;

        pub(super) struct VersionsReply {
            pub mode: VersionsMode,
            pub delay: Option<std::time::Duration>,
            pub seen: Option<SeenVersions>,
        }

        /// `GET …/manifest` listing every frame a [`VersionsReply`] saw, at
        /// `cv` (`None` → `expectedVersion + 1`) and with the posted BLAKE3
        /// (`same_bytes`) or other bytes — the hub after our own lost reply,
        /// or after another device's version.
        pub(super) struct ManifestEcho {
            pub seen: SeenVersions,
            pub cv: Option<i64>,
            pub same_bytes: bool,
        }

        impl wiremock::Respond for ManifestEcho {
            fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
                let seen = self.seen.lock().unwrap();
                let rows: Vec<serde_json::Value> = seen
                    .iter()
                    .map(|(uuid, (expected, blake3))| {
                        serde_json::json!({
                            "frameUuid": uuid, "frameSeq": 1, "publisherAccountId": "acc-me",
                            "publisherDisplayName": "Me Myself", "own": true,
                            "fileName": "c_L_0000.fits",
                            "contentVersion": self.cv.unwrap_or(expected + 1),
                            "blake3": if self.same_bytes { blake3.clone() } else { "f".repeat(64) },
                            "byteSize": 1000, "xxh3": "0123456789abcdef", "filterRaw": "L",
                            "filterCanonical": "L", "channel": "mono", "exptimeSec": 300.0,
                            "meta": {}, "gateVersion": 0, "accepted": true, "state": "published",
                            "manifestVersion": 100, "createdAt": "2026-09-26T00:00:00Z"
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "projectVersion": 100, "rows": rows, "hasMore": false, "next": null
                }))
            }
        }

        impl wiremock::Respond for VersionsReply {
            fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
                let results: Vec<serde_json::Value> = body["versions"]
                    .as_array()
                    .map(|vs| {
                        vs.iter()
                            .map(|v| {
                                let expected = v["expectedVersion"].as_i64().unwrap_or(0);
                                if let Some(seen) = &self.seen {
                                    seen.lock().unwrap().insert(
                                        v["uuid"].as_str().unwrap_or_default().to_string(),
                                        (
                                            expected,
                                            v["blake3"].as_str().unwrap_or_default().to_string(),
                                        ),
                                    );
                                }
                                let (status, cv) = match self.mode {
                                    VersionsMode::Ok => ("ok", expected + 1),
                                    VersionsMode::Conflict(n) => ("conflict", n),
                                    VersionsMode::ConflictNext => ("conflict", expected + 1),
                                };
                                serde_json::json!({
                                    "uuid": v["uuid"], "status": status, "contentVersion": cv
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let t = ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "projectVersion": 6, "results": results }));
                match self.delay {
                    Some(d) => t.set_delay(d),
                    None => t,
                }
            }
        }

        fn versions_path() -> String {
            format!("/api/v1/projects/{PID}/frames/versions")
        }

        /// Hub routes every run touches: announce, holder report, versions.
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
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "holderSeq": 1, "digestMatch": true, "nextFlushMs": 1000, "refused": []
                })))
                .mount(server)
                .await;
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::Ok,
                    delay: None,
                    seen: None,
                })
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

        /// One `…/frames/{uuid}/version` path per versioned frame, across
        /// every `POST …/frames/versions` batch the hub received.
        async fn version_calls(server: &MockServer) -> Vec<String> {
            requests(server)
                .await
                .into_iter()
                .filter(|(m, p, _)| m == "POST" && p == &versions_path())
                .flat_map(|(_, _, b)| {
                    b["versions"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|v| {
                            format!(
                                "/api/v1/projects/{PID}/frames/{}/version",
                                v["uuid"].as_str().unwrap_or_default()
                            )
                        })
                })
                .collect()
        }

        fn claims(fx: &PubFx) -> Vec<(String, i32)> {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            crate::db::collab_live::my_claims(&conn, PID).unwrap()
        }

        fn outbox_len(fx: &PubFx) -> usize {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            crate::db::collab_live::outbox_len(&conn, PID).unwrap()
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

        struct NoHolders;
        impl crate::api::collab_live::storage_task::HolderView for NoHolders {
            fn other_holders(
                &self,
                _project_id: &str,
                _frame_uuid: &str,
            ) -> crate::collab::live::holders::Redundancy {
                Default::default()
            }
        }

        /// One stat sweep of the storage engine over the fixture's root —
        /// the wave-3 check that replaced disk truth (Task 15).
        async fn storage_sweep(
            fx: &PubFx,
        ) -> Vec<crate::api::collab_live::storage_task::StorageEvent> {
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            let recorded = crate::db::collab_live::recorded_store_marker(
                &crate::api::db(&fx.ctx).unwrap().conn(),
            )
            .unwrap();
            let guard = Arc::new(crate::collab::storage::marker::StoreGuard::new(
                fx.collab.clone(),
                me,
                recorded,
            ));
            let mut eng = crate::api::collab_live::storage_task::StorageEngine::start(
                Arc::clone(&fx.ctx),
                Arc::clone(&fx.node),
                guard,
            );
            eng.sweep(&NoHolders).await
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

        pub(crate) fn set_mtime(path: &Path, ahead_secs: u64) {
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

        /// What [`seed_real_light_set`] wrote.
        pub(crate) struct RealLightSet {
            pub set_id: i64,
            pub frame_ids: Vec<i64>,
            pub lights: Vec<PathBuf>,
            pub master: PathBuf,
            pub uuids: Vec<String>,
        }

        /// One frame set of `n` gate-passing LIGHT frames (FILTER `filter`,
        /// on the M31 target, analyzed, uuid `uuid-pub-<i>`), each a real
        /// `W`×`H` FITS under `<root>/src/`, all linked to ONE real master
        /// dark at `<root>/masters/master_dark.fits` (calibration set 700).
        /// The caller links the set to its project.
        pub(crate) fn seed_real_light_set(
            conn: &rusqlite::Connection,
            root: &Path,
            n: usize,
            filter: &str,
        ) -> RealLightSet {
            let master = root.join("masters").join("master_dark.fits");
            write_dark(&master, 300.0);
            let mut lights = Vec::new();
            let mut uuids = Vec::new();
            let mut frame_ids = Vec::new();
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
                let light = root.join("src").join(&name);
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
                     VALUES (?1, 'Light', 'M31', 'ASI2600MM', 10.68, 41.27, 3.76, 1000.0, 300.0, ?3, ?2, \
                             '2026-07-01T21:00:00Z')",
                    rusqlite::params![file_id, uuid, filter],
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
            RealLightSet {
                set_id,
                frame_ids,
                lights,
                master,
                uuids,
            }
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

            {
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
                        feed_epoch: None,
                        holder_seq: -1,
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
            }
            let set = {
                let conn = crate::api::db(&ctx).unwrap().conn();
                seed_real_light_set(&conn, tmp.path(), n, "L")
            };
            link_frame_set(&ctx, PID, set.set_id).unwrap();
            PubFx {
                tmp,
                ctx: Arc::new(ctx),
                server,
                node,
                collab,
                set_id: set.set_id,
                frame_ids: set.frame_ids,
                lights: set.lights,
                master: set.master,
                uuids: set.uuids,
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

            // The announce claimed both frames at version 1 for this device
            // (hub § "Implicit claims"): in the claim set, never reported.
            let mut want: Vec<(String, i32)> = fx.uuids.iter().map(|u| (u.clone(), 1)).collect();
            want.sort();
            assert_eq!(claims(&fx), want);
            assert_eq!(outbox_len(&fx), 0);
            assert!(
                requests(&fx.server)
                    .await
                    .iter()
                    .all(|(m, _, _)| m != "PUT"),
                "no holder report for implicit claims"
            );
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

        /// Final fix B-M2: a publish's regeneration temp never shares the
        /// landing temp's suffix — the live session's orphaned-temp sweep at
        /// its start leaves an in-flight publish temp alone.
        #[test]
        fn a_publish_temp_survives_the_landing_temp_sweep() {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("m31").join("Me").join("c_L_0001.fits");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            let temp = update_temp_path(&target);
            std::fs::write(&temp, b"regenerating").unwrap();
            let landing = crate::sharing::iroh::blobs::athtmp_path(&target);
            std::fs::write(&landing, b"a crashed landing").unwrap();
            crate::api::collab_live::landing::sweep_orphaned_athtmp(root.path());
            assert!(temp.exists(), "the publish temp stays");
            assert!(!landing.exists(), "a landing's orphan goes");
            assert!(crate::collab::storage::watch::is_ignored(
                root.path(),
                &temp
            ));
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

        /// Task 10 fix round 2 (C11): an own row still marked staged whose
        /// regeneration matches the hub's version byte for byte (the P19
        /// identical-pixels shortcut — no `set_own_version`) is confirmed:
        /// the mark is cleared and the collab serve check serves it again.
        #[tokio::test]
        async fn identical_output_confirms_a_staged_own_row() {
            use crate::collab::serve::ServeOracle as _;
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let staged = |fx: &PubFx| -> bool {
                crate::api::db(&fx.ctx)
                    .unwrap()
                    .conn()
                    .query_row(
                        "SELECT own_staged FROM project_frames_local WHERE frame_uuid = ?1",
                        [&fx.uuids[0]],
                        |r| r.get(0),
                    )
                    .unwrap()
            };
            crate::api::db(&fx.ctx)
                .unwrap()
                .conn()
                .execute(
                    "UPDATE project_frames_local SET own_staged = 1 WHERE frame_uuid = ?1",
                    [&fx.uuids[0]],
                )
                .unwrap();
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            let oracle = crate::api::collab_live::serve_oracle::DbServeOracle::catalog_only(
                crate::api::db(&fx.ctx).unwrap().clone(),
            );
            assert!(oracle.lookup(&row.blake3).is_none(), "staged: refused");

            set_mtime(&fx.lights[0], 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.unchanged, 1, "{res:?}");
            assert!(!staged(&fx), "the identical regeneration confirmed it");
            assert!(oracle.lookup(&row.blake3).is_some(), "served again");
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
            // The version claimed v2 for this device implicitly: no report.
            assert_eq!(claims(&fx), vec![(fx.uuids[0].clone(), 2)]);
            assert_eq!(outbox_len(&fx), 0);
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
            // The failed frame's row and claim roll back together.
            assert_eq!(claims(&fx), vec![(fx.uuids[0].clone(), 1)]);
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

        // ── Final-review fix wave ────────────────────────────────────────────

        /// C1(a), owner decision 2026-09-24: two overlapping runs of one
        /// project never both run — the second is refused at once with the
        /// busy Conflict, so a changed frame gets exactly ONE `…/version`.
        #[tokio::test]
        async fn overlapping_publish_runs_post_one_version_per_changed_frame() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let (a, b) = tokio::join!(
                republish_collab_frames(&fx.ctx, PID, None),
                publish_collab_frames(&fx.ctx, PID, None)
            );
            let (ran, refused) = match (a, b) {
                (Ok(r), Err(e)) | (Err(e), Ok(r)) => (r, e),
                other => panic!("exactly one run must go through: {other:?}"),
            };
            match refused {
                ApiError::Conflict(m) => assert_eq!(m, PUBLISH_BUSY_MSG),
                other => panic!("expected the busy Conflict, got {other:?}"),
            }
            assert_eq!(ran.updated, 2, "{ran:?}");
            let mut calls = version_calls(&fx.server).await;
            calls.sort();
            let mut expected: Vec<String> = fx
                .uuids
                .iter()
                .map(|u| format!("/api/v1/projects/{PID}/frames/{u}/version"))
                .collect();
            expected.sort();
            assert_eq!(calls, expected, "one …/version per changed frame");

            // The next run finds nothing to do: no second version, ever.
            let again = republish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!((again.updated, again.unchanged), (0, 2), "{again:?}");
            assert_eq!(version_calls(&fx.server).await.len(), 2);
        }

        /// C1(a): while a publish run holds the project's lock, a manual
        /// publish, a republish and an auto-publish are each refused at once
        /// with `publication of this project is already running` — no
        /// calibration, no hub call, no file.
        #[tokio::test]
        async fn a_publish_while_a_run_holds_the_lock_is_refused() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            let lock = publish_lock(&fx.ctx, PID).unwrap();
            let held = lock.lock().await;
            for result in [
                publish_collab_frames(&fx.ctx, PID, None).await,
                republish_collab_frames(&fx.ctx, PID, None).await,
                auto_publish_collab_frames(&fx.ctx, PID, None).await,
            ] {
                match result {
                    Err(ApiError::Conflict(m)) => {
                        assert_eq!(m, "publication of this project is already running")
                    }
                    other => panic!("expected the busy Conflict, got {other:?}"),
                }
            }
            assert!(requests(&fx.server).await.is_empty(), "no hub call");
            assert!(!own_dir(&fx).exists(), "nothing generated");
            drop(held);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "released: the next run goes through");
        }

        /// C1(c): the split read the row before another run posted the same
        /// bytes as a version; right before posting, the row is read again
        /// and its CURRENT BLAKE3 equals the regenerated one — no
        /// `…/version`, the frame stays at the hub's version, its tag and
        /// file intact.
        #[tokio::test]
        async fn a_version_the_hub_already_has_is_not_posted_again() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let v1 = own_row(&fx, &fx.uuids[0]).unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let v2 = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(v2.content_version, 2);
            assert_eq!(version_calls(&fx.server).await.len(), 1);

            // Rewind the row to v1 so the split plans an update and the
            // generation sees new bytes; the hook then puts the hub's current
            // v2 back — what a concurrent run's write-back would have left.
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE project_frames_local SET content_version = 1, blake3 = ?1, \
                     recipe_hash = ?2",
                    rusqlite::params![v1.blake3, v1.recipe_hash],
                )
                .unwrap();
            }
            let (b2, cv2) = (v2.blake3.clone(), v2.content_version);
            let hook = move |conn: &Connection| {
                conn.execute(
                    "UPDATE project_frames_local SET content_version = ?1, blake3 = ?2",
                    rusqlite::params![cv2, b2],
                )
                .unwrap();
            };
            let res = run_publish(&fx.ctx, PID, None, false, Some(&hook))
                .await
                .unwrap();
            assert_eq!((res.updated, res.unchanged), (0, 1), "{res:?}");
            assert_eq!(
                version_calls(&fx.server).await.len(),
                1,
                "no second version"
            );
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert_eq!(row.blake3, v2.blake3);
            assert!(row.on_disk);
            assert!(tag_present(&fx, &fx.uuids[0], 2).await);
            assert!(!tag_present(&fx, &fx.uuids[0], 3).await);
            let landed = PathBuf::from(row.landed_path.unwrap());
            assert_eq!(crate::package::xxh3_full_file(&landed).unwrap(), v2.xxh3);
        }

        /// I1: an adoption whose recorded path is unknown lands at
        /// `<own>/<fileName>`; that target is reserved before any new frame
        /// picks a name, so a new frame whose natural name is the same file
        /// gets `_2` instead of writing over it.
        #[tokio::test]
        async fn an_adoption_target_is_reserved_before_new_frames_pick_names() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            // Frame 1 is never published at first (held back by a hub rule).
            let set_exptime = |v: Option<f64>| {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE frames SET exptime = ?1 WHERE id = ?2",
                    rusqlite::params![v, fx.frame_ids[1]],
                )
                .unwrap();
            };
            set_exptime(None);
            let first = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (first.announced, first.held_back.len()),
                (1, 1),
                "{first:?}"
            );
            set_exptime(Some(300.0));
            {
                // Frame 0 becomes a manifest-delivered own row the hub knows
                // as `c_L_0001.fits` — frame 1's natural name — not bound
                // locally.
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE project_frames_local SET source_frame_id = NULL, recipe_hash = NULL, \
                     landed_path = NULL, on_disk = 0, file_name = 'c_L_0001.fits' \
                     WHERE frame_uuid = ?1",
                    [&fx.uuids[0]],
                )
                .unwrap();
            }

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert!(res.held_back.is_empty(), "{:?}", res.held_back);
            assert_eq!((res.announced, res.unchanged), (1, 1), "{res:?}");
            let adopted = own_row(&fx, &fx.uuids[0]).unwrap();
            let new = own_row(&fx, &fx.uuids[1]).unwrap();
            assert_eq!(
                adopted.landed_path.as_deref(),
                Some(&*own_dir(&fx).join("c_L_0001.fits").to_string_lossy())
            );
            assert_eq!(
                new.landed_path.as_deref(),
                Some(&*own_dir(&fx).join("c_L_0001_2.fits").to_string_lossy()),
                "the new frame steps aside"
            );
            for row in [&adopted, &new] {
                let path = PathBuf::from(row.landed_path.as_deref().unwrap());
                assert_eq!(
                    crate::package::xxh3_full_file(&path).unwrap(),
                    row.xxh3,
                    "{} holds its own frame",
                    path.display()
                );
            }
        }

        /// M1: a frame without EXPTIME would make the hub refuse the WHOLE
        /// atomic batch; it is held back with its reason before announce,
        /// and the other frame goes out.
        #[tokio::test]
        async fn a_frame_breaking_a_hub_rule_is_held_back_before_announce() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE frames SET exptime = NULL WHERE id = ?1",
                    [fx.frame_ids[1]],
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            assert_eq!(res.held_back.len(), 1, "{:?}", res.held_back);
            assert_eq!(res.held_back[0].frame_id, fx.frame_ids[1]);
            assert!(
                res.held_back[0].reasons[0].contains("EXPTIME"),
                "{:?}",
                res.held_back[0].reasons
            );
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 1);
            assert_eq!(bodies[0]["frames"].as_array().unwrap().len(), 1);
            assert_eq!(bodies[0]["frames"][0]["frameUuid"], fx.uuids[0].as_str());
            assert!(
                !own_dir(&fx).join("c_L_0001.fits").exists(),
                "held back before calibration"
            );
        }

        /// M1: the per-frame rules themselves.
        #[test]
        fn hub_frame_rules() {
            let ok = || crate::collab::frame_meta::FrameMeta {
                filter_raw: "Red".into(),
                channel: "mono".into(),
                exptime_sec: 300.0,
                date_obs: None,
                meta: serde_json::json!({ "instrume": "cam" }),
            };
            assert_eq!(hub_frame_rule_problem(&ok()), None);
            for exptime in [0.0, -1.0, 86_400.5, f64::NAN] {
                let mut m = ok();
                m.exptime_sec = exptime;
                assert!(hub_frame_rule_problem(&m).is_some(), "{exptime}");
            }
            let mut m = ok();
            m.exptime_sec = 86_400.0;
            assert_eq!(
                hub_frame_rule_problem(&m),
                None,
                "the upper bound is inclusive"
            );
            // Controller ruling 2026-09-28: an empty (or whitespace-only,
            // which trims to empty) filter is now accepted — only over 80
            // characters is refused.
            for filter in ["", "   "] {
                let mut m = ok();
                m.filter_raw = filter.to_string();
                assert_eq!(
                    hub_frame_rule_problem(&m),
                    None,
                    "an empty filter is accepted: {filter:?}"
                );
            }
            let mut m = ok();
            m.filter_raw = "x".repeat(81);
            assert!(
                hub_frame_rule_problem(&m).is_some(),
                "over 80 chars is refused"
            );
            let mut m = ok();
            m.channel = "rgb".into();
            assert!(hub_frame_rule_problem(&m).is_some());
            let mut m = ok();
            m.meta = serde_json::json!({ "blob": "x".repeat(8200) });
            assert!(hub_frame_rule_problem(&m).is_some_and(|p| p.contains("at most 8192")));
            let mut m = ok();
            m.meta = serde_json::json!([1, 2]);
            assert!(hub_frame_rule_problem(&m).is_some());
        }

        /// I2 (the wave-2 disk-truth test, on the wave-3 storage engine —
        /// Task 15): a stat sweep running while an update waits for its
        /// `…/version` reply sees the regenerated file as the frame's own —
        /// no state change, and the new seed tag survives — so the frame
        /// ends seeded at v2 AND on disk.
        #[tokio::test]
        async fn a_storage_sweep_mid_update_leaves_the_frame_seeded_and_on_disk() {
            let fx = fixture(1).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::Ok,
                    delay: Some(std::time::Duration::from_millis(1500)),
                    seen: None,
                })
                .with_priority(1)
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let mid_sweep = async {
                // The update has replaced the file and seeded v2; its
                // `…/version` reply is held back 1.5 s by the mock.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !tag_present(&fx, &fx.uuids[0], 2).await {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the update never seeded v2"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                let events = storage_sweep(&fx).await;
                let row = own_row(&fx, &fx.uuids[0]).unwrap();
                (events, row, tag_present(&fx, &fx.uuids[0], 2).await)
            };
            let (res, (events, mid_row, mid_tag)) =
                tokio::join!(publish_collab_frames(&fx.ctx, PID, None), mid_sweep);
            let res = res.unwrap();
            assert_eq!(res.updated, 1, "{res:?}");
            assert!(
                !events.iter().any(|e| matches!(
                    e,
                    crate::api::collab_live::storage_task::StorageEvent::StateChanged { .. }
                )),
                "{events:?}"
            );
            assert!(mid_row.on_disk, "mid-update the file is the frame's own");
            assert!(mid_tag, "the v2 tag survives the sweep");

            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert!(row.on_disk);
            assert!(tag_present(&fx, &fx.uuids[0], 2).await, "seeded at v2");
            assert!(storage_sweep(&fx).await.is_empty());
            assert_eq!(
                own_row(&fx, &fx.uuids[0]).unwrap().local_state,
                crate::db::collab_frames::LocalState::OwnHeld
            );
        }

        /// I2: a `…/version` the hub refuses puts the row back to the hub's
        /// content and marks the regenerated file not held; the next run
        /// regenerates and posts again.
        #[tokio::test]
        async fn a_refused_version_is_unstaged_and_retried() {
            let fx = fixture(1).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(
                    ResponseTemplate::new(409)
                        .set_body_json(serde_json::json!({"error": "project is closed"})),
                )
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let v1 = own_row(&fx, &fx.uuids[0]).unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let err = publish_collab_frames(&fx.ctx, PID, None).await;
            assert!(err.is_err(), "{err:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                (row.content_version, row.blake3.as_str(), row.xxh3.as_str()),
                (1, v1.blake3.as_str(), v1.xxh3.as_str()),
                "the hub's content keys are back"
            );
            assert!(!row.on_disk, "the regenerated file is not v1");
            assert_eq!(row.recipe_hash, None, "the version is unconfirmed");
            assert_eq!(project_tag_count(&fx).await, 0, "nothing advertised");
            storage_sweep(&fx).await;
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            assert!(
                crate::db::collab_live::my_claims(&conn, PID)
                    .unwrap()
                    .iter()
                    .all(|(u, _)| u != &fx.uuids[0]),
                "the unconfirmed file is not claimed"
            );
            assert!(!own_row(&fx, &fx.uuids[0]).unwrap().on_disk);
            drop(conn);

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{res:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert!(row.on_disk);
            assert!(tag_present(&fx, &fx.uuids[0], 2).await);
        }

        /// Task 6: a CAS version conflict (the hub holds a newer version
        /// than the one this run's update superseded) holds the frame back
        /// with the hub's number and unstages it — its claim leaves through
        /// the outbox. The manifest delta is pulled at once, so the row
        /// carries the hub's version before another run is asked for (fix
        /// round 1, item 6: never a regenerate → conflict loop).
        #[tokio::test]
        async fn a_version_conflict_holds_the_frame_back_with_the_hub_version() {
            let fx = fixture(1).await;
            let seen: SeenVersions = Default::default();
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::Conflict(5),
                    delay: None,
                    seen: Some(Arc::clone(&seen)),
                })
                .with_priority(1)
                .mount(&fx.server)
                .await;
            Mock::given(wm_method("GET"))
                .and(wm_path(format!("/api/v1/projects/{PID}/manifest")))
                .respond_with(ManifestEcho {
                    seen: Arc::clone(&seen),
                    cv: Some(5),
                    same_bytes: false,
                })
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(claims(&fx), vec![(fx.uuids[0].clone(), 1)]);
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 0, "{res:?}");
            assert_eq!(res.held_back.len(), 1);
            assert_eq!(
                res.held_back[0].reasons,
                vec!["version conflict: the hub has content version 5".to_string()]
            );
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                row.content_version, 5,
                "the manifest caught up in the same run"
            );
            assert_eq!(row.blake3, "f".repeat(64));
            assert!(
                !row.on_disk,
                "unstaged: the regenerated file is not the hub's"
            );
            assert_eq!(
                row.local_state,
                crate::db::collab_frames::LocalState::OwnMissing
            );
            assert!(
                !tag_present(&fx, &fx.uuids[0], 2).await,
                "the unposted tag is gone"
            );
            assert!(
                claims(&fx).is_empty(),
                "no claim on bytes this device lacks"
            );
            assert_eq!(outbox_len(&fx), 1, "the claim's removal is owed to the hub");
        }

        /// Controller ruling (fix round 1, item 10): a retried version call
        /// whose first reply was lost answers `conflict` at
        /// `expectedVersion + 1` — ours ONLY when the hub's BLAKE3 for that
        /// version is the one this run posted (checked by a manifest read);
        /// then it is recorded as `ok`.
        #[tokio::test]
        async fn a_conflict_at_the_next_version_with_our_bytes_is_our_own_lost_reply() {
            let fx = fixture(1).await;
            let seen: SeenVersions = Default::default();
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::ConflictNext,
                    delay: None,
                    seen: Some(Arc::clone(&seen)),
                })
                .with_priority(1)
                .mount(&fx.server)
                .await;
            Mock::given(wm_method("GET"))
                .and(wm_path(format!("/api/v1/projects/{PID}/manifest")))
                .respond_with(ManifestEcho {
                    seen: Arc::clone(&seen),
                    cv: None,
                    same_bytes: true,
                })
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!((res.updated, res.held_back.len()), (1, 0), "{res:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert!(row.on_disk);
            assert_eq!(claims(&fx), vec![(fx.uuids[0].clone(), 2)]);
        }

        /// The other branch of the same ruling: `conflict` at
        /// `expectedVersion + 1` with OTHER bytes is another device of this
        /// account's version — a real conflict, held back.
        #[tokio::test]
        async fn a_conflict_at_the_next_version_with_other_bytes_is_a_real_conflict() {
            let fx = fixture(1).await;
            let seen: SeenVersions = Default::default();
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::ConflictNext,
                    delay: None,
                    seen: Some(Arc::clone(&seen)),
                })
                .with_priority(1)
                .mount(&fx.server)
                .await;
            Mock::given(wm_method("GET"))
                .and(wm_path(format!("/api/v1/projects/{PID}/manifest")))
                .respond_with(ManifestEcho {
                    seen: Arc::clone(&seen),
                    cv: None,
                    same_bytes: false,
                })
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!((res.updated, res.held_back.len()), (0, 1), "{res:?}");
            assert_eq!(
                res.held_back[0].reasons,
                vec!["version conflict: the hub has content version 2".to_string()]
            );
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                row.content_version, 2,
                "the hub's version, via the manifest"
            );
            assert_eq!(row.blake3, "f".repeat(64), "not our bytes");
            assert!(!row.on_disk);
            assert!(claims(&fx).is_empty());
        }

        /// Hub rule: a pending outbox entry is flushed BEFORE the version
        /// call, and the versions go out as one CAS batch whose
        /// `expectedVersion` is the re-read row's version.
        #[tokio::test]
        async fn the_outbox_is_flushed_before_the_version_batch() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            {
                // an unsent claim change, as a landing would leave it
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                crate::db::collab_live::record_claim_change(
                    &conn,
                    PID,
                    &fx.uuids[1],
                    crate::db::collab_live::ClaimOp::Add { content_version: 1 },
                )
                .unwrap();
            }
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 2, "{res:?}");
            let reqs = requests(&fx.server).await;
            let put = reqs
                .iter()
                .position(|(m, _, _)| m == "PUT")
                .expect("the outbox was flushed");
            let post = reqs
                .iter()
                .position(|(m, p, _)| m == "POST" && p == &versions_path())
                .expect("the versions were posted");
            assert!(put < post, "flush first, then the versions");
            assert_eq!(reqs[put].2["reportSeq"], 1, "the one pending journal entry");
            let batch = reqs[post].2["versions"].as_array().unwrap().clone();
            assert_eq!(batch.len(), 2, "one batch for both frames");
            assert!(batch.iter().all(|v| v["expectedVersion"] == 1));
            assert_eq!(outbox_len(&fx), 0);
            let mut want: Vec<(String, i32)> = fx.uuids.iter().map(|u| (u.clone(), 2)).collect();
            want.sort();
            assert_eq!(claims(&fx), want);
        }

        /// An update whose run dies between staging the new file and the
        /// hub's `…/version` reply (app quit, lost connection) is posted by
        /// the next PLAIN publish — here the update came from a republish
        /// (a plate solve moved the bytes, not the recipe), so an unchanged
        /// recipe would call the frame unchanged forever.
        #[tokio::test]
        async fn an_interrupted_update_is_posted_by_the_next_plain_publish() {
            let fx = fixture(1).await;
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(VersionsReply {
                    mode: VersionsMode::Ok,
                    delay: Some(std::time::Duration::from_secs(60)),
                    seen: None,
                })
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&fx.server)
                .await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let v1 = own_row(&fx, &fx.uuids[0]).unwrap();
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                seed_plate_solve(&conn, fx.frame_ids[0], 0.776, 10.68, 41.27);
            }

            // The republish stages v2, then waits on `…/version`; the run is
            // dropped there.
            let staged = async {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    let row = own_row(&fx, &fx.uuids[0]).unwrap();
                    if row.xxh3 != v1.xxh3 && tag_present(&fx, &fx.uuids[0], 2).await {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the republish never staged v2"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            };
            tokio::select! {
                res = republish_collab_frames(&fx.ctx, PID, None) => {
                    panic!("the republish finished before it was interrupted: {res:?}")
                }
                () = staged => {}
            }
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 1, "the hub never confirmed v2");
            assert_eq!(
                row.recipe_hash, None,
                "a staged file is not a confirmed recipe"
            );

            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!((res.updated, res.unchanged), (1, 0), "{res:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(row.content_version, 2);
            assert_eq!(
                row.recipe_hash, v1.recipe_hash,
                "the recipe is confirmed again"
            );
            assert!(row.on_disk);
            assert!(tag_present(&fx, &fx.uuids[0], 2).await, "seeded at v2");
        }

        /// I3: a collab store that cannot be mounted refuses the publish
        /// before anything is calibrated — never a regenerate-then-hold-back
        /// loop.
        #[tokio::test]
        async fn an_unmountable_store_refuses_publish_before_generation() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            fx.node.set_collab_root(None).await.unwrap();
            let store_dir = fx.collab.join(".athenaeum");
            std::fs::remove_dir_all(&store_dir).unwrap();
            std::fs::write(&store_dir, b"not a folder").unwrap();
            match publish_collab_frames(&fx.ctx, PID, None).await {
                Err(ApiError::Conflict(m)) => assert_eq!(m, COLLAB_STORE_UNMOUNTED),
                other => panic!("expected the unmounted refusal, got {other:?}"),
            }
            assert!(!own_dir(&fx).exists(), "nothing calibrated");
            assert!(requests(&fx.server).await.is_empty(), "no hub call");
        }

        // ── Amendment A6: one publishing device per (project, account) ─────

        /// The fixture, re-pointed at a fake hub where the fixture's account
        /// ("acc-Me Myself", as its snapshot names it) has two devices: this
        /// one (`tok`) and "Obs PC" (`tok-other`), which already published
        /// `x1` under the name this device's first frame would get — so
        /// "Obs PC" is the project's publishing device.
        async fn two_devices_one_account(
            n: usize,
        ) -> (PubFx, crate::collab::fake_hub::FakeHub, String) {
            const OTHER: &str = "T0JTLVBD"; // "OBS-PC"
            let fx = fixture(n).await;
            let hub = crate::collab::fake_hub::FakeHub::start().await;
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            hub.add_account("tok", "acc-Me Myself", "Me Myself", &me, None);
            hub.add_account("tok-other", "acc-Me Myself", "Me Myself", OTHER, None);
            hub.add_device("acc-Me Myself", OTHER, "dev-obs", "Obs PC", None);
            hub.add_project(
                PID,
                "m31",
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            CollabClient::new(hub.uri())
                .unwrap()
                .announce_frames(
                    "tok-other",
                    PID,
                    &[crate::collab::hub_client::FrameInWire {
                        frame_uuid: "x1".into(),
                        file_name: "c_L_0000.fits".into(),
                        blake3: "a".repeat(64),
                        byte_size: 10,
                        xxh3: "0".repeat(16),
                        filter_raw: "L".into(),
                        filter_canonical: "L".into(),
                        channel: "mono".into(),
                        exptime_sec: 300.0,
                        date_obs: None,
                        gate_version: 0,
                        meta: serde_json::json!({}),
                    }],
                )
                .await
                .expect("the other device announces first and is bound");
            wire_hub(&fx.ctx, &hub.uri());
            (fx, hub, me)
        }

        /// A6, the bug it fixes: a second device of the account is refused
        /// with the typed error (nothing announced, nothing recorded); the
        /// refusal is recorded, so the next run stops before any generation
        /// or hub call — no retry storm; after "Publish from this device" it
        /// publishes, with names unique against the whole manifest of its
        /// account (the other device's `c_L_0000.fits` is never reused), and
        /// the other device's frame is a REPLICA here.
        #[tokio::test]
        async fn a_second_device_is_refused_until_it_switches() {
            let (fx, hub, me) = two_devices_one_account(2).await;
            crate::api::collab_exchange::sync_manifest(&fx.ctx, PID, None, None)
                .await
                .unwrap();
            let x1 = own_row(&fx, "x1").expect("the other device's frame is cached");
            assert_eq!(x1.origin, crate::db::collab_frames::FrameOrigin::Replica);
            assert_eq!(x1.local_state, crate::db::collab_frames::LocalState::Wanted);

            match publish_collab_frames(&fx.ctx, PID, None).await {
                Err(ApiError::Conflict(m)) => assert_eq!(m, "collab_publishing_device:Obs PC"),
                other => panic!("expected the typed refusal, got {other:?}"),
            }
            for uuid in &fx.uuids {
                assert!(own_row(&fx, uuid).is_none(), "nothing recorded for {uuid}");
                assert!(hub.frame(PID, uuid).is_none(), "nothing announced");
            }
            assert_eq!(project_tag_count(&fx).await, 0, "every seed rolled back");
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                let bound = crate::db::collab::publishing_device(&conn, PID)
                    .unwrap()
                    .expect("the refusal is recorded");
                assert_eq!(bound.name.as_deref(), Some("Obs PC"));
            }
            let card = list_projects(&fx.ctx).unwrap().remove(0);
            assert!(!card.publishing_here);
            assert_eq!(
                card.publishing_device.and_then(|d| d.name).as_deref(),
                Some("Obs PC")
            );

            // No retry storm: the next run (manual or auto) is refused from
            // the record — no announce reaches the hub, nothing is calibrated.
            let announces = hub.requests_to(&format!("/projects/{PID}/frames")).await;
            std::fs::remove_dir_all(own_dir(&fx)).ok();
            match auto_publish_collab_frames(&fx.ctx, PID, None).await {
                Err(ApiError::Conflict(m)) => assert!(m.starts_with("collab_publishing_device:")),
                other => panic!("expected the typed refusal, got {other:?}"),
            }
            assert_eq!(
                hub.requests_to(&format!("/projects/{PID}/frames")).await,
                announces,
                "no announce while the binding stands"
            );
            assert!(!own_dir(&fx).exists(), "nothing calibrated");

            // "Publish from this device".
            let card = set_collab_publishing_device(&fx.ctx, PID).await.unwrap();
            assert!(card.publishing_here);
            assert_eq!(
                hub.publishing_device(PID, "acc-Me Myself").as_deref(),
                Some(me.as_str())
            );
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 2, "{res:?}");
            let mut names: Vec<String> = fx
                .uuids
                .iter()
                .map(|u| hub.frame(PID, u).expect("announced").file_name)
                .collect();
            names.sort();
            assert_eq!(names, vec!["c_L_0000_2.fits", "c_L_0001.fits"]);
            for uuid in &fx.uuids {
                assert_eq!(
                    hub.frame(PID, uuid).unwrap().publisher_device_id.as_deref(),
                    Some(me.as_str())
                );
                assert_eq!(
                    own_row(&fx, uuid).unwrap().origin,
                    crate::db::collab_frames::FrameOrigin::Own
                );
            }
            // The previously bound device may no longer announce.
            let x2 = crate::collab::hub_client::FrameInWire {
                frame_uuid: "x2".into(),
                file_name: "x2.fits".into(),
                blake3: "a".repeat(64),
                byte_size: 10,
                xxh3: "0".repeat(16),
                filter_raw: "L".into(),
                filter_canonical: "L".into(),
                channel: "mono".into(),
                exptime_sec: 300.0,
                date_obs: None,
                gate_version: 0,
                meta: serde_json::json!({}),
            };
            let refused = CollabClient::new(hub.uri())
                .unwrap()
                .announce_frames("tok-other", PID, &[x2])
                .await;
            assert!(
                matches!(&refused, Err(crate::account::AccountClientError::PublishingDevice { device_id, .. }) if *device_id == me),
                "{refused:?}"
            );
        }

        /// A6 fix round 1: a device replace keeps authorship. Frames the
        /// retired device published (a reinstall that kept the catalog under
        /// a new key) are replicas here until the replace is recorded, then
        /// own again; a recalibration posts their new version from THIS
        /// device, the hub accepts it through its fallback (the old device
        /// is retired) and adopts the frame — the manifest then names this
        /// device.
        #[tokio::test]
        async fn a_replaced_devices_frames_are_versioned_here_and_adopted() {
            const OLD: &str = "T0xELURFVg=="; // "OLD-DEV"
            let fx = fixture(1).await;
            let hub = crate::collab::fake_hub::FakeHub::start().await;
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            hub.add_account("tok", "acc-Me Myself", "Me Myself", &me, None);
            hub.add_account("tok-old", "acc-Me Myself", "Me Myself", OLD, None);
            hub.add_device("acc-Me Myself", OLD, "dev-old", "Old PC", None);
            hub.add_project(
                PID,
                "m31",
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            wire_hub(&fx.ctx, &hub.uri());
            // The old install (the old key) published the frame.
            crate::api::account::store_token_for_test(&fx.ctx, "tok-old").unwrap();
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1);
            let uuid = fx.uuids[0].clone();
            assert_eq!(
                hub.frame(PID, &uuid)
                    .unwrap()
                    .publisher_device_id
                    .as_deref(),
                Some(OLD)
            );
            // This install (the new key) reads the manifest: another device's frame.
            crate::api::account::store_token_for_test(&fx.ctx, "tok").unwrap();
            crate::api::collab_exchange::sync_manifest(&fx.ctx, PID, None, None)
                .await
                .unwrap();
            assert_eq!(
                own_row(&fx, &uuid).unwrap().origin,
                crate::db::collab_frames::FrameOrigin::Replica
            );
            // The replace: the old device is retired, and recorded here.
            hub.revoke_device(OLD, true);
            crate::api::collab_exchange::record_device_replaced(&fx.ctx, OLD, "acc-Me Myself")
                .unwrap();
            crate::api::collab_exchange::rederive_own_frames(&fx.ctx);
            assert_eq!(
                own_row(&fx, &uuid).unwrap().origin,
                crate::db::collab_frames::FrameOrigin::Own
            );
            // A recalibration: a new version from this device, accepted and adopted.
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{res:?}");
            let hub_row = hub.frame(PID, &uuid).unwrap();
            assert_eq!(hub_row.content_version, 2);
            assert_eq!(
                hub_row.publisher_device_id.as_deref(),
                Some(me.as_str()),
                "the hub adopted the frame"
            );
            crate::api::collab_exchange::sync_manifest(&fx.ctx, PID, None, None)
                .await
                .unwrap();
            let row = own_row(&fx, &uuid).unwrap();
            assert_eq!(row.origin, crate::db::collab_frames::FrameOrigin::Own);
            assert_eq!(row.content_version, 2);
            assert_eq!(row.source_frame_id, Some(fx.frame_ids[0]), "bound again");
        }

        /// A6 fix round 2 (M3): a cached binding naming a device THIS
        /// device replaced is not "bound elsewhere" — the publish announces.
        #[tokio::test]
        async fn a_binding_naming_a_replaced_device_does_not_refuse_the_publish() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                crate::db::collab::set_publishing_device(
                    &conn,
                    PID,
                    Some(&crate::db::collab::PublishingDevice {
                        device_id: "T0xELURFVg==".into(),
                        name: Some("Old PC".into()),
                    }),
                )
                .unwrap();
                crate::db::collab_live::record_replaced_device(
                    &conn,
                    "T0xELURFVg==",
                    Some("acc-Me Myself"),
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            assert!(res.held_back.iter().all(|h| h.publishing_device.is_none()));
        }

        /// A6 (UI follow-up): a held-back frame carries `publishingDevice`
        /// ONLY when another device of this account is bound — a run that
        /// also posts a version resolves Ok, and the refused new frame is
        /// told apart by the field, not the reason text. Every other
        /// held-back frame has `None`.
        #[tokio::test]
        async fn a_frame_held_back_for_the_binding_names_the_device() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            // Publish frame 0 only (frame 1 not linked yet → gate-held).
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE frames SET uuid = NULL WHERE id = ?1",
                    [fx.frame_ids[1]],
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{res:?}");
            assert!(
                res.held_back.iter().all(|h| h.publishing_device.is_none()),
                "a gate hold-back names no device: {:?}",
                res.held_back
            );
            // Another device of the account is bound now; frame 1 becomes
            // publishable and frame 0's recipe moves (a version).
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                conn.execute(
                    "UPDATE frames SET uuid = ?2 WHERE id = ?1",
                    rusqlite::params![fx.frame_ids[1], fx.uuids[1]],
                )
                .unwrap();
                crate::db::collab::set_publishing_device(
                    &conn,
                    PID,
                    Some(&crate::db::collab::PublishingDevice {
                        device_id: "T0JTLVBD".into(),
                        name: Some("Obs PC".into()),
                    }),
                )
                .unwrap();
            }
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "the version still goes out: {res:?}");
            let refused: Vec<&HeldBackFrame> = res
                .held_back
                .iter()
                .filter(|h| h.publishing_device.is_some())
                .collect();
            assert_eq!(refused.len(), 1, "{:?}", res.held_back);
            assert_eq!(refused[0].frame_id, fx.frame_ids[1]);
            assert_eq!(refused[0].publishing_device.as_deref(), Some("Obs PC"));
            assert!(refused[0].reasons[0].contains(" publishes new frames to this project"));
            let json = serde_json::to_value(refused[0]).unwrap();
            assert_eq!(json["publishingDevice"], "Obs PC");
        }

        /// A6: a frame this device holds as own but whose manifest row names
        /// another device is never versioned here (a debug skip, no hub
        /// call); a `not_publishing_device` answer is recorded against the
        /// manifest version and not retried until the manifest changes.
        #[tokio::test]
        async fn versions_are_posted_only_for_this_devices_frames() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            let uuid = fx.uuids[0].clone();
            // The hub answers every version with not_publishing_device.
            fx.server.reset().await;
            Mock::given(wm_method("PUT"))
                .and(wm_path(format!("/api/v1/projects/{PID}/holders/self")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "holderSeq": 1, "digestMatch": true, "nextFlushMs": 1000, "refused": []
                })))
                .mount(&fx.server)
                .await;
            Mock::given(wm_method("POST"))
                .and(wm_path(versions_path()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "projectVersion": 6,
                    "results": [{"uuid": uuid, "status": "not_publishing_device", "contentVersion": 0}]
                })))
                .mount(&fx.server)
                .await;
            write_dark(&fx.master, 310.0);
            set_mtime(&fx.master, 120);
            let res = publish_collab_frames(&fx.ctx, PID, None).await;
            let row = own_row(&fx, &uuid).unwrap();
            assert_eq!(row.content_version, 1, "{res:?}");
            assert!(
                row.last_error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("not_publishing_device:")),
                "{:?}",
                row.last_error
            );
            assert_eq!(version_calls(&fx.server).await.len(), 1);
            // Not retried while the manifest row stays as it is.
            let res = publish_collab_frames(&fx.ctx, PID, None).await;
            assert_eq!(version_calls(&fx.server).await.len(), 1, "{res:?}");

            // A manifest row naming ANOTHER device: never versioned here.
            {
                let conn = crate::api::db(&fx.ctx).unwrap().conn();
                let mut wire: serde_json::Value = serde_json::from_str(&row.manifest_json).unwrap();
                wire["publisherDeviceId"] = serde_json::json!("T1RIRVI=");
                conn.execute(
                    "UPDATE project_frames_local SET manifest_json = ?3, manifest_version = 99,
                         last_error = NULL
                     WHERE project_id = ?1 AND frame_uuid = ?2",
                    rusqlite::params![PID, uuid, wire.to_string()],
                )
                .unwrap();
            }
            let _ = publish_collab_frames(&fx.ctx, PID, None).await;
            assert_eq!(
                version_calls(&fx.server).await.len(),
                1,
                "another device's frame is never versioned here"
            );
        }

        // ── Spec 2026-09-28 §6: attestation and the external publish branch ──

        /// F5: attesting the set alone carries the gate (calibration links
        /// dropped), and every light is seeded IN PLACE — no generation, the
        /// publisher folder stays empty, the recipe is `external:…`, and
        /// `meta.calibration` names it external.
        #[tokio::test]
        async fn attested_lights_are_seeded_in_place_without_generation() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            // I4: snapshot the originals BEFORE publish — the covering
            // assertion is that the run never touches them, not merely that
            // the row ends up pointing at them.
            let before: Vec<(Vec<u8>, std::time::SystemTime)> = fx
                .lights
                .iter()
                .map(|p| {
                    (
                        std::fs::read(p).unwrap(),
                        std::fs::metadata(p).unwrap().modified().unwrap(),
                    )
                })
                .collect();
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
                // Drop the calibration links: attestation alone must carry the gate.
                conn.execute("DELETE FROM calibration_set_to_frames", [])
                    .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 2, "{:?}", res.held_back);
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 1);
            let frames = bodies[0]["frames"].as_array().unwrap();
            assert_eq!(frames.len(), 2);
            for f in frames {
                assert_eq!(
                    f["meta"]["calibration"],
                    serde_json::json!({"dark": false, "flat": false, "bias": false, "external": true})
                );
                let uuid = f["frameUuid"].as_str().unwrap();
                let row = crate::db::collab_frames::get(&fx.conn(), PID, uuid)
                    .unwrap()
                    .unwrap();
                let landed = PathBuf::from(row.landed_path.clone().unwrap());
                assert!(
                    fx.lights.contains(&landed),
                    "landed path is the original: {}",
                    landed.display()
                );
                assert!(row.recipe_hash.as_deref().unwrap().starts_with("external:"));
                assert_eq!(
                    f["fileName"].as_str().unwrap(),
                    landed.file_name().unwrap().to_string_lossy()
                );
                assert_eq!(
                    f["xxh3"].as_str().unwrap(),
                    crate::package::xxh3_full_file(&landed).unwrap()
                );
            }
            assert_eq!(
                std::fs::read_dir(own_dir(&fx))
                    .map(|d| d.count())
                    .unwrap_or(0),
                0,
                "the publisher folder stays empty for attested frames"
            );
            // F5/A1: byte-for-byte and mtime untouched.
            for (i, light) in fx.lights.iter().enumerate() {
                assert_eq!(
                    std::fs::read(light).unwrap(),
                    before[i].0,
                    "{light:?} bytes changed"
                );
                assert_eq!(
                    std::fs::metadata(light).unwrap().modified().unwrap(),
                    before[i].1,
                    "{light:?} mtime changed"
                );
            }
        }

        /// F5: the recipe is `external:<size>:<modified_at>` — unchanged
        /// inputs are `unchanged`, a catalog-visible drift (the scanner's
        /// in-place re-parse after an external overwrite) is `updated`.
        #[tokio::test]
        async fn external_recipe_changes_with_size_or_mtime() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            assert_eq!(
                publish_collab_frames(&fx.ctx, PID, None)
                    .await
                    .unwrap()
                    .announced,
                1
            );
            assert_eq!(
                publish_collab_frames(&fx.ctx, PID, None)
                    .await
                    .unwrap()
                    .unchanged,
                1
            );
            // The scanner's in-place re-parse bumps files.size/modified_at after
            // an external overwrite.
            {
                let conn = fx.conn();
                conn.execute(
                    "UPDATE files SET size = size + 1, modified_at = '2027-01-01T00:00:00Z' \
                     WHERE id = (SELECT file_id FROM frames WHERE id = ?1)",
                    [fx.frame_ids[0]],
                )
                .unwrap();
            }
            std::fs::write(&fx.lights[0], b"new bytes that differ").unwrap();
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{:?}", res.held_back);
            // I4: the own row describes the (drifted) original, and the
            // publish itself never further touches the file beyond the
            // drift the test itself introduced.
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                row.landed_path.as_deref(),
                Some(fx.lights[0].to_string_lossy()).as_deref()
            );
            assert_eq!(
                std::fs::read(&fx.lights[0]).unwrap(),
                b"new bytes that differ"
            );
            assert_eq!(
                row.xxh3,
                crate::package::xxh3_full_file(&fx.lights[0]).unwrap()
            );
            assert_eq!(
                row.byte_size,
                std::fs::metadata(&fx.lights[0]).unwrap().len() as i64
            );
        }

        /// I2: an attested light whose ORIGINAL basename breaks the hub's
        /// `fileName` rule (here, a `:`) is held back at that check —
        /// `hub_file_name_problem`, exercised on the attested/external
        /// branch the same as the generated one — never announced under a
        /// name the hub would refuse the whole atomic batch over.
        #[tokio::test]
        async fn attested_light_with_a_hub_illegal_file_name_is_held_back() {
            let fx = fixture(1).await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
                // Drop the calibration links: attestation alone must carry
                // the gate (mirrors `attested_lights_are_seeded_in_place_without_generation`).
                conn.execute("DELETE FROM calibration_set_to_frames", [])
                    .unwrap();
                let bad_path = fx.lights[0].with_file_name("bad:name.fits");
                conn.execute(
                    "UPDATE files SET path = ?1, filename = ?2 \
                     WHERE id = (SELECT file_id FROM frames WHERE id = ?3)",
                    rusqlite::params![bad_path.to_string_lossy(), "bad:name.fits", fx.frame_ids[0]],
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 0, "{:?}", res.held_back);
            assert_eq!(res.held_back.len(), 1);
            assert!(
                res.held_back[0]
                    .reasons
                    .iter()
                    .any(|r| r.contains("the hub refuses this file name")),
                "{:?}",
                res.held_back
            );
        }

        /// C1 (critical, fix round 1): a set attested, published, then
        /// UN-attested with its real calibration links still present — the
        /// frame is now a normal generated Update. It must land at a FRESH
        /// own-dir name, never write over the original: `landed_path` moves
        /// off it, and its bytes/mtime never change.
        #[tokio::test]
        async fn un_attesting_a_published_set_never_overwrites_the_original() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            assert_eq!(
                own_row(&fx, &fx.uuids[0]).unwrap().landed_path.as_deref(),
                Some(fx.lights[0].to_string_lossy()).as_deref(),
                "seeded in place, as attested"
            );
            let original_bytes = std::fs::read(&fx.lights[0]).unwrap();
            let original_mtime = std::fs::metadata(&fx.lights[0])
                .unwrap()
                .modified()
                .unwrap();

            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, false).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{:?}", res.held_back);

            // C1: the original is byte-for-byte and mtime untouched.
            assert_eq!(
                std::fs::read(&fx.lights[0]).unwrap(),
                original_bytes,
                "the original's bytes must never change"
            );
            assert_eq!(
                std::fs::metadata(&fx.lights[0])
                    .unwrap()
                    .modified()
                    .unwrap(),
                original_mtime,
                "the original's mtime must never change"
            );

            // A generated file now exists in own_dir, and landed_path moved
            // there — never back onto the original.
            let names: Vec<String> = std::fs::read_dir(own_dir(&fx))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            assert_eq!(names, vec!["c_L_0000.fits".to_string()], "{names:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            let landed = PathBuf::from(row.landed_path.unwrap());
            assert_eq!(landed, own_dir(&fx).join("c_L_0000.fits"));
            assert!(!row.recipe_hash.as_deref().unwrap().starts_with("external:"));
        }

        /// I1: the reverse crossing — a frame first published GENERATED,
        /// then its set is attested. The next publish becomes an external
        /// Update targeting the original: `landed_path` moves there, and
        /// the now-superseded generated file (this publisher's own
        /// artifact, never the user's data) is removed. The original itself
        /// is untouched throughout, same guarantee as the other direction.
        #[tokio::test]
        async fn attesting_after_a_generated_publish_moves_landed_path_and_removes_the_old_file() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            let old_landed = own_row(&fx, &fx.uuids[0]).unwrap().landed_path.unwrap();
            let old_path = PathBuf::from(&old_landed);
            assert_eq!(old_path.parent().unwrap(), own_dir(&fx));
            assert!(
                old_path.exists(),
                "the generated file exists before attestation"
            );

            let original_bytes = std::fs::read(&fx.lights[0]).unwrap();
            let original_mtime = std::fs::metadata(&fx.lights[0])
                .unwrap()
                .modified()
                .unwrap();

            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{:?}", res.held_back);

            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                row.landed_path.as_deref(),
                Some(fx.lights[0].to_string_lossy()).as_deref(),
                "landed_path moved to the original"
            );
            assert!(row.recipe_hash.as_deref().unwrap().starts_with("external:"));
            assert!(
                !old_path.exists(),
                "the superseded generated file is removed"
            );
            assert_eq!(
                std::fs::read(&fx.lights[0]).unwrap(),
                original_bytes,
                "the original itself is untouched"
            );
            assert_eq!(
                std::fs::metadata(&fx.lights[0])
                    .unwrap()
                    .modified()
                    .unwrap(),
                original_mtime,
                "the original's mtime is untouched"
            );
        }

        /// C1 (fix round 3, critical): the same un-attest transition as
        /// [`un_attesting_a_published_set_never_overwrites_the_original`],
        /// but with the original INSIDE the Collaboration root at
        /// `<root>/src/…` — the shape spec §10 allows and the shape
        /// `collab_v3_live_e2e_tests.rs`'s `seed_one_light` actually uses.
        /// Before the fix, `publisher_dir` read this attested-in-place
        /// landing back as "this publisher's own folder" (it is the only
        /// own row and its path sits under the root), so the un-attest
        /// publish's own_dir resolved to `<root>/src` instead of
        /// `<root>/m31/me-myself`, and the post-move cleanup then deleted
        /// the user's original out from under them.
        #[tokio::test]
        async fn un_attesting_inside_the_collaboration_root_never_deletes_the_original() {
            let fx = fixture(1).await;
            // Relocate the fixture's light from outside the Collaboration
            // root to `<root>/src/…`, matching the e2e layout, and keep the
            // catalog's `files` row in sync with the move.
            let original_path = fx.collab.join("src").join("L_0000.fits");
            std::fs::create_dir_all(original_path.parent().unwrap()).unwrap();
            std::fs::rename(&fx.lights[0], &original_path).unwrap();
            {
                let conn = fx.conn();
                conn.execute(
                    "UPDATE files SET path = ?1 WHERE path = ?2",
                    rusqlite::params![
                        original_path.to_string_lossy(),
                        fx.lights[0].to_string_lossy()
                    ],
                )
                .unwrap();
            }

            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            assert_eq!(
                own_row(&fx, &fx.uuids[0]).unwrap().landed_path.as_deref(),
                Some(original_path.to_string_lossy()).as_deref(),
                "seeded in place, as attested, still under the Collaboration root"
            );
            let original_bytes = std::fs::read(&original_path).unwrap();
            let original_mtime = std::fs::metadata(&original_path)
                .unwrap()
                .modified()
                .unwrap();

            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, false).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{:?}", res.held_back);

            // C1: the original, still cataloged under the Collaboration
            // root, is byte-for-byte and mtime untouched — and still there.
            assert!(original_path.exists(), "the original must not be deleted");
            assert_eq!(
                std::fs::read(&original_path).unwrap(),
                original_bytes,
                "the original's bytes must never change"
            );
            assert_eq!(
                std::fs::metadata(&original_path)
                    .unwrap()
                    .modified()
                    .unwrap(),
                original_mtime,
                "the original's mtime must never change"
            );

            // own_dir resolves to the real own folder, not `<root>/src`, and
            // the generated file lands there with landed_path moved onto it.
            let names: Vec<String> = std::fs::read_dir(own_dir(&fx))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            assert_eq!(names, vec!["c_L_0000.fits".to_string()], "{names:?}");
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            let landed = PathBuf::from(row.landed_path.unwrap());
            assert_eq!(landed, own_dir(&fx).join("c_L_0000.fits"));
            assert!(!row.recipe_hash.as_deref().unwrap().starts_with("external:"));
        }

        /// C1 (fix round 3, critical), part 1 in isolation: the original
        /// sits DIRECTLY in this publisher's own folder — "a user who moves
        /// lights into their own publisher folder" (fix brief) — so
        /// `own_dir` resolves correctly (this test never depends on the
        /// `publisher_dir` fix; the row is excluded from consideration by
        /// its `external:` recipe hash regardless of the path-shape filter)
        /// and yet `is_own_dir_landing` is legitimately true for the
        /// original's path once un-attested. Only the removal-site guard —
        /// the `files` row lookup and `was_external_recipe` check — stands
        /// between this and deleting the user's data. Confirmed load-bearing
        /// by temporarily reverting just that guard: this test then fails
        /// (the original is deleted) while
        /// `un_attesting_inside_the_collaboration_root_never_deletes_the_original`
        /// above still passes, since that one's protection comes entirely
        /// from the `publisher_dir` fix.
        #[tokio::test]
        async fn un_attesting_never_deletes_an_original_moved_into_the_own_folder() {
            let fx = fixture(1).await;
            let original_path = own_dir(&fx).join("L_0000.fits");
            std::fs::create_dir_all(original_path.parent().unwrap()).unwrap();
            std::fs::rename(&fx.lights[0], &original_path).unwrap();
            {
                let conn = fx.conn();
                conn.execute(
                    "UPDATE files SET path = ?1 WHERE path = ?2",
                    rusqlite::params![
                        original_path.to_string_lossy(),
                        fx.lights[0].to_string_lossy()
                    ],
                )
                .unwrap();
            }

            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            assert_eq!(
                own_row(&fx, &fx.uuids[0]).unwrap().landed_path.as_deref(),
                Some(original_path.to_string_lossy()).as_deref(),
                "seeded in place, as attested, already inside the own folder"
            );
            let original_bytes = std::fs::read(&original_path).unwrap();
            let original_mtime = std::fs::metadata(&original_path)
                .unwrap()
                .modified()
                .unwrap();

            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, false).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.updated, 1, "{:?}", res.held_back);

            // C1: the original, still sitting right where the user put it
            // (in their own publisher folder), is never deleted.
            assert!(
                original_path.exists(),
                "the original must not be deleted, even inside the own folder"
            );
            assert_eq!(
                std::fs::read(&original_path).unwrap(),
                original_bytes,
                "the original's bytes must never change"
            );
            assert_eq!(
                std::fs::metadata(&original_path)
                    .unwrap()
                    .modified()
                    .unwrap(),
                original_mtime,
                "the original's mtime must never change"
            );

            // A freshly generated file lands ALONGSIDE it, and landed_path
            // moves onto the new file, never back onto the original.
            let mut names: Vec<String> = std::fs::read_dir(own_dir(&fx))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            assert_eq!(
                names,
                vec!["L_0000.fits".to_string(), "c_L_0000.fits".to_string()],
                "{names:?}"
            );
            let row = own_row(&fx, &fx.uuids[0]).unwrap();
            let landed = PathBuf::from(row.landed_path.unwrap());
            assert_eq!(landed, own_dir(&fx).join("c_L_0000.fits"));
        }

        /// I2: an attested original moved with the app's own file browser —
        /// size and mtime survive a plain rename, so the recipe
        /// (`external_recipe(size, mtime)`) is unchanged and a forced
        /// Republish takes the identical-bytes branch (`identical &&
        /// !w.identical`). Before the fix that branch just `continue`d:
        /// `landed_path` stayed pointed at the OLD path forever, reading
        /// "not on disk" even though the file is right there under a new
        /// name.
        #[tokio::test]
        async fn republish_after_moving_an_attested_original_moves_landed_path() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            let before = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                before.landed_path.as_deref(),
                Some(fx.lights[0].to_string_lossy()).as_deref()
            );

            // Move the original to a sibling folder — same bytes, same size,
            // same mtime, a fresh `files.path`.
            let sibling = fx.lights[0].parent().unwrap().join("moved");
            std::fs::create_dir_all(&sibling).unwrap();
            let new_path = sibling.join(fx.lights[0].file_name().unwrap());
            std::fs::rename(&fx.lights[0], &new_path).unwrap();
            {
                let conn = fx.conn();
                conn.execute(
                    "UPDATE files SET path = ?1 WHERE path = ?2",
                    rusqlite::params![new_path.to_string_lossy(), fx.lights[0].to_string_lossy()],
                )
                .unwrap();
            }
            let original_bytes = std::fs::read(&new_path).unwrap();

            let res = republish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(
                (res.announced, res.updated, res.unchanged),
                (0, 0, 1),
                "{:?}",
                res.held_back
            );

            let after = own_row(&fx, &fx.uuids[0]).unwrap();
            assert_eq!(
                after.landed_path.as_deref(),
                Some(new_path.to_string_lossy()).as_deref(),
                "landed_path follows the file to its new location"
            );
            assert_eq!(
                after.content_version, before.content_version,
                "no new version — the bytes never changed"
            );
            assert_eq!(std::fs::read(&new_path).unwrap(), original_bytes);
        }

        // ── Fix round 2: landed_path is TEXT UNIQUE table-wide, and a frame
        // set can be linked to more than one project ──────────────────────

        /// A second cached project, linked to the fixture's own set —
        /// registered on the SAME [`crate::collab::fake_hub::FakeHub`] the
        /// caller already pointed `fx.ctx` at (frame sets are global, so a
        /// set can back more than one project's publish).
        async fn second_project(
            fx: &PubFx,
            hub: &crate::collab::fake_hub::FakeHub,
            project_id: &str,
            slug: &str,
        ) {
            hub.add_project(
                project_id,
                slug,
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            let me = DeviceKey::load_or_create(&device_key_path(
                &crate::api::sync::sync_dirs(&fx.ctx).unwrap().identity_dir,
            ))
            .unwrap()
            .node_id();
            let members = serde_json::json!([member_json("Me Myself", "send_receive", false, &me)]);
            {
                let conn = fx.conn();
                crate::db::collab::upsert_project(
                    &conn,
                    &CollabProjectRow {
                        project_id: project_id.into(),
                        slug: slug.into(),
                        title: slug.to_uppercase(),
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
                        feed_epoch: None,
                        holder_seq: -1,
                    },
                )
                .unwrap();
                crate::db::collab::set_dictionary(
                    &conn,
                    project_id,
                    Some(1),
                    Some(r#"[{"canonical":"L","aliases":["Lum"],"kind":"broadband"}]"#),
                )
                .unwrap();
            }
            link_frame_set(&fx.ctx, project_id, fx.set_id).unwrap();
        }

        /// Finding 2 (fix round 2): project B publishes GENERATED before the
        /// set is attested; once attested, its own row would collide with
        /// project A's (which already landed the original) on the
        /// table-wide `landed_path` UNIQUE constraint. The failed move must
        /// never delete B's still-current own-dir file — B's row keeps
        /// pointing at it.
        #[tokio::test]
        async fn a_landed_path_collision_across_projects_leaves_the_losing_row_intact() {
            let fx = fixture(1).await;
            let hub = crate::collab::fake_hub::FakeHub::start().await;
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            hub.add_account("tok", "acc-Me Myself", "Me Myself", &me, None);
            hub.add_project(
                PID,
                "m31",
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            wire_hub(&fx.ctx, &hub.uri());
            second_project(&fx, &hub, "p2", "p2proj").await;

            // B publishes GENERATED first, while the set is not attested.
            let res_b1 = publish_collab_frames(&fx.ctx, "p2", None).await.unwrap();
            assert_eq!(res_b1.announced, 1, "{:?}", res_b1.held_back);
            let b_old_landed = crate::db::collab_frames::get(&fx.conn(), "p2", &fx.uuids[0])
                .unwrap()
                .unwrap()
                .landed_path
                .unwrap();
            assert!(PathBuf::from(&b_old_landed).exists());

            // The set is attested; A publishes fresh (New) and lands the
            // original first.
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }
            let res_a = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res_a.announced, 1, "{:?}", res_a.held_back);
            assert_eq!(
                hub.frame(PID, &fx.uuids[0]).unwrap().file_name,
                fx.lights[0].file_name().unwrap().to_string_lossy()
            );

            // B publishes again: now attested too (same set), it crosses
            // toward the SAME original — colliding with A's row.
            let _res_b2 = publish_collab_frames(&fx.ctx, "p2", None).await.unwrap();
            let b_row = crate::db::collab_frames::get(&fx.conn(), "p2", &fx.uuids[0])
                .unwrap()
                .unwrap();
            assert_eq!(
                b_row.landed_path.as_deref(),
                Some(b_old_landed.as_str()),
                "B's row still points at its own file, not the collided original"
            );
            assert!(
                PathBuf::from(&b_old_landed).exists(),
                "B's own-dir file must not be deleted when the move failed"
            );
        }

        /// Finding 3 (fix round 2): a NEW attested frame whose original
        /// already backs ANOTHER project's own row is held back before
        /// anything is planned — never announced (which would orphan it on
        /// the hub once the local table-wide UNIQUE write failed anyway).
        #[tokio::test]
        async fn a_new_attested_frame_whose_original_already_backs_another_project_is_held_back() {
            let fx = fixture(1).await;
            let hub = crate::collab::fake_hub::FakeHub::start().await;
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            hub.add_account("tok", "acc-Me Myself", "Me Myself", &me, None);
            hub.add_project(
                PID,
                "m31",
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            wire_hub(&fx.ctx, &hub.uri());
            second_project(&fx, &hub, "p2", "p2proj").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            }

            // A publishes first and lands the original.
            let res_a = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res_a.announced, 1, "{:?}", res_a.held_back);

            // B, same set, also attested, never published before: New —
            // held back instead of racing the hub for an orphaned announce.
            let res_b = publish_collab_frames(&fx.ctx, "p2", None).await.unwrap();
            assert_eq!(res_b.announced, 0, "{:?}", res_b.held_back);
            assert_eq!(res_b.held_back.len(), 1, "{:?}", res_b.held_back);
            assert!(
                res_b.held_back[0].reasons[0].contains("already backs a frame in project \"M 31\""),
                "{:?}",
                res_b.held_back
            );
            assert!(
                crate::db::collab_frames::get(&fx.conn(), "p2", &fx.uuids[0])
                    .unwrap()
                    .is_none(),
                "no B own row"
            );
            assert!(hub.frame("p2", &fx.uuids[0]).is_none(), "no hub orphan");
        }

        /// F5: two attested originals that resolve to the same basename (a
        /// different folder each) are never renamed on disk — the second is
        /// held back.
        #[tokio::test]
        async fn attested_duplicate_basename_is_held_back() {
            let fx = fixture(2).await;
            mount_hub(&fx.server, "published").await;
            {
                let conn = fx.conn();
                crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
                // Two originals with the same basename in different folders.
                let dup = fx.lights[0]
                    .parent()
                    .unwrap()
                    .join("other")
                    .join(fx.lights[0].file_name().unwrap());
                std::fs::create_dir_all(dup.parent().unwrap()).unwrap();
                std::fs::copy(&fx.lights[1], &dup).unwrap();
                conn.execute(
                    "UPDATE files SET path = ?1, filename = ?2 WHERE id = (SELECT file_id FROM frames WHERE id = ?3)",
                    rusqlite::params![
                        dup.to_string_lossy(),
                        fx.lights[0].file_name().unwrap().to_string_lossy(),
                        fx.frame_ids[1]
                    ],
                )
                .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            assert_eq!(res.held_back.len(), 1, "{:?}", res.held_back);
            assert!(
                res.held_back[0].reasons[0].contains("already published by you — rename the file"),
                "{:?}",
                res.held_back
            );
        }

        /// A second frames_set of ONE ATTESTED light named `name`, linked to
        /// no calibration (F5 never checks it), on the same M31 target and
        /// filter `L` fixture's own set uses — so only the basename check
        /// under test can hold it back. Returns the new set's id.
        fn seed_attested_set(
            conn: &rusqlite::Connection,
            root: &Path,
            name: &str,
            uuid: &str,
        ) -> i64 {
            conn.execute(
                "INSERT INTO frames_set (name, objctra, objctdec) VALUES ('M31 Set 2', '00:42:44', '+41:16:09')",
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

            let light = root.join("src2").join(name);
            write_plane(&light, |x, y| 500.0 + ((x * 3 + y) % 7) as f32);
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
            set_id
        }

        /// M3 (sweep item 6): the GENERATED branch must insert its picked
        /// basename into `taken_names` too — before this fix only the
        /// attested/external branch did, so an attested New and a generated
        /// New could announce under the very same `fileName`.
        ///
        /// Two sets: `fixture(1)`'s un-attested `L_0000.fits` (generates
        /// `c_L_0000.fits`) is planned FIRST — `project_gate`'s union is
        /// ordered by `frame_id`, and this set's light was created first, so
        /// its frame_id is smaller — and a second, ATTESTED set whose
        /// ORIGINAL is already named `c_L_0000.fits`, planned second. Load
        /// bearing: reverting the one-line `taken_names.insert(file_name)`
        /// this test guards makes it fail — verified by hand (temporarily
        /// commenting the line out reproduces `res.announced == 2` and no
        /// held-back frame; see the fix report for Task 10 fix round 1).
        #[tokio::test]
        async fn a_generated_new_blocks_an_attested_new_picking_its_same_output_name() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            let collision_name = crate::export::calibrated_output_filename(
                fx.lights[0].file_name().unwrap().to_str().unwrap(),
                false,
                crate::fits_writer::OutputFormat::Fits,
            );
            assert_eq!(collision_name, "c_L_0000.fits");
            {
                let conn = fx.conn();
                let set2_id =
                    seed_attested_set(&conn, fx.tmp.path(), &collision_name, "uuid-m3-collision");
                crate::db::collab::set_frames_set_attestation(&conn, set2_id, true).unwrap();
                drop(conn);
                link_frame_set(&fx.ctx, PID, set2_id).unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            assert_eq!(res.held_back.len(), 1, "{:?}", res.held_back);
            assert!(
                res.held_back[0].reasons.iter().any(|r| r.contains(&format!(
                    "a frame named {collision_name:?} is already published by you"
                ))),
                "{:?}",
                res.held_back
            );
            let bodies = announce_bodies(&fx.server).await;
            assert_eq!(bodies.len(), 1, "only one announce call: {bodies:?}");
            let names: Vec<&str> = bodies[0]["frames"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["fileName"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                vec![collision_name.as_str()],
                "only the generated candidate announced, under its own name"
            );
        }

        /// F8: a generated (non-external) light's `meta.calibration` names
        /// the masters the generation actually used.
        #[tokio::test]
        async fn generated_lights_report_their_masters_in_meta() {
            let fx = fixture(1).await;
            mount_hub(&fx.server, "published").await;
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            let bodies = announce_bodies(&fx.server).await;
            let f = &bodies[0]["frames"].as_array().unwrap()[0];
            assert_eq!(f["meta"]["calibration"]["external"], false);
            assert_eq!(
                f["meta"]["calibration"]["dark"], true,
                "the fixture links a master dark"
            );
        }

        /// Controller ruling 2026-09-28 (Step 4b): the hub now accepts an
        /// empty `filterRaw` — an unmapped-to-nothing light (FILTER NULL,
        /// mapped to the dictionary's `None`) announces with `filterRaw ==
        /// ""` and `filterCanonical == "None"`, and the (fake) hub accepts it.
        #[tokio::test]
        async fn a_null_filter_light_announces_with_empty_filter_raw_and_none_canonical() {
            let fx = fixture(1).await;
            let hub = crate::collab::fake_hub::FakeHub::start().await;
            let me = crate::api::account::own_device_id(&fx.ctx).unwrap();
            hub.add_account("tok", "acc-Me Myself", "Me Myself", &me, None);
            hub.add_project(
                PID,
                "m31",
                &[("acc-Me Myself", "send_receive", false)],
                false,
            );
            wire_hub(&fx.ctx, &hub.uri());
            {
                let conn = fx.conn();
                sign_in_as(&conn, "a@x.io");
                crate::db::collab::set_dictionary(
                    &conn,
                    PID,
                    Some(2),
                    Some(
                        r#"[{"canonical":"L","aliases":["Lum"],"kind":"broadband"},
                            {"canonical":"None","aliases":["none"],"kind":"unfiltered"}]"#,
                    ),
                )
                .unwrap();
                conn.execute(
                    "UPDATE frames SET filter = NULL WHERE id = ?1",
                    [fx.frame_ids[0]],
                )
                .unwrap();
                crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "", "None")
                    .unwrap();
            }
            let res = publish_collab_frames(&fx.ctx, PID, None).await.unwrap();
            assert_eq!(res.announced, 1, "{:?}", res.held_back);
            let f = hub.frame(PID, &fx.uuids[0]).expect("announced");
            assert_eq!(f.filter_raw, "");
            assert_eq!(f.filter_canonical, "None");
        }
    }
}
