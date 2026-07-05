//! Shared `lights` command-layer handlers (B5 Task 4): light-calibration
//! readiness for a frame set — single business-logic source for the Tauri
//! (`commands/lights.rs`) and web (`routes/lights.rs`) wrappers, mirroring
//! `api/masters.rs`. See `docs/superpowers/specs/2026-07-05-light-calibration-design.md`
//! §5 (derived status) and §8 (readiness dialog).
//!
//! `get_light_calibration_readiness` answers, for every LIGHT frame of a
//! frame set: what Dark/Flat/Bias calibration is available and whether the
//! frame's existing calibrated output (if any) is still current. It drives
//! the Calibrate-Lights dialog summary and the per-frame status badge.
//!
//! Two independent axes are reported per frame:
//! - **link readiness** (`dark`/`flat`/`bias` = `master` | `rawSet` |
//!   `missing`): can we calibrate now, must masters be built first, or is a
//!   calibration type simply not linked?
//! - **output status** (`status`, via `db::light_calibrations::derive_status`):
//!   is the already-written calibrated file fresh, partial, or stale?
//!
//! The core logic lives in `compute_readiness(&Connection, …)` so it is
//! unit-testable against a seeded in-memory connection (the `api/calibration.rs`
//! inner-fn precedent); the public handler is a thin `ctx` → `conn` wrapper.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::masters::{library_dir_or_err, start_master_builds_batch, MasterRecipe};
use crate::api::{db, ApiError};
use crate::calibration_library::light_cal::{
    calibrate_light, flat_norm_constant, LightCalInputs, OUTPUT_SCALE_DIVISOR,
};
use crate::calibration_library::light_headers::{build_light_cal_cards, LightCalCardInputs};
use crate::calibration_library::paths::{calibrated_light_relative_path, resolve_collision};
use crate::db::calibration_links::get_links_for_frame;
use crate::db::light_calibrations::{
    derive_status, upsert_light_calibration, LightCalRow, LightCalStatus, LIGHT_CAL_ENGINE_VERSION,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::{Card, CardValue};
use crate::integration::IntegrationError;
use crate::models::{CalibrationLink, FileFormat};
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{LightCalHandle, ServiceContext};

// ── DTOs (single-sourced; both wrapper crates import these) ─────────────────

/// Per-frame readiness row for the Calibrate-Lights dialog + frame-table badge.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightFrameReadiness {
    pub frame_id: i64,
    pub filename: String,
    /// `db::light_calibrations::derive_status` mapped to the frontend's
    /// verbatim strings: `"notCalibrated"` | `"calibrated"` | `"partial"` |
    /// `"stale"`.
    pub status: String,
    /// Dark-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub dark: String,
    /// Flat-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub flat: String,
    /// Bias-link classification: `"master"` | `"rawSet"` | `"missing"`.
    pub bias: String,
    /// Distinct raw (non-master, non-superseded) calibration-set ids this
    /// frame links to — the sets a preflight would have to build masters for.
    pub raw_set_ids: Vec<i64>,
}

/// Frame-set-level readiness summary for the Calibrate-Lights dialog.
///
/// `ready_count` + `raw_set_count` + `missing_count` partition `frames`:
/// - `raw_set_count`: frames with at least one raw-set link (masters get
///   built automatically first).
/// - `missing_count`: of the rest, frames missing a Dark or Flat link (Bias
///   is optional for lights under the raw-master-dark convention — a missing
///   Bias never blocks readiness, though it is still reported in `bias`).
/// - `ready_count`: the remainder — Dark and Flat both present as masters,
///   ready to calibrate now.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightCalReadiness {
    pub frames: Vec<LightFrameReadiness>,
    pub ready_count: i64,
    pub raw_set_count: i64,
    pub missing_count: i64,
    /// Distinct raw calibration-set ids across all frames that a preflight
    /// must build masters for, in first-seen order. `raw_set_ids_to_build.len()`
    /// is the number of master builds; `raw_set_count` is the number of
    /// affected frames (a single raw set can serve many frames).
    pub raw_set_ids_to_build: Vec<i64>,
}

// ── Classification ──────────────────────────────────────────────────────────

const MASTER: &str = "master";
const RAW_SET: &str = "rawSet";
const MISSING: &str = "missing";

/// Classify one calibration-type link for a light frame.
///
/// Returns the wire classification string plus, when the link points at a raw
/// non-superseded set, that set's id (the caller collects these into the
/// build list). Rules (Task 4 brief):
/// - no link of this type → `missing`.
/// - link → a master-library set (`is_master_library = 1`) → `master`.
/// - link → a raw set already superseded (`superseded_by_set_id IS NOT NULL`)
///   → resolves to its master, counts as `master`, nothing to build.
/// - link → a raw, non-superseded set → `rawSet`, id returned for the build
///   list.
///
/// A link that targets a set id with no `calibration_set` row (dangling FK —
/// should not happen, `no action` FK) is treated as `missing` and logged.
fn classify(
    conn: &Connection,
    links: &[CalibrationLink],
    cal_type: &str,
) -> Result<(&'static str, Option<i64>), ApiError> {
    let set_id = match links.iter().find(|l| l.calibration_type == cal_type) {
        Some(l) => l.calibration_set_id,
        None => return Ok((MISSING, None)),
    };

    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT is_master_library, superseded_by_set_id FROM calibration_set WHERE id = ?1",
            params![set_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;

    match row {
        None => {
            tracing::warn!(set_id, cal_type, "calibration link targets a missing set");
            Ok((MISSING, None))
        }
        Some((is_master, superseded_by)) => {
            if is_master == 1 || superseded_by.is_some() {
                Ok((MASTER, None))
            } else {
                Ok((RAW_SET, Some(set_id)))
            }
        }
    }
}

/// Map the derived status enum to the frontend's verbatim camelCase strings.
fn status_str(s: LightCalStatus) -> &'static str {
    match s {
        LightCalStatus::NotCalibrated => "notCalibrated",
        LightCalStatus::Calibrated => "calibrated",
        LightCalStatus::Partial => "partial",
        LightCalStatus::Stale => "stale",
    }
}

