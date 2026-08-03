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
//! Alongside them, at the SET level, `cfa_warnings` carries purely advisory
//! notes about mosaic compatibility between the lights and the masters they
//! link (a mono master flat over an OSC light, a phase shift). It never feeds
//! the counts or gates the run — see the "Advisory CFA compatibility" block.
//!
//! The core logic lives in `compute_readiness(&Connection, …)` so it is
//! unit-testable against a seeded in-memory connection (the `api/calibration.rs`
//! inner-fn precedent); the public handler is a thin `ctx` → `conn` wrapper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::masters::{library_dir_or_err, start_master_builds_batch, MasterRecipe};
use crate::api::{db, ApiError};
use crate::calibration_library::light_cal::{
    calibrate_light, resolve_flat_norm_divisor, scale_divisor_for_bitpix, FlatNormDivisor,
    LightCalInputs,
};
// Re-export the flat-normalization statistic + advanced-parameter types so the
// thin Tauri/Axum wrappers can name them via `api::lights::…` (their canonical
// home is the calibration engine). Also brings them into this module's scope.
pub use crate::calibration_library::light_cal::{BiasFallback, FlatNormMode, LightCalParams};
use crate::calibration_library::light_headers::{build_light_cal_cards, LightCalCardInputs};
use crate::calibration_library::paths::{calibrated_light_relative_path, resolve_collision_free};
use crate::db::calibration_links::get_links_for_frame;
use crate::db::light_calibrations::{
    derive_status, get_light_calibration_for_frame, upsert_light_calibration, LightCalRow,
    LightCalStatus, LIGHT_CAL_ENGINE_VERSION,
};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::models::ExportMode;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_writer::keywords::Bayer;
use crate::fits_writer::{Card, CardValue};
use crate::integration::cfa::CfaGeometry;
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
    /// ADVISORY notes about CFA compatibility between the set's light frames
    /// and the masters they link — a mono master flat against an OSC light, a
    /// mosaic phase shift, a pattern that does not match. Empty = nothing to
    /// say. Purely informational: it never blocks the run, never changes the
    /// counts above, and a failure to compute it leaves the list empty rather
    /// than failing readiness.
    pub cfa_warnings: Vec<String>,
}

/// Export-readiness tallies for the WBPP export dialog's mode selector
/// (spec §12.2). Reported per frame set: `total` = in-scope LIGHT members,
/// `calibrated` = fresh calibrated outputs, `stale` = has an output but its
/// derived status is Stale/Partial, `missing` = no calibrated output at all.
/// The dialog blocks the `calibratedLights` mode (and the export command
/// re-checks) while `stale + missing > 0`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportReadiness {
    pub total: i64,
    pub calibrated: i64,
    pub stale: i64,
    pub missing: i64,
}

/// Per-frame calibration recipe for the Calibration Coverage lights table
/// (spec §12.1). Reported only for LIGHT members that already carry a
/// `light_calibrations` tracking row — the recipe truth comes from that row, not
/// from readiness classification. The three `*_master` names are the on-disk
/// FILENAME of each referenced master set's single member file (`None` when the
/// set id is NULL or the file can't be resolved — a debug log, never an error).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LightCalDetails {
    pub frame_id: i64,
    /// CALSTAT recorded on the calibrated output (`"BDF"`, `"BF"`, `"BD"`, …).
    pub calstat: String,
    /// Master-dark filename, or `None` when unlinked/unresolvable.
    pub dark_master: Option<String>,
    /// Master-flat filename, or `None`.
    pub flat_master: Option<String>,
    /// Master-bias filename, or `None`.
    pub bias_master: Option<String>,
    pub flat_norm_applied: bool,
    /// Normalization statistic wire string (`"centralThird"` |
    /// `"pixinsightTrimmed"`); meaningful only when `flat_norm_applied`.
    pub flat_norm_mode: String,
    /// Canonical JSON of the advanced parameters actually applied.
    pub cal_params: String,
    /// Whether the flat was normalized per CFA channel. `None` = the row
    /// predates per-channel scaling and does not say. Comes from the row's own
    /// column, NOT from `cal_params` — that field records what was requested,
    /// which a mono light satisfies globally.
    pub cfa_scaling_applied: Option<bool>,
    pub engine_version: i64,
    /// RFC3339 timestamp the tracking row was written.
    pub calibrated_at: String,
    pub output_path: String,
    /// `derive_status` against the caller's wanted preferences resolved to Stale
    /// or Partial — the coverage view should flag this row for re-calibration.
    pub stale: bool,
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

// ── Advisory CFA compatibility ───────────────────────────────────────────────
//
// Nothing else validates that the masters a light frame links actually share
// its mosaic layout: a MONO master flat divides an OSC light without a word,
// and a one-pixel phase shift swaps R and B invisibly. This block states that
// comparison for the operator — at readiness time, before any pixel is read.
//
// **Advisory only.** It never blocks, never changes classification, counts,
// the build list, or what the engine does; a failure to compute it is logged
// and dropped, never propagated. The one behavioural CFA guard lives in the
// divisor path (`light_cal::read_ath_channel_norms`), which falls back to
// recomputing from the flat's own pixels; this is the user-visible layer on
// top of it, and both share `CfaGeometry::same_phase` so they cannot drift.

/// What a catalog row's CFA columns declare, for advisory comparison.
///
/// Offsets are folded modulo 2 at construction — the [`CfaGeometry::same_phase`]
/// rule — and an UNDECLARED offset reads as 0, matching what every other CFA
/// reader in the codebase assumes (`resolve_cfa_geometry`,
/// `read_ath_channel_norms`, `measure_flat_channel_norms`). So two declarations
/// that describe the same mosaic phase compare equal however their
/// `XBAYROFF`/`YBAYROFF` happen to be spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclaredCfa {
    /// No `BAYERPAT` — the row declares itself mono.
    Mono,
    /// A pattern [`Bayer::parse`] can vouch for, at its folded phase.
    Cfa(CfaGeometry),
    /// A `BAYERPAT` string the parser does not recognize. Deliberately NOT
    /// folded into `Mono`: "declares nothing" and "declares something we
    /// cannot read" are different facts, and only the second is worth telling
    /// the operator about.
    Unrecognized(String),
}

impl DeclaredCfa {
    fn from_columns(bayerpat: Option<String>, xoff: Option<i64>, yoff: Option<i64>) -> Self {
        let Some(raw) = bayerpat.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return DeclaredCfa::Mono;
        };
        match Bayer::parse(raw) {
            Some(pattern) => DeclaredCfa::Cfa(CfaGeometry {
                pattern,
                xoff: xoff.unwrap_or(0).rem_euclid(2),
                yoff: yoff.unwrap_or(0).rem_euclid(2),
            }),
            None => DeclaredCfa::Unrecognized(raw.to_string()),
        }
    }

    /// Short human label — `RGGB`, `RGGB at (1, 0)`, `mono`, `unrecognized
    /// 'XTRANS'`. Used both in the sentences and as the log field value, so a
    /// log line and the dialog say the same thing.
    fn label(&self) -> String {
        match self {
            DeclaredCfa::Mono => "mono".to_string(),
            DeclaredCfa::Cfa(g) if g.xoff == 0 && g.yoff == 0 => g.pattern.as_str().to_string(),
            DeclaredCfa::Cfa(g) => {
                format!("{} at ({}, {})", g.pattern.as_str(), g.xoff, g.yoff)
            }
            DeclaredCfa::Unrecognized(p) => format!("unrecognized '{p}'"),
        }
    }
}

/// One advisory note. `message` is the operator-facing sentence (the readiness
/// payload carries only these); the rest are log fields for the calibrate-time
/// emission, so the same finding reads identically in the dialog and the log.
struct CfaAdvisory {
    /// `"lights"` | `"dark"` | `"flat"` | `"bias"`.
    kind: &'static str,
    light_pattern: String,
    /// Empty for a `"lights"`-kind note (nothing was compared against).
    master_pattern: String,
    message: String,
}

