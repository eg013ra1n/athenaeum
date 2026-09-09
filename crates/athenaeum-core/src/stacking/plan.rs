//! `StackingPlan` and the run gate (spec §2, §9.3, §9.4): what a stacking
//! run WOULD do for a frame set — its groups, every reason it must not start
//! yet, and which stages a previous run's cached artifacts can still cover —
//! without starting one. Pure DB reads plus cheap filesystem probes
//! (`std::fs::metadata`, `statvfs` via [`crate::stacking::paths::free_bytes`]);
//! no pixel I/O, no [`crate::services::ServiceContext`].
//!
//! [`build_plan`] is also the one place the three per-stage config-hash
//! helpers ([`calibration_hash_for`], [`measurement_hash_for`],
//! [`registration_hash_for`]) get their exact inputs pinned — Tasks 6 and 7
//! (the actual calibrate/measure/register stages) call the same functions to
//! decide what to store as an artifact's `config_hash`, so a run and the plan
//! that preceded it can never disagree about what "fresh" means.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::lights::{check_mode_ready, compute_export_readiness, ExportReadiness};
use crate::api::{ApiError, PathPolicy};
use crate::db::stacking::{active_run_for_set, find_artifact, get_set_config};
use crate::export::models::ExportMode;
use crate::export::{resolve_generation, resolved_master_paths};
use crate::registration::db::{
    get_frame_set_reference, get_registration_for_frame_set, RegistrationRecord,
};
use crate::settings::{keys, SettingsManager};
use crate::stacking::config::{
    calibration_subtree, config_hash, measurement_subtree, registration_subtree, resolve_config,
    stage_hash, ReferenceMode, SourceIdentity, StackingConfig,
};
use crate::stacking::groups::{group_frames, ColorMode, GroupFrame, IntegrationGroup};
use crate::stacking::paths::{self, EstimateInputs};

/// One pipeline stage (spec §10.2's `stage` enum, minus the `Ok`-only
/// distinction between "never run" and "cached"). [`Self::as_str`] is the
/// same spelling the progress/complete events use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Calibrate,
    Measure,
    Reference,
    Register,
    Normalize,
    Integrate,
    Drizzle,
    Output,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Calibrate => "calibrate",
            Stage::Measure => "measure",
            Stage::Reference => "reference",
            Stage::Register => "register",
            Stage::Normalize => "normalize",
            Stage::Integrate => "integrate",
            Stage::Drizzle => "drizzle",
            Stage::Output => "output",
        }
    }
}

/// One reason the plan refuses to run (or a strictly informational note the
/// tab should show even though it does not block). `code` is a stable
/// machine key the frontend switches on (`masters` | `links` |
/// `masterFiles` | `reference` | `folders` | `space` | `frames` |
/// `unsupported`); `message` is the sentence a human reads.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanBlocker {
    pub code: String,
    pub message: String,
}

/// One integration group as the plan would run it: [`IntegrationGroup`]'s
/// own fields, plus how many of its frames are actually in scope
/// (`included_count`, after manual exclusions) and how many already have a
/// fresh cached artifact for the first two per-frame stages.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanGroup {
    pub key: String,
    pub instrume: Option<String>,
    pub color_mode: ColorMode,
    pub filter: Option<String>,
    pub binning: i64,
    pub width: i64,
    pub height: i64,
    pub exposure_s: Option<f64>,
    pub frame_count: usize,
    pub included_count: usize,
    pub total_exposure_s: f64,
    pub calibrated_cached: usize,
    pub metrics_cached: usize,
}

/// The plan's resolved reference frame, or the lack of one. `Auto` never
/// blocks — the reference is only decided once a run actually weighs the
/// frames — so `frame_id`/`filename` stay `None` and `on_disk` is
/// meaningless (`false`) for it.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanReference {
    pub mode: ReferenceMode,
    pub frame_id: Option<i64>,
    pub filename: Option<String>,
    pub on_disk: bool,
}