// ── Handler ─────────────────────────────────────────────────────────────────

/// LIGHT members (frame_id, filename) of a frame set, mirroring the
/// membership join used elsewhere in the calibration layer
/// (`db::calibration_links::get_calibration_groups_for_frame_set`), plus a
/// `files` join for the filename.
fn load_light_members(conn: &Connection, set_id: i64) -> Result<Vec<(i64, String)>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id, fi.filename
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY sm.frame_id",
    )?;
    let rows = stmt
        .query_map(params![set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

/// Compute readiness for every LIGHT frame of `set_id`. Pure DB work — no
/// pixel I/O — so both transports' wrappers can run it inside `spawn_blocking`.
fn compute_readiness(
    conn: &Connection,
    set_id: i64,
    flat_norm: bool,
) -> Result<LightCalReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;

    let mut frames = Vec::with_capacity(members.len());
    let mut ready_count = 0i64;
    let mut raw_set_count = 0i64;
    let mut missing_count = 0i64;
    let mut raw_set_ids_to_build: Vec<i64> = Vec::new();

    for (frame_id, filename) in members {
        let links = get_links_for_frame(conn, frame_id)?;

        let (dark, dark_raw) = classify(conn, &links, "Dark")?;
        let (flat, flat_raw) = classify(conn, &links, "Flat")?;
        let (bias, bias_raw) = classify(conn, &links, "Bias")?;

        let mut raw_set_ids: Vec<i64> = Vec::new();
        for r in [dark_raw, flat_raw, bias_raw].into_iter().flatten() {
            if !raw_set_ids.contains(&r) {
                raw_set_ids.push(r);
            }
            if !raw_set_ids_to_build.contains(&r) {
                raw_set_ids_to_build.push(r);
            }
        }

        // Partition into ready / raw / missing. Raw sets take precedence: a
        // preflight builds their masters first, after which the frame is
        // re-evaluated. Among frames with no raw links, a missing Dark or
        // Flat makes the frame "missing"; Bias is optional for lights
        // (raw-master-dark convention) so its absence never blocks readiness.
        if !raw_set_ids.is_empty() {
            raw_set_count += 1;
        } else if dark == MISSING || flat == MISSING {
            missing_count += 1;
        } else {
            ready_count += 1;
        }

        let status = status_str(derive_status(conn, frame_id, &links, flat_norm)?);

        frames.push(LightFrameReadiness {
            frame_id,
            filename,
            status: status.to_string(),
            dark: dark.to_string(),
            flat: flat.to_string(),
            bias: bias.to_string(),
            raw_set_ids,
        });
    }

    tracing::debug!(
        set_id,
        total = frames.len() as i64,
        ready_count,
        raw_set_count,
        missing_count,
        to_build = raw_set_ids_to_build.len() as i64,
        "light calibration readiness computed"
    );

    Ok(LightCalReadiness {
        frames,
        ready_count,
        raw_set_count,
        missing_count,
        raw_set_ids_to_build,
    })
}

/// Readiness summary + per-frame status for the frame set's LIGHT members.
/// `flat_norm` is the dialog's "Normalize master flat" toggle — it feeds
/// `derive_status`'s flat-normalization staleness check (a frame calibrated
/// with a different normalization choice than the user now wants is stale).
pub fn get_light_calibration_readiness(
    ctx: &ServiceContext,
    set_id: i64,
    flat_norm: bool,
) -> Result<LightCalReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_readiness(&conn, set_id, flat_norm)
}

// ── Orchestration DTOs (B5 Task 5) ──────────────────────────────────────────

/// Which of a frame set's LIGHT frames a run touches. `only_stale = false`
/// calibrates every light; `true` skips frames whose derived status is already
/// [`LightCalStatus::Calibrated`] (spec §6 scope).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightCalScope {
    pub only_stale: bool,
}

// ── Per-frame resolution (pure, connection-level, unit-tested) ───────────────

/// One resolved master link for a light frame: the master calibration set, the
/// on-disk file, and the frame uuid stamped into the `ATH_C*` provenance card.
#[derive(Debug, Clone, PartialEq)]
struct ResolvedMaster {
    /// The MASTER calibration_set id (post-supersede the link points here); goes
    /// into the tracking row's `dark_set_id`/`flat_set_id`/`bias_set_id`.
    set_id: i64,
    /// Master frame uuid → `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` value.
    uuid: String,
    /// Master file path (the engine subtrahend/divisor input).
    path: String,
}