/// The distinct CFA layouts declared by a frame set's LIGHT members, most
/// common first (ties broken by the pattern string, so the result is stable).
/// One query for the whole set — this runs per readiness call and must not cost
/// a query per frame.
fn light_cfa_declarations(
    conn: &Connection,
    set_id: i64,
) -> Result<Vec<(DeclaredCfa, i64)>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT f.bayerpat, f.xbayroff, f.ybayroff, COUNT(DISTINCT sm.frame_id) AS n
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         GROUP BY f.bayerpat, f.xbayroff, f.ybayroff
         ORDER BY n DESC, f.bayerpat, f.xbayroff, f.ybayroff",
    )?;
    let rows = stmt
        .query_map(params![set_id], |r| {
            Ok((
                DeclaredCfa::from_columns(r.get(0)?, r.get(1)?, r.get(2)?),
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // SQL grouped on the RAW columns; fold the rows that describe the same
    // phase into one (an XBAYROFF of 2 and one of 0, a NULL and a 0) so the
    // consensus and the "more than one layout" check see phases, not spellings.
    let mut folded: Vec<(DeclaredCfa, i64)> = Vec::new();
    for (declared, n) in rows {
        match folded.iter_mut().find(|(d, _)| *d == declared) {
            Some((_, count)) => *count += n,
            None => folded.push((declared, n)),
        }
    }
    folded.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(folded)
}

/// Distinct `(calibration_type, calibration_set_id)` pairs the frame set's
/// LIGHT members link, for the three types a light can apply.
fn light_calibration_links_for_set(
    conn: &Connection,
    set_id: i64,
) -> Result<Vec<(String, i64)>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT l.calibration_type, l.calibration_set_id
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN calibration_set_to_frames l
           ON l.source_id = f.id AND l.source_type = 'frame'
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
           AND l.calibration_type IN ('Dark', 'Flat', 'Bias')
         ORDER BY l.calibration_type, l.calibration_set_id",
    )?;
    let rows = stmt
        .query_map(params![set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    Ok(rows)
}

/// The MASTER set a link actually resolves to: the set itself when it is one,
/// its supersede target when the link still names the raw set (Phase 2 repoints
/// links on supersede, but a stale link resolves the same way `classify` does).
/// `None` for a raw, unbuilt set — its master does not exist yet, so its layout
/// is not a fact to report on.
fn effective_master_set_id(conn: &Connection, set_id: i64) -> Result<Option<i64>, ApiError> {
    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT is_master_library, superseded_by_set_id FROM calibration_set WHERE id = ?1",
            params![set_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((1, _)) => Some(set_id),
        Some((_, Some(master_id))) => Some(master_id),
        _ => None,
    })
}

/// What a calibration set's member frame declares. Mirrors [`resolve_master`]'s
/// `LIMIT 1` — a master library set holds exactly one member — and reads the
/// `frames` columns directly, because the index-based list readers hardcode
/// `None` for `xbayroff`/`ybayroff` whatever is stored (see `models.rs`), which
/// would erase every phase and make the check report agreement it never saw.
fn set_member_cfa(conn: &Connection, set_id: i64) -> Result<Option<DeclaredCfa>, ApiError> {
    let row: Option<(Option<String>, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT fr.bayerpat, fr.xbayroff, fr.ybayroff
             FROM calibration_set_frames csf
             JOIN frames fr ON fr.id = csf.frame_id
             WHERE csf.set_id = ?1
             LIMIT 1",
            params![set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(pat, x, y)| DeclaredCfa::from_columns(pat, x, y)))
}

/// The advisory sentence for one (lights, master) pair, or `None` when they
/// agree. `label` is the master's display name ("Flat master", …).
fn advisory_message(label: &str, lights: &DeclaredCfa, master: &DeclaredCfa) -> Option<String> {
    match (lights, master) {
        // Both mono, or the same mosaic — the two quiet cases.
        (DeclaredCfa::Mono, DeclaredCfa::Mono) => None,
        (DeclaredCfa::Cfa(l), DeclaredCfa::Cfa(m)) if l.same_phase(*m) => None,

        (DeclaredCfa::Cfa(l), DeclaredCfa::Cfa(m)) if l.pattern != m.pattern => Some(format!(
            "{label} is {} while the light frames are {} — the colour channels do not line up. Check that the right master is linked.",
            m.pattern.as_str(),
            l.pattern.as_str()
        )),
        // Same pattern, different phase: the invisible one — R and B swap.
        (DeclaredCfa::Cfa(l), DeclaredCfa::Cfa(m)) => Some(format!(
            "{label} is {} at CFA offset ({}, {}) while the light frames declare ({}, {}) — the mosaic phase differs by a pixel. Check that the right master is linked.",
            m.pattern.as_str(),
            m.xoff,
            m.yoff,
            l.xoff,
            l.yoff
        )),
        (DeclaredCfa::Cfa(l), DeclaredCfa::Mono) => Some(format!(
            "{label} declares no CFA pattern (mono) while the light frames are {} — check that the right master is linked.",
            l.pattern.as_str()
        )),
        (DeclaredCfa::Mono, DeclaredCfa::Cfa(m)) => Some(format!(
            "{label} is {} while the light frames declare no CFA pattern (mono) — check that the right master is linked.",
            m.pattern.as_str()
        )),
        (_, DeclaredCfa::Unrecognized(p)) => Some(format!(
            "{label} declares an unrecognized CFA pattern '{p}' — CFA compatibility cannot be checked."
        )),
        // The lights' own unreadable pattern is reported once, at the set
        // level, and short-circuits the master comparison before this point.
        (DeclaredCfa::Unrecognized(_), _) => None,
    }
}

/// Compare the frame set's LIGHT consensus against every master it will apply.
/// Pure DB work, a handful of queries, no pixel I/O — shared verbatim by
/// readiness (which surfaces the sentences) and the calibrate batch (which logs
/// them once at start).
///
/// Consensus = the layout declared by the most light frames. When the lights
/// disagree among themselves that is its own advisory line, and the master
/// comparison still runs against the majority: an OSC set with one stray mono
/// frame must not lose its mono-flat warning.
fn collect_cfa_advisories(conn: &Connection, set_id: i64) -> Result<Vec<CfaAdvisory>, ApiError> {
    let declarations = light_cfa_declarations(conn, set_id)?;
    let Some((consensus, _)) = declarations.first().cloned() else {
        return Ok(Vec::new()); // no lights — nothing to compare
    };
    let mut out: Vec<CfaAdvisory> = Vec::new();

    if declarations.len() > 1 {
        let listed = declarations
            .iter()
            .map(|(d, n)| format!("{} ({n})", d.label()))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(CfaAdvisory {
            kind: "lights",
            light_pattern: listed.clone(),
            master_pattern: String::new(),
            message: format!(
                "Light frames in this set declare more than one CFA layout: {listed}. Master comparison uses {}.",
                consensus.label()
            ),
        });
    }

    if let DeclaredCfa::Unrecognized(pattern) = &consensus {
        // Nothing can be said about a layout we cannot read — say THAT, and
        // stop rather than emitting three meaningless per-master lines.
        out.push(CfaAdvisory {
            kind: "lights",
            light_pattern: consensus.label(),
            master_pattern: String::new(),
            message: format!(
                "Light frames declare an unrecognized CFA pattern '{pattern}' — CFA compatibility cannot be checked."
            ),
        });
        return Ok(out);
    }

    let links = light_calibration_links_for_set(conn, set_id)?;
    for (cal_type, kind, label) in [
        ("Dark", "dark", "Dark master"),
        ("Flat", "flat", "Flat master"),
        ("Bias", "bias", "Bias master"),
    ] {
        for (_, linked_set_id) in links.iter().filter(|(t, _)| t == cal_type) {
            let Some(master_set_id) = effective_master_set_id(conn, *linked_set_id)? else {
                continue; // raw and unbuilt, or a dangling link — not a fact yet
            };
            let Some(master_cfa) = set_member_cfa(conn, master_set_id)? else {
                continue; // memberless set — nothing declared to compare
            };
            let Some(message) = advisory_message(label, &consensus, &master_cfa) else {
                continue;
            };
            // Different lights can link different masters of one type; an
            // identical sentence twice would just be noise.
            if out.iter().any(|a| a.message == message) {
                continue;
            }
            out.push(CfaAdvisory {
                kind,
                light_pattern: consensus.label(),
                master_pattern: master_cfa.label(),
                message,
            });
        }
    }
    Ok(out)
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

// ── Current calibration links + self-consistency status (shared with collab) ─

/// The frame's current Dark/Flat/Bias calibration links, resolved from the
/// catalog. Single resolution point shared by the readiness path
/// ([`compute_readiness`]) and the Stage-II collab gate's self-consistency
/// cal-status policy ([`frame_cal_status`]) — one copy of the links query, so
/// the two callers can never drift.
pub(crate) fn current_calibration_links(
    conn: &Connection,
    frame_id: i64,
) -> anyhow::Result<Vec<CalibrationLink>> {
    Ok(get_links_for_frame(conn, frame_id)?)
}

/// A light frame's derived calibration status under the **self-consistency
/// policy** used by the collab gate (Task 4 brief): the "wanted" flat-norm
/// flag/mode and advanced params are read back from the frame's OWN stored
/// `light_calibrations` row (what was actually applied), never from settings or
/// a dialog. Under this policy `derive_status`'s param-/norm-mismatch checks are
/// vacuous by construction, while the staleness checks it cares about — link
/// changes, master rebuilds, engine-version bumps — stay fully live. No tracking
/// row → [`LightCalStatus::NotCalibrated`].
pub(crate) fn frame_cal_status(
    conn: &Connection,
    frame_id: i64,
) -> anyhow::Result<LightCalStatus> {
    let Some(row) = get_light_calibration_for_frame(conn, frame_id)? else {
        return Ok(LightCalStatus::NotCalibrated);
    };
    let links = current_calibration_links(conn, frame_id)?;
    // The row stores what was applied; parse it back as the wanted values.
    let flat_norm_mode = if row.flat_norm_mode == FlatNormMode::PixinsightTrimmed.as_wire_str() {
        FlatNormMode::PixinsightTrimmed
    } else {
        FlatNormMode::CentralThird
    };
    let mut params = serde_json::from_str::<LightCalParams>(&row.cal_params).unwrap_or_default();
    // `cal_params` records what was REQUESTED; the CFA arm compares what was
    // APPLIED, so the self-consistency policy has to feed it the row's own
    // column or the check stops being vacuous. It would otherwise fire on every
    // pre-feature CFA row: their `'{}'` decodes to today's default (`true`)
    // while the column says NULL/global — a disagreement with the caller's
    // dialog, which is exactly what this policy exists to ignore.
    params.cfa_flat_scaling = row.cfa_scaling_applied.unwrap_or(false);
    derive_status(
        conn,
        frame_id,
        &links,
        row.flat_norm_applied,
        flat_norm_mode,
        &params,
    )
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
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<LightCalReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;

    let mut frames = Vec::with_capacity(members.len());
    let mut ready_count = 0i64;
    let mut raw_set_count = 0i64;
    let mut missing_count = 0i64;
    let mut raw_set_ids_to_build: Vec<i64> = Vec::new();

    for (frame_id, filename) in members {
        let links = current_calibration_links(conn, frame_id)?;

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

        let status = status_str(derive_status(
            conn,
            frame_id,
            &links,
            flat_norm,
            flat_norm_mode,
            &params,
        )?);

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

    // Advisory only: a failure here must not cost the operator the dialog, so
    // it is logged and dropped rather than propagated (the readiness answer is
    // complete without it).
    let cfa_warnings: Vec<String> = match collect_cfa_advisories(conn, set_id) {
        Ok(advisories) => advisories.into_iter().map(|a| a.message).collect(),
        Err(e) => {
            tracing::warn!(set_id, error = %e, "cfa compatibility check failed — readiness reported without it");
            Vec::new()
        }
    };

    tracing::debug!(
        set_id,
        total = frames.len() as i64,
        ready_count,
        raw_set_count,
        missing_count,
        to_build = raw_set_ids_to_build.len() as i64,
        cfa_warnings = cfa_warnings.len() as i64,
        "light calibration readiness computed"
    );

    Ok(LightCalReadiness {
        frames,
        ready_count,
        raw_set_count,
        missing_count,
        raw_set_ids_to_build,
        cfa_warnings,
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
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<LightCalReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_readiness(&conn, set_id, flat_norm, flat_norm_mode, params)
}

// ── Export readiness gate (spec §12.2) ───────────────────────────────────────

/// Tally per-frame calibration status for a frame set's LIGHT members, in the
/// four buckets the export dialog's `calibratedLights` gate needs. Pure DB work
/// (no pixel I/O). For the two raw modes there is nothing to gate — the raw
/// lights are always on hand — so everything counts as `calibrated` and the
/// caller never blocks.
fn compute_export_readiness(
    conn: &Connection,
    set_id: i64,
    mode: ExportMode,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<ExportReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;
    let total = members.len() as i64;

    if mode != ExportMode::CalibratedLights {
        return Ok(ExportReadiness {
            total,
            calibrated: total,
            stale: 0,
            missing: 0,
        });
    }

    let mut calibrated = 0i64;
    let mut stale = 0i64;
    let mut missing = 0i64;
    for (frame_id, _filename) in members {
        let links = get_links_for_frame(conn, frame_id)?;
        match derive_status(conn, frame_id, &links, flat_norm, flat_norm_mode, &params)? {
            LightCalStatus::Calibrated => calibrated += 1,
            // Partial (new coverage available) is not a fresh full output — for
            // an export it must be recalibrated, so it groups with Stale.
            LightCalStatus::Stale | LightCalStatus::Partial => stale += 1,
            LightCalStatus::NotCalibrated => missing += 1,
        }
    }

    tracing::debug!(set_id, total, calibrated, stale, missing, "export readiness computed");
    Ok(ExportReadiness {
        total,
        calibrated,
        stale,
        missing,
    })
}

/// Export-readiness tallies for the WBPP export dialog + the `calibratedLights`
/// strict gate (spec §12.2). `flat_norm` / `flat_norm_mode` / `params` are the
/// caller's calibration preferences; staleness is derived against them exactly
/// like [`get_light_calibration_readiness`], so a frame calibrated with settings
/// the user has since changed reads as stale and blocks the export.
pub fn get_export_readiness(
    ctx: &ServiceContext,
    set_id: i64,
    mode: ExportMode,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<ExportReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_export_readiness(&conn, set_id, mode, flat_norm, flat_norm_mode, params)
}

// ── Per-frame recipe details (spec §12.1) ────────────────────────────────────

/// Human-readable master name for a calibration set: the FILENAME of the set's
/// single member frame's file. `None` when `set_id` is `None` or the file can't
/// be resolved (dangling/empty set, missing join) — logged at debug, never an
/// error, so a missing master name never fails the whole details query.
fn master_filename(conn: &Connection, set_id: Option<i64>) -> Option<String> {
    let set_id = set_id?;
    let resolved = conn
        .query_row(
            "SELECT fi.filename
             FROM calibration_set_frames csf
             JOIN frames fr ON fr.id = csf.frame_id
             JOIN files fi ON fi.id = fr.file_id
             WHERE csf.set_id = ?1
             LIMIT 1",
            params![set_id],
            |r| r.get::<_, String>(0),
        )
        .optional();
    match resolved {
        Ok(Some(name)) => Some(name),
        Ok(None) => {
            tracing::debug!(set_id, "no master file to name for light-cal details (empty/dangling set)");
            None
        }
        Err(error) => {
            tracing::debug!(set_id, %error, "failed to resolve master filename for light-cal details");
            None
        }
    }
}

/// The per-frame recipe rows for a frame set's calibrated LIGHT members. Pure DB
/// work — no pixel I/O — so both transports' wrappers can run it inside
/// `spawn_blocking`, and it is unit-testable against a seeded connection. Reuses
/// [`load_light_members`] (the same membership join as readiness); a member with
/// no `light_calibrations` row is omitted (nothing to report yet).
fn compute_details(
    conn: &Connection,
    set_id: i64,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<Vec<LightCalDetails>, ApiError> {
    let members = load_light_members(conn, set_id)?;
    let mut out = Vec::new();
    for (frame_id, _filename) in members {
        // Only LIGHT members with a tracking row have a recipe to report.
        let Some(row) = get_light_calibration_for_frame(conn, frame_id)? else {
            continue;
        };

        // Staleness against the caller's PERSISTED preferences (spec §12.1) —
        // the same wanted flags readiness uses. Partial (new coverage available)
        // counts as stale for the coverage view: the row no longer reflects a
        // full re-calibration.
        let links = get_links_for_frame(conn, frame_id)?;
        let status = derive_status(conn, frame_id, &links, flat_norm, flat_norm_mode, &params)?;
        let stale = matches!(status, LightCalStatus::Stale | LightCalStatus::Partial);

        out.push(LightCalDetails {
            frame_id,
            calstat: row.calstat,
            dark_master: master_filename(conn, row.dark_set_id),
            flat_master: master_filename(conn, row.flat_set_id),
            bias_master: master_filename(conn, row.bias_set_id),
            flat_norm_applied: row.flat_norm_applied,
            flat_norm_mode: row.flat_norm_mode,
            cal_params: row.cal_params,
            cfa_scaling_applied: row.cfa_scaling_applied,
            engine_version: row.engine_version,
            calibrated_at: row.created_at,
            output_path: row.output_path,
            stale,
        });
    }

    tracing::debug!(set_id, reported = out.len() as i64, "light calibration details computed");
    Ok(out)
}

/// Per-frame calibration recipe for the Coverage lights table (spec §12.1).
/// Returns one row per LIGHT member that already has a calibrated output;
/// uncalibrated members are omitted. `flat_norm` / `flat_norm_mode` / `params`
/// are the caller's persisted preferences — staleness is derived against them,
/// exactly like [`get_light_calibration_readiness`].
pub fn get_light_calibration_details(
    ctx: &ServiceContext,
    set_id: i64,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<Vec<LightCalDetails>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_details(&conn, set_id, flat_norm, flat_norm_mode, params)
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
    /// The light's own mosaic phase when it declares a CFA pattern the parser
    /// can vouch for — what makes per-channel flat scaling applicable.
    cfa_geometry: Option<CfaGeometry>,
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
/// disk re-read of the (possibly huge) light file. Missing blob → a warn (an
/// output stripped of its source metadata is a real, visible loss, never a
/// silent one) and the catalog-derived Bayer cards only.
///
/// The Bayer fallback runs on BOTH paths deliberately: sync-ingest inserts an
/// EMPTY `fits_header` row while three scanner branches insert no row at all —
/// the same information state (no header keywords, populated `frames` columns)
/// reached two ways. Skipping the fallback on the row-less path would give
/// those two identical states opposite CFA outcomes.
fn source_cards_for_file(
    conn: &Connection,
    frame_id: i64,
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
        tracing::warn!(
            frame_id,
            file_id,
            "no stored header for light — calibrated output will carry only catalog-derived cards"
        );
        let mut cards = Vec::new();
        append_bayer_cards_from_columns(conn, frame_id, file_id, &mut cards)?;
        return Ok(cards);
    };
    let keys = parse_stored_header_keys(format, &header_text);
    let mut cards: Vec<Card> = keys
        .iter()
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    append_bayer_cards_from_columns(conn, frame_id, file_id, &mut cards)?;
    Ok(cards)
}

/// Bayer/CFA cards the stored blob can legitimately lack while the catalog
/// columns hold the truth: an XISF that declares its CFA only through the
/// `<ColorFilterArray>` element populates `frames.bayerpat` but has no
/// BAYERPAT line anywhere in its raw XML blob — and the blob is what
/// copy-through reads. Without this fallback the calibrated output of such a
/// source would ship with no CFA geometry at all.
///
/// Rules, in order:
/// - a card parsed from the blob wins — but only if it actually carries a
///   value. A BLANK card does not: the XISF stored-header parser has no
///   empty-value check, so `<FITSKeyword name="BAYERPAT" value=""/>` lands as
///   `Str("")`, which says nothing and must not out-rank a real column value.
///   Such a card is REPLACED in place (never left beside the derived one — two
///   cards for one keyword would contradict each other in the output header);
/// - a NULL/blank column adds nothing — absent beats fabricated, exactly as in
///   `headers.rs::load_bayer_consensus`;
/// - the blob itself is never touched. `fits_header` stays the raw
///   scan-time record; only the OUTPUT header gains the derived card.
///
/// The columns are read directly rather than through a `Frame`: the index-based
/// list readers hardcode `None` for xbayroff/ybayroff/roworder regardless of
/// what is stored (see `models.rs`), so a `Frame` round-trip would erase them.
fn append_bayer_cards_from_columns(
    conn: &Connection,
    frame_id: i64,
    file_id: i64,
    cards: &mut Vec<Card>,
) -> Result<(), ApiError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<String>, Option<i64>, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT bayerpat, xbayroff, ybayroff, roworder FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((bayerpat, xbayroff, ybayroff, roworder)) = row else {
        tracing::debug!(
            frame_id,
            file_id,
            "no frames row for light — no catalog-derived bayer cards"
        );
        return Ok(());
    };

    let text = |v: Option<String>| -> Option<String> {
        v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let candidates: Vec<(&str, CardValue)> = [
        text(bayerpat).map(|v| ("BAYERPAT", CardValue::Str(v))),
        xbayroff.map(|v| ("XBAYROFF", CardValue::Integer(v))),
        ybayroff.map(|v| ("YBAYROFF", CardValue::Integer(v))),
        text(roworder).map(|v| ("ROWORDER", CardValue::Str(v))),
    ]
    .into_iter()
    .flatten()
    .collect();

    // A card carrying no usable value: either valueless, or a `Str` that is
    // empty/whitespace (the XISF `value=""` case). Such a card conveys nothing,
    // so it must not out-rank a real catalog column.
    let is_blank = |c: &Card| match &c.value {
        Some(CardValue::Str(s)) => s.trim().is_empty(),
        Some(_) => false,
        None => true, // valueless card (COMMENT/HISTORY shape) — no CFA info
    };

    for (keyword, value) in candidates {
        let existing = cards.iter().position(|c| c.keyword == keyword);
        let blank_at = match existing {
            Some(i) if !is_blank(&cards[i]) => continue, // the file's own card wins
            Some(i) => Some(i),
            None => None,
        };
        // These four keywords are all writer-legal by construction, so a
        // rejection here means something changed underneath us — log it rather
        // than dropping the CFA geometry silently.
        let card = match Card::new(keyword, value) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    frame_id,
                    file_id,
                    field = keyword,
                    error = %e,
                    "derived bayer card rejected by the writer — omitted"
                );
                continue;
            }
        };
        match blank_at {
            Some(i) => {
                tracing::debug!(
                    frame_id,
                    file_id,
                    field = keyword,
                    "bayer card derived from catalog column (stored header value blank)"
                );
                cards[i] = card;
            }
            None => {
                tracing::debug!(
                    frame_id,
                    file_id,
                    field = keyword,
                    "bayer card derived from catalog column (absent from stored header)"
                );
                cards.push(card);
            }
        }
    }
    Ok(())
}

/// The LIGHT frame's mosaic phase for per-channel flat scaling, or `None` when
/// it declares no CFA pattern (mono) or one [`Bayer::parse`] cannot vouch for.
///
/// Read straight from the `frames` columns, NOT through a `Frame`: the
/// index-based list readers hardcode `None` for `xbayroff`/`ybayroff` whatever
/// is stored, so a `Frame` round-trip would silently erase the phase and put
/// every offset light one row/column out — swapping R and B.
///
/// A missing offset defaults to 0 with a `debug!`. That guess is allowed here
/// for the same reason `measure_flat_channel_norms` allows it and
/// `build_master_cards` does not: it only decides which pixels are grouped for
/// a divisor — a wrong guess costs a colour cast the operator can see — whereas
/// writing the guess into `XBAYROFF` would be a fabricated claim every future
/// debayer acts on.
///
/// `ROWORDER` is deliberately not folded in: `BAYERPAT` describes the mosaic in
/// FILE row order (see [`CfaGeometry`]'s own rustdoc, which ratifies this).
fn resolve_cfa_geometry(
    conn: &Connection,
    frame_id: i64,
) -> Result<Option<CfaGeometry>, ApiError> {
    let row: Option<(Option<String>, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT bayerpat, xbayroff, ybayroff FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((bayerpat, xbayroff, ybayroff)) = row else {
        return Ok(None);
    };
    let Some(raw) = bayerpat.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None); // mono — the common case, not worth a log line
    };
    let Some(pattern) = Bayer::parse(raw) else {
        tracing::warn!(
            frame_id,
            bayerpat = %raw,
            "unrecognized bayer pattern — per-channel flat scaling not applied to this light"
        );
        return Ok(None);
    };
    let assumed = match (xbayroff, ybayroff) {
        (Some(_), Some(_)) => None,
        (None, Some(_)) => Some("xbayroff"),
        (Some(_), None) => Some("ybayroff"),
        (None, None) => Some("xbayroff,ybayroff"),
    };
    if let Some(field) = assumed {
        tracing::debug!(frame_id, field, "cfa phase not declared — per-channel flat scaling assumes 0");
    }
    Ok(Some(CfaGeometry {
        pattern,
        xoff: xbayroff.unwrap_or(0),
        yoff: ybayroff.unwrap_or(0),
    }))
}

/// YYYY-MM-DD from a DATE-OBS string (`2026-07-05T20:30:00Z` → `2026-07-05`).
/// The result becomes a path segment, so it goes through the shared
/// sanitizer — a malformed non-ISO DATE-OBS must not nest directories ('/')
/// or hit Windows-illegal chars (':'). Missing/empty/unsalvageable →
/// `"UnknownDate"` so the layout never gets an empty segment.
fn date_part(date_obs: Option<&str>) -> String {
    let raw: String = date_obs
        .and_then(|d| d.split('T').next())
        .map(|d| d.chars().take(10).collect())
        .unwrap_or_default();
    let sanitized = crate::archive::path_layout::sanitize_for_filename(&raw);
    if sanitized.is_empty() {
        "UnknownDate".to_string()
    } else {
        sanitized
    }
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
    let source_cards = source_cards_for_file(conn, frame_id, file_id, format)?;

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
        cfa_geometry: resolve_cfa_geometry(conn, frame_id)?,
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
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
    library_dir: &Path,
    scratch: &Path,
    cancel: &AtomicBool,
) -> Result<bool, CalError> {
    // Any existing tracking row's `output_path` — a re-calibration overwrites
    // that file in place (spec §1) instead of minting a `_2` collision suffix.
    let (resolved, existing_output) = {
        let conn = db.conn();
        if scope.only_stale {
            let links = get_links_for_frame(&conn, frame_id)?;
            if derive_status(&conn, frame_id, &links, flat_norm, flat_norm_mode, &params)?
                == LightCalStatus::Calibrated
            {
                return Ok(true);
            }
        }
        let existing_output =
            get_light_calibration_for_frame(&conn, frame_id)?.map(|r| r.output_path);
        (resolve_frame_inputs(&conn, frame_id, flat_norm)?, existing_output)
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

    // bias_fallback policy (spec §2), enforced HERE — the single decision point
    // for the dark-else-bias choice, so the engine stays agnostic. When a light
    // has no dark master but a bias would be subtracted, `skipFrame` fails the
    // frame ("no dark master") instead of doing bias-only calibration. Returned
    // before any output file/dir is created, so no artifact is left behind.
    if bias_applied && params.bias_fallback == BiasFallback::SkipFrame {
        return Err(CalError::Failed(
            "no dark master (bias fallback disabled)".into(),
        ));
    }

    let calstat = compute_calstat(dark_applied, bias_applied, flat_applied);

    // Flat-normalization divisor for the ATH_CFNM / ATH_CFN[RGB] / ATH_CCFA
    // cards, resolved BEFORE the engine (which resolves the identical value
    // through the same function) so the card list is complete at write time.
    // Global(1.0) when normalization is off or no flat applies.
    let divisor = match (&resolved.flat, flat_norm) {
        (Some(m), true) => resolve_flat_norm_divisor(
            Path::new(&m.path),
            scratch,
            flat_norm_mode,
            &params,
            resolved.cfa_geometry,
        )?,
        _ => FlatNormDivisor::Global(1.0),
    };
    let flat_norm_divisor = divisor.global_value();

    // Output scale divisor from the LIGHT's own bit depth (spec §2), resolved
    // once so the stamped ATH_CSCL card and the value the engine divides by are
    // the same number. An unreadable / decode-and-spill source yields None and
    // keeps the historic 16-bit divisor.
    let scale_divisor =
        scale_divisor_for_bitpix(crate::integration::banded::probe_bitpix(&resolved.light_path));

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
        scale_divisor,
        flat_norm_divisor,
        // The three per-channel constants + ATH_CCFA, stamped only when the
        // flat was actually normalized per CFA channel (mono lights, global
        // runs and pixinsightTrimmed stamp none of them).
        flat_channel_divisors: divisor.channel_values(),
        // ATH_CPED is stamped always (spec §2); the DN value that produced this
        // file, even when 0.
        pedestal_dn: params.pedestal_dn,
        // ATH_CTRM only when the pixinsightTrimmed statistic was actually used
        // (a normalized flat in PI mode); omitted otherwise.
        trim_fraction: if flat_applied
            && flat_norm
            && flat_norm_mode == FlatNormMode::PixinsightTrimmed
        {
            Some(params.trim_fraction)
        } else {
            None
        },
    };
    let cards = build_light_cal_cards(&resolved.source_cards, &card_inputs)?;

    // Re-calibration overwrites the recorded output in place (the engine write
    // is atomic tmp+rename); only a first-time calibration allocates a fresh,
    // collision-suffixed path (spec §1). Without this, every re-run would mint a
    // `c_<name>_2.fits` and orphan the previous file.
    let output_abs = match existing_output {
        Some(path) => PathBuf::from(path),
        None => {
            let rel = calibrated_light_relative_path(
                &resolved.object,
                &resolved.instrume,
                &resolved.date_obs_date,
                &output_basename_fits(&resolved.source_filename),
            );
            let conn = db.conn();
            resolve_collision_free(&library_dir.join(&rel), &|p| {
                crate::db::light_calibrations::output_path_exists(&conn, p).unwrap_or(false)
            })
        }
    };
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
        flat_norm_mode,
        cfa_geometry: resolved.cfa_geometry,
        params,
        scale_divisor,
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
        // Record the statistic actually used when a normalized flat was applied;
        // otherwise the column default (the mode is irrelevant when nothing was
        // normalized, and `derive_status` never compares it in that case).
        flat_norm_mode: if flat_applied && flat_norm {
            flat_norm_mode.as_wire_str().to_string()
        } else {
            FlatNormMode::CentralThird.as_wire_str().to_string()
        },
        // Canonical JSON of the advanced parameters actually applied (spec §5);
        // parsed back for staleness comparison, so serialization must not fail —
        // fall back to '{}' (== Default) if it somehow does.
        cal_params: serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string()),
        // What the ENGINE actually did, not what `params` asked for — a mono
        // light, a pixinsightTrimmed run or a flat with degenerate channel
        // constants all satisfy a per-channel request globally, and the row has
        // to say so or `derive_status` would read it back wrong.
        cfa_scaling_applied: Some(outcome.cfa_scaling_applied),
        output_hash: outcome.output_hash.clone(),
        engine_version: LIGHT_CAL_ENGINE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    {
        let conn = db.conn();
        upsert_light_calibration(&conn, &row)?;
    }
    // Report-only: the floored-pixel count is not persisted (no schema change),
    // so the per-frame record of it lives here alongside the row write. The
    // engine already warns when the count is non-zero.
    tracing::debug!(
        frame_id = resolved.frame_id,
        dest = %output_abs.display(),
        calstat = %row.calstat,
        floored_flat_pixels = outcome.floored_flat_pixels,
        cfa_scaling_applied = outcome.cfa_scaling_applied,
        // The BITPIX-derived divisor this frame was actually scaled by (== the
        // stamped ATH_CSCL card), so a mis-scaled artifact is diagnosable from
        // the log without re-reading the output header.
        scale_divisor,
        "light calibration recorded"
    );
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn calibrate_one(
    db: &crate::db::Database,
    frame_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
    library_dir: &Path,
    scratch: &Path,
    cancel: &AtomicBool,
) -> FrameOutcome {
    match calibrate_one_inner(
        db,
        frame_id,
        scope,
        flat_norm,
        flat_norm_mode,
        params,
        library_dir,
        scratch,
        cancel,
    ) {
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

/// Block the calling thread until none of `build_set_ids` remain registered in
/// `active_master_builds` — i.e. every preflight master build has completed and
/// removed its handle (`start_master_build` inserts the handle synchronously
/// before spawn and removes it on completion) — or the cancel flag is set
/// (returns `true`). This is the explicit handshake that orders the light job
/// AFTER its preflight builds: it must run BEFORE `ComputeQueue::acquire`,
/// because at `max_concurrent = 1` a still-running build holds the only slot and
/// waiting while holding a permit would deadlock. No overall timeout is needed —
/// builds always terminate and drop their handle; the poll only backstops.
fn wait_for_preflight_builds<V>(
    active_master_builds: &Mutex<HashMap<i64, V>>,
    build_set_ids: &[i64],
    cancel_flag: &AtomicBool,
) -> bool {
    if build_set_ids.is_empty() {
        return false;
    }
    let mut announced = false;
    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            return true;
        }
        let still_building = {
            let active = active_master_builds.lock().unwrap();
            build_set_ids.iter().any(|id| active.contains_key(id))
        };
        if !still_building {
            return false;
        }
        if !announced {
            tracing::debug!(
                count = build_set_ids.len(),
                "waiting for preflight master builds before light calibration"
            );
            announced = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// The batch body: wait for preflight builds, queue admission, then per-frame
/// calibration. Returns `Ok(true)` if cancelled, `Ok(false)` if it ran to
/// completion; `ok_count` and `failed` accumulate in place so the caller can
/// emit the finished event even on an early return. A per-frame failure is
/// collected, never fatal.
#[allow(clippy::too_many_arguments)]
fn run_light_cal(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
    preflight_build_set_ids: &[i64],
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

    // Explicit ordering handshake (spec §6): wait for every preflight master
    // build to finish BEFORE we acquire a compute slot. At max_concurrent=1 the
    // build holds the only slot, so this MUST precede `acquire` (waiting after
    // would deadlock). This — not FIFO admission — is what guarantees masters
    // build first; the supersede/relink they perform is then visible when each
    // light re-resolves its links at calibration time.
    if wait_for_preflight_builds(
        &ctx.active_master_builds,
        preflight_build_set_ids,
        cancel_flag.as_ref(),
    ) {
        return Ok(true);
    }

    // Admission (spec §6): cancelled-while-queued → treat the whole batch as
    // cancelled.
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

    tracing::info!(
        set_id,
        total,
        only_stale = scope.only_stale,
        flat_norm,
        flat_norm_mode = flat_norm_mode.as_wire_str(),
        pedestal_dn = params.pedestal_dn,
        trim_fraction = params.trim_fraction,
        bias_fallback = ?params.bias_fallback,
        "light calibration batch started"
    );

    // Advisory CFA compatibility, once per (set, master type) at batch start —
    // NOT per frame: the divisor path already warns per frame when a flat's
    // stamped constants disagree with the light, and repeating that here for
    // every frame would bury it. Never blocks; a failed check is logged and the
    // batch runs on.
    {
        let conn = db.conn();
        match collect_cfa_advisories(&conn, set_id) {
            Ok(advisories) => {
                for advisory in advisories {
                    if advisory.kind == "lights" {
                        // `light_patterns`, plural: a set-level note carries the
                        // JOINED list of the layouts its lights declared, where
                        // the per-master notes below carry one label. Same field
                        // name for both shapes would make the log unqueryable.
                        tracing::warn!(
                            set_id,
                            kind = advisory.kind,
                            light_patterns = %advisory.light_pattern,
                            "cfa layouts disagree among light frames"
                        );
                    } else {
                        tracing::warn!(
                            set_id,
                            kind = advisory.kind,
                            light_pattern = %advisory.light_pattern,
                            master_pattern = %advisory.master_pattern,
                            "cfa mismatch between light set and master"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(set_id, error = %e, "cfa compatibility check failed — calibrating anyway")
            }
        }
    }

    for (index, (frame_id, filename)) in members.iter().enumerate() {
        if cancel_flag.load(Ordering::Relaxed) {
            return Ok(true);
        }
        match calibrate_one(
            db,
            *frame_id,
            scope,
            flat_norm,
            flat_norm_mode,
            params,
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
#[allow(clippy::too_many_arguments)]
fn run_light_cal_thread(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
    preflight_build_set_ids: Vec<i64>,
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
            flat_norm_mode,
            params,
            &preflight_build_set_ids,
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
#[allow(clippy::too_many_arguments)]
pub fn start_light_calibration(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    app_version: String,
    set_id: i64,
    scope: LightCalScope,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<(), ApiError> {
    // Register the cancel handle FIRST, rejecting a concurrent run for the same
    // set (mirrors start_master_build's duplicate-run guard). Doing this before
    // the preflight means a double-click can't kick off a redundant, side-
    // effecting master-build batch before the second call is rejected.
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

    // Preflight: build masters for every raw calibration set the lights link.
    // Non-fatal — a skipped/failed build just means those lights calibrate
    // best-effort at run time (§6). Never aborts the light run. A hard readiness
    // error must release the handle we just registered before returning.
    let readiness =
        match get_light_calibration_readiness(&ctx, set_id, flat_norm, flat_norm_mode, params) {
            Ok(r) => r,
            Err(e) => {
                ctx.active_light_cal.lock().unwrap().remove(&set_id);
                return Err(e);
            }
        };
    // The set ids whose preflight builds actually STARTED (handle registered in
    // `active_master_builds`); the worker thread waits on exactly these before
    // acquiring its compute slot. Skipped/unstarted sets are never in the map,
    // so they'd be a no-op wait — but tracking the started set precisely keeps
    // the handshake honest.
    let mut preflight_build_set_ids: Vec<i64> = Vec::new();
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
                preflight_build_set_ids = report.started_set_ids.clone();
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
                flat_norm_mode,
                params,
                preflight_build_set_ids,
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
    use crate::fits_writer::keywords::{FrameKind, HeaderBuilder};
    use crate::fits_writer::write_fits_f32;
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
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    // ── Real-file fixtures for the end-to-end thread smoke test ──────────────

    fn fnrm_card(v: f64) -> Card {
        Card::new("ATH_FNRM", CardValue::Real(v)).unwrap()
    }

    /// Write a constant-valued 32-bit FITS plane to `dir/name`, return its path.
    fn write_plane(dir: &Path, name: &str, w: usize, h: usize, val: f32, cards: &[Card]) -> PathBuf {
        let data = vec![val; w * h];
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
        p
    }

    /// A MASTER set whose single member frame's file is a real FITS at `path`.
    fn seed_master_set_with_file(
        conn: &Connection,
        set_id: i64,
        imagetyp: &str,
        path: &Path,
    ) -> i64 {
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
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
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

    /// A LIGHT frame whose file is a real FITS at `path` (OBJECT M31 / camera
    /// TestCam / 2026-07-05, so the calibrated output layout is deterministic).
    fn seed_light_with_file(conn: &Connection, frame_id: i64, session_id: i64, path: &Path) {
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
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

    /// Records every emitted `(event_name, payload)` so a test can assert on the
    /// real event stream a run produced (the test-side `ProgressEmitter`).
    #[derive(Default)]
    struct CollectingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }
    impl ProgressEmitter for CollectingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((event_name.to_string(), payload));
        }
    }
    impl CollectingEmitter {
        fn count(&self, name: &str) -> usize {
            self.events.lock().unwrap().iter().filter(|(n, _)| n == name).count()
        }
        fn first(&self, name: &str) -> serde_json::Value {
            self.events
                .lock()
                .unwrap()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("no {name} event emitted"))
        }
    }

    /// Sorted final calibrated output basenames in `dir` (`c_*.fits`, excluding
    /// tmp files) — the file set a re-run must leave unchanged.
    fn list_calibrated(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("c_") && n.ends_with(".fits") && !n.contains(".tmp"))
            .collect();
        names.sort();
        names
    }

    /// A frame set with two LIGHT frames on disk, each linked to a BUILT master
    /// Dark + master Flat (also on disk), a configured calibration-library
    /// output dir, and a `ServiceContext`. Returns the tempdir guard (keep it
    /// alive), the ctx, the library dir, the frame-set id, and the light ids.
    fn build_smoke_fixture() -> (tempfile::TempDir, Arc<ServiceContext>, PathBuf, i64, Vec<i64>) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let (w, h) = (8usize, 8usize);
        let light_ids = {
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            let session = seed_frame_set(&conn, 1);

            let dark_path = write_plane(&src, "master_dark.fits", w, h, 100.0, &[]);
            let flat_path = write_plane(&src, "master_flat.fits", w, h, 2.0, &[fnrm_card(2.0)]);
            let dark = seed_master_set_with_file(&conn, 100, "MasterDark", &dark_path);
            let flat = seed_master_set_with_file(&conn, 101, "MasterFlat", &flat_path);

            let mut ids = Vec::new();
            for fid in [1i64, 2] {
                let lp = write_plane(&src, &format!("light_{fid}.fits"), w, h, 1100.0, &[]);
                seed_light_with_file(&conn, fid, session, &lp);
                add_link(&conn, fid, dark, "Dark");
                add_link(&conn, fid, flat, "Flat");
                ids.push(fid);
            }
            ids
        };
        let ctx = test_ctx(db);
        (tmp, ctx, library_dir, 1, light_ids)
    }

    /// Register a fresh handle and drive the real `light-cal` thread body on the
    /// current thread (synchronous), returning the emitter it wrote to so the
    /// caller can assert on the produced event stream.
    fn run_thread_body(
        ctx: &Arc<ServiceContext>,
        set_id: i64,
        only_stale: bool,
    ) -> Arc<CollectingEmitter> {
        let emitter = Arc::new(CollectingEmitter::default());
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let job_id_slot = Arc::new(AtomicI64::new(0));
        ctx.active_light_cal.lock().unwrap().insert(
            set_id,
            LightCalHandle {
                cancel_flag: cancel_flag.clone(),
                job_id: job_id_slot.clone(),
            },
        );
        run_light_cal_thread(
            ctx.clone(),
            emitter.clone(),
            set_id,
            LightCalScope { only_stale },
            true,
            FlatNormMode::CentralThird,
            LightCalParams::default(),
            Vec::new(), // no preflight builds in this direct-thread harness
            cancel_flag,
            job_id_slot,
        );
        emitter
    }

    #[test]
    fn thread_smoke_calibrates_frame_set() {
        let (_tmp, ctx, _lib, set_id, light_ids) = build_smoke_fixture();

        let emitter = run_thread_body(&ctx, set_id, false);

        // Exactly one finished event, outcome "success".
        assert_eq!(
            emitter.count("calibration-finished"),
            1,
            "finished must fire exactly once"
        );
        let finished = emitter.first("calibration-finished");
        assert_eq!(finished["outcome"], "success", "finished payload: {finished}");
        assert_eq!(finished["ok_count"], light_ids.len() as u64);
        assert_eq!(finished["failed"].as_array().unwrap().len(), 0);

        // One progress event per calibrated light frame.
        assert_eq!(
            emitter.count("calibration-progress"),
            light_ids.len(),
            "one progress event per calibrated frame"
        );

        // Tracking rows upserted for every light, each pointing at a real file.
        let db_handle = db(&ctx).unwrap();
        let conn = db_handle.conn();
        for fid in &light_ids {
            let row = get_light_calibration_for_frame(&conn, *fid)
                .unwrap()
                .unwrap_or_else(|| panic!("no tracking row for light {fid}"));
            assert!(
                Path::new(&row.output_path).exists(),
                "calibrated output {} must exist on disk",
                row.output_path
            );
            assert_eq!(row.calstat, "BDF", "dark subtraction + flat division");
            assert!(row.flat_norm_applied, "flat present and flat_norm requested");
        }

        // Handle removed at the single exit path.
        assert!(
            ctx.active_light_cal.lock().unwrap().get(&set_id).is_none(),
            "the thread must remove its handle on exit"
        );
    }

    /// End-to-end: the output scale divisor follows the LIGHT's own bit depth,
    /// not a hardcoded 16-bit constant. The fixture's lights are 32-bit float
    /// (BITPIX −32) — already physical values — so nothing is scaled: every
    /// output pixel is exactly `(L − D) / (F / ATH_FNRM)`, and the stamped
    /// `ATH_CSCL` card reports the same `1.0` the engine divided by (a
    /// mis-scaled file that still claimed 65535 would be invisible downstream).
    #[test]
    fn float_source_is_written_unscaled_and_stamps_its_divisor() {
        use crate::integration::banded::BandSource;

        let (_tmp, ctx, _lib, set_id, light_ids) = build_smoke_fixture();
        let emitter = run_thread_body(&ctx, set_id, false);
        assert_eq!(emitter.first("calibration-finished")["outcome"], "success");

        let db_handle = db(&ctx).unwrap();
        let out = {
            let conn = db_handle.conn();
            PathBuf::from(
                get_light_calibration_for_frame(&conn, light_ids[0]).unwrap().unwrap().output_path,
            )
        };

        // Pixels: (1100 − 100) / (2.0 / 2.0) = 1000.0, undivided by any scale.
        let scratch = std::env::temp_dir();
        let mut src = BandSource::open(&[out.clone()], &scratch).unwrap();
        let h = src.height();
        let mut bufs = vec![Vec::new()];
        src.read_band(0, h, &mut bufs).unwrap();
        assert!(
            bufs[0].iter().all(|&v| v == 1000.0),
            "a float source must not be scaled by 65535: got {}",
            bufs[0][0]
        );

        // …and the header agrees with what was actually applied.
        let (_f, text) = crate::fits_parser::parse_fits_with_header(&out, 0).unwrap();
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        let scl: f64 = keys
            .get("ATH_CSCL")
            .expect("ATH_CSCL card")
            .parse()
            .expect("ATH_CSCL parses as a real");
        assert_eq!(scl, 1.0, "ATH_CSCL must report the divisor the engine used");
    }

    /// End-to-end for per-channel flat scaling: a light that declares RGGB,
    /// divided by a flat carrying its own strong colour. The orchestrator has to
    /// (a) read the CFA phase off the `frames` columns, (b) hand it to the
    /// engine, (c) stamp the per-channel provenance cards, and (d) record what
    /// was applied on the tracking row — a break anywhere in that chain silently
    /// reverts to the colour-flattening global divisor.
    #[test]
    fn cfa_light_calibrates_per_channel_end_to_end() {
        use crate::integration::banded::BandSource;

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        // 6x6 RGGB mosaics. Light R=1000/G=2000/B=500, flat R=2000/G=4000/B=1000
        // — the flat is exactly 2x the light in every channel, so per-channel
        // scaling returns the light verbatim while a global divisor (the
        // central-third blend, 2750) would flatten all three onto one value.
        let (w, h) = (6usize, 6usize);
        let mosaic = |r: f32, g: f32, b: f32| {
            let mut data = vec![0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    data[y * w + x] = match (x % 2 == 0, y % 2 == 0) {
                        (true, true) => r,
                        (false, false) => b,
                        _ => g,
                    };
                }
            }
            data
        };
        let write_mosaic = |name: &str, data: Vec<f32>, cards: &[Card]| {
            let p = src.join(name);
            write_fits_f32(&p, w, h, 1, &data, cards).unwrap();
            p
        };

        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = database.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            let session = seed_frame_set(&conn, 1);
            // No ATH_FN* cards on the flat: this exercises the recompute path an
            // imported master flat takes.
            let flat_path = write_mosaic("master_flat.fits", mosaic(2000.0, 4000.0, 1000.0), &[]);
            let flat = seed_master_set_with_file(&conn, 101, "MasterFlat", &flat_path);
            let lp = write_mosaic("light_1.fits", mosaic(1000.0, 2000.0, 500.0), &[]);
            seed_light_with_file(&conn, 1, session, &lp);
            // The CFA phase lives on `frames`, which is where the orchestrator
            // must read it from (the Frame list-readers drop the offsets).
            conn.execute(
                "UPDATE frames SET bayerpat = 'RGGB', xbayroff = 0, ybayroff = 0 WHERE id = 1",
                [],
            )
            .unwrap();
            add_link(&conn, 1, flat, "Flat");
        }
        let ctx = test_ctx(database);

        let emitter = run_thread_body(&ctx, 1, false);
        assert_eq!(emitter.first("calibration-finished")["outcome"], "success");

        let db_handle = db(&ctx).unwrap();
        let row = {
            let conn = db_handle.conn();
            get_light_calibration_for_frame(&conn, 1).unwrap().unwrap()
        };
        assert_eq!(
            row.cfa_scaling_applied,
            Some(true),
            "the tracking row must record that per-channel scaling was applied"
        );
        assert_eq!(row.calstat, "F");

        // Pixels: each colour divided by its own constant → the light verbatim
        // (the source is 32-bit float, so no scale divisor either).
        let out = PathBuf::from(&row.output_path);
        let mut band = BandSource::open(&[out.clone()], tmp.path()).unwrap();
        let mut bufs = vec![Vec::new()];
        band.read_band(0, h, &mut bufs).unwrap();
        let data = &bufs[0];
        for y in 0..h {
            for x in 0..w {
                let want = match (x % 2 == 0, y % 2 == 0) {
                    (true, true) => 1000.0,
                    (false, false) => 500.0,
                    _ => 2000.0,
                };
                assert_eq!(data[y * w + x], want, "pixel ({x},{y})");
            }
        }

        // Provenance: the applied constants and the CFA flag, plus ATH_CFNM as
        // their mean for a reader that knows only the global card.
        let (_f, text) = crate::fits_parser::parse_fits_with_header(&out, 0).unwrap();
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        assert_eq!(
            keys.get("ATH_CCFA").map(|s| s.trim().trim_matches('\'').trim().to_string()),
            Some("T".to_string()),
            "ATH_CCFA must be stamped T"
        );
        for (kw, want) in [("ATH_CFNR", 2000.0), ("ATH_CFNG", 4000.0), ("ATH_CFNB", 1000.0)] {
            let got: f64 = keys.get(kw).unwrap_or_else(|| panic!("{kw} card")).trim().parse().unwrap();
            assert!((got - want).abs() < 1e-6, "{kw} = {got}, want {want}");
        }
        let fnm: f64 = keys.get("ATH_CFNM").expect("ATH_CFNM card").trim().parse().unwrap();
        assert!(
            (fnm - 2750.0).abs() < 1e-6,
            "ATH_CFNM must be the mosaic-weighted blend (R+2G+B)/4, got {fnm}"
        );
    }

    // ── End-to-end CFA metadata (Task 10) ────────────────────────────────────
    //
    // Every layer of the Bayer chain is unit-pinned on its own (header/element
    // -> `frames` columns, columns/blob -> cards, cards -> writer). What these
    // two add is the chain ASSERTED AGAINST A FILE ON DISK: the calibrated
    // output a user actually feeds to WBPP/Siril. A break in any seam ships a
    // frame that debayers to the wrong colours — or to none at all — while the
    // pixels themselves look perfectly healthy.

    /// The mosaic colour index of pixel `(x, y)` for RGGB at CFA phase (1, 0) —
    /// the phase the fixtures below declare. Spelled out rather than routed
    /// through `cfa_channel_at` so the fixture states the layout
    /// INDEPENDENTLY: a bug in the production mapping must not also rewrite
    /// the data the test measures it against. `0` = R, `1` = G, `2` = B.
    fn rggb_at_phase_1_0(x: usize, y: usize) -> usize {
        // grid[y % 2][(x + 1) % 2] over [[R, G], [G, B]].
        match (x % 2 == 1, y % 2 == 0) {
            (true, true) => 0,
            (false, false) => 2,
            _ => 1,
        }
    }

    /// A `w`x`h` RGGB mosaic at phase (1, 0) with the given `[R, G, B]` levels.
    fn mosaic_phase_1_0(w: usize, h: usize, rgb: [f32; 3]) -> Vec<f32> {
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = rgb[rggb_at_phase_1_0(x, y)];
            }
        }
        data
    }

    /// The four CFA cards a real OSC capture writes, at the phase
    /// [`rggb_at_phase_1_0`] describes. `XBAYROFF = 1` deliberately: a fixture
    /// at phase 0 cannot tell a copied-through offset from a fabricated one.
    fn osc_bayer_cards() -> Vec<Card> {
        vec![
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
            Card::new("ROWORDER", CardValue::Str("BOTTOM-UP".into())).unwrap(),
        ]
    }

    /// Seed a LIGHT frame the way the scanner does: parse the real file on
    /// disk, then write BOTH the `frames` row (Bayer columns included,
    /// straight from that parse) and the `fits_header` blob. The blob is what
    /// light-cal copy-through reads, so a fixture that skips it cannot
    /// exercise copy-through at all.
    fn seed_scanned_light(conn: &Connection, frame_id: i64, session_id: i64, path: &Path) {
        let (parsed, header_text) = crate::fits_parser::parse_fits_with_header(path, 0).unwrap();
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                path.to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs,
                                 bayerpat, xbayroff, ybayroff, roworder)
             VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', ?3, ?4, ?5, ?6)",
            params![
                frame_id,
                file_id,
                parsed.bayerpat,
                parsed.xbayroff,
                parsed.ybayroff,
                parsed.roworder
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
        crate::db::insert_fits_header(conn, file_id, &header_text).unwrap();
    }

    /// End-to-end Bayer metadata: the CFA cards an OSC capture writes must
    /// reach the calibrated OUTPUT FILE verbatim — the same pattern, the same
    /// REAL phase offsets, and the row order that tells a debayer which way
    /// the mosaic runs. The phase is (1, 0), not (0, 0), precisely so a
    /// fabricated zero cannot pass.
    #[test]
    fn cfa_bayer_cards_survive_the_full_run_into_the_written_output() {
        use crate::integration::banded::BandSource;

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        let (w, h) = (6usize, 6usize);
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = database.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            let session = seed_frame_set(&conn, 1);

            // Masters: a constant dark plus a flat that is exactly 2x the light
            // in every channel, so a correctly-phased run returns the
            // dark-subtracted light verbatim and a phase error shows up as
            // swapped colour levels rather than as a rounding difference.
            let dark_path = src.join("master_dark.fits");
            write_fits_f32(&dark_path, w, h, 1, &vec![100.0f32; w * h], &osc_bayer_cards())
                .unwrap();
            let flat_path = src.join("master_flat.fits");
            write_fits_f32(
                &flat_path,
                w,
                h,
                1,
                &mosaic_phase_1_0(w, h, [2000.0, 4000.0, 1000.0]),
                &osc_bayer_cards(),
            )
            .unwrap();
            let dark = seed_master_set_with_file(&conn, 100, "MasterDark", &dark_path);
            let flat = seed_master_set_with_file(&conn, 101, "MasterFlat", &flat_path);
            // The masters declare the same mosaic as the lights, so the
            // advisory CFA check has nothing to report and the fixture reads as
            // one coherent OSC rig rather than a mismatched one.
            conn.execute(
                "UPDATE frames SET bayerpat = 'RGGB', xbayroff = 1, ybayroff = 0,
                                   roworder = 'BOTTOM-UP'
                 WHERE id IN (?1, ?2)",
                params![dark + 4_000_000, flat + 4_000_000],
            )
            .unwrap();

            // The LIGHT is written carrying the four CFA cards and ingested
            // through the real parser, so both its `frames` columns and its
            // stored header blob are exactly what a scan of this file produces.
            let light_path = src.join("light_1.fits");
            let mut cards = HeaderBuilder::new(FrameKind::Light)
                .instrume("TestCam")
                .object("M31")
                .exptime(120.0)
                .build()
                .unwrap();
            cards.extend(osc_bayer_cards());
            write_fits_f32(
                &light_path,
                w,
                h,
                1,
                &mosaic_phase_1_0(w, h, [1100.0, 2100.0, 600.0]),
                &cards,
            )
            .unwrap();
            seed_scanned_light(&conn, 1, session, &light_path);
            add_link(&conn, 1, dark, "Dark");
            add_link(&conn, 1, flat, "Flat");
        }
        let ctx = test_ctx(database);

        let emitter = run_thread_body(&ctx, 1, false);
        assert_eq!(emitter.first("calibration-finished")["outcome"], "success");

        let db_handle = db(&ctx).unwrap();
        let row = {
            let conn = db_handle.conn();
            get_light_calibration_for_frame(&conn, 1).unwrap().unwrap()
        };
        assert_eq!(row.calstat, "BDF");
        assert_eq!(row.cfa_scaling_applied, Some(true));

        // ── The pin: the written FILE's own header, re-read from disk. ──
        let out = PathBuf::from(&row.output_path);
        let header = crate::fits_parser::FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_str("BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(
            header.get_i32("XBAYROFF"),
            Some(1),
            "the source's REAL phase must survive, not a fabricated 0"
        );
        assert_eq!(header.get_i32("YBAYROFF"), Some(0));
        assert_eq!(
            header.get_str("ROWORDER").as_deref(),
            Some("BOTTOM-UP"),
            "without ROWORDER a debayer cannot know which way the mosaic runs"
        );

        // XBAYROFF must be an INTEGER card. The header reader strips quotes, so
        // `get_i32` alone cannot tell `1` from `'1'` — and a quoted offset is
        // exactly the spelling third-party debayerers refuse to read.
        let raw = header.to_header_text();
        let xcard = raw
            .lines()
            .find(|l| l.starts_with("XBAYROFF"))
            .expect("XBAYROFF card in the written output");
        assert!(
            !xcard.contains('\''),
            "XBAYROFF must be an integer card, got {xcard:?}"
        );

        // …and the phase actually drove the math: every colour divided by its
        // own flat constant returns the dark-subtracted light verbatim (the
        // source is 32-bit float, so no scale divisor either).
        let mut band = BandSource::open(&[out.clone()], tmp.path()).unwrap();
        let mut bufs = vec![Vec::new()];
        band.read_band(0, h, &mut bufs).unwrap();
        for y in 0..h {
            for x in 0..w {
                let want = [1000.0f32, 2000.0, 500.0][rggb_at_phase_1_0(x, y)];
                assert_eq!(bufs[0][y * w + x], want, "pixel ({x},{y})");
            }
        }
    }

    /// Minimal valid monolithic XISF: the 16-byte prefix, an XML header, and an
    /// attached UInt16 sample block. `image_body` lands verbatim inside
    /// `<Image>`, so a caller can declare the CFA the native XISF way — a
    /// `<ColorFilterArray>` child and no FITSKeyword in sight.
    fn write_xisf_u16(path: &Path, w: usize, h: usize, samples: &[u16], image_body: &str) {
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        // `location` carries the attachment's byte offset, which depends on the
        // XML's length, which depends on the offset. Broken by formatting the
        // offset zero-padded to a FIXED width: substituting the real value
        // cannot resize the header.
        let xml_for = |pos: usize| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xisf version=\"1.0\">\n\
                 <Image geometry=\"{w}:{h}:1\" sampleFormat=\"UInt16\" colorSpace=\"Gray\" \
                 location=\"attachment:{pos:08}:{}\">\n{image_body}</Image>\n</xisf>",
                bytes.len()
            )
        };
        let probe_len = xml_for(0).len();
        let xml = xml_for(16 + probe_len);
        assert_eq!(
            xml.len(),
            probe_len,
            "the zero-padded offset must not resize the header"
        );
        let xml_bytes = xml.as_bytes();
        let mut out = Vec::with_capacity(16 + xml_bytes.len() + bytes.len());
        out.extend_from_slice(b"XISF0100");
        out.extend_from_slice(&(xml_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // reserved
        out.extend_from_slice(xml_bytes);
        out.extend_from_slice(&bytes);
        std::fs::write(path, out).unwrap();
    }

    /// An XISF whose ONLY CFA declaration is the native `<ColorFilterArray>`
    /// element — no `BAYERPAT` keyword anywhere in the file, and therefore none
    /// in the stored header blob copy-through reads. The pattern survives only
    /// if the XISF parser adopts the element into `frames.bayerpat` AND the
    /// card builder falls back to that column. Drop either half and the
    /// calibrated output ships with no CFA geometry at all, which every
    /// downstream tool reads as mono.
    #[test]
    fn xisf_color_filter_array_element_reaches_the_written_output() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        let (w, h) = (6usize, 6usize);
        // RGGB at phase (0, 0): the element declares no offsets, so the engine
        // assumes 0 — the mosaic is laid out to match that assumption.
        let cell = |x: usize, y: usize| match (x % 2 == 0, y % 2 == 0) {
            (true, true) => 0usize,  // R
            (false, false) => 2,     // B
            _ => 1,                  // G
        };
        let mut samples = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                samples[y * w + x] = [1100u16, 2100, 600][cell(x, y)];
            }
        }
        let light_path = src.join("light_1.xisf");
        write_xisf_u16(
            &light_path,
            w,
            h,
            &samples,
            // No BAYERPAT keyword anywhere — the CFA is declared ONLY by the
            // native element, which is the entire point of this fixture.
            "<FITSKeyword name=\"OBJECT\" value=\"'M31'\" comment=\"\"/>\n\
             <FITSKeyword name=\"INSTRUME\" value=\"'TestCam'\" comment=\"\"/>\n\
             <ColorFilterArray pattern=\"RGGB\" width=\"2\" height=\"2\" name=\"RGGB\"/>\n",
        );

        // The scanner's own two reads: the parser fills the catalog columns,
        // `extract_xisf_header` fills the blob copy-through consults.
        let parsed = crate::fits_parser::parse_xisf(&light_path, 0).unwrap();
        assert_eq!(
            parsed.bayerpat.as_deref(),
            Some("RGGB"),
            "fixture sanity: the element must be adopted into the catalog column"
        );
        assert_eq!(
            parsed.xbayroff, None,
            "the element carries no phase — none may be invented"
        );
        let header_text = crate::fits_parser::extract_xisf_header(&light_path).unwrap();
        assert!(
            !parse_stored_header_keys(FileFormat::XISF, &header_text).contains_key("BAYERPAT"),
            "fixture sanity: the blob copy-through reads holds no BAYERPAT"
        );

        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        {
            let conn = database.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            let session = seed_frame_set(&conn, 1);

            let flat_path = src.join("master_flat.fits");
            let mut flat_data = vec![0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    flat_data[y * w + x] = [2000.0f32, 4000.0, 1000.0][cell(x, y)];
                }
            }
            write_fits_f32(&flat_path, w, h, 1, &flat_data, &[]).unwrap();
            let flat = seed_master_set_with_file(&conn, 101, "MasterFlat", &flat_path);

            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (1, ?1, 'light_1.xisf', 0, '2026-07-05T00:00:00Z', 'XISF')",
                params![light_path.to_string_lossy()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs,
                                     bayerpat, xbayroff, ybayroff, roworder)
                 VALUES (1, 1, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', ?1, ?2, ?3, ?4)",
                params![
                    parsed.bayerpat,
                    parsed.xbayroff,
                    parsed.ybayroff,
                    parsed.roworder
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, 1)",
                params![session],
            )
            .unwrap();
            crate::db::insert_fits_header(&conn, 1, &header_text).unwrap();
            add_link(&conn, 1, flat, "Flat");
        }
        let ctx = test_ctx(database);

        let emitter = run_thread_body(&ctx, 1, false);
        assert_eq!(emitter.first("calibration-finished")["outcome"], "success");

        let db_handle = db(&ctx).unwrap();
        let row = {
            let conn = db_handle.conn();
            get_light_calibration_for_frame(&conn, 1).unwrap().unwrap()
        };
        assert_eq!(row.calstat, "F");
        assert_eq!(
            row.cfa_scaling_applied,
            Some(true),
            "the adopted pattern must reach the ENGINE, not only the header"
        );

        let out = PathBuf::from(&row.output_path);
        let header = crate::fits_parser::FitsHeader::from_path(&out).unwrap();
        assert_eq!(
            header.get_str("BAYERPAT").as_deref(),
            Some("RGGB"),
            "the element-declared pattern must reach the output header"
        );
        // No offsets were declared, and none may be invented: an XBAYROFF = 0
        // card is a claim every future debayer acts on, not a harmless default.
        assert_eq!(header.get_i32("XBAYROFF"), None);
        assert_eq!(header.get_i32("YBAYROFF"), None);
    }

    #[test]
    fn recalibration_overwrites_in_place() {
        let (_tmp, ctx, _lib, set_id, light_ids) = build_smoke_fixture();

        // First calibration.
        let e1 = run_thread_body(&ctx, set_id, false);
        assert_eq!(e1.first("calibration-finished")["outcome"], "success");

        // Record the output paths + the leaf-dir file count after run 1.
        let (out_paths, leaf, count_before) = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            let out_paths: Vec<String> = light_ids
                .iter()
                .map(|fid| {
                    get_light_calibration_for_frame(&conn, *fid)
                        .unwrap()
                        .unwrap()
                        .output_path
                })
                .collect();
            let leaf = Path::new(&out_paths[0]).parent().unwrap().to_path_buf();
            let names = list_calibrated(&leaf);
            (out_paths, leaf, names)
        };
        assert_eq!(count_before.len(), light_ids.len(), "one output per light after run 1");
        // A bug (unconditional resolve_collision) would mint c_light_1_2.fits.
        assert!(
            count_before.iter().all(|n| !n.contains("_2.fits") || n == "c_light_2.fits"),
            "no collision suffix expected after the first run: {count_before:?}"
        );

        // Second calibration — full re-run of the same frames.
        let e2 = run_thread_body(&ctx, set_id, false);
        assert_eq!(e2.first("calibration-finished")["outcome"], "success");

        // Tracking rows still point at the SAME files, and the output dir's file
        // set is byte-for-byte unchanged — overwrite in place, no new collision-
        // suffixed file, no orphan (spec §1).
        let db_handle = db(&ctx).unwrap();
        let conn = db_handle.conn();
        for (fid, prev) in light_ids.iter().zip(&out_paths) {
            let now = get_light_calibration_for_frame(&conn, *fid)
                .unwrap()
                .unwrap()
                .output_path;
            assert_eq!(&now, prev, "re-run must reuse the same output path");
        }
        assert_eq!(
            list_calibrated(&leaf),
            count_before,
            "the calibrated-output file set must be identical after a re-run"
        );
    }

    #[test]
    fn thread_partial_outcome_when_one_frame_fails() {
        let (_tmp, ctx, _lib, set_id, _ids) = build_smoke_fixture();

        // Add a third LIGHT with NO calibration links — it resolves no masters
        // and fails per-frame (collected, never a batch abort), so the whole
        // batch reports "partial" while still emitting finished exactly once.
        {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            let session: i64 = conn
                .query_row(
                    "SELECT s.id FROM sessions s
                     JOIN imaging_nights n ON n.id = s.imaging_night_id
                     WHERE n.frames_set_id = ?1",
                    params![set_id],
                    |r| r.get(0),
                )
                .unwrap();
            seed_light_with_file(&conn, 3, session, Path::new("/nonexistent/light_3.fits"));
        }

        let emitter = run_thread_body(&ctx, set_id, false);

        assert_eq!(
            emitter.count("calibration-finished"),
            1,
            "finished must fire exactly once even with a failing frame"
        );
        let finished = emitter.first("calibration-finished");
        assert_eq!(finished["outcome"], "partial", "one failure among successes: {finished}");
        assert_eq!(finished["ok_count"], 2u64, "the two linked lights still succeed");
        let failed = finished["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1, "exactly the unlinked light failed");
        assert_eq!(failed[0]["frame_id"], 3u64);

        // Only the two successful frames emitted progress; the handle is gone.
        assert_eq!(emitter.count("calibration-progress"), 2);
        assert!(ctx.active_light_cal.lock().unwrap().get(&set_id).is_none());
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
            FlatNormMode::CentralThird,
            LightCalParams::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "expected Conflict, got {err:?}");

        // The pre-existing handle is untouched — the rejected call must not
        // remove or replace it.
        assert!(ctx.active_light_cal.lock().unwrap().contains_key(&1));
    }

    /// bias_fallback = skipFrame (spec §2): a light with NO dark master but a
    /// linked bias must FAIL per-frame (no output, no tracking row) instead of
    /// doing bias-only calibration — while the default subtractBias calibrates
    /// the same light to CALSTAT 'BF'.
    #[test]
    fn skip_frame_policy_fails_bias_only_light() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();

        let db = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let (w, h) = (8usize, 8usize);
        let light_id = 1i64;
        {
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            let session = seed_frame_set(&conn, 1);
            let bias_path = write_plane(&src, "master_bias.fits", w, h, 50.0, &[]);
            let bias = seed_master_set_with_file(&conn, 102, "MasterBias", &bias_path);
            let lp = write_plane(&src, "light_1.fits", w, h, 1100.0, &[]);
            seed_light_with_file(&conn, light_id, session, &lp);
            add_link(&conn, light_id, bias, "Bias");
        }

        let scratch = std::env::temp_dir();
        let cancel = AtomicBool::new(false);
        let scope = LightCalScope { only_stale: false };

        // skipFrame → per-frame failure, no artifact left behind.
        let params_skip = LightCalParams {
            bias_fallback: BiasFallback::SkipFrame,
            ..LightCalParams::default()
        };
        let outcome = calibrate_one(
            &db,
            light_id,
            scope,
            true,
            FlatNormMode::CentralThird,
            params_skip,
            &library_dir,
            &scratch,
            &cancel,
        );
        let reason = match &outcome {
            FrameOutcome::Failed(r) => r.clone(),
            _ => String::new(),
        };
        assert!(
            matches!(outcome, FrameOutcome::Failed(_)),
            "skipFrame must fail a bias-only light"
        );
        assert!(
            reason.contains("no dark master"),
            "skipFrame failure reason should name the missing dark: {reason}"
        );
        {
            let conn = db.conn();
            assert!(
                get_light_calibration_for_frame(&conn, light_id).unwrap().is_none(),
                "skipFrame must not write a tracking row"
            );
        }
        let has_output = std::fs::read_dir(&library_dir)
            .map(|rd| rd.flatten().next().is_some())
            .unwrap_or(false);
        assert!(!has_output, "skipFrame must not write any output under the library dir");

        // subtractBias (default) → the same light calibrates to CALSTAT 'BF'.
        let outcome2 = calibrate_one(
            &db,
            light_id,
            scope,
            true,
            FlatNormMode::CentralThird,
            LightCalParams::default(),
            &library_dir,
            &scratch,
            &cancel,
        );
        assert!(matches!(outcome2, FrameOutcome::Done), "subtractBias must calibrate the light");
        {
            let conn = db.conn();
            let row = get_light_calibration_for_frame(&conn, light_id).unwrap().unwrap();
            assert_eq!(row.calstat, "B", "bias-only subtraction, no flat linked → 'B'");
            assert!(Path::new(&row.output_path).exists(), "output must exist under subtractBias");
        }
    }

    // ── Real-data end-to-end (B5 Task 9, spec §10 integration) ───────────────
    //
    // Full pipeline against the owner's real archive, sandboxed: copy a real
    // LIGHT cluster + its raw dark/flat member frames into a scratch tree,
    // scan them into a fresh catalog (auto-creating the raw calibration sets),
    // seed the frame-set structure, run the real matcher, designate a sandbox
    // calibration-library root, then drive the *public* `start_light_calibration`
    // path — preflight master builds + calibration — and assert masters were
    // built + superseded, every light is CALSTAT='BDF', the ATH_C* header
    // vocabulary is present, pixels are finite and match the §2 formula, and the
    // scanner reconcile-adopt branches (known / moved / duplicate) all fire.
    //
    // `#[ignore]` because it needs the owner's catalog DB + reachable FITS on
    // disk. Run it with:
    //   ATHENAEUM_E2E_DB=<path/to/athenaeum.db> \
    //   ATHENAEUM_E2E_SANDBOX=<scratch dir> \
    //   cargo test -p athenaeum-core --release real_data_e2e -- --ignored --nocapture
    // The DB is opened strictly READ-ONLY; every artifact lives under the
    // sandbox. If the DB or the FITS are unreachable the test prints SKIP and
    // returns green (never a spurious failure in a checkout without the data).
    #[test]
    #[ignore = "real-data e2e: set ATHENAEUM_E2E_DB + ATHENAEUM_E2E_SANDBOX, run with --ignored"]
    fn real_data_e2e_light_calibration() {
        use crate::api::calibration::find_calibration_for_frame_set;
        use crate::fits_parser::stored_header::parse_stored_header_keys;
        use crate::integration::banded::BandSource;
        use crate::models::FileFormat;
        use crate::scanner::scan_directory;
        use rusqlite::OpenFlags;

        // ── Read a full f32 plane (one band) for exact pixel assertions ──────
        fn read_plane(path: &Path, scratch: &Path) -> (usize, usize, Vec<f32>) {
            let mut src = BandSource::open(&[path.to_path_buf()], scratch).unwrap();
            let (w, h) = (src.width(), src.height());
            let mut bufs = vec![Vec::new()];
            src.read_band(0, h, &mut bufs).unwrap();
            (w, h, bufs.remove(0))
        }
        fn header_keys(path: &Path) -> std::collections::HashMap<String, String> {
            let (_f, text) = crate::fits_parser::parse_fits_with_header(path, 0).unwrap();
            parse_stored_header_keys(FileFormat::FITS, &text)
        }
        let files_count = |conn: &Connection, path: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap()
        };
        let count = |conn: &Connection, sql: &str| -> i64 {
            conn.query_row(sql, [], |r| r.get(0)).unwrap()
        };

        // ── Locate the real catalog (read-only) ──────────────────────────────
        let db_path = std::env::var("ATHENAEUM_E2E_DB").unwrap_or_else(|_| {
            format!(
                "{}/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        if !Path::new(&db_path).exists() {
            eprintln!("[b5-e2e] SKIP: real catalog not found at {db_path}");
            return;
        }
        let real = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .expect("open real DB read-only");

        // ── Pick a cluster: a LIGHT frame set whose lights link to a RAW dark
        //    set AND a RAW flat set, with the member FITS present on disk. ─────
        let query_paths = |conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]| -> Vec<String> {
            let mut stmt = conn.prepare(sql).unwrap();
            stmt.query_map(p, |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .filter(|path| Path::new(path).exists())
                .collect()
        };

        let mut candidates = real
            .prepare(
                "SELECT fs.id, d.calibration_set_id, fl.calibration_set_id
                 FROM frames_set fs
                 JOIN imaging_nights ino ON ino.frames_set_id = fs.id
                 JOIN sessions s ON s.imaging_night_id = ino.id
                 JOIN session_members sm ON sm.session_id = s.id
                 JOIN frames f ON f.id = sm.frame_id AND f.imagetyp = 'Light'
                 JOIN calibration_set_to_frames d
                     ON d.source_id = f.id AND d.source_type = 'frame' AND d.calibration_type = 'Dark'
                 JOIN calibration_set dcs
                     ON dcs.id = d.calibration_set_id AND dcs.is_master_library = 0 AND dcs.superseded_by_set_id IS NULL
                 JOIN calibration_set_to_frames fl
                     ON fl.source_id = f.id AND fl.source_type = 'frame' AND fl.calibration_type = 'Flat'
                 JOIN calibration_set fcs
                     ON fcs.id = fl.calibration_set_id AND fcs.is_master_library = 0 AND fcs.superseded_by_set_id IS NULL
                 GROUP BY fs.id, d.calibration_set_id, fl.calibration_set_id
                 LIMIT 40",
            )
            .unwrap();
        let triples: Vec<(i64, i64, i64)> = candidates
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let mut chosen: Option<(i64, i64, i64, Vec<String>, Vec<String>, Vec<String>)> = None;
        for (fs_id, dark_set, flat_set) in triples {
            let lights = query_paths(
                &real,
                "SELECT DISTINCT fi.path FROM frames_set fs
                 JOIN imaging_nights ino ON ino.frames_set_id = fs.id
                 JOIN sessions s ON s.imaging_night_id = ino.id
                 JOIN session_members sm ON sm.session_id = s.id
                 JOIN frames f ON f.id = sm.frame_id AND f.imagetyp = 'Light'
                 JOIN files fi ON fi.id = f.file_id
                 WHERE fs.id = ?1
                   AND EXISTS (SELECT 1 FROM calibration_set_to_frames d WHERE d.source_id = f.id AND d.calibration_type='Dark' AND d.calibration_set_id=?2)
                   AND EXISTS (SELECT 1 FROM calibration_set_to_frames l WHERE l.source_id = f.id AND l.calibration_type='Flat' AND l.calibration_set_id=?3)
                 ORDER BY f.date_obs LIMIT 3",
                &[&fs_id as &dyn rusqlite::ToSql, &dark_set, &flat_set],
            );
            let darks = query_paths(
                &real,
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 ORDER BY f.date_obs LIMIT 5",
                &[&dark_set as &dyn rusqlite::ToSql],
            );
            let flats = query_paths(
                &real,
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 ORDER BY f.date_obs LIMIT 5",
                &[&flat_set as &dyn rusqlite::ToSql],
            );
            if lights.len() >= 2 && darks.len() >= 2 && flats.len() >= 2 {
                chosen = Some((fs_id, dark_set, flat_set, lights, darks, flats));
                break;
            }
        }
        drop(candidates);
        let Some((real_fs, real_dark_set, real_flat_set, lights, darks, flats)) = chosen else {
            eprintln!("[b5-e2e] SKIP: no reachable real cluster (raw dark+flat with files on disk)");
            return;
        };
        eprintln!(
            "[b5-e2e] cluster: frame_set={real_fs} raw_dark_set={real_dark_set} raw_flat_set={real_flat_set} \
             lights={} darks={} flats={}",
            lights.len(),
            darks.len(),
            flats.len()
        );
        for p in lights.iter().chain(&darks).chain(&flats) {
            eprintln!("[b5-e2e]   src {p}");
        }

        // ── Build the sandbox tree; copy (never move) the real FITS in ───────
        let sandbox = std::env::var("ATHENAEUM_E2E_SANDBOX")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("athenaeum-b5-e2e"));
        let _ = std::fs::remove_dir_all(&sandbox);
        let src = sandbox.join("src");
        let library_dir = sandbox.join("library");
        std::fs::create_dir_all(&library_dir).unwrap();
        let copy_group = |paths: &[String], sub: &str| -> Vec<PathBuf> {
            let dir = src.join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            paths
                .iter()
                .map(|p| {
                    let dest = dir.join(Path::new(p).file_name().unwrap());
                    std::fs::copy(p, &dest).unwrap_or_else(|e| panic!("copy {p}: {e}"));
                    dest
                })
                .collect()
        };
        copy_group(&lights, "LIGHT");
        copy_group(&darks, "DARK");
        copy_group(&flats, "FLAT");

        // ── Fresh catalog: scan the sandbox → real frames + raw calib sets ───
        let database = crate::db::Database::new(sandbox.join("catalog.db")).unwrap();
        let (frame_set_id, light_frame_ids, lib_root_id) = {
            let conn = database.conn();
            conn.execute(
                "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
                params![src.to_string_lossy()],
            )
            .unwrap();
            let sr = scan_directory(&src, &conn, None, false, false, 1);
            assert!(sr.errors.is_empty(), "scan errors: {:?}", sr.errors);
            eprintln!(
                "[b5-e2e] scan: lights={} darks={} flats={} calib_sets_created={}",
                sr.lights_count, sr.darks_count, sr.flats_count, sr.calibration_sets_created
            );
            assert!(sr.lights_count >= 2 && sr.darks_count >= 2 && sr.flats_count >= 2);

            let light_ids: Vec<i64> = {
                let mut stmt = conn
                    .prepare("SELECT id FROM frames WHERE imagetyp='Light' ORDER BY id")
                    .unwrap();
                stmt.query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
            };
            assert!(light_ids.len() >= 2, "scanned lights: {}", light_ids.len());

            // Seed the frame-set / night / session structure and enrol the
            // scanned real light frames (clustering itself is Phase-1 machinery,
            // out of B5 scope — we wire the members directly).
            let fs_id = 9001;
            let session = seed_frame_set(&conn, fs_id);
            for lid in &light_ids {
                conn.execute(
                    "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                    params![session, lid],
                )
                .unwrap();
            }

            // Designate the sandbox calibration-library root.
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO scan_roots (id, path, kind) VALUES (2, ?1, 'calibration_library')",
                params![library_dir.to_string_lossy()],
            )
            .unwrap();
            (fs_id, light_ids, 2i64)
        };

        let ctx = test_ctx(database);

        // ── Run the real matcher → raw dark/flat links on every light ────────
        let stats =
            find_calibration_for_frame_set(&ctx, frame_set_id, None, None, None, None).unwrap();
        eprintln!(
            "[b5-e2e] matcher: total={} full_calibration={}",
            stats.total_frames, stats.frames_with_full_calibration
        );

        // ── Readiness: every light must classify dark+flat as rawSet ─────────
        let readiness = get_light_calibration_readiness(
            &ctx,
            frame_set_id,
            true,
            FlatNormMode::CentralThird,
            LightCalParams::default(),
        )
        .unwrap();
        eprintln!(
            "[b5-e2e] readiness: ready={} raw={} missing={} to_build={:?}",
            readiness.ready_count, readiness.raw_set_count, readiness.missing_count,
            readiness.raw_set_ids_to_build
        );
        assert!(
            !readiness.raw_set_ids_to_build.is_empty(),
            "expected raw sets to build; matcher produced no raw links"
        );
        for fr in &readiness.frames {
            assert_eq!(fr.dark, "rawSet", "frame {} dark class", fr.frame_id);
            assert_eq!(fr.flat, "rawSet", "frame {} flat class", fr.frame_id);
            assert_eq!(fr.status, "notCalibrated", "frame {} status", fr.frame_id);
        }

        // ── Drive the public start path: preflight master builds + calibrate ─
        let emitter = Arc::new(CollectingEmitter::default());
        let emitter_dyn: Arc<dyn ProgressEmitter> = emitter.clone();
        start_light_calibration(
            ctx.clone(),
            emitter_dyn,
            "0.2.5".into(),
            frame_set_id,
            LightCalScope { only_stale: false },
            true,
            FlatNormMode::CentralThird,
            LightCalParams::default(),
        )
        .unwrap();

        // Wait for the detached light-cal thread to finish (masters build first
        // at max_concurrent=1, then the light job). Generous cap for real frames.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        loop {
            let running = ctx.active_light_cal.lock().unwrap().contains_key(&frame_set_id);
            if !running {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "light calibration timed out");
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        // ── Assert the run outcome ───────────────────────────────────────────
        let finished = emitter.first("calibration-finished");
        eprintln!("[b5-e2e] finished: {finished}");
        assert_eq!(finished["outcome"], "success", "finished payload: {finished}");
        assert_eq!(finished["ok_count"], light_frame_ids.len() as u64);

        let db_ref = db(&ctx).unwrap();

        // Masters: raw sets superseded, master-library sets + provenance exist.
        {
            let conn = db_ref.conn();
            let master_sets = count(&conn, "SELECT COUNT(*) FROM calibration_set WHERE is_master_library=1");
            let provenance = count(&conn, "SELECT COUNT(*) FROM master_provenance");
            let superseded = count(
                &conn,
                "SELECT COUNT(*) FROM calibration_set WHERE superseded_by_set_id IS NOT NULL",
            );
            eprintln!(
                "[b5-e2e] masters: library_sets={master_sets} provenance={provenance} superseded={superseded}"
            );
            assert!(master_sets >= 2, "expected >=2 master sets (dark+flat)");
            assert!(provenance >= 2, "expected master_provenance rows");
            assert!(superseded >= 2, "raw dark+flat sets must be superseded");
        }

        // Per-light: BDF, output on disk, headers + pixels sane.
        let scratch = std::env::temp_dir();
        let mut checked = 0usize;
        let mut first_output: Option<String> = None;
        for &fid in &light_frame_ids {
            let conn = db_ref.conn();
            let row = get_light_calibration_for_frame(&conn, fid).unwrap().unwrap();
            assert_eq!(row.calstat, "BDF", "frame {fid} calstat");
            assert!(row.flat_norm_applied, "frame {fid} flat_norm_applied");
            let out = PathBuf::from(&row.output_path);
            assert!(out.exists(), "output {} missing", row.output_path);
            first_output.get_or_insert(row.output_path.clone());

            // Header vocabulary (§7).
            let keys = header_keys(&out);
            for k in ["CALSTAT", "ATH_CSRC", "ATH_CSRN", "ATH_CSCL", "ATH_CFNM", "ATH_CVER"] {
                assert!(keys.contains_key(k), "output {} missing card {k}", row.output_path);
            }
            assert_eq!(keys.get("CALSTAT").map(String::as_str), Some("BDF"));

            // Master file paths + fnrm from the (superseded → master) sets.
            let dark_path: String = conn
                .query_row(
                    "SELECT fi.path FROM calibration_set_frames csf JOIN frames fr ON fr.id=csf.frame_id JOIN files fi ON fi.id=fr.file_id WHERE csf.set_id=?1 LIMIT 1",
                    params![row.dark_set_id.unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            let flat_path: String = conn
                .query_row(
                    "SELECT fi.path FROM calibration_set_frames csf JOIN frames fr ON fr.id=csf.frame_id JOIN files fi ON fi.id=fr.file_id WHERE csf.set_id=?1 LIMIT 1",
                    params![row.flat_set_id.unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            let light_path: String = conn
                .query_row(
                    "SELECT fi.path FROM frames fr JOIN files fi ON fi.id=fr.file_id WHERE fr.id=?1",
                    params![fid],
                    |r| r.get(0),
                )
                .unwrap();
            drop(conn);

            let fnrm: f64 = header_keys(Path::new(&flat_path))
                .get("ATH_FNRM")
                .and_then(|s| s.parse().ok())
                .expect("master flat carries ATH_FNRM");

            let (lw, lh, lpix) = read_plane(Path::new(&light_path), &scratch);
            let (dw, dh, dpix) = read_plane(Path::new(&dark_path), &scratch);
            let (fw, fh, fpix) = read_plane(Path::new(&flat_path), &scratch);
            let (ow, oh, opix) = read_plane(&out, &scratch);
            assert_eq!((lw, lh), (ow, oh), "output geometry");
            assert_eq!((dw, dh), (ow, oh), "dark geometry");
            assert_eq!((fw, fh), (ow, oh), "flat geometry");

            // No NaN/Inf anywhere in the full plane; background positive & <<1.
            assert!(opix.iter().all(|v| v.is_finite()), "output has NaN/Inf");
            let mean = opix.iter().map(|&v| v as f64).sum::<f64>() / opix.len() as f64;
            eprintln!(
                "[b5-e2e] frame {fid}: out={} {ow}x{oh} mean={mean:.6} fnrm={fnrm:.2}",
                Path::new(&row.output_path).file_name().unwrap().to_string_lossy()
            );
            assert!(mean > 0.0 && mean < 1.0, "background mean {mean} out of (0,1)");

            // Spot-check the §2 formula on a spread of pixels. The scale
            // divisor follows the real light's own bit depth, so it is probed
            // here exactly as the engine's caller probes it.
            let scale_divisor = scale_divisor_for_bitpix(
                crate::integration::banded::probe_bitpix(Path::new(&light_path)),
            );
            let n = opix.len();
            for i in [0usize, n / 4, n / 2, (3 * n) / 4, n - 1] {
                let expect = (((lpix[i] as f64) - (dpix[i] as f64))
                    / ((fpix[i] as f64) / fnrm))
                    / scale_divisor;
                let got = opix[i] as f64;
                let tol = 1e-4 * expect.abs().max(1e-6);
                assert!(
                    (got - expect).abs() <= tol,
                    "frame {fid} px {i}: got {got} want {expect}"
                );
            }
            checked += 1;
        }
        assert!(checked >= 2, "checked {checked} lights");

        // ── Scanner reconcile-adopt over the library root ────────────────────
        // (a) rescan: calibrated outputs skipped, zero new frames.
        {
            let conn = db_ref.conn();
            let frames_before = count(&conn, "SELECT COUNT(*) FROM frames");
            let cal_before = count(&conn, "SELECT COUNT(*) FROM light_calibrations");
            let sr = scan_directory(&library_dir, &conn, None, false, false, lib_root_id);
            assert!(sr.errors.is_empty(), "rescan errors: {:?}", sr.errors);
            let frames_after = count(&conn, "SELECT COUNT(*) FROM frames");
            let cal_after = count(&conn, "SELECT COUNT(*) FROM light_calibrations");
            eprintln!(
                "[b5-e2e] rescan(a): frames {frames_before}->{frames_after} cal {cal_before}->{cal_after} dups={}",
                sr.calibrated_duplicates.len()
            );
            assert_eq!(frames_before, frames_after, "rescan must add no frames");
            assert_eq!(cal_before, cal_after, "rescan must add no tracking rows");
            assert!(sr.calibrated_duplicates.is_empty(), "known paths are not duplicates");
            let out0 = first_output.clone().unwrap();
            assert_eq!(files_count(&conn, &out0), 0, "calibrated output never registered as a file");
        }

        // (b) move one output → rescan repairs output_path.
        {
            let moved_fid = light_frame_ids[0];
            let (old_path, moved_path) = {
                let conn = db_ref.conn();
                let old = get_light_calibration_for_frame(&conn, moved_fid).unwrap().unwrap().output_path;
                let moved = format!("{old}.moved.fits");
                std::fs::rename(&old, &moved).unwrap();
                (old, moved)
            };
            let conn = db_ref.conn();
            let sr = scan_directory(&library_dir, &conn, None, false, false, lib_root_id);
            assert!(sr.errors.is_empty(), "move-rescan errors: {:?}", sr.errors);
            let repaired = get_light_calibration_for_frame(&conn, moved_fid).unwrap().unwrap().output_path;
            eprintln!("[b5-e2e] move(b): {old_path} -> {repaired}");
            assert_eq!(repaired, moved_path, "output_path must be repaired to the new location");
            assert!(sr.calibrated_duplicates.is_empty(), "a move is not a duplicate");
        }

        // (c) copy one output → rescan reports a duplicate; row untouched.
        {
            let dup_fid = light_frame_ids[1];
            let (kept_path, dup_path) = {
                let conn = db_ref.conn();
                let kept = get_light_calibration_for_frame(&conn, dup_fid).unwrap().unwrap().output_path;
                let dup = format!("{kept}.copy.fits");
                std::fs::copy(&kept, &dup).unwrap();
                (kept, dup)
            };
            let conn = db_ref.conn();
            let sr = scan_directory(&library_dir, &conn, None, false, false, lib_root_id);
            assert!(sr.errors.is_empty(), "copy-rescan errors: {:?}", sr.errors);
            eprintln!("[b5-e2e] copy(c): dups={:?}", sr.calibrated_duplicates);
            assert!(
                sr.calibrated_duplicates.iter().any(|d| d.duplicate_path == dup_path),
                "duplicate copy must be reported: {:?}",
                sr.calibrated_duplicates
            );
            let row = get_light_calibration_for_frame(&conn, dup_fid).unwrap().unwrap();
            assert_eq!(row.output_path, kept_path, "tracking row untouched by a duplicate");
        }

        eprintln!("[b5-e2e] PASS: real-data end-to-end light calibration verified");
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

    /// The collab gate's self-consistency policy judges a contribution by its
    /// OWN recorded settings, never by a dialog preference — so the CFA arm has
    /// to stay vacuous through `frame_cal_status`. The trap: a row written
    /// before per-channel scaling carries `cal_params = '{}'`, which decodes to
    /// today's default (`cfa_flat_scaling = true`) while its column says
    /// NULL/global. Reading the request instead of the column would flag every
    /// pre-feature CFA contribution Stale.
    #[test]
    fn frame_cal_status_ignores_the_cfa_toggle_on_a_pre_feature_row() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        // A colour frame with a normalized flat — every condition the CFA
        // staleness arm needs in order to fire.
        conn.execute("UPDATE frames SET bayerpat = 'RGGB' WHERE id = 1", []).unwrap();
        seed_masters(&conn);
        add_link(&conn, 1, 101, "Flat");

        let mut row = LightCalRow {
            id: 0,
            frame_id: Some(1),
            source_uuid: None,
            source_filename: None,
            output_path: "/lib/c_light_1.fits".to_string(),
            dark_set_id: None,
            flat_set_id: Some(101),
            bias_set_id: None,
            calstat: "F".to_string(),
            flat_norm_applied: true,
            flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
            cal_params: "{}".to_string(), // the pre-feature shape
            cfa_scaling_applied: None,    // …and its NULL column
            output_hash: "hash".to_string(),
            engine_version: LIGHT_CAL_ENGINE_VERSION,
            created_at: "2026-07-05T00:00:00Z".to_string(),
        };
        upsert_light_calibration(&conn, &row).unwrap();
        assert_eq!(
            frame_cal_status(&conn, 1).unwrap(),
            LightCalStatus::Calibrated,
            "a pre-feature CFA row must stay self-consistent"
        );

        // Same for a row that DID record per-channel scaling: judged against
        // itself, it is consistent whatever the local default happens to be.
        row.cfa_scaling_applied = Some(true);
        upsert_light_calibration(&conn, &row).unwrap();
        assert_eq!(frame_cal_status(&conn, 1).unwrap(), LightCalStatus::Calibrated);

        // …while the dialog-driven path (caller's wanted params) DOES see the
        // difference — that is the policy split this test guards.
        let links = current_calibration_links(&conn, 1).unwrap();
        assert_eq!(
            derive_status(
                &conn,
                1,
                &links,
                true,
                FlatNormMode::CentralThird,
                &LightCalParams { cfa_flat_scaling: false, ..LightCalParams::default() },
            )
            .unwrap(),
            LightCalStatus::Stale,
            "the caller-driven path still compares the toggle"
        );
    }

    #[test]
    fn date_part_sanitizes_non_iso_values() {
        assert_eq!(date_part(Some("2026-07-05T20:30:00Z")), "2026-07-05");
        // Malformed locale date: '/' must not become directory nesting, ':' is
        // Windows-illegal — both map to '_' (audit F6).
        assert_eq!(date_part(Some("05/07/2026")), "05_07_2026");
        assert_eq!(date_part(None), "UnknownDate");
        assert_eq!(date_part(Some("")), "UnknownDate");
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
                flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
                cal_params: "{}".to_string(),
                cfa_scaling_applied: None,
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

        let r = compute_readiness(&conn, 1, false, FlatNormMode::CentralThird, LightCalParams::default()).unwrap();
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

        let r = compute_readiness(&conn, 1, false, FlatNormMode::CentralThird, LightCalParams::default()).unwrap();

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

    /// A MASTER calibration set (`is_master_library = 1`) with exactly one
    /// member frame + file, so [`master_filename`] can resolve its FILENAME.
    fn seed_master_with_file(conn: &Connection, set_id: i64, imagetyp: &str, filename: &str) {
        seed_set(conn, set_id, imagetyp, true);
        let file_id = set_id + 5_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![file_id, format!("/lib/{filename}"), filename],
        )
        .unwrap();
        let frame_id = set_id + 6_000_000;
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
    }

    // ── Advisory CFA compatibility (readiness) ──────────────────────────────

    /// CFA columns on a master set's single member frame (the row
    /// [`seed_master_with_file`] created).
    fn set_master_bayer(
        conn: &Connection,
        set_id: i64,
        bayerpat: Option<&str>,
        xoff: Option<i64>,
        yoff: Option<i64>,
    ) {
        conn.execute(
            "UPDATE frames SET bayerpat = ?2, xbayroff = ?3, ybayroff = ?4 WHERE id = ?1",
            params![set_id + 6_000_000, bayerpat, xoff, yoff],
        )
        .unwrap();
    }

    /// CFA columns on a LIGHT frame seeded by [`seed_light`].
    fn set_light_bayer(
        conn: &Connection,
        frame_id: i64,
        bayerpat: Option<&str>,
        xoff: Option<i64>,
        yoff: Option<i64>,
    ) {
        conn.execute(
            "UPDATE frames SET bayerpat = ?2, xbayroff = ?3, ybayroff = ?4 WHERE id = ?1",
            params![frame_id, bayerpat, xoff, yoff],
        )
        .unwrap();
    }

    fn readiness_for(conn: &Connection, set_id: i64) -> LightCalReadiness {
        compute_readiness(conn, set_id, true, FlatNormMode::CentralThird, LightCalParams::default())
            .unwrap()
    }

    /// The finding this advisory closes: a MONO master flat divides an OSC
    /// light silently. Readiness must say so — once, for the flat, naming both
    /// layouts — while leaving the classification and the counts untouched
    /// (advisory never blocks).
    #[test]
    fn readiness_warns_when_a_mono_flat_master_meets_osc_lights() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));

        seed_master_with_file(&conn, 100, "MasterDark", "master_dark.fits");
        set_master_bayer(&conn, 100, Some("RGGB"), Some(0), Some(0)); // agrees
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits"); // mono
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "only the flat mismatches: {:?}", r.cfa_warnings);
        let w = &r.cfa_warnings[0];
        assert!(w.contains("Flat master"), "names the offending master: {w}");
        assert!(w.contains("mono"), "says the master is mono: {w}");
        assert!(w.contains("RGGB"), "names the lights' layout: {w}");

        // Advisory ONLY: classification, counts and the build list are as if
        // the check did not exist.
        assert_eq!(r.frames[0].flat, MASTER);
        assert_eq!(r.frames[0].dark, MASTER);
        assert_eq!(r.ready_count, 1);
        assert_eq!(r.missing_count, 0);
        assert!(r.raw_set_ids_to_build.is_empty());
    }

    /// The reversed mismatch is just as wrong and just as invisible: a CFA
    /// master against mono lights.
    #[test]
    fn readiness_warns_when_a_cfa_master_meets_mono_lights() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session); // mono light — no bayerpat
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        set_master_bayer(&conn, 101, Some("RGGB"), Some(0), Some(0));
        add_link(&conn, 1, 101, "Flat");

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "{:?}", r.cfa_warnings);
        assert!(r.cfa_warnings[0].contains("RGGB"), "{}", r.cfa_warnings[0]);
        assert!(r.cfa_warnings[0].contains("mono"), "{}", r.cfa_warnings[0]);
    }

    /// Agreement is silent — including the two spellings of agreement that a
    /// naive comparison would call a mismatch: an UNDECLARED offset (compares
    /// as 0) and an offset of 2 (same phase modulo 2).
    #[test]
    fn readiness_is_silent_when_light_and_master_cfa_agree() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));

        seed_master_with_file(&conn, 100, "MasterDark", "master_dark.fits");
        set_master_bayer(&conn, 100, Some("RGGB"), Some(2), Some(-2)); // == (0,0)
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        set_master_bayer(&conn, 101, Some("rggb"), None, None); // undeclared == 0
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");

        assert!(readiness_for(&conn, 1).cfa_warnings.is_empty());
    }

    /// A mono set (the majority of rigs) must never produce an advisory.
    #[test]
    fn readiness_is_silent_for_a_mono_set() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_master_with_file(&conn, 100, "MasterDark", "master_dark.fits");
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");

        assert!(readiness_for(&conn, 1).cfa_warnings.is_empty());
    }

    /// Same pattern, different phase — the invisible one. R and B swap, and
    /// nothing downstream says a word without this.
    #[test]
    fn readiness_warns_on_a_cfa_phase_shift() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(1), Some(0));
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        set_master_bayer(&conn, 101, Some("RGGB"), Some(0), Some(0));
        add_link(&conn, 1, 101, "Flat");

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "{:?}", r.cfa_warnings);
        let w = &r.cfa_warnings[0];
        assert!(w.contains("phase"), "says it is a phase shift: {w}");
        assert!(w.contains("RGGB"), "{w}");
    }

    /// A different PATTERN is its own message — the channels do not line up at
    /// all, which is a stronger statement than a phase shift.
    #[test]
    fn readiness_warns_on_a_different_pattern() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        set_master_bayer(&conn, 101, Some("BGGR"), Some(0), Some(0));
        add_link(&conn, 1, 101, "Flat");

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "{:?}", r.cfa_warnings);
        assert!(r.cfa_warnings[0].contains("BGGR"), "{}", r.cfa_warnings[0]);
        assert!(r.cfa_warnings[0].contains("RGGB"), "{}", r.cfa_warnings[0]);
    }

    /// Lights that disagree AMONG THEMSELVES get their own line; the master
    /// comparison then runs against the majority layout, so a master matching
    /// the majority adds nothing on top.
    #[test]
    fn readiness_warns_when_the_lights_disagree_among_themselves() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        for id in 1..=3 {
            seed_light(&conn, id, session);
        }
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));
        set_light_bayer(&conn, 2, Some("RGGB"), Some(0), Some(0));
        // …and one stray mono light.
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits");
        set_master_bayer(&conn, 101, Some("RGGB"), Some(0), Some(0));
        for id in 1..=3 {
            add_link(&conn, id, 101, "Flat");
        }

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "{:?}", r.cfa_warnings);
        let w = &r.cfa_warnings[0];
        assert!(w.contains("more than one"), "{w}");
        assert!(w.contains("RGGB") && w.contains("mono"), "lists both layouts: {w}");
    }

    /// A raw, unbuilt set has no master to compare against yet — its layout is
    /// decided when the preflight builds it, so readiness stays quiet rather
    /// than reporting a fact that is not one.
    #[test]
    fn readiness_skips_cfa_advisories_for_unbuilt_raw_sets() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));
        seed_master_with_file(&conn, 200, "Flat", "flat_001.fits"); // mono member
        conn.execute("UPDATE calibration_set SET is_master_library = 0 WHERE id = 200", [])
            .unwrap();
        add_link(&conn, 1, 200, "Flat");

        let r = readiness_for(&conn, 1);
        assert_eq!(r.frames[0].flat, RAW_SET, "precondition: an unbuilt raw link");
        assert!(r.cfa_warnings.is_empty(), "{:?}", r.cfa_warnings);
    }

    /// A superseded raw link resolves to its MASTER, and that master is what
    /// gets compared — the same set the engine will actually apply.
    #[test]
    fn readiness_compares_the_master_behind_a_superseded_link() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        set_light_bayer(&conn, 1, Some("RGGB"), Some(0), Some(0));
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat.fits"); // mono
        seed_set(&conn, 300, "Flat", false);
        supersede(&conn, 300, 101);
        add_link(&conn, 1, 300, "Flat"); // link still on the raw set

        let r = readiness_for(&conn, 1);
        assert_eq!(r.cfa_warnings.len(), 1, "{:?}", r.cfa_warnings);
        assert!(r.cfa_warnings[0].contains("Flat master"), "{}", r.cfa_warnings[0]);
    }

    /// spec §12.1: details report the tracking-row recipe (calstat, master
    /// FILENAMES, flat-norm mode) for every calibrated LIGHT member, omit
    /// members with no tracking row, and flip `stale` when the wanted
    /// normalization mode differs from what the row recorded.
    #[test]
    fn details_reports_recipe_masters_and_staleness() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);

        // Master dark #100 + master flat #101, each with a member file on disk.
        seed_master_with_file(&conn, 100, "MasterDark", "master_dark_120s.fits");
        seed_master_with_file(&conn, 101, "MasterFlat", "master_flat_Ha.fits");

        // Light 1 — calibrated: a tracking row whose links still match.
        seed_light(&conn, 1, session);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");
        upsert_light_calibration(
            &conn,
            &LightCalRow {
                id: 0,
                frame_id: Some(1),
                source_uuid: None,
                source_filename: Some("light_1.fits".to_string()),
                output_path: "/lib/M31/TestCam/2026-07-05/c_light_1.fits".to_string(),
                dark_set_id: Some(100),
                flat_set_id: Some(101),
                bias_set_id: None,
                calstat: "BDF".to_string(),
                flat_norm_applied: true,
                flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().to_string(),
                cal_params: "{}".to_string(),
                cfa_scaling_applied: None,
                output_hash: "deadbeef".to_string(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-07-05T00:00:00Z".to_string(),
            },
        )
        .unwrap();

        // Light 2 — has links but NO tracking row → absent from the result.
        seed_light(&conn, 2, session);
        add_link(&conn, 2, 100, "Dark");

        // Fresh: wanted prefs match the row → exactly one detail, not stale, with
        // the recipe pulled from the row and master FILENAMES resolved.
        let details = compute_details(
            &conn,
            1,
            true,
            FlatNormMode::CentralThird,
            LightCalParams::default(),
        )
        .unwrap();
        assert_eq!(details.len(), 1, "only the calibrated light is reported");
        let d = &details[0];
        assert_eq!(d.frame_id, 1);
        assert_eq!(d.calstat, "BDF");
        assert_eq!(d.dark_master.as_deref(), Some("master_dark_120s.fits"));
        assert_eq!(d.flat_master.as_deref(), Some("master_flat_Ha.fits"));
        assert_eq!(d.bias_master, None, "no bias link → no master name");
        assert!(d.flat_norm_applied);
        assert_eq!(d.flat_norm_mode, "centralThird");
        assert_eq!(d.engine_version, LIGHT_CAL_ENGINE_VERSION);
        assert_eq!(d.output_path, "/lib/M31/TestCam/2026-07-05/c_light_1.fits");
        assert!(!d.stale, "matching preferences → fresh");

        // A different wanted normalization statistic makes the row stale (the row
        // applied a normalized flat in centralThird; caller now wants trimmed).
        let stale = compute_details(
            &conn,
            1,
            true,
            FlatNormMode::PixinsightTrimmed,
            LightCalParams::default(),
        )
        .unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].stale, "a different flat-norm mode flips stale");
    }

    // ── F3: preflight-build handshake ───────────────────────────────────────

    /// The wait returns (not cancelled) once the last build handle is removed
    /// from the map — the ordering the handshake guarantees.
    #[test]
    fn wait_for_preflight_builds_returns_when_handles_clear() {
        let active: Arc<Mutex<HashMap<i64, ()>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut m = active.lock().unwrap();
            m.insert(1, ());
            m.insert(2, ());
        }
        let cancel = Arc::new(AtomicBool::new(false));

        let active2 = active.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            active2.lock().unwrap().remove(&1);
            std::thread::sleep(std::time::Duration::from_millis(80));
            active2.lock().unwrap().remove(&2);
        });

        let start = std::time::Instant::now();
        let cancelled = wait_for_preflight_builds(&active, &[1, 2], &cancel);
        remover.join().unwrap();
        assert!(!cancelled, "not cancelled — the builds completed");
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(120),
            "must wait until BOTH build handles are gone"
        );
        assert!(active.lock().unwrap().is_empty());
    }

    /// The cancel flag breaks the wait even while a build handle lingers.
    #[test]
    fn wait_for_preflight_builds_breaks_on_cancel() {
        let active: Arc<Mutex<HashMap<i64, ()>>> = Arc::new(Mutex::new(HashMap::new()));
        active.lock().unwrap().insert(7, ()); // never removed
        let cancel = Arc::new(AtomicBool::new(false));

        let cancel2 = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            cancel2.store(true, Ordering::SeqCst);
        });

        let cancelled = wait_for_preflight_builds(&active, &[7], &cancel);
        canceller.join().unwrap();
        assert!(cancelled, "cancel must break the wait even with a build handle still present");
    }

    /// No preflight builds → immediate no-op, never cancelled.
    #[test]
    fn wait_for_preflight_builds_empty_is_noop() {
        let active: Arc<Mutex<HashMap<i64, ()>>> = Arc::new(Mutex::new(HashMap::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(!wait_for_preflight_builds(&active, &[], &cancel));
    }

    // ── Bayer copy-through: catalog-column fallback ─────────────────────────

    /// Seed a stored header blob + the frame's Bayer columns for frame 1.
    fn seed_header_and_bayer(
        conn: &Connection,
        file_id: i64,
        header: &str,
        bayerpat: Option<&str>,
        xbayroff: Option<i64>,
        ybayroff: Option<i64>,
        roworder: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![file_id, header],
        )
        .unwrap();
        conn.execute(
            "UPDATE frames SET bayerpat = ?2, xbayroff = ?3, ybayroff = ?4, roworder = ?5
             WHERE file_id = ?1",
            params![file_id, bayerpat, xbayroff, ybayroff, roworder],
        )
        .unwrap();
    }

    fn card_str(cards: &[Card], keyword: &str) -> Option<String> {
        cards.iter().find(|c| c.keyword == keyword).and_then(|c| match &c.value {
            Some(CardValue::Str(s)) => Some(s.clone()),
            _ => None,
        })
    }

    fn card_int(cards: &[Card], keyword: &str) -> Option<i64> {
        cards.iter().find(|c| c.keyword == keyword).and_then(|c| match &c.value {
            Some(CardValue::Integer(i)) => Some(*i),
            _ => None,
        })
    }

    /// An XISF whose CFA is declared only by the `<ColorFilterArray>` element
    /// populates `frames.bayerpat` but leaves the stored blob (raw XML) without
    /// any BAYERPAT line. Copy-through reads the blob, so without the column
    /// fallback the calibrated output would carry no CFA geometry at all and
    /// could not be debayered downstream. A NULL column still adds nothing.
    #[test]
    fn bayer_cards_derived_from_columns_when_blob_is_silent() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "INSTRUME= 'TestCam'\nEXPTIME = 120.0\nEND",
            Some("RGGB"),
            Some(0),
            Some(1),
            None, // roworder unknown → nothing to derive
        );

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(0), "phase 0 is a real value, not absence");
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
        assert!(
            !cards.iter().any(|c| c.keyword == "ROWORDER"),
            "a NULL column must never be fabricated into a card"
        );
        // The blob's own cards are still there — the fallback appends, never replaces.
        assert_eq!(card_str(&cards, "INSTRUME").as_deref(), Some("TestCam"));
    }

    /// Precedence: whatever the file's own header declared wins. The column is
    /// a fallback for a silent blob, not an override of a present card.
    #[test]
    fn stored_header_bayer_cards_win_over_catalog_columns() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "BAYERPAT= 'GRBG'\nXBAYROFF= 1\nROWORDER= 'BOTTOM-UP'\nEND",
            Some("RGGB"),
            Some(0),
            Some(1),
            Some("TOP-DOWN"),
        );

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("GRBG"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(1));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("BOTTOM-UP"));
        // ...and only the blob one is present — no duplicate keyword pair.
        for kw in ["BAYERPAT", "XBAYROFF", "ROWORDER"] {
            assert_eq!(
                cards.iter().filter(|c| c.keyword == kw).count(),
                1,
                "{kw} must appear exactly once"
            );
        }
        // The one keyword the blob did NOT declare still comes from its column.
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
    }

    /// No stored header blob at all: nothing to copy through (and the caller is
    /// warned rather than silently shipping a bare calibrated header) — but the
    /// catalog columns are still derived. Sync-ingest inserts an EMPTY
    /// `fits_header` row while three scanner branches insert no row at all; the
    /// same information state must not produce opposite CFA outcomes.
    #[test]
    fn missing_header_blob_still_derives_bayer_cards_from_columns() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        conn.execute(
            "UPDATE frames SET bayerpat = 'RGGB', xbayroff = 0, ybayroff = 1,
             roworder = 'TOP-DOWN' WHERE file_id = ?1",
            params![file_id],
        )
        .unwrap();

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(0));
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("TOP-DOWN"));
        assert_eq!(cards.len(), 4, "only the derived cards — nothing to copy through");
    }

    /// …and with nothing in the columns either, a missing blob really does mean
    /// no cards: absence is never padded out with fabricated values.
    #[test]
    fn missing_header_blob_and_empty_columns_yield_no_cards() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let cards = source_cards_for_file(&conn, 1, 1 + 1_000_000, FileFormat::FITS).unwrap();
        assert!(cards.is_empty(), "no blob + no columns -> no cards");
    }

    /// A blank stored value is not a declaration. The XISF stored-header parser
    /// has no empty-value check, so `<FITSKeyword name="BAYERPAT" value=""/>`
    /// lands as an empty-string card — it must NOT out-rank a real
    /// `frames.bayerpat`, and it must be replaced rather than left beside the
    /// derived card (two cards for one keyword would contradict each other in
    /// the output header).
    #[test]
    fn blank_stored_header_value_loses_to_catalog_column() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "<xisf><FITSKeyword name=\"BAYERPAT\" value=\"\"/>\
             <FITSKeyword name=\"ROWORDER\" value=\"'  '\"/>\
             <FITSKeyword name=\"INSTRUME\" value=\"'TestCam'\"/></xisf>",
            Some("RGGB"),
            None,
            None,
            Some("TOP-DOWN"),
        );

        // Sanity: the parser really does hand back a blank BAYERPAT card here —
        // if that ever changes, this test's premise is gone, not just its assert.
        let raw: HashMap<String, String> = parse_stored_header_keys(
            FileFormat::XISF,
            &conn
                .query_row(
                    "SELECT header FROM fits_header WHERE file_id = ?1",
                    params![file_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
        );
        assert_eq!(raw.get("BAYERPAT").map(String::as_str), Some(""));

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::XISF).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("TOP-DOWN"));
        for kw in ["BAYERPAT", "ROWORDER"] {
            assert_eq!(
                cards.iter().filter(|c| c.keyword == kw).count(),
                1,
                "{kw}: the blank card must be replaced, not duplicated"
            );
        }
        // A non-blank blob card in the same header still wins its keyword.
        assert_eq!(card_str(&cards, "INSTRUME").as_deref(), Some("TestCam"));
    }
}
