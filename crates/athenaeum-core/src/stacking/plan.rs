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
use crate::db::stacking::{active_run_for_set, find_artifact, get_set_config, StackingArtifactRow};
use crate::export::models::ExportMode;
use crate::export::{resolve_generation_cached, resolved_master_paths, DivisorCache};
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
/// into the string as `"<path>|missing"` rather than propagated. That token
/// is a CONSTANT, not a changing one — it keeps the hash *defined* (so a
/// caller can still store/compare it) rather than panicking or aborting the
/// whole plan; it is the `masterFiles` blocker, not this string, that does
/// the actual refusing while the file stays gone.
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
/// (`export::resolve_generation_cached`) to learn which masters apply — the
/// same resolution the calibrate stage itself performs — using the process
/// temp directory as the flat-norm-divisor scratch space (the same fallback
/// `api::export`/`api::sync_prepare` use when no run-specific scratch
/// directory is available; only read when a master flat carries no usable
/// `ATH_FNRM`/`ATH_FNR*` card, so the common case touches no scratch file at
/// all).
///
/// `divisors` is the caller's [`DivisorCache`] — a build resolves every
/// frame in a frame set, and a set overwhelmingly shares one flat per group,
/// so a fresh cache per call (the original round's mistake) re-reads that
/// whole flat plane once per frame instead of once per (flat, mosaic phase)
/// pair. **Callers within this module never call this directly — go through
/// [`HashMemo::calibration_hash`]**, which also memoizes the RESULT per
/// frame for the life of one [`build_plan`] call; this function itself does
/// no memoization of its own.
pub(crate) fn calibration_hash_for(
    conn: &Connection,
    cfg: &StackingConfig,
    frame: &GroupFrame,
    divisors: &mut DivisorCache,
) -> Result<String, ApiError> {
    let scratch_dir = std::env::temp_dir();
    let spec = resolve_generation_cached(
        conn,
        frame.frame_id,
        &cfg.calibration,
        &scratch_dir,
        divisors,
    )?;

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

/// One [`build_plan`] call's memo: a single [`DivisorCache`] shared by every
/// frame this build resolves, plus the per-frame stage-1 hash result itself
/// (`None` on a resolution failure — no calibration linked or resolvable, a
/// source file gone), so a frame whose hash is needed twice in one build
/// (once for its own `calibrated`/`metrics` freshness, again as the
/// [`compute_register_stale`] reference) is resolved — and, on failure,
/// warned about — exactly once.
///
/// `pub(crate)`: Task 6's run holds ONE of these for its whole calibrate
/// stage (the same "one `DivisorCache`, memoized per frame" contract this
/// doc describes for a `build_plan` call), via [`Self::calibration_hash_checked`].
pub(crate) struct HashMemo {
    divisors: DivisorCache,
    by_frame: HashMap<i64, Option<String>>,
}

impl HashMemo {
    pub(crate) fn new() -> Self {
        HashMemo {
            divisors: DivisorCache::new(),
            by_frame: HashMap::new(),
        }
    }

    /// This frame's current stage-1 hash, memoized. The plan treats "can't
    /// tell" the same as "stale" rather than failing the whole build over a
    /// frame the masters/links blocker already reports as unresolvable.
    fn calibration_hash(
        &mut self,
        conn: &Connection,
        cfg: &StackingConfig,
        frame: &GroupFrame,
    ) -> Option<String> {
        if let Some(cached) = self.by_frame.get(&frame.frame_id) {
            return cached.clone();
        }
        let result = match calibration_hash_for(conn, cfg, frame, &mut self.divisors) {
            Ok(hash) => Some(hash),
            Err(error) => {
                tracing::warn!(
                    frame_id = frame.frame_id,
                    %error,
                    "stacking: could not resolve stage-1 hash; treating as stale"
                );
                None
            }
        };
        self.by_frame.insert(frame.frame_id, result.clone());
        result
    }

    /// [`Self::calibration_hash`]'s Result-returning counterpart, for a
    /// caller that needs the real failure text instead of the plan's
    /// collapse-to-`None` (Task 6's calibrate stage: a resolution failure
    /// here becomes the frame's exclusion reason, exactly like a pixel-phase
    /// failure). Shares the same memo and the same one
    /// [`calibration_hash_for`] call per successfully-resolved frame: a hit
    /// against a remembered success returns `Ok` without recomputing; a miss
    /// OR a hit against a remembered FAILURE (`None` — the text itself was
    /// never kept) calls `calibration_hash_for` fresh and returns its
    /// `Result` directly, so the caller sees the actual error.
    pub(crate) fn calibration_hash_checked(
        &mut self,
        conn: &Connection,
        cfg: &StackingConfig,
        frame: &GroupFrame,
    ) -> Result<String, ApiError> {
        if let Some(cached) = self.by_frame.get(&frame.frame_id).and_then(|o| o.as_ref()) {
            return Ok(cached.clone());
        }
        let result = calibration_hash_for(conn, cfg, frame, &mut self.divisors);
        self.by_frame
            .insert(frame.frame_id, result.as_ref().ok().cloned());
        result
    }

    /// The memo's own [`DivisorCache`], for a caller (Task 6's calibrate
    /// stage) that needs to call [`resolve_generation_cached`] a SECOND time
    /// itself — once to build the stage-1 hash (via
    /// [`Self::calibration_hash_checked`] above), once more to get the
    /// actual [`crate::export::GenerationSpec`] to execute. Sharing this
    /// cache across both calls is the whole point of holding one `HashMemo`
    /// per run: a flat's divisor is still resolved at most once per (flat,
    /// mosaic phase) pair even though the run touches it via two different
    /// call sites.
    pub(crate) fn divisors_mut(&mut self) -> &mut DivisorCache {
        &mut self.divisors
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

/// The `calibrated`-artifact reuse contract (spec §9.3): a row is reusable
/// only when ALL of these hold — its `config_hash` equals the frame's
/// CURRENT stage-1 hash, its `path` is `Some`, its `size` is `Some`, AND a
/// live `std::fs::metadata` read of that path reports exactly that size. A
/// row missing `path`/`size` (an artifact recorded before the pixel phase
/// ever wrote anything, or one some other kind stores without a path) is
/// NEVER assumed fresh — "we don't know" is "stale", not "trust the hash
/// alone". A `metrics` artifact has no equivalent disk check: it may be
/// `payload_json`-only with no file to verify (see the `Measure` staleness
/// check in [`build_plan`], which compares only the hash).
///
/// `pub(crate)`: `run.rs`'s calibrate stage (Task 6) calls this SAME rule to
/// decide whether an existing `calibrated` artifact can be reused instead of
/// regenerated — the plan and the run it precedes must never disagree about
/// what "fresh" means.
pub(crate) fn is_fresh(artifact: &StackingArtifactRow, current_hash: &str) -> bool {
    artifact.config_hash == current_hash
        && artifact
            .path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .is_some_and(|meta| Some(meta.len() as i64) == artifact.size)
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
///
/// The reference's own hash is resolved ONLY in `Manual` mode with
/// `reference.on_disk` already `true` — never unconditionally. A manual set
/// with no reference chosen yet, or one whose reference file is already
/// known missing (the `reference` gate blocker reported it), is stale by
/// construction without a second resolution attempt (and its own `warn!`)
/// over a frame the plan already knows is unusable.
fn compute_register_stale(
    conn: &Connection,
    cfg: &StackingConfig,
    frames_set_id: i64,
    groups: &[IntegrationGroup],
    excluded: &HashSet<i64>,
    reference: &PlanReference,
    memo: &mut HashMemo,
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

    let (Some(reference_frame_id), true) = (reference.frame_id, reference.on_disk) else {
        return Ok(true);
    };
    let Some(reference_group_frame) = find_group_frame(groups, reference_frame_id) else {
        return Ok(true);
    };
    let Some(reference_calib_hash) = memo.calibration_hash(conn, cfg, reference_group_frame) else {
        return Ok(true);
    };

    for f in &included_frames {
        let Some(row) = by_frame.get(&f.frame_id) else {
            return Ok(true);
        };
        let Some(frame_calib_hash) = memo.calibration_hash(conn, cfg, f) else {
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

    let excluded_set: HashSet<i64> = excluded_frame_ids.iter().copied().collect();

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

    // The estimate counts only IN-SCOPE frames — a manually excluded frame
    // is never calibrated/registered/integrated, so it must not inflate the
    // footprint a run would actually leave behind.
    let estimate_groups: Vec<IntegrationGroup> = groups
        .iter()
        .map(|g| {
            let mut g = g.clone();
            g.frames.retain(|f| !excluded_set.contains(&f.frame_id));
            g
        })
        .collect();
    let estimate_bytes = paths::estimate_bytes(&EstimateInputs {
        groups: &estimate_groups,
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
    // the `stale_stages` output share this one pass over every frame). One
    // `HashMemo` for the whole build: it owns the one `DivisorCache` a build
    // resolves every frame through, and memoizes each frame's own stage-1
    // hash so this loop and `compute_register_stale` below never resolve
    // the same frame twice.
    let mut memo = HashMemo::new();
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
                memo.calibration_hash(conn, &cfg, f)
            } else {
                None
            };

            let calib_fresh = match (&calib_artifact, &current_calib_hash) {
                (Some(row), Some(hash)) => is_fresh(row, hash),
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
        &mut memo,
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

    /// [`light_spec`], but with a real on-disk FITS — needed whenever a test
    /// actually resolves a frame's calibration plan (`calibration_hash_for`,
    /// which every fresh-path test below drives directly or through
    /// `build_plan`) or needs a `reference.on_disk == true` reference frame.
    fn light_spec_written<'a>(stem: &'a str, date_obs: &'a str) -> LightSpec<'a> {
        LightSpec {
            write_file: true,
            ..light_spec(stem, date_obs)
        }
    }

    const THREE_TIMES: [&str; 3] = [
        "2025-01-01T00:00:00",
        "2025-01-01T00:05:00",
        "2025-01-01T00:10:00",
    ];

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
        let settings = SettingsManager::new();
        let plan_all =
            build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert_eq!(plan_all.included_count, 4);

        set_set_config(&f.conn, f.set_id, "{}", &[ids[0]]).unwrap();

        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert_eq!(plan.included_count, 3);
        assert_eq!(plan.excluded_frame_ids, vec![ids[0]]);
        assert!(
            plan.estimate_bytes < plan_all.estimate_bytes,
            "the estimate must count only in-scope frames: {} vs {}",
            plan.estimate_bytes,
            plan_all.estimate_bytes
        );
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

    /// The artifact reuse contract (`is_fresh`, fix round 1 item 3): a
    /// `calibrated` row with the CURRENT hash but no `path`/`size` is never
    /// assumed fresh, and neither is one whose `path`/`size` no longer
    /// describe a real file on disk.
    #[test]
    fn artifact_without_path_or_size_is_stale() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let cfg = StackingConfig::default();
        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1);
        let group_key = groups[0].key.clone();
        let frame = groups[0].frames[0].clone();

        let mut divisors = DivisorCache::new();
        let hash = calibration_hash_for(&f.conn, &cfg, &frame, &mut divisors).unwrap();

        // `path: None` — the hash matches, but there is no file to trust.
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                frames_set_id: f.set_id,
                frame_id: Some(frame.frame_id),
                group_key: &group_key,
                kind: "calibrated",
                path: None,
                config_hash: &hash,
                size: None,
                modified_at: None,
                payload_json: None,
            },
        )
        .unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg.clone()),
        )
        .unwrap();
        assert!(
            plan.stale_stages.contains(&Stage::Calibrate),
            "{:?}",
            plan.stale_stages
        );
        assert_eq!(plan.groups[0].calibrated_cached, 0);

        // A real file at `path`, but the recorded `size` disagrees with it.
        let path = f.dir.path().join("mismatch.fits");
        std::fs::write(&path, [0u8; 5]).unwrap();
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                frames_set_id: f.set_id,
                frame_id: Some(frame.frame_id),
                group_key: &group_key,
                kind: "calibrated",
                path: Some(path.to_str().unwrap()),
                config_hash: &hash,
                size: Some(999),
                modified_at: None,
                payload_json: None,
            },
        )
        .unwrap();

        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();
        assert!(
            plan2.stale_stages.contains(&Stage::Calibrate),
            "{:?}",
            plan2.stale_stages
        );
        assert_eq!(plan2.groups[0].calibrated_cached, 0);
    }

    /// The fresh path, end to end: every frame gets a matching `calibrated`
    /// artifact (real file, recorded size), a matching `metrics` artifact,
    /// and (manual reference mode) a matching `registration_results` row —
    /// `stale_stages` comes back empty and both cache counters read the
    /// group's full frame count. Then one non-reference frame's
    /// registration row is flipped to a hash that no longer matches, and
    /// ONLY `Register` goes stale — Calibrate/Measure are untouched by a
    /// change that is registration-only.
    #[test]
    fn fresh_artifacts_and_registration_leave_nothing_stale() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
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

        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        set_frame_set_reference(&f.conn, f.set_id, ids[0]).unwrap();

        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");
        let group_key = groups[0].key.clone();

        let mut divisors = DivisorCache::new();
        let mut calib_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            calib_hashes.insert(gf.frame_id, hash);
        }

        for gf in &groups[0].frames {
            let hash = calib_hashes.get(&gf.frame_id).unwrap().clone();

            let cal_path = f
                .dir
                .path()
                .join(format!("calibrated_{}.fits", gf.frame_id));
            std::fs::write(&cal_path, [0u8]).unwrap();
            let size = std::fs::metadata(&cal_path).unwrap().len() as i64;
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    frames_set_id: f.set_id,
                    frame_id: Some(gf.frame_id),
                    group_key: &group_key,
                    kind: "calibrated",
                    path: Some(cal_path.to_str().unwrap()),
                    config_hash: &hash,
                    size: Some(size),
                    modified_at: None,
                    payload_json: None,
                },
            )
            .unwrap();

            let metrics_hash = measurement_hash_for(&cfg, &hash);
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    frames_set_id: f.set_id,
                    frame_id: Some(gf.frame_id),
                    group_key: &group_key,
                    kind: "metrics",
                    path: None,
                    config_hash: &metrics_hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some("{}"),
                },
            )
            .unwrap();
        }

        let reference_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        for gf in &groups[0].frames {
            let frame_hash = calib_hashes.get(&gf.frame_id).unwrap();
            let expected = registration_hash_for(&cfg, ids[0], &reference_hash, frame_hash);
            let is_reference = gf.frame_id == ids[0];
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id: gf.frame_id,
                reference_frame_id: ids[0],
                is_reference,
                status: if is_reference { "reference" } else { "aligned" }.to_string(),
                compute_time_ms: 0,
                registered_at: "2025-01-01T00:00:00Z".to_string(),
                config_hash: Some(expected),
                source_kind: Some("calibrated".to_string()),
                ..RegistrationRecord::default()
            };
            crate::registration::db::upsert_registration(&f.conn, &rec).unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg.clone()),
        )
        .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(plan.stale_stages.is_empty(), "{:?}", plan.stale_stages);
        assert_eq!(plan.groups[0].calibrated_cached, plan.groups[0].frame_count);
        assert_eq!(plan.groups[0].metrics_cached, plan.groups[0].frame_count);

        // Flip one non-reference frame's registration row to a hash that no
        // longer matches — only Register should go stale.
        let flipped_frame = groups[0]
            .frames
            .iter()
            .find(|gf| gf.frame_id != ids[0])
            .expect("a non-reference frame exists");
        let bad = RegistrationRecord {
            frames_set_id: f.set_id,
            frame_id: flipped_frame.frame_id,
            reference_frame_id: ids[0],
            is_reference: false,
            status: "aligned".to_string(),
            compute_time_ms: 0,
            registered_at: "2025-01-01T00:00:00Z".to_string(),
            config_hash: Some("stale-hash".to_string()),
            source_kind: Some("calibrated".to_string()),
            ..RegistrationRecord::default()
        };
        crate::registration::db::upsert_registration(&f.conn, &bad).unwrap();

        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();
        assert_eq!(
            plan2.stale_stages,
            vec![Stage::Register],
            "{:?}",
            plan2.stale_stages
        );
    }

    /// Blocker order is stable and matches the gate order exactly (fix
    /// round 1 item 7): no calibration links (masters/links) fires first,
    /// then both folder sentences (in `working`, `output` order), then
    /// `frames` (fewer than 3 included), then both `unsupported` toggles —
    /// `reference`/`space` never appear here (Auto mode; free space is
    /// never probed once the folders themselves are blocked).
    #[test]
    fn blocker_order_is_stable() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in ["2025-01-01T00:00:00", "2025-01-01T00:05:00"]
            .iter()
            .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;
        cfg.drizzle.enabled = true;

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(
            codes,
            vec![
                "links",
                "folders",
                "folders",
                "frames",
                "unsupported",
                "unsupported"
            ],
            "{:?}",
            plan.blockers
        );
        assert_eq!(plan.blockers[1].message, "Choose a working folder");
        assert_eq!(plan.blockers[2].message, "Choose an output folder");
    }
}