/// Everything the engine + header builder + tracking row need for ONE light
/// frame, resolved against the current catalog in a single pooled connection.
/// No pixel I/O — so it is unit-testable against a seeded in-memory conn.
struct ResolvedFrameInputs {
    frame_id: i64,
    light_path: PathBuf,
    source_filename: String,
    source_uuid: Option<String>,
    /// OBJECT / INSTRUME / DATE-OBS date → the `<object>/<cam>/<date>/` output
    /// folder (spec §3). Empty strings fall back to `Unknown*` in the path.
    object: String,
    instrume: String,
    date_obs_date: String,
    /// Still-valid header cards copied from the source (WCS/optics/session);
    /// [`build_light_cal_cards`] filters these to its whitelist.
    source_cards: Vec<Card>,
    dark: Option<ResolvedMaster>,
    flat: Option<ResolvedMaster>,
    bias: Option<ResolvedMaster>,
}

/// Current calibration link of `cal_type` for a frame, if any.
fn link_set_id(links: &[CalibrationLink], cal_type: &str) -> Option<i64> {
    links
        .iter()
        .find(|l| l.calibration_type == cal_type)
        .map(|l| l.calibration_set_id)
}

/// Resolve a calibration set to its single master member file — `Some` only
/// when the set is a built master (`is_master_library = 1`). A raw, unbuilt set
/// (or one with no member file) yields `None`, which the caller treats as
/// "skip this term" per the best-effort policy.
fn resolve_master(conn: &Connection, set_id: i64) -> Result<Option<ResolvedMaster>, ApiError> {
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT fr.uuid, fi.path
             FROM calibration_set cs
             JOIN calibration_set_frames csf ON csf.set_id = cs.id
             JOIN frames fr ON fr.id = csf.frame_id
             JOIN files fi ON fi.id = fr.file_id
             WHERE cs.id = ?1 AND cs.is_master_library = 1
             LIMIT 1",
            params![set_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(uuid, path)| ResolvedMaster {
        set_id,
        uuid: uuid.unwrap_or_default(),
        path,
    }))
}

/// Resolve one calibration type's link to a built master, warning (best-effort
/// policy, spec §6) when the link points at a raw set the preflight did not or
/// could not build.
fn resolve_type(
    conn: &Connection,
    links: &[CalibrationLink],
    frame_id: i64,
    cal_type: &str,
) -> Result<Option<ResolvedMaster>, ApiError> {
    match link_set_id(links, cal_type) {
        Some(set_id) => {
            let resolved = resolve_master(conn, set_id)?;
            if resolved.is_none() {
                tracing::warn!(
                    frame_id,
                    set_id,
                    cal_type,
                    "linked calibration set is not a built master — skipping this term (best-effort)"
                );
            }
            Ok(resolved)
        }
        None => Ok(None),
    }
}

/// Typed FITS card from a `KEYWORD -> value-string` pair. Numeric strings are
/// preserved as `Integer`/`Real` so copied-through WCS/optics cards keep their
/// type; everything else becomes a string. A keyword `fits_writer` rejects
/// (>8 chars, reserved) is dropped rather than erroring the whole frame.
fn card_from_kv(keyword: &str, value: &str) -> Option<Card> {
    let cv = if let Ok(i) = value.parse::<i64>() {
        CardValue::Integer(i)
    } else if let Ok(f) = value.parse::<f64>() {
        CardValue::Real(f)
    } else {
        CardValue::Str(value.to_string())
    };
    Card::new(keyword, cv).ok()
}