/// What a stacking run would do for a frame set right now: its groups, the
/// resolved config and its hash, every blocking/informational reason
/// (`blockers`/`warnings`), the export-readiness tally the masters/links
/// gate is computed from, the resolved reference, folder/space state, and
/// which of the three cacheable per-frame stages a fresh run would still
/// have to redo (`stale_stages` — always a subset of `{Calibrate, Measure,
/// Register}`; the later stages have no cache in M1).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingPlan {
    pub set_id: i64,
    pub set_name: String,
    pub config: StackingConfig,
    pub config_hash: String,
    pub groups: Vec<PlanGroup>,
    pub blockers: Vec<PlanBlocker>,
    pub warnings: Vec<String>,
    pub readiness: ExportReadiness,
    pub reference: PlanReference,
    pub frame_count: usize,
    pub included_count: usize,
    pub excluded_frame_ids: Vec<i64>,
    pub estimate_bytes: u64,
    pub free_bytes: Option<u64>,
    pub working_dir: Option<String>,
    pub output_dir: Option<String>,
    pub stale_stages: Vec<Stage>,
    pub active_run_id: Option<i64>,
}

// ── config-hash helpers (spec §9.3) ─────────────────────────────────────────
//
// Every helper below is `pub(crate)`, not private: Task 6 (calibrate/measure)
// and Task 7 (register) call these SAME functions to compute the hash they
// store on a freshly written artifact — the plan and a run must derive
// "fresh" from identical inputs, or a run would immediately invalidate its
// own output the next time the plan is built.

/// Format one resolved master file's identity for stage-1 hashing:
/// `"<path>|<size>|<modified_at>"`. A metadata read failure (the master went
/// missing between the readiness check and this one) is logged and folded
/// into the string as `"<path>|missing"` rather than propagated — the
/// missing-master-files blocker already reports this state; the hash just
/// needs to keep changing while it persists.
fn master_identity(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified_at = meta
                .modified()
                .map(|m| chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339())
                .unwrap_or_default();
            format!("{}|{}|{modified_at}", path.display(), meta.len())
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "stacking: master file metadata read failed while hashing stage 1"
            );
            format!("{}|missing", path.display())
        }
    }
}

/// Stage 1 (calibration) config-hash for one LIGHT frame (spec §9.3):
/// `calibration_subtree(cfg)` + the identity of every resolved master path
/// this frame would actually apply ([`resolved_master_paths`]) + the light's
/// own `files` identity. Resolves the frame's calibration plan
/// (`export::resolve_generation`) to learn which masters apply — the same
/// resolution the calibrate stage itself performs — using the process temp
/// directory as the flat-norm-divisor scratch space (the same fallback
/// `api::export`/`api::sync_prepare` use when no run-specific scratch
/// directory is available; only read when a master flat carries no usable
/// `ATH_FNRM`/`ATH_FNR*` card, so the common case touches no scratch file at
/// all).
pub(crate) fn calibration_hash_for(
    conn: &Connection,
    cfg: &StackingConfig,
    frame: &GroupFrame,
) -> anyhow::Result<String> {
    let scratch_dir = std::env::temp_dir();
    let spec = resolve_generation(conn, frame.frame_id, &cfg.calibration, &scratch_dir)?;

    let mut specs = HashMap::with_capacity(1);
    specs.insert(frame.frame_id, spec);
    let master_paths = resolved_master_paths(&specs);

    let upstream: Vec<String> = master_paths.iter().map(|p| master_identity(p)).collect();
    let upstream_refs: Vec<&str> = upstream.iter().map(String::as_str).collect();

    let sources = [SourceIdentity {
        file_id: frame.file_id,
        size: frame.size,
        modified_at: frame.modified_at.clone(),
    }];

    Ok(stage_hash(
        &calibration_subtree(cfg),
        &upstream_refs,
        &sources,
    ))
}

/// [`calibration_hash_for`], but a resolution failure (no calibration linked
/// or resolvable, a source file gone) is logged and folded to `None` instead
/// of propagated — the plan treats "can't tell" the same as "stale" rather
/// than failing to build at all over a frame the masters/links blocker
/// already reports.
fn calibration_hash_or_warn(
    conn: &Connection,
    cfg: &StackingConfig,
    frame: &GroupFrame,
) -> Option<String> {
    match calibration_hash_for(conn, cfg, frame) {
        Ok(hash) => Some(hash),
        Err(error) => {
            tracing::warn!(
                frame_id = frame.frame_id,
                %error,
                "stacking: could not resolve stage-1 hash; treating as stale"
            );
            None
        }
    }
}

/// Stage 3 (measurement) config-hash for one frame: `measurement_subtree`
/// keyed on the frame's OWN stage-1 hash — never the DB row's stored one,
/// which may itself be stale (spec §9.3: stage 3 depends on stage 1).
pub(crate) fn measurement_hash_for(cfg: &StackingConfig, calibrated_hash: &str) -> String {
    stage_hash(&measurement_subtree(cfg), &[calibrated_hash], &[])
}

/// Stage 5 (registration) config-hash for one frame against a given
/// reference: `registration_subtree` keyed on the reference frame's identity
/// (`"ref:<id>"`, distinguishing a reference CHANGE from a reference whose
/// own calibration merely changed), the reference's own stage-1 hash, and
/// this frame's stage-1 hash.
pub(crate) fn registration_hash_for(
    cfg: &StackingConfig,
    reference_frame_id: i64,
    reference_calibrated_hash: &str,
    frame_calibrated_hash: &str,
) -> String {
    let reference_token = format!("ref:{reference_frame_id}");
    stage_hash(
        &registration_subtree(cfg),
        &[
            reference_token.as_str(),
            reference_calibrated_hash,
            frame_calibrated_hash,
        ],
        &[],
    )
}

// ── build_plan ──────────────────────────────────────────────────────────────

fn frame_set_name(conn: &Connection, frames_set_id: i64) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        params![frames_set_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| ApiError::NotFound(format!("frame set {frames_set_id} not found")))
}

fn read_global_config_json(conn: &Connection, settings: &SettingsManager) -> Option<String> {
    match settings.get_with_precedence(conn, keys::STACKING_DEFAULTS, "") {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(error) => {
            tracing::warn!(%error, "stacking: failed reading global defaults; using built-in defaults");
            None
        }
    }
}

/// Resolve the effective [`StackingConfig`] and the frame set's manual
/// exclusions. `config_override` (a caller-supplied, already-valid config —
/// e.g. the Stacking tab's unsaved edit) wins outright, without even reading
/// the stored/global JSON; otherwise the stored per-set config wins over the
/// global default, per [`resolve_config`]'s own precedence.
fn resolve_config_and_exclusions(
    conn: &Connection,
    settings: &SettingsManager,
    frames_set_id: i64,
    config_override: Option<StackingConfig>,
) -> Result<(StackingConfig, Vec<i64>), ApiError> {
    let set_row = get_set_config(conn, frames_set_id)?;
    let excluded_frame_ids = set_row
        .as_ref()
        .map(|r| r.excluded_frame_ids.clone())
        .unwrap_or_default();

    let config = match config_override {
        Some(cfg) => cfg,
        None => {
            let set_json = set_row.as_ref().map(|r| r.config_json.as_str());
            let global_json = read_global_config_json(conn, settings);
            resolve_config(set_json, global_json.as_deref())
                .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?
        }
    };

    Ok((config, excluded_frame_ids))
}