/// Rebuild the source frame's header cards from the scanner-stored
/// `fits_header` blob (format-aware, so an XISF source works too). Pure DB — no
/// disk re-read of the (possibly huge) light file. Missing blob → no cards.
fn source_cards_for_file(
    conn: &Connection,
    file_id: i64,
    format: FileFormat,
) -> Result<Vec<Card>, ApiError> {
    let header_text: Option<String> = conn
        .query_row(
            "SELECT header FROM fits_header WHERE file_id = ?1 LIMIT 1",
            params![file_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(header_text) = header_text else {
        return Ok(Vec::new());
    };
    let keys = parse_stored_header_keys(format, &header_text);
    Ok(keys
        .iter()
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect())
}

/// YYYY-MM-DD from a DATE-OBS string (`2026-07-05T20:30:00Z` → `2026-07-05`).
/// Missing/empty → `"UnknownDate"` so the output layout never gets an empty
/// path segment.
fn date_part(date_obs: Option<&str>) -> String {
    date_obs
        .and_then(|d| d.split('T').next())
        .map(|d| d.chars().take(10).collect::<String>())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "UnknownDate".to_string())
}

fn resolve_frame_inputs(
    conn: &Connection,
    frame_id: i64,
    _flat_norm: bool,
) -> Result<ResolvedFrameInputs, ApiError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        String,
        String,
        String,
    )> = conn
        .query_row(
            "SELECT fr.uuid, fr.object, fr.instrume, fr.date_obs, fi.id, fi.path, fi.filename, fi.format
             FROM frames fr JOIN files fi ON fi.id = fr.file_id
             WHERE fr.id = ?1",
            params![frame_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((uuid, object, instrume, date_obs, file_id, path, filename, format_str)) = row else {
        return Err(ApiError::NotFound(format!("light frame {frame_id} not found")));
    };

    let format = if format_str.eq_ignore_ascii_case("XISF") {
        FileFormat::XISF
    } else {
        FileFormat::FITS
    };
    let source_cards = source_cards_for_file(conn, file_id, format)?;

    let links = get_links_for_frame(conn, frame_id)?;
    let dark = resolve_type(conn, &links, frame_id, "Dark")?;
    let flat = resolve_type(conn, &links, frame_id, "Flat")?;
    let bias = resolve_type(conn, &links, frame_id, "Bias")?;

    Ok(ResolvedFrameInputs {
        frame_id,
        light_path: PathBuf::from(path),
        source_filename: filename,
        source_uuid: uuid,
        object: object.unwrap_or_default(),
        instrume: instrume.unwrap_or_default(),
        date_obs_date: date_part(date_obs.as_deref()),
        source_cards,
        dark,
        flat,
        bias,
    })
}

/// The `.fits`-normalized original filename to feed
/// [`calibrated_light_relative_path`] (which prepends `c_`): strips the source
/// extension and forces `.fits` so an XISF source yields a FITS output
/// (spec §3: `c_<original>.fits`).
fn output_basename_fits(source_filename: &str) -> String {
    let stem = Path::new(source_filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(source_filename);
    format!("{stem}.fits")
}

/// CALSTAT computed from which masters actually apply — must match the engine's
/// own construction ([`calibrate_light`]): dark subtraction removes bias+dark
/// (`"BD"`), else a bias subtraction (`"B"`), plus `"F"` when a flat divides.
fn compute_calstat(dark: bool, bias: bool, flat: bool) -> String {
    let base = if dark {
        "BD"
    } else if bias {
        "B"
    } else {
        ""
    };
    if flat {
        format!("{base}F")
    } else {
        base.to_string()
    }
}

// ── Per-frame execution ──────────────────────────────────────────────────────

/// Internal per-frame error: cancellation (aborts the whole batch) vs. a
/// per-frame failure (collected, batch continues) — mirrors `masters.rs`'s
/// `BuildStepError`.
enum CalError {
    Cancelled,
    Failed(String),
}

impl From<ApiError> for CalError {
    fn from(e: ApiError) -> Self {
        CalError::Failed(e.to_string())
    }
}
impl From<rusqlite::Error> for CalError {
    fn from(e: rusqlite::Error) -> Self {
        CalError::Failed(e.to_string())
    }
}
impl From<anyhow::Error> for CalError {
    fn from(e: anyhow::Error) -> Self {
        CalError::Failed(format!("{e:#}"))
    }
}
impl From<std::io::Error> for CalError {
    fn from(e: std::io::Error) -> Self {
        CalError::Failed(e.to_string())
    }
}
impl From<IntegrationError> for CalError {
    fn from(e: IntegrationError) -> Self {
        match e {
            IntegrationError::Cancelled => CalError::Cancelled,
            other => CalError::Failed(other.to_string()),
        }
    }
}

enum FrameOutcome {
    Done,
    Skipped,
    Failed(String),
    Cancelled,
}

/// Resolve → build cards → run engine → UPSERT tracking row for ONE frame.
/// Returns `Ok(true)` when skipped by the `only_stale` scope, `Ok(false)` on a
/// completed calibration. All DB/pixel-I/O errors funnel through [`CalError`].
#[allow(clippy::too_many_arguments)]
fn calibrate_one_inner(
    db: &crate::db::Database,
    frame_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    library_dir: &Path,
    scratch: &Path,
    cancel: &AtomicBool,
) -> Result<bool, CalError> {
    let resolved = {
        let conn = db.conn();
        if scope.only_stale {
            let links = get_links_for_frame(&conn, frame_id)?;
            if derive_status(&conn, frame_id, &links, flat_norm)? == LightCalStatus::Calibrated {
                return Ok(true);
            }
        }
        resolve_frame_inputs(&conn, frame_id, flat_norm)?
    };

    // Best-effort policy: never write a meaningless raw-scale copy. If nothing
    // resolved to a master, this frame is a per-frame failure, not a batch abort.
    if resolved.dark.is_none() && resolved.flat.is_none() && resolved.bias.is_none() {
        return Err(CalError::Failed(
            "no calibration masters available (dark/flat/bias unbuilt or unlinked)".into(),
        ));
    }

    // What actually applies (spec §2): a dark subtraction removes bias+dark, so
    // a linked bias is NOT separately subtracted when a dark is present.
    let dark_applied = resolved.dark.is_some();
    let bias_applied = resolved.dark.is_none() && resolved.bias.is_some();
    let flat_applied = resolved.flat.is_some();
    let calstat = compute_calstat(dark_applied, bias_applied, flat_applied);

    // Flat-normalization divisor for the ATH_CFNM card, resolved BEFORE the
    // engine (which recomputes the identical value) so the card list is complete
    // at write time. 1.0 when normalization is off or no flat applies.
    let flat_norm_divisor = match (&resolved.flat, flat_norm) {
        (Some(m), true) => flat_norm_constant(Path::new(&m.path), scratch)?,
        _ => 1.0,
    };

    let card_inputs = LightCalCardInputs {
        source_uuid: resolved.source_uuid.clone().unwrap_or_default(),
        source_filename: resolved.source_filename.clone(),
        calstat: calstat.clone(),
        dark: if dark_applied {
            resolved.dark.as_ref().map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        flat: if flat_applied {
            resolved.flat.as_ref().map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        bias: if bias_applied {
            resolved.bias.as_ref().map(|m| (m.uuid.clone(), m.path.clone()))
        } else {
            None
        },
        scale_divisor: OUTPUT_SCALE_DIVISOR,
        flat_norm_divisor,
    };
    let cards = build_light_cal_cards(&resolved.source_cards, &card_inputs)?;

    let rel = calibrated_light_relative_path(
        &resolved.object,
        &resolved.instrume,
        &resolved.date_obs_date,
        &output_basename_fits(&resolved.source_filename),
    );
    let output_abs = resolve_collision(&library_dir.join(&rel));
    if let Some(parent) = output_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // The engine picks the subtrahend itself (dark over bias); passing both is
    // harmless — bias_path is ignored when dark_path is present.
    let inputs = LightCalInputs {
        light_path: resolved.light_path.clone(),
        dark_path: resolved.dark.as_ref().map(|m| PathBuf::from(&m.path)),
        bias_path: resolved.bias.as_ref().map(|m| PathBuf::from(&m.path)),
        flat_path: resolved.flat.as_ref().map(|m| PathBuf::from(&m.path)),
        flat_norm,
        output_path: output_abs.clone(),
        cards,
        scratch_dir: scratch.to_path_buf(),
    };
    let outcome = calibrate_light(&inputs, cancel)?;

    // Record every resolved master link (even a bias covered by a dark) so the
    // frame's derived status stays Calibrated against its current links; the
    // authoritative "what was subtracted" lives in `calstat`.
    let row = LightCalRow {
        id: 0,
        frame_id: Some(resolved.frame_id),
        source_uuid: resolved.source_uuid.clone(),
        source_filename: Some(resolved.source_filename.clone()),
        output_path: output_abs.to_string_lossy().to_string(),
        dark_set_id: resolved.dark.as_ref().map(|m| m.set_id),
        flat_set_id: resolved.flat.as_ref().map(|m| m.set_id),
        bias_set_id: resolved.bias.as_ref().map(|m| m.set_id),
        calstat: outcome.calstat.clone(),
        flat_norm_applied: flat_applied && flat_norm,
        output_hash: outcome.output_hash.clone(),
        engine_version: LIGHT_CAL_ENGINE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    {
        let conn = db.conn();
        upsert_light_calibration(&conn, &row)?;
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn calibrate_one(
    db: &crate::db::Database,
    frame_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    library_dir: &Path,
    scratch: &Path,
    cancel: &AtomicBool,
) -> FrameOutcome {
    match calibrate_one_inner(db, frame_id, scope, flat_norm, library_dir, scratch, cancel) {
        Ok(true) => FrameOutcome::Skipped,
        Ok(false) => FrameOutcome::Done,
        Err(CalError::Cancelled) => FrameOutcome::Cancelled,
        Err(CalError::Failed(reason)) => FrameOutcome::Failed(reason),
    }
}

// ── Progress / completion event payloads (snake_case, masters precedent) ─────

#[derive(Clone, serde::Serialize)]
struct CalibrationProgressEvent {
    set_id: i64,
    frame_id: i64,
    index: usize,
    total: usize,
    filename: String,
}

#[derive(Clone, serde::Serialize)]
struct CalibrationFailure {
    frame_id: i64,
    reason: String,
}

#[derive(Clone, serde::Serialize)]
struct CalibrationFinishedEvent {
    set_id: i64,
    outcome: &'static str,
    ok_count: usize,
    failed: Vec<CalibrationFailure>,
}

/// Frame-set display name for the compute-queue label; falls back to the id.
fn frame_set_label(conn: &Connection, set_id: i64) -> String {
    conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        params![set_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| format!("set {set_id}"))
}

/// The batch body: queue admission, then per-frame calibration. Returns
/// `Ok(true)` if cancelled, `Ok(false)` if it ran to completion; `ok_count` and
/// `failed` accumulate in place so the caller can emit the finished event even
/// on an early return. A per-frame failure is collected, never fatal.
#[allow(clippy::too_many_arguments)]
fn run_light_cal(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    cancel_flag: &Arc<AtomicBool>,
    job_id_slot: &AtomicI64,
    ok_count: &mut usize,
    failed: &mut Vec<CalibrationFailure>,
) -> Result<bool, ApiError> {
    let db = db(ctx)?;

    let (members, label) = {
        let conn = db.conn();
        let members = load_light_members(&conn, set_id)?;
        let label = format!("Calibrate lights — {}", frame_set_label(&conn, set_id));
        (members, label)
    };

    // Admission (spec §6): any preflighted master builds were enqueued ahead of
    // us, so at max_concurrent=1 they finish first. Cancelled-while-queued →
    // treat the whole batch as cancelled.
    let (_permit, job_id) = match ctx.compute_queue.acquire(
        ComputeJobKind::LightCalibration,
        &label,
        cancel_flag.clone(),
    ) {
        Ok(v) => v,
        Err(_cancelled) => return Ok(true),
    };
    job_id_slot.store(job_id, Ordering::SeqCst);

    let library_dir = {
        let conn = db.conn();
        library_dir_or_err(&conn)?
    };
    let scratch = std::env::temp_dir();
    let total = members.len();

    tracing::info!(set_id, total, only_stale = scope.only_stale, flat_norm, "light calibration batch started");

    for (index, (frame_id, filename)) in members.iter().enumerate() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Ok(true);
        }
        match calibrate_one(
            db,
            *frame_id,
            scope,
            flat_norm,
            &library_dir,
            &scratch,
            cancel_flag.as_ref(),
        ) {
            FrameOutcome::Done => {
                *ok_count += 1;
                emit_event(
                    emitter,
                    "calibration-progress",
                    &CalibrationProgressEvent {
                        set_id,
                        frame_id: *frame_id,
                        index,
                        total,
                        filename: filename.clone(),
                    },
                );
            }
            FrameOutcome::Skipped => {
                tracing::debug!(set_id, frame_id, "light already calibrated — skipped (only_stale)");
            }
            FrameOutcome::Failed(reason) => {
                tracing::warn!(set_id, frame_id, %reason, "light frame calibration failed — continuing batch");
                failed.push(CalibrationFailure {
                    frame_id: *frame_id,
                    reason,
                });
            }
            FrameOutcome::Cancelled => return Ok(true),
        }
    }
    Ok(false)
}

/// Runs on the dedicated `light-cal-{set_id}` thread. Single exit path: handle
/// removal + `calibration-finished` ALWAYS fire here, exactly once, regardless
/// of how the body ended — success, partial, cancel, batch error, or panic
/// (caught so the finished event still fires; mirrors `run_master_build_thread`'s
/// finally discipline, hardened against a mid-frame panic).
fn run_light_cal_thread(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    cancel_flag: Arc<AtomicBool>,
    job_id_slot: Arc<AtomicI64>,
) {
    let mut ok_count = 0usize;
    let mut failed: Vec<CalibrationFailure> = Vec::new();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_light_cal(
            &ctx,
            emitter.as_ref(),
            set_id,
            scope,
            flat_norm,
            &cancel_flag,
            &job_id_slot,
            &mut ok_count,
            &mut failed,
        )
    }));

    let outcome: &'static str = match result {
        Ok(Ok(true)) => "cancelled",
        Ok(Ok(false)) => {
            if ok_count == 0 && !failed.is_empty() {
                "error"
            } else if !failed.is_empty() {
                "partial"
            } else {
                "success"
            }
        }
        Ok(Err(e)) => {
            tracing::error!(set_id, error = %e, "light calibration batch aborted before completing");
            "error"
        }
        Err(_panic) => {
            tracing::error!(set_id, "light calibration thread panicked");
            "error"
        }
    };

    ctx.active_light_cal.lock().unwrap().remove(&set_id);
    tracing::info!(set_id, outcome, ok_count, failed = failed.len(), "light calibration batch finished");
    emit_event(
        emitter.as_ref(),
        "calibration-finished",
        &CalibrationFinishedEvent {
            set_id,
            outcome,
            ok_count,
            failed,
        },
    );
}

// ── Public start / cancel API ────────────────────────────────────────────────

/// Preflight (build masters for any raw links), register the cancel handle
/// (reject a concurrent run for the same frame set), then spawn the detached
/// `light-cal-<set_id>` worker (queue admission happens INSIDE the thread) and
/// return immediately. Mirrors `start_master_build`'s validate-then-hand-off
/// shape.
pub fn start_light_calibration(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
) -> Result<(), ApiError> {
    // Preflight: build masters for every raw calibration set the lights link.
    // Non-fatal — a skipped/failed build just means those lights calibrate
    // best-effort at run time (§6). Never aborts the light run.
    let readiness = get_light_calibration_readiness(&ctx, set_id, flat_norm)?;
    if !readiness.raw_set_ids_to_build.is_empty() {
        let recipe = MasterRecipe {
            combine: None,
            synthetic_bias: None,
            archive_after: false,
        };
        match start_master_builds_batch(
            ctx.clone(),
            emitter.clone(),
            app_version,
            readiness.raw_set_ids_to_build.clone(),
            recipe,
        ) {
            Ok(report) => {
                for skip in &report.skipped {
                    tracing::warn!(
                        set_id,
                        skipped_set = skip.set_id,
                        reason = %skip.reason,
                        "preflight master build skipped — lights calibrate best-effort"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    set_id, error = %e,
                    "preflight master builds failed to start — continuing best-effort"
                );
            }
        }
    }

    // Register the cancel handle, rejecting a concurrent run for the same set
    // (mirrors start_master_build's duplicate-run guard).
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let job_id_slot = Arc::new(AtomicI64::new(0));
    {
        let mut active = ctx.active_light_cal.lock().unwrap();
        if active.contains_key(&set_id) {
            return Err(ApiError::Conflict(format!(
                "a light calibration is already in progress for frame set {set_id}"
            )));
        }
        active.insert(
            set_id,
            LightCalHandle {
                cancel_flag: cancel_flag.clone(),
                job_id: job_id_slot.clone(),
            },
        );
    }

    let thread_ctx = ctx.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("light-cal-{set_id}"))
        .spawn(move || {
            run_light_cal_thread(
                thread_ctx,
                emitter,
                set_id,
                scope,
                flat_norm,
                cancel_flag,
                job_id_slot,
            );
        });

    if let Err(e) = spawn_result {
        // The thread never started — nothing will remove the handle or emit
        // calibration-finished, so clean up right here.
        ctx.active_light_cal.lock().unwrap().remove(&set_id);
        return Err(ApiError::Internal(format!(
            "failed to spawn light-cal thread: {e}"
        )));
    }

    Ok(())
}

/// Cancel an active light-calibration batch (queued-in-compute-queue or
/// running). Sets the cancel flag — sufficient on its own, since both the
/// queue's `acquire` loop and the per-frame loop poll it — and, when a queue
/// ticket is already known, calls `ComputeQueue::cancel` to drop it promptly.
pub fn cancel_light_calibration(ctx: &ServiceContext, set_id: i64) -> Result<(), ApiError> {
    let active = ctx.active_light_cal.lock().unwrap();
    if let Some(handle) = active.get(&set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        let job_id = handle.job_id.load(Ordering::SeqCst);
        if job_id > 0 {
            ctx.compute_queue.cancel(job_id);
        }
        Ok(())
    } else {
        Err(ApiError::NotFound(format!(
            "no active light calibration for frame set {set_id}"
        )))
    }
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;
    use crate::cache::MemoryImageCache;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use crate::services::compute_queue::ComputeQueue;
    use crate::services::operation_queue::OperationQueue;
    use crate::settings::SettingsManager;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, RwLock};

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("Obj {fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64) {
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/test/light_{frame_id}.fits"),
                format!("light_{frame_id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs)
             VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z')",
            params![frame_id, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
    }

    /// A MASTER calibration set (`is_master_library = 1`) with exactly one
    /// member frame + file, so [`resolve_frame_inputs`] can pull its path/uuid.
    /// Returns the master set id.
    fn seed_master_set(conn: &Connection, set_id: i64, imagetyp: &str) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 1)",
            params![set_id, imagetyp],
        )
        .unwrap();
        let file_id = set_id + 3_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/lib/master_{set_id}.fits"),
                format!("master_{set_id}.fits")
            ],
        )
        .unwrap();
        let frame_id = set_id + 4_000_000;
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?2, ?3, 1)",
            params![frame_id, file_id, imagetyp],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();
        set_id
    }

    /// A raw (non-master, unbuilt) calibration set — no member frame needed.
    fn seed_raw_set(conn: &Connection, set_id: i64, imagetyp: &str) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 0)",
            params![set_id, imagetyp],
        )
        .unwrap();
        set_id
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    fn master_frame_uuid(conn: &Connection, master_set_id: i64) -> String {
        conn.query_row(
            "SELECT fr.uuid FROM calibration_set_frames csf
             JOIN frames fr ON fr.id = csf.frame_id
             WHERE csf.set_id = ?1",
            params![master_set_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap()
        .unwrap()
    }

    fn test_ctx(db: crate::db::Database) -> Arc<ServiceContext> {
        let cell = OnceLock::new();
        let _ = cell.set(db);
        Arc::new(ServiceContext {
            db: cell,
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
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
        })
    }

    #[test]
    fn per_frame_resolution_prefers_master_and_skips_unbuilt_raw() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);

        // Dark + Bias link BUILT masters; Flat links a raw, unbuilt set.
        let dark = seed_master_set(&conn, 100, "MasterDark");
        let bias = seed_master_set(&conn, 102, "MasterBias");
        let raw_flat = seed_raw_set(&conn, 200, "Flat");
        add_link(&conn, 1, dark, "Dark");
        add_link(&conn, 1, bias, "Bias");
        add_link(&conn, 1, raw_flat, "Flat");

        let r = resolve_frame_inputs(&conn, 1, true).unwrap();

        // Master links resolve to their file path + frame uuid.
        let rd = r.dark.expect("dark master resolved");
        assert_eq!(rd.set_id, dark);
        assert_eq!(rd.path, "/lib/master_100.fits");
        assert_eq!(rd.uuid, master_frame_uuid(&conn, dark));

        let rb = r.bias.expect("bias master resolved");
        assert_eq!(rb.set_id, bias);
        assert_eq!(rb.path, "/lib/master_102.fits");

        // The raw, unbuilt flat term is skipped (best-effort policy).
        assert!(r.flat.is_none(), "raw unbuilt flat link must not resolve");

        // Frame identity + path fields are carried through for the output layout.
        assert_eq!(r.frame_id, 1);
        assert_eq!(r.source_filename, "light_1.fits");
        assert_eq!(r.object, "M31");
        assert_eq!(r.instrume, "TestCam");
        assert_eq!(r.date_obs_date, "2026-07-05");
        assert!(r.source_uuid.is_some(), "light frame uuid populated by trigger");
    }

    #[test]
    fn output_basename_forces_fits_extension() {
        assert_eq!(output_basename_fits("L_0001.xisf"), "L_0001.fits");
        assert_eq!(output_basename_fits("L_0001.fits"), "L_0001.fits");
        assert_eq!(output_basename_fits("L_0001"), "L_0001.fits");
        // Full relative path prepends c_ (spec §3: c_<original>.fits) even for
        // an XISF source.
        let rel = calibrated_light_relative_path(
            "M31",
            "Cam",
            "2026-07-05",
            &output_basename_fits("L_0001.xisf"),
        );
        assert!(rel.to_string_lossy().ends_with("c_L_0001.fits"), "{rel:?}");
    }

    #[test]
    fn start_rejects_concurrent_run_for_same_set() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = db.conn();
            seed_frame_set(&conn, 1);
        }
        let ctx = test_ctx(db);

        // Simulate an in-flight run by pre-registering the handle.
        ctx.active_light_cal.lock().unwrap().insert(
            1,
            LightCalHandle {
                cancel_flag: Arc::new(AtomicBool::new(false)),
                job_id: Arc::new(AtomicI64::new(0)),
            },
        );

        let emitter: Arc<dyn ProgressEmitter> = Arc::new(NullEmitter);
        let err = start_light_calibration(
            ctx.clone(),
            emitter,
            "0.0.0".into(),
            1,
            LightCalScope { only_stale: false },
            true,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "expected Conflict, got {err:?}");

        // The pre-existing handle is untouched — the rejected call must not
        // remove or replace it.
        assert!(ctx.active_light_cal.lock().unwrap().contains_key(&1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("fs_{fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// One LIGHT frame (files + frames rows) joined into `session_id`.
    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64) -> String {
        let file_id = frame_id + 1_000_000;
        let filename = format!("light_{frame_id}.fits");
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![file_id, format!("/test/{filename}"), filename],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume) VALUES (?1, ?2, 'Light', 'TestCam')",
            params![frame_id, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
        filename
    }

    fn seed_set(conn: &Connection, id: i64, imagetyp: &str, is_master: bool) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', ?3)",
            params![id, imagetyp, is_master as i64],
        )
        .unwrap();
    }

    fn supersede(conn: &Connection, raw_id: i64, master_id: i64) {
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            params![master_id, raw_id],
        )
        .unwrap();
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    /// Master sets used across the tests: Dark #100, Flat #101, Bias #102.
    fn seed_masters(conn: &Connection) {
        seed_set(conn, 100, "MasterDark", true);
        seed_set(conn, 101, "MasterFlat", true);
        seed_set(conn, 102, "MasterBias", true);
    }

    #[test]
    fn readiness_classifies_master_raw_missing() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_set(&conn, 200, "Dark", false); // raw dark set

        // Light 1 — fully mastered, with a fresh matching tracking row.
        seed_light(&conn, 1, session);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");
        add_link(&conn, 1, 102, "Bias");
        upsert_light_calibration(
            &conn,
            &LightCalRow {
                id: 0,
                frame_id: Some(1),
                source_uuid: None,
                source_filename: None,
                output_path: "/lib/c_light_1.fits".to_string(),
                dark_set_id: Some(100),
                flat_set_id: Some(101),
                bias_set_id: Some(102),
                calstat: "BDF".to_string(),
                flat_norm_applied: false,
                output_hash: "deadbeef".to_string(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-07-05T00:00:00Z".to_string(),
            },
        )
        .unwrap();

        // Light 2 — Dark links a raw set; Flat/Bias mastered.
        seed_light(&conn, 2, session);
        add_link(&conn, 2, 200, "Dark");
        add_link(&conn, 2, 101, "Flat");
        add_link(&conn, 2, 102, "Bias");

        // Light 3 — no Flat link; Dark/Bias mastered.
        seed_light(&conn, 3, session);
        add_link(&conn, 3, 100, "Dark");
        add_link(&conn, 3, 102, "Bias");

        let r = compute_readiness(&conn, 1, false).unwrap();
        assert_eq!(r.frames.len(), 3);

        let f1 = &r.frames[0];
        assert_eq!(f1.frame_id, 1);
        assert_eq!(f1.filename, "light_1.fits");
        assert_eq!((f1.dark.as_str(), f1.flat.as_str(), f1.bias.as_str()), (MASTER, MASTER, MASTER));
        assert!(f1.raw_set_ids.is_empty());
        assert_eq!(f1.status, "calibrated", "fresh tracking row that matches links is calibrated");

        let f2 = &r.frames[1];
        assert_eq!(f2.frame_id, 2);
        assert_eq!((f2.dark.as_str(), f2.flat.as_str(), f2.bias.as_str()), (RAW_SET, MASTER, MASTER));
        assert_eq!(f2.raw_set_ids, vec![200]);
        assert_eq!(f2.status, "notCalibrated", "no tracking row yet");

        let f3 = &r.frames[2];
        assert_eq!(f3.frame_id, 3);
        assert_eq!((f3.dark.as_str(), f3.flat.as_str(), f3.bias.as_str()), (MASTER, MISSING, MASTER));
        assert!(f3.raw_set_ids.is_empty());
    }

    #[test]
    fn readiness_counts_and_build_list() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_masters(&conn);
        seed_set(&conn, 200, "Dark", false); // raw dark set (needs building)
        seed_set(&conn, 300, "Dark", false); // raw dark set, already superseded
        supersede(&conn, 300, 100); // 300 → master 100

        // Light 1 — fully mastered → ready.
        seed_light(&conn, 1, session);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");

        // Light 2 — raw dark 200 → raw bucket, contributes 200 to build list.
        seed_light(&conn, 2, session);
        add_link(&conn, 2, 200, "Dark");
        add_link(&conn, 2, 101, "Flat");

        // Light 3 — same raw dark 200 → raw bucket; 200 must dedupe.
        seed_light(&conn, 3, session);
        add_link(&conn, 3, 200, "Dark");
        add_link(&conn, 3, 101, "Flat");

        // Light 4 — Dark links a superseded raw set (300 → master) → master,
        // NOT added to the build list. No Flat → missing bucket.
        seed_light(&conn, 4, session);
        add_link(&conn, 4, 300, "Dark");

        let r = compute_readiness(&conn, 1, false).unwrap();

        assert_eq!(r.frames.len(), 4);
        assert_eq!(r.ready_count, 1, "only light 1 is fully ready");
        assert_eq!(r.raw_set_count, 2, "lights 2 and 3 link raw sets");
        assert_eq!(r.missing_count, 1, "light 4 is missing its Flat link");
        assert_eq!(
            r.raw_set_ids_to_build,
            vec![200],
            "raw set 200 appears once; superseded 300 resolves to a master and is excluded"
        );

        // Light 4's Dark resolves through the supersede pointer to a master.
        let f4 = r.frames.iter().find(|f| f.frame_id == 4).unwrap();
        assert_eq!(f4.dark, MASTER);
        assert_eq!(f4.flat, MISSING);
        assert!(f4.raw_set_ids.is_empty());
    }
}