/// Resolve the plan's reference frame (gate step 2). `Auto` never blocks.
/// `Manual` requires a `frame_set_reference` row whose frame still resolves
/// to an on-disk file; either miss pushes a `reference` blocker and returns
/// an empty [`PlanReference`].
fn resolve_reference(
    conn: &Connection,
    frames_set_id: i64,
    cfg: &StackingConfig,
    blockers: &mut Vec<PlanBlocker>,
) -> Result<PlanReference, ApiError> {
    if cfg.reference.mode == ReferenceMode::Auto {
        return Ok(PlanReference {
            mode: ReferenceMode::Auto,
            frame_id: None,
            filename: None,
            on_disk: false,
        });
    }

    let no_reference_chosen = |blockers: &mut Vec<PlanBlocker>| {
        blockers.push(PlanBlocker {
            code: "reference".to_string(),
            message: "Choose a reference frame in Analysis".to_string(),
        });
        PlanReference {
            mode: ReferenceMode::Manual,
            frame_id: None,
            filename: None,
            on_disk: false,
        }
    };

    let Some(reference_row) = get_frame_set_reference(conn, frames_set_id)? else {
        return Ok(no_reference_chosen(blockers));
    };

    let file_row: Option<(String, String)> = conn
        .query_row(
            "SELECT fi.filename, fi.path FROM frames f JOIN files fi ON fi.id = f.file_id \
             WHERE f.id = ?1",
            params![reference_row.reference_frame_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let Some((filename, path)) = file_row else {
        return Ok(no_reference_chosen(blockers));
    };

    let on_disk = std::fs::metadata(&path).is_ok();
    if !on_disk {
        blockers.push(PlanBlocker {
            code: "reference".to_string(),
            message: format!("{filename} is not on disk"),
        });
    }

    Ok(PlanReference {
        mode: ReferenceMode::Manual,
        frame_id: Some(reference_row.reference_frame_id),
        filename: Some(filename),
        on_disk,
    })
}

fn find_group_frame<'a>(groups: &'a [IntegrationGroup], frame_id: i64) -> Option<&'a GroupFrame> {
    groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .find(|f| f.frame_id == frame_id)
}

/// Register-stage staleness (spec §9.3, gate step's `stale_stages`).
///
/// `Auto` reference mode cannot pre-verify a `config_hash` — the reference
/// itself is only decided once a run actually weighs the frames — so it
/// uses a looser reusability rule: every non-excluded frame must already
/// have a `registration_results` row whose `source_kind` is `"calibrated"`
/// (a v2-pipeline row, not a leftover from the old registration service).
///
/// `Manual` reusability additionally requires the row's `reference_frame_id`
/// to equal the plan's OWN reference, an `aligned`/`aligned_flipped`/
/// `reference` status, and a `config_hash` matching
/// [`registration_hash_for`] computed from the CURRENT stage-1 hashes of
/// both the reference and the frame — so a reference whose calibration
/// changed (or a frame whose own calibration did) makes registration stale
/// even though the row itself is untouched.
fn compute_register_stale(
    conn: &Connection,
    cfg: &StackingConfig,
    frames_set_id: i64,
    groups: &[IntegrationGroup],
    excluded: &HashSet<i64>,
    reference: &PlanReference,
) -> Result<bool, ApiError> {
    let rows = get_registration_for_frame_set(conn, frames_set_id)?;
    let by_frame: HashMap<i64, &RegistrationRecord> =
        rows.iter().map(|r| (r.frame_id, r)).collect();

    let included_frames: Vec<&GroupFrame> = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .filter(|f| !excluded.contains(&f.frame_id))
        .collect();

    if reference.mode == ReferenceMode::Auto {
        return Ok(included_frames
            .iter()
            .any(|f| match by_frame.get(&f.frame_id) {
                None => true,
                Some(row) => row.source_kind.as_deref() != Some("calibrated"),
            }));
    }

    let Some(reference_frame_id) = reference.frame_id else {
        return Ok(true);
    };
    let Some(reference_group_frame) = find_group_frame(groups, reference_frame_id) else {
        return Ok(true);
    };
    let Some(reference_calib_hash) = calibration_hash_or_warn(conn, cfg, reference_group_frame)
    else {
        return Ok(true);
    };

    for f in &included_frames {
        let Some(row) = by_frame.get(&f.frame_id) else {
            return Ok(true);
        };
        let Some(frame_calib_hash) = calibration_hash_or_warn(conn, cfg, f) else {
            return Ok(true);
        };
        let expected = registration_hash_for(
            cfg,
            reference_frame_id,
            &reference_calib_hash,
            &frame_calib_hash,
        );
        let reusable = row.reference_frame_id == reference_frame_id
            && matches!(
                row.status.as_str(),
                "aligned" | "aligned_flipped" | "reference"
            )
            && row.config_hash.as_deref() == Some(expected.as_str());
        if !reusable {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Build the plan for a frame set: its groups, the resolved config, every
/// gate blocker (spec §2, checked in a fixed order so the first blocker a
/// user sees is always the most fundamental one), and which of the three
/// cacheable per-frame stages a run would still have to redo. Pure DB reads
/// plus cheap filesystem probes — never starts a run, never touches
/// [`crate::services::ServiceContext`].
pub fn build_plan(
    conn: &Connection,
    settings: &SettingsManager,
    policy: &PathPolicy,
    frames_set_id: i64,
    config_override: Option<StackingConfig>,
) -> Result<StackingPlan, ApiError> {
    let set_name = frame_set_name(conn, frames_set_id)?;
    let (cfg, excluded_frame_ids) =
        resolve_config_and_exclusions(conn, settings, frames_set_id, config_override)?;
    let hash = config_hash(&cfg);

    let groups = group_frames(conn, frames_set_id, &cfg.grouping)?;
    let readiness = compute_export_readiness(conn, frames_set_id)?;

    let mut blockers: Vec<PlanBlocker> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Gate 1: masters/links/masterFiles — the export-v2 gate's own sentence,
    // just re-coded per the ordered readiness counts it was derived from.
    if let Err(message) = check_mode_ready(&readiness, ExportMode::CalibratedLights) {
        let code = if readiness.raw_sets_without_master > 0 {
            "masters"
        } else if readiness.unlinked_lights > 0 {
            "links"
        } else if readiness.missing_master_files > 0 {
            "masterFiles"
        } else {
            "links"
        };
        blockers.push(PlanBlocker {
            code: code.to_string(),
            message,
        });
    }

    // Gate 2: reference.
    let reference = resolve_reference(conn, frames_set_id, &cfg, &mut blockers)?;

    // Gate 3: folders — also resolves free space and the estimate's probe
    // target.
    let resolved_dirs = paths::resolve_dirs(conn, settings, &cfg.paths);
    let mut free_bytes: Option<u64> = None;
    let mut working_dir: Option<String> = None;
    let mut output_dir: Option<String> = None;
    match (resolved_dirs.working, resolved_dirs.output) {
        (Some(working), Some(output)) => {
            match paths::validate_dirs(conn, policy, &working, &output) {
                Ok(validated) => {
                    warnings.extend(validated.warnings);
                    working_dir = Some(validated.working.to_string_lossy().into_owned());
                    output_dir = Some(validated.output.to_string_lossy().into_owned());
                    free_bytes = paths::free_bytes(&validated.working);
                    if free_bytes.is_none() {
                        warnings.push("free space could not be determined".to_string());
                    }
                }
                Err(error) => {
                    blockers.push(PlanBlocker {
                        code: "folders".to_string(),
                        message: error.to_string(),
                    });
                }
            }
        }
        (working, output) => {
            if working.is_none() {
                blockers.push(PlanBlocker {
                    code: "folders".to_string(),
                    message: "Choose a working folder".to_string(),
                });
            }
            if output.is_none() {
                blockers.push(PlanBlocker {
                    code: "folders".to_string(),
                    message: "Choose an output folder".to_string(),
                });
            }
        }
    }

    let estimate_bytes = paths::estimate_bytes(&EstimateInputs {
        groups: &groups,
        write_registered: cfg.registration.write_registered_frames,
        write_maps: cfg.integration.write_rejection_maps,
    });

    // Gate 4: space.
    if let Some(free) = free_bytes {
        if free < estimate_bytes {
            let needed_gb = estimate_bytes as f64 / 1_000_000_000.0;
            let free_gb = free as f64 / 1_000_000_000.0;
            blockers.push(PlanBlocker {
                code: "space".to_string(),
                message: format!(
                    "Not enough free space: {needed_gb:.1} GB needed, {free_gb:.1} GB free"
                ),
            });
        }
    }

    // Per-group plan rows, plus stage-1/stage-3 staleness (gate 5's input and
    // the `stale_stages` output share this one pass over every frame).
    let excluded_set: HashSet<i64> = excluded_frame_ids.iter().copied().collect();
    let mut plan_groups = Vec::with_capacity(groups.len());
    let mut calibrate_stale = false;
    let mut measure_stale = false;
    let mut any_group_has_three_included = false;

    for g in &groups {
        let mut calibrated_cached = 0usize;
        let mut metrics_cached = 0usize;
        let mut included_count = 0usize;

        for f in &g.frames {
            let is_excluded = excluded_set.contains(&f.frame_id);
            if !is_excluded {
                included_count += 1;
            }

            let calib_artifact =
                find_artifact(conn, frames_set_id, &g.key, "calibrated", Some(f.frame_id))?;
            let metrics_artifact =
                find_artifact(conn, frames_set_id, &g.key, "metrics", Some(f.frame_id))?;

            // Only worth resolving the frame's current stage-1 hash when
            // there is an existing artifact to compare it against — a frame
            // that was never calibrated/measured is trivially stale at both
            // stages, and resolving it would error on a frame the
            // masters/links blocker already reports as unresolvable.
            let current_calib_hash = if calib_artifact.is_some() || metrics_artifact.is_some() {
                calibration_hash_or_warn(conn, &cfg, f)
            } else {
                None
            };

            let calib_fresh = match (&calib_artifact, &current_calib_hash) {
                (Some(row), Some(hash)) => {
                    row.config_hash == *hash
                        && row
                            .path
                            .as_deref()
                            .and_then(|p| std::fs::metadata(p).ok())
                            .is_some_and(|meta| Some(meta.len() as i64) == row.size)
                }
                _ => false,
            };
            if calib_fresh {
                calibrated_cached += 1;
            } else if !is_excluded {
                calibrate_stale = true;
            }

            let metrics_fresh = match (&metrics_artifact, &current_calib_hash) {
                (Some(row), Some(hash)) => row.config_hash == measurement_hash_for(&cfg, hash),
                _ => false,
            };
            if metrics_fresh {
                metrics_cached += 1;
            } else if !is_excluded {
                measure_stale = true;
            }
        }

        if included_count >= 3 {
            any_group_has_three_included = true;
        }

        plan_groups.push(PlanGroup {
            key: g.key.clone(),
            instrume: g.instrume.clone(),
            color_mode: g.color_mode,
            filter: g.filter.clone(),
            binning: g.binning,
            width: g.width,
            height: g.height,
            exposure_s: g.exposure_s,
            frame_count: g.frames.len(),
            included_count,
            total_exposure_s: g.total_exposure_s,
            calibrated_cached,
            metrics_cached,
        });
    }

    // Gate 5: frames.
    if !any_group_has_three_included {
        blockers.push(PlanBlocker {
            code: "frames".to_string(),
            message: "At least 3 included frames in one group".to_string(),
        });
    }

    let register_stale = compute_register_stale(
        conn,
        &cfg,
        frames_set_id,
        &groups,
        &excluded_set,
        &reference,
    )?;

    let mut stale_stages = Vec::new();
    if calibrate_stale {
        stale_stages.push(Stage::Calibrate);
    }
    if measure_stale {
        stale_stages.push(Stage::Measure);
    }
    if register_stale {
        stale_stages.push(Stage::Register);
    }

    // Gate 6: unsupported — checked last, so a user only sees these once
    // everything more fundamental is already in order.
    if cfg.normalization.local.enabled {
        blockers.push(PlanBlocker {
            code: "unsupported".to_string(),
            message: "Local normalization arrives in M2".to_string(),
        });
    }
    if cfg.drizzle.enabled {
        blockers.push(PlanBlocker {
            code: "unsupported".to_string(),
            message: "Drizzle arrives in M3".to_string(),
        });
    }

    let frame_count: usize = groups.iter().map(|g| g.frames.len()).sum();
    let included_count: usize = plan_groups.iter().map(|g| g.included_count).sum();
    let active_run_id = active_run_for_set(conn, frames_set_id)?;

    tracing::debug!(
        set_id = frames_set_id,
        groups = plan_groups.len(),
        frame_count,
        included_count,
        blockers = blockers.len(),
        stale_stages = stale_stages.len(),
        "stacking plan built"
    );

    Ok(StackingPlan {
        set_id: frames_set_id,
        set_name,
        config: cfg,
        config_hash: hash,
        groups: plan_groups,
        blockers,
        warnings,
        readiness,
        reference,
        frame_count,
        included_count,
        excluded_frame_ids,
        estimate_bytes,
        free_bytes,
        working_dir,
        output_dir,
        stale_stages,
        active_run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::stacking::set_set_config;
    use crate::registration::db::set_frame_set_reference;
    use crate::stacking::test_fixtures::{self, LightSpec};

    fn light_spec<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
        LightSpec {
            stem,
            instrume: "cam",
            filter: None,
            binning: 1,
            width: 64,
            height: 48,
            exptime: 60.0,
            date_obs,
            bayerpat: None,
            write_file: false,
        }
    }

    #[test]
    fn plan_blocks_without_masters() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
        ]
        .iter()
        .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert_eq!(plan.included_count, 3);
        assert!(
            plan.blockers.iter().any(|b| b.code == "links"),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn plan_ready_with_masters_and_folders() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
        ]
        .iter()
        .enumerate()
        {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &f.conn,
            keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &f.conn,
            keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(
            plan.stale_stages,
            vec![Stage::Calibrate, Stage::Measure, Stage::Register]
        );
        assert!(plan.estimate_bytes > 0);
    }

    #[test]
    fn manual_reference_must_exist_on_disk() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
        ]
        .iter()
        .enumerate()
        {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        // `write_file: false` in `light_spec` means the catalog's `files.path`
        // never had anything written there — exactly a reference frame whose
        // file is gone.
        set_frame_set_reference(&f.conn, f.set_id, ids[0]).unwrap();

        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        assert_eq!(plan.reference.frame_id, Some(ids[0]));
        assert!(!plan.reference.on_disk);
        let blocker = plan
            .blockers
            .iter()
            .find(|b| b.code == "reference")
            .expect("reference blocker");
        assert!(blocker.message.ends_with("is not on disk"), "{blocker:?}");
    }

    #[test]
    fn too_few_frames_is_a_blocker() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in ["2025-01-01T00:00:00", "2025-01-01T00:05:00"]
            .iter()
            .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(
            plan.blockers.iter().any(|b| b.code == "frames"),
            "{:?}",
            plan.blockers
        );
    }

    #[test]
    fn manual_exclusions_count() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
            "2025-01-01T00:15:00",
        ]
        .iter()
        .enumerate()
        {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        set_set_config(&f.conn, f.set_id, "{}", &[ids[0]]).unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert_eq!(plan.included_count, 3);
        assert_eq!(plan.excluded_frame_ids, vec![ids[0]]);
    }

    #[test]
    fn local_normalization_is_unsupported_in_m1() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
        ]
        .iter()
        .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        let blocker = plan
            .blockers
            .iter()
            .find(|b| b.code == "unsupported")
            .expect("unsupported blocker");
        assert_eq!(blocker.message, "Local normalization arrives in M2");
    }

    #[test]
    fn stage_as_str_matches_the_wire_spelling() {
        for (s, name) in [
            (Stage::Calibrate, "calibrate"),
            (Stage::Measure, "measure"),
            (Stage::Reference, "reference"),
            (Stage::Register, "register"),
            (Stage::Normalize, "normalize"),
            (Stage::Integrate, "integrate"),
            (Stage::Drizzle, "drizzle"),
            (Stage::Output, "output"),
        ] {
            assert_eq!(s.as_str(), name);
        }
    }
}
