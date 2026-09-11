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

use crate::api::lights::{compute_export_readiness_for_frames, ExportReadiness};
use crate::api::{ApiError, PathPolicy};
use crate::db::stacking::{
    active_run_for_set, find_artifact, get_set_config, list_frame_rows, list_runs,
    StackingArtifactRow,
};
use crate::export::{resolve_generation_cached, resolved_master_paths, DivisorCache};
use crate::integration::stats::RejectionNormalization;
use crate::registration::db::{
    get_frame_set_reference, get_registration_for_frame_set, RegistrationRecord,
};
use crate::settings::{keys, SettingsManager};
use crate::stacking::config::{
    calibration_subtree, config_hash, measurement_subtree, normalization_subtree,
    registration_subtree, resolve_config, stage_hash, ReferenceMode, SourceIdentity,
    StackingConfig,
};
use crate::stacking::groups::{
    group_frames, median_f64, ColorMode, GroupFrame, IntegrationGroup, ScaleSource,
};
use crate::stacking::paths::{self, EstimateInputs};
use crate::stacking::register::{RegistrationGeometry, SCALE_TOLERANCE};
use crate::stacking::run::group_anchor_geometry;

/// One pipeline stage (spec §10.2's `stage` enum, minus the `Ok`-only
/// distinction between "never run" and "cached"). [`Self::as_str`] is the
/// same spelling the progress/complete events use.
///
/// `Masters` (spec §2 row 0.5, owner requirement 2026-09-09 — "the pipeline
/// builds its own masters") is FIRST: it runs before `Calibrate`, building or
/// rebuilding every [`PlanMaster`] the gate listed as planned work rather
/// than a blocker. It carries no cache of its own (unlike `Calibrate`/
/// `Measure`/`Register`, which `stale_stages` tracks) — a master is either
/// already on disk (nothing to do) or it isn't (planned work every run
/// re-attempts), so `stale_stages` never lists it (see [`build_plan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Masters,
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
            Stage::Masters => "masters",
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

/// One [`PlanMaster`]'s kind of work: `Build` a raw calibration set that has
/// never had a master (`ExportReadiness::raw_sets_buildable`), or `Rebuild`
/// a built master whose file is missing but whose source is still
/// recoverable (`ExportReadiness::masters_rebuildable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum MasterWork {
    Build,
    Rebuild,
}

impl MasterWork {
    pub fn as_str(self) -> &'static str {
        match self {
            MasterWork::Build => "build",
            MasterWork::Rebuild => "rebuild",
        }
    }
}

/// One master the stage-0.5 run would build or rebuild (spec §2 row 0.5).
/// `set_id` follows [`ExportReadiness`]'s own two lists: for `Build` it is
/// the RAW calibration set id (`raw_sets_buildable`); for `Rebuild` it is the
/// MASTER calibration set id (`masters_rebuildable`) — `stacking::run`'s
/// `stage_masters` resolves a `Rebuild` item's source set itself, via
/// `crate::api::masters::resolve_rebuild_target`. `frame_count` is always the
/// count of RAW frames that would actually be combined — the source set's
/// own `calibration_set.frame_count` in both cases (a master set is always
/// `frame_count = 1` by invariant, which would say nothing useful about the
/// work about to happen).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanMaster {
    pub set_id: i64,
    pub kind: MasterWork,
    pub imagetyp: String,
    pub frame_count: i64,
    pub label: String,
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
///
/// `instrume`/`cameras` (owner decision 2026-09-10): a group is
/// camera-agnostic now — `cameras` lists every distinct camera actually
/// present, `instrume` is a DISPLAY-only value (the reference-anchor
/// member's own camera). There is no group-level `width`/`height` any
/// more — a group's frames may differ in native geometry; see
/// `GroupFrame.width`/`height` for the per-frame fact.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanGroup {
    pub key: String,
    pub instrume: Option<String>,
    pub color_mode: ColorMode,
    pub filter: Option<String>,
    pub binning: i64,
    pub cameras: Vec<String>,
    pub exposure_s: Option<f64>,
    pub frame_count: usize,
    pub included_count: usize,
    pub total_exposure_s: f64,
    pub calibrated_cached: usize,
    pub metrics_cached: usize,
    /// Frames (of `included_count`) whose `.athln` sidecar already exists on
    /// disk (spec §9.3, M2) — `0` when local normalization is off. A
    /// PRESENCE check, not a hash-verified freshness one: see the doc on
    /// this field's computation in [`build_plan`] for why (the LN reference
    /// member list is a stage-3 weight quantity, unavailable at plan time).
    pub ln_cached: usize,
    /// M3 Task 5: the group's reference-anchor member's own native
    /// `NAXIS1`/`NAXIS2` — the SAME member and the SAME convention
    /// `stacking::run::group_anchor_geometry` uses to stamp
    /// `stacking_run_groups.width`/`height` at insert time (a plan and the
    /// run it precedes must never disagree about which member anchors a
    /// group's geometry). `None` only for an (unreachable in practice)
    /// empty group. Frontend's Task 6 drizzle estimate line reads these —
    /// the run's own actual reference geometry is not known this early.
    pub anchor_width: Option<i64>,
    pub anchor_height: Option<i64>,
    /// M4b: this group's own pixel scale — the median of whichever members
    /// have one (`IntegrationGroup::pixel_scale_arcsec`, propagated
    /// verbatim). `None` when no member has a usable scale.
    pub pixel_scale_arcsec: Option<f64>,
    /// M4b: `pixel_scale_arcsec / reference_scale`, where `reference_scale`
    /// is the plan's resolved reference frame's OWN pixel scale. `None`
    /// when either side is unavailable — the reference isn't resolved yet
    /// (`Auto` mode with no prior run) or carries no scale, or this group
    /// has none of its own. A ratio outside `[1 / SCALE_TOLERANCE,
    /// SCALE_TOLERANCE]` is what the plan's "far from reference" warning
    /// (never a blocker) is about.
    pub scale_ratio_to_reference: Option<f64>,
    /// M4b: whether `pixel_scale_arcsec` rests entirely on measured plate
    /// solves (`Solve`) or includes at least one member whose scale came
    /// from the header's focal length/pixel size instead (`Header`) —
    /// the UI's `~` prefix on a header-implied number. `None` alongside
    /// `pixel_scale_arcsec == None` (no member has a scale at all).
    pub scale_source: Option<ScaleSource>,
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
    /// Stage 0.5's work list (spec §2 row 0.5, owner requirement 2026-09-09):
    /// every buildable raw set and rebuildable missing master, sorted by
    /// [`crate::api::masters::type_build_rank`] then id — bias/darkflat
    /// before dark before flat, the same dependency order
    /// `start_master_builds_batch` submits a manual batch in, so a flat
    /// built by stage 0.5 sees its own precal master already on disk.
    pub masters_to_build: Vec<PlanMaster>,
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

/// The group's LN reference artifact payload (fix round 1, item 5, spec
/// §9.3): the resolved `reference_member_ids` (ruling R-M3-17 v2:
/// sky-penalized-score-ordered — `s_i = weight.normalized_mean /
/// sqrt(background)`, not raw weight — the top `referenceFrames` included
/// members by that score, spec §4.4/§5.2) and the reference's OWN
/// [`normalization_hash_for`] value (redundant with the
/// artifact row's own `config_hash` column, kept here too so [`build_plan`]
/// needs only this one `payload_json` parse to verify every `ln` row — it
/// never re-reads the `ln_reference` row's `config_hash` column
/// separately). Written by `stacking::run`'s `build_and_write_ln_reference`;
/// read by [`build_plan`] to verify per-frame `ln` rows WITHOUT the stage-3
/// weight ranking that produced the member list in the first place — the
/// documented residual: a weight-driven member-list change (as opposed to a
/// config or registration change) is invisible to this DB-only check until
/// a real run updates the stored list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LnReferencePayload {
    pub reference_member_ids: Vec<i64>,
    pub reference_hash: String,
}

/// Stage 6 (local normalization, M2) config-hash: `normalization_subtree`
/// keyed on `registration_hash` — either this ONE frame's own stage-5 hash
/// (the `ln` artifact, per frame) or a caller-combined token standing in for
/// EVERY reference member's stage-5 hash (the `ln_reference` artifact, one
/// per group — see `stacking::run`'s own construction of that token, since
/// there is no single frame to key it on) — plus `reference_member_ids`, the
/// group's chosen LN reference frame ids (the sky-penalized-score-ordered
/// top `referenceFrames` included members, ruling R-M3-17 v2, spec §4.4/
/// §5.2): a changed member list invalidates every
/// frame's sidecar AND the reference itself, even when neither `cfg` nor
/// this frame's own registration changed. `reference_hash` (fix round 1,
/// item 2) is the group's OWN LN-reference hash — empty (`""`) when
/// computing THAT hash itself (it has no further reference to fold in),
/// otherwise every per-frame `ln` hash folds it in too: re-registering or
/// recalibrating ONE reference member rebuilds the reference (a new
/// `B_ref`, a new scale anchor) and must invalidate every OTHER frame's
/// sidecar as well, not just that one member's — folding in
/// `reference_member_ids` alone (the SET of ids, unchanged by a member's own
/// re-registration) could not catch that. No DB access here (unlike
/// [`calibration_hash_for`]'s `SourceIdentity`s) — the caller has already
/// resolved every hash; `sources` stays empty, same convention as
/// [`registration_hash_for`]'s own plain-string upstream. `registration_hash`
/// is a plain caller-supplied string with no NULL handling of its own — a
/// `NULL` stored `registration_results.config_hash` must be turned into
/// `""` by BOTH sides before it reaches here (M12, final fix wave):
/// `run.rs`'s writer already does (`unwrap_or_default()`), and
/// `plan.rs`'s own freshness read now mirrors it, so a frame with a
/// genuinely-NULL config hash is not reported permanently stale by the plan
/// gate while cache-hitting correctly inside a real run.
pub(crate) fn normalization_hash_for(
    cfg: &StackingConfig,
    registration_hash: &str,
    reference_member_ids: &[i64],
    reference_hash: &str,
) -> String {
    let mut upstream: Vec<String> = vec!["register".to_string(), registration_hash.to_string()];
    upstream.extend(reference_member_ids.iter().map(|id| format!("ln_ref:{id}")));
    if !reference_hash.is_empty() {
        upstream.push(format!("ln_reference_hash:{reference_hash}"));
    }
    let upstream_refs: Vec<&str> = upstream.iter().map(String::as_str).collect();
    stage_hash(&normalization_subtree(cfg), &upstream_refs, &[])
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

pub(crate) fn frame_set_name(conn: &Connection, frames_set_id: i64) -> Result<String, ApiError> {
    conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        params![frames_set_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| ApiError::NotFound(format!("frame set {frames_set_id} not found")))
}

pub(crate) fn read_global_config_json(
    conn: &Connection,
    settings: &SettingsManager,
) -> Option<String> {
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

/// [`find_group_frame`] plus the group the frame was found in — M4b's
/// per-group staleness needs to know which group's reference a frame is
/// judged against.
fn find_group_of<'a>(
    groups: &'a [IntegrationGroup],
    frame_id: i64,
) -> Option<(&'a IntegrationGroup, &'a GroupFrame)> {
    groups.iter().find_map(|g| {
        g.frames
            .iter()
            .find(|f| f.frame_id == frame_id)
            .map(|f| (g, f))
    })
}

/// M4b ruling R-T1-1: the plan-time comparison point for the pixel-scale
/// warning in `Auto` reference mode, first choice — the last run's OWN
/// recorded `reference_frame_id`, the SAME lookup [`compute_register_stale`]
/// already performs (`list_runs(_, _, 1)` → `last_run.reference_frame_id`),
/// so a set that has run once compares against the reference it actually
/// used rather than a guess. `None` when there is no previous run, the run
/// recorded no reference, that frame isn't found among the current groups
/// (deleted/re-typed since), or it carries no scale of its own.
fn last_run_reference_scale(
    conn: &Connection,
    frames_set_id: i64,
    groups: &[IntegrationGroup],
) -> Result<Option<f64>, ApiError> {
    let Some(last_run) = list_runs(conn, frames_set_id, 1)?.into_iter().next() else {
        return Ok(None);
    };
    Ok(last_run
        .reference_frame_id
        .and_then(|id| find_group_frame(groups, id))
        .and_then(|gf| gf.pixel_scale_arcsec))
}

/// M4b ruling R-T6-7: the median `pixel_scale_arcsec` over `g`'s INCLUDED
/// members only — a manually excluded frame's scale contributes nothing to
/// "what a run will actually integrate" (the real-data finding: a group's
/// median read 1.73 because 68 excluded 448 mm frames outvoted the 30 + 30
/// kept ones). `None` when the group has no included member with a scale
/// (including when every member is excluded) — [`group_display_scale`]
/// below is what falls back to the group's own all-member
/// [`IntegrationGroup::pixel_scale_arcsec`] for that case.
fn included_median_scale(g: &IntegrationGroup, excluded: &HashSet<i64>) -> Option<f64> {
    let mut scales: Vec<f64> = g
        .frames
        .iter()
        .filter(|f| !excluded.contains(&f.frame_id))
        .filter_map(|f| f.pixel_scale_arcsec)
        .collect();
    if scales.is_empty() {
        None
    } else {
        Some(median_f64(&mut scales))
    }
}

/// M4b ruling R-T6-7: the scale [`PlanGroup.pixel_scale_arcsec`] and the
/// stored `scale_ratio_to_reference` use — [`included_median_scale`] when
/// the group has at least one included member with a scale, else the
/// group's own all-member median ([`IntegrationGroup::pixel_scale_arcsec`],
/// `groups.rs`) so a FULLY excluded group still shows something in the
/// Scale column rather than a bare `None`.
///
/// The returned flag says WHICH of the two it is, because they may not be
/// used for the same things (ruling R-FR-1): only an included-member scale
/// speaks for frames a run would actually touch, so only it may drive the
/// far-from-reference warning. A fully excluded group keeps its Scale
/// column value and its ratio and says nothing — matching `build_plan`'s
/// "mixes pixel scales" check, which has only ever looked at included
/// members, and [`largest_group_scale`]'s ranking, which skips a group
/// with zero included members outright.
fn group_display_scale(g: &IntegrationGroup, excluded: &HashSet<i64>) -> (Option<f64>, bool) {
    match included_median_scale(g, excluded) {
        Some(scale) => (Some(scale), true),
        None => (g.pixel_scale_arcsec, false),
    }
}

/// M4b ruling R-T1-1's second-choice fallback (no previous run at all): the
/// INCLUDED-member median pixel scale of the LARGEST group — ranked by
/// INCLUDED member count, never total membership (ruling R-T6-7's
/// real-data finding: a 127-frame group, every member manually excluded,
/// was picked over the 30 + 30 frames a run would actually integrate) —
/// ties broken by the smaller key, as before. A group with ZERO included
/// members is skipped outright, never just outranked, so it can never be
/// picked even when it is the only group. This is an approximation, not
/// the eventual pick: if the real `Auto` reference lands in a different
/// group, comparing "the wrong way round" still reports the ratio's
/// reciprocal, which sits just as far outside `[1 / SCALE_TOLERANCE,
/// SCALE_TOLERANCE]` — the warning still fires when it should, only the
/// wording's "the reference's" number would differ from what a run later
/// actually uses.
fn largest_group_scale(groups: &[IntegrationGroup], excluded: &HashSet<i64>) -> Option<f64> {
    let mut best: Option<(&IntegrationGroup, usize)> = None;
    for g in groups {
        let included_count = g
            .frames
            .iter()
            .filter(|f| !excluded.contains(&f.frame_id))
            .count();
        if included_count == 0 {
            continue;
        }
        let take = match best {
            None => true,
            Some((b, b_count)) => {
                included_count > b_count || (included_count == b_count && g.key < b.key)
            }
        };
        if take {
            best = Some((g, included_count));
        }
    }
    let (g, _) = best?;
    included_median_scale(g, excluded)
}

/// The `registration_results` reuse predicate itself (ruling 10): a row is
/// reusable against a given reference/expected-hash pair iff its
/// `reference_frame_id` matches, its `status` is `aligned` /
/// `aligned_flipped` / `reference`, and its `config_hash` equals the
/// expected one. Shared by [`compute_register_stale`] (the plan's own
/// whole-build staleness check, below) and `run.rs`'s stage 5 (Task 7's
/// per-frame reuse decision) — the plan and the run it precedes must never
/// disagree about what "fresh" means, so both call this SAME function
/// rather than each re-deriving the three-part check.
pub(crate) fn registration_row_is_fresh(
    row: &RegistrationRecord,
    reference_frame_id: i64,
    expected_hash: &str,
) -> bool {
    row.reference_frame_id == reference_frame_id
        && matches!(
            row.status.as_str(),
            "aligned" | "aligned_flipped" | "reference"
        )
        && row.config_hash.as_deref() == Some(expected_hash)
}

/// Register-stage staleness (spec §9.3, gate step's `stale_stages`) — final
/// fix wave item 3: staleness follows the LATEST RUN's own included frames,
/// in BOTH reference modes, rather than re-deriving "which frames matter"
/// from the current group membership.
///
/// The old rule demanded a fresh `registration_results` row for every
/// non-manually-excluded frame in `Manual` mode — but a run only ever
/// registers the frames stage 3 (measure & select) actually kept in a
/// viable group, so a single selection-dropped frame (weight below the
/// floor, a group that fell under 3 included) left `Register` stale
/// forever, even immediately after a successful run. `Auto` mode's old rule
/// (any row with `source_kind == "calibrated"`, ignoring which reference
/// the run actually chose) went the other way: it read "fresh" even when
/// the run's own chosen reference had since changed.
///
/// No previous run at all ([`list_runs`] empty) is stale by construction —
/// there is nothing to compare against. Otherwise: the frames to check are
/// the latest run's `stacking_run_frames` rows with `included = true` (a
/// frame the run itself excluded needs no registration row at all to read
/// as not stale — it was never going to be registered by that run either).
/// The reference to check against is the plan's OWN manual reference
/// (`reference.frame_id`, already gated on `reference.on_disk`) in `Manual`
/// mode, or the latest run's OWN recorded `reference_frame_id` in `Auto`
/// mode (`None` there — a run that recorded no reference — is stale). Each
/// included frame then needs a `registration_results` row passing
/// [`registration_row_is_fresh`] against the CURRENT stage-1 hashes of both
/// the reference and the frame — the SAME predicate `run.rs`'s stage 5
/// uses, so the plan and the run it precedes can never disagree about what
/// "fresh" means. A reference change (manual re-pick, or an auto run
/// choosing a different frame than last time) is caught the same way: the
/// stored row's own `reference_frame_id` no longer matches.
fn compute_register_stale(
    conn: &Connection,
    cfg: &StackingConfig,
    frames_set_id: i64,
    groups: &[IntegrationGroup],
    reference: &PlanReference,
    memo: &mut HashMemo,
) -> Result<bool, ApiError> {
    let Some(last_run) = list_runs(conn, frames_set_id, 1)?.into_iter().next() else {
        return Ok(true);
    };

    let manual_reference = match reference.mode {
        ReferenceMode::Manual => {
            let (Some(id), true) = (reference.frame_id, reference.on_disk) else {
                return Ok(true);
            };
            Some(id)
        }
        ReferenceMode::Auto => None,
    };

    // The reference each GROUP is expected to be registered onto next time
    // (M4b ruling R-M4b-4). Co-registered: the one run-wide reference, for
    // every group alike — the rule this function has always applied.
    // Native: each group's own, as the last run itself recorded it in its
    // summary, since the plan cannot re-derive a per-group best-by-weight
    // without measuring the set first (the same approximation `Auto` mode
    // has always made run-wide).
    let expected_reference: HashMap<String, i64> =
        if cfg.registration.geometry == RegistrationGeometry::Native {
            // A manual pin that has MOVED since that run invalidates every
            // group's recorded reference, not only its own group's: the
            // group that lost the pin now auto-picks a frame no run has
            // ever recorded.
            if let Some(id) = manual_reference {
                if last_run.reference_frame_id != Some(id) {
                    return Ok(true);
                }
            }
            summary_group_references(last_run.summary_json.as_deref())
        } else {
            let id = match manual_reference {
                Some(id) => id,
                None => match last_run.reference_frame_id {
                    Some(id) => id,
                    None => return Ok(true),
                },
            };
            groups.iter().map(|g| (g.key.clone(), id)).collect()
        };

    // Every distinct expected reference's own stage-1 hash, resolved up
    // front: one unresolvable reference makes the whole stage stale, the
    // same way it always has.
    let mut reference_hashes: HashMap<i64, String> = HashMap::new();
    for &reference_frame_id in expected_reference.values() {
        if reference_hashes.contains_key(&reference_frame_id) {
            continue;
        }
        let Some(reference_group_frame) = find_group_frame(groups, reference_frame_id) else {
            return Ok(true);
        };
        let Some(hash) = memo.calibration_hash(conn, cfg, reference_group_frame) else {
            return Ok(true);
        };
        reference_hashes.insert(reference_frame_id, hash);
    }

    let rows = get_registration_for_frame_set(conn, frames_set_id)?;
    let by_frame: HashMap<i64, &RegistrationRecord> =
        rows.iter().map(|r| (r.frame_id, r)).collect();

    let included_frame_ids: Vec<i64> = list_frame_rows(conn, last_run.id)?
        .into_iter()
        .filter(|f| f.included)
        .map(|f| f.frame_id)
        .collect();

    for frame_id in included_frame_ids {
        let Some((group, f)) = find_group_of(groups, frame_id) else {
            return Ok(true);
        };
        let Some(&reference_frame_id) = expected_reference.get(&group.key) else {
            return Ok(true);
        };
        let Some(reference_calib_hash) = reference_hashes.get(&reference_frame_id) else {
            return Ok(true);
        };
        let Some(frame_calib_hash) = memo.calibration_hash(conn, cfg, f) else {
            return Ok(true);
        };
        let expected = registration_hash_for(
            cfg,
            reference_frame_id,
            reference_calib_hash,
            &frame_calib_hash,
        );
        let Some(row) = by_frame.get(&frame_id) else {
            return Ok(true);
        };
        if !registration_row_is_fresh(row, reference_frame_id, &expected) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// `group key -> that group's OWN registration reference`, read out of the
/// last run's stored `summary_json` (M4b ruling R-M4b-4 — `SummaryGroup::
/// reference_frame_id`).
///
/// Parsed as a bare `Value`, not through `RunSummary`: this needs two
/// fields per group, and a summary written by an older build — or a newer
/// one with fields this build has never heard of — must not cost the plan
/// its answer. A group the map has no entry for reads as stale at the call
/// site, which is the honest verdict: nothing recorded what that group was
/// registered onto.
fn summary_group_references(summary_json: Option<&str>) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    let Some(text) = summary_json else {
        return out;
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(
                %error,
                "stacking: the last run's summary could not be parsed; \
                 treating registration as stale"
            );
            return out;
        }
    };
    let Some(groups) = value.get("groups").and_then(|g| g.as_array()) else {
        return out;
    };
    for group in groups {
        let (Some(key), Some(reference_frame_id)) = (
            group.get("key").and_then(|k| k.as_str()),
            group.get("referenceFrameId").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        out.insert(key.to_string(), reference_frame_id);
    }
    out
}

// ── Stage 0.5: masters to build (spec §2 row 0.5) ───────────────────────────

/// `(imagetyp, frame_count)` for a calibration set — the two fields a
/// [`PlanMaster`] needs beyond its id, read straight off `calibration_set`.
fn calibration_set_imagetyp_and_count(
    conn: &Connection,
    set_id: i64,
) -> Result<(String, i64), ApiError> {
    Ok(conn.query_row(
        "SELECT imagetyp, frame_count FROM calibration_set WHERE id = ?1",
        [set_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// A short human label for a calibration set — `"Dark 180s -10C TestCam"`
/// style — for the Stacking tab's masters-to-build row. No dedicated
/// calibration-set display-name helper exists elsewhere in the codebase
/// (Coverage's own table builds its labels in the frontend); this is a
/// small, self-contained one rather than reaching for the calibration
/// library's file-NAMING helper (`calibration_library::paths::master_relative_path`),
/// which produces a filesystem-safe slug, not prose. Falls back to
/// `"<imagetyp> · set <id>"` if the row can't be read — shouldn't happen,
/// since `set_id` came from the readiness split moments ago, but a label is
/// never worth failing the whole plan over.
///
/// Fix round 1, item 5: propagates a real SQL error (via `?` on `.optional()`
/// — `rusqlite::Error::QueryReturnedNoRows` still collapses to `Ok(None)`,
/// exactly as before; any OTHER error, e.g. a poisoned/broken connection,
/// now surfaces instead of being silently treated the same as "no row" and
/// papered over with the fallback label.
fn calibration_set_label(
    conn: &Connection,
    set_id: i64,
    imagetyp: &str,
) -> Result<String, ApiError> {
    let row: Option<(Option<f64>, Option<f64>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT exptime, ccd_temp, instrume, filter FROM calibration_set WHERE id = ?1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((exptime, ccd_temp, instrume, filter)) = row else {
        return Ok(format!("{imagetyp} · set {set_id}"));
    };
    let mut parts = vec![imagetyp.to_string()];
    if imagetyp == "Flat" {
        if let Some(f) = filter {
            parts.push(f);
        }
    }
    if let Some(e) = exptime {
        // Plan 5b final fix wave, review item A2: reuse the calibration
        // library's own sub-second-exposure formatter (`0.39s`, `180s`)
        // instead of `{:.0}` truncating every flat under a second to `0s`.
        parts.push(format!(
            "{}s",
            crate::calibration_library::paths::fmt_num(e)
        ));
    }
    if let Some(t) = ccd_temp {
        parts.push(format!("{t:.0}\u{00B0}C"));
    }
    if let Some(i) = instrume {
        parts.push(i);
    }
    Ok(parts.join(" "))
}

/// Stage 0.5's work list (spec §2 row 0.5): every buildable raw set
/// (`readiness.raw_sets_buildable`) as a `Build` item, every rebuildable
/// missing master (`readiness.masters_rebuildable`) as a `Rebuild` item —
/// resolving a rebuild item's imagetyp/frame_count off its RAW SOURCE set
/// (via `master_provenance`, same as `run_build`'s own Rebuild path reads),
/// never the master's own `calibration_set` row (always `frame_count = 1` by
/// invariant, which would say nothing about the work about to happen).
/// Sorted by [`crate::api::masters::type_build_rank`] then id — the same
/// dependency order a manual batch build submits in
/// (`start_master_builds_batch`), so a flat this stage builds sees its own
/// precal master already on disk.
///
/// Read failures are logged and the item is dropped rather than failing the
/// whole plan — a set/master the readiness split named moments ago should
/// always resolve; if it somehow doesn't (a real inconsistency, e.g. a
/// `master_provenance` row deleted between the readiness split and this
/// collection), the second element of the returned tuple names it with a
/// reason so [`build_plan`] can turn it into a `masters` blocker instead of
/// letting it vanish from the plan silently — neither built nor blocked,
/// discovered only when the run dies inside Calibrate (Plan 5b final fix
/// wave, review finding B5).
fn collect_masters_to_build(
    conn: &Connection,
    readiness: &ExportReadiness,
) -> (Vec<PlanMaster>, Vec<(i64, String)>) {
    let mut items: Vec<PlanMaster> = Vec::new();
    let mut dropped: Vec<(i64, String)> = Vec::new();

    for &raw_set_id in &readiness.raw_sets_buildable {
        match calibration_set_imagetyp_and_count(conn, raw_set_id) {
            Ok((imagetyp, frame_count)) => {
                let label = match calibration_set_label(conn, raw_set_id, &imagetyp) {
                    Ok(label) => label,
                    Err(error) => {
                        tracing::warn!(raw_set_id, %error, "masters_to_build: calibration set label unreadable");
                        dropped.push((
                            raw_set_id,
                            format!("calibration set label unreadable: {error}"),
                        ));
                        continue;
                    }
                };
                items.push(PlanMaster {
                    set_id: raw_set_id,
                    kind: MasterWork::Build,
                    imagetyp,
                    frame_count,
                    label,
                });
            }
            Err(error) => {
                tracing::warn!(raw_set_id, %error, "masters_to_build: buildable raw set unreadable");
                dropped.push((raw_set_id, format!("buildable raw set unreadable: {error}")));
            }
        }
    }

    for &master_set_id in &readiness.masters_rebuildable {
        let source_set_id = match crate::db::master_provenance::get(conn, master_set_id) {
            Ok(Some(prov)) => prov.source_set_id,
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(master_set_id, %error, "masters_to_build: master provenance unreadable");
                dropped.push((
                    master_set_id,
                    format!("master provenance unreadable: {error}"),
                ));
                continue;
            }
        };
        let Some(source_set_id) = source_set_id else {
            tracing::warn!(
                master_set_id,
                "masters_to_build: master provenance missing or has no source set"
            );
            dropped.push((
                master_set_id,
                "master provenance missing or has no source set".to_string(),
            ));
            continue;
        };
        match calibration_set_imagetyp_and_count(conn, source_set_id) {
            Ok((imagetyp, frame_count)) => {
                let label = match calibration_set_label(conn, source_set_id, &imagetyp) {
                    Ok(label) => label,
                    Err(error) => {
                        tracing::warn!(master_set_id, source_set_id, %error, "masters_to_build: calibration set label unreadable");
                        dropped.push((
                            master_set_id,
                            format!("calibration set label unreadable: {error}"),
                        ));
                        continue;
                    }
                };
                items.push(PlanMaster {
                    set_id: master_set_id,
                    kind: MasterWork::Rebuild,
                    imagetyp,
                    frame_count,
                    label,
                });
            }
            Err(error) => {
                tracing::warn!(master_set_id, source_set_id, %error, "masters_to_build: rebuild source set unreadable");
                dropped.push((
                    master_set_id,
                    format!("rebuild source set unreadable: {error}"),
                ));
            }
        }
    }

    items.sort_by_key(|m| (crate::api::masters::type_build_rank(&m.imagetyp), m.set_id));
    (items, dropped)
}

/// A `masters` blocker for every calibration set/master [`collect_masters_to_build`]
/// could not resolve, even though the readiness split said it was
/// buildable/rebuildable moments ago (B5) — `None` when nothing was dropped,
/// which is the overwhelming majority of plans.
fn dropped_masters_blocker(dropped: &[(i64, String)]) -> Option<PlanBlocker> {
    if dropped.is_empty() {
        return None;
    }
    let n = dropped.len();
    let first_reason = dropped[0].1.as_str();
    Some(PlanBlocker {
        code: "masters".to_string(),
        message: format!(
            "Build masters first — {n} set{} could not be resolved while planning: {first_reason}",
            if n == 1 { "" } else { "s" }
        ),
    })
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

    // M4b (ruling R-T6-6, real-data finding on set 195 "Ghost Nebula"):
    // readiness must be judged over the frames a run will actually touch —
    // the set's LIGHT membership minus manual exclusions — never the whole
    // set's membership. A calibration set only an excluded light needs
    // (the finding's own example: a 1-frame flat linked to 9 lights, all
    // manually excluded) must never block a run or get built by stage 0.5.
    // Built once here — both `compute_export_readiness_for_frames` below
    // and the pixel-scale warnings further down (which also need to know
    // which frames are excluded, for the SAME "over what will run" reason)
    // share this one set rather than each deriving their own.
    let excluded_set: HashSet<i64> = excluded_frame_ids.iter().copied().collect();
    let light_frame_ids: Vec<i64> = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .map(|f| f.frame_id)
        .filter(|id| !excluded_set.contains(id))
        .collect();
    let readiness = compute_export_readiness_for_frames(conn, frames_set_id, &light_frame_ids)?;
    let (masters_to_build, dropped_masters) = collect_masters_to_build(conn, &readiness);

    let mut blockers: Vec<PlanBlocker> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Owner decision 2026-09-10: exposure always splits a group now, and a
    // frame with no EXPTIME lands in its own "unknown" cluster
    // (`groups::cluster_indices`) rather than being silently folded in with
    // a real 0 s exposure — but that frame's set is never actually excluded
    // from stacking, so this is a WARNING, never a blocker, naming the
    // frames so the operator can go fix the header if that was a mistake.
    // Fix round 1, minor 4: cap the inlined name list — a large "unknown"
    // cluster (an import missing EXPTIME on hundreds of frames) must not
    // turn one plan warning into a multi-KB string.
    const EXPTIME_WARNING_NAME_CAP: usize = 5;
    for g in &groups {
        if g.exposure_s.is_none() {
            let names: Vec<&str> = g.frames.iter().map(|f| f.filename.as_str()).collect();
            let shown = names
                .iter()
                .take(EXPTIME_WARNING_NAME_CAP)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            let remaining = names.len().saturating_sub(EXPTIME_WARNING_NAME_CAP);
            let suffix = if remaining > 0 {
                format!(" and {remaining} more")
            } else {
                String::new()
            };
            warnings.push(format!(
                "{} frame{} without EXPTIME grouped separately: {shown}{suffix}",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
            ));
        }
    }

    // Gate 1: masters/links/masterFiles — reinterpreted 2026-09-09 (owner
    // requirement — "the pipeline should build the calibration masters
    // itself when they are missing"). The export-v2 gate
    // (`check_mode_ready`'s CalibratedLights arms) blocked on
    // `raw_sets_without_master`/`missing_master_files` outright; the
    // stacking gate instead blocks ONLY the subset the readiness split says
    // cannot be built/rebuilt (`raw_sets_unbuildable`/`masters_unrebuildable`)
    // — everything else became `masters_to_build` above, planned work for
    // stage 0.5 rather than a blocker. Same relative priority as the export
    // gate's own arms (masters, then links, then masterFiles) and the same
    // sentence style, just re-derived from the split fields instead of
    // calling `check_mode_ready` directly.
    if !readiness.raw_sets_unbuildable.is_empty() {
        let n = readiness.raw_sets_unbuildable.len();
        let first_reason = readiness.raw_sets_unbuildable[0].1.as_str();
        blockers.push(PlanBlocker {
            code: "masters".to_string(),
            message: format!(
                "Build masters first — {n} set{} cannot be built: {first_reason}",
                if n == 1 { "" } else { "s" }
            ),
        });
    }
    if let Some(blocker) = dropped_masters_blocker(&dropped_masters) {
        blockers.push(blocker);
    }
    if readiness.unlinked_lights > 0 {
        let n = readiness.unlinked_lights;
        blockers.push(PlanBlocker {
            code: "links".to_string(),
            message: format!(
                "{n} light{} {} no calibration links",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" }
            ),
        });
    }
    if !readiness.masters_unrebuildable.is_empty() {
        let n = readiness.masters_unrebuildable.len();
        // Every unrebuildable reason is archival ("archived — restore
        // first") -> the export gate's own C-2 sentence; a mix (or a
        // "no provenance"/missing-not-archived reason) names the first
        // reason instead, since "restore from archive" would be actively
        // wrong advice for those.
        let all_archival = readiness
            .masters_unrebuildable
            .iter()
            .all(|(_, reason)| reason.contains("archived"));
        let message = if all_archival {
            format!("{n} master file(s) missing on disk — restore from archive first")
        } else {
            format!(
                "{n} master file(s) missing on disk and cannot be rebuilt: {}",
                readiness.masters_unrebuildable[0].1
            )
        };
        blockers.push(PlanBlocker {
            code: "masterFiles".to_string(),
            message,
        });
    }

    // Gate 2: reference.
    let reference = resolve_reference(conn, frames_set_id, &cfg, &mut blockers)?;

    // M4b (rulings R-M4b-1/7, R-T1-1): pixel-scale warnings — never
    // blockers. A mixed-pixel-scale set is a real, supportable
    // configuration (co-registered mode resamples a foreign-scale group
    // into the reference geometry; native mode keeps each group's own), so
    // this is purely informational. `reference_scale` is the plan-time
    // comparison point: the resolved reference frame's OWN pixel scale in
    // `Manual` mode (found among `groups` by its `frame_id`); in `Auto`
    // mode — where `resolve_reference` never populates `frame_id`, the
    // real pick happens only once a run weighs the frames — it falls back
    // to the last run's own recorded reference ([`last_run_reference_scale`],
    // the same lookup `compute_register_stale` uses), then to the median
    // scale of the largest group ([`largest_group_scale`]) when there is no
    // previous run at all. `None` only when every one of those sources is
    // unavailable, in which case the "far from reference" warning below
    // simply can't fire, while the per-group "mixes pixel scales" warning
    // (which needs no reference at all) still can. Placed right after the
    // reference resolves (the earliest point both warnings' inputs are
    // available), alongside the EXPTIME warning above as this build's other
    // purely-informational, never-blocking group note.
    let reference_scale: Option<f64> = match reference.frame_id {
        Some(id) => find_group_frame(&groups, id).and_then(|gf| gf.pixel_scale_arcsec),
        None => last_run_reference_scale(conn, frames_set_id, &groups)?
            .or_else(|| largest_group_scale(&groups, &excluded_set)),
    };
    // Fix round 1: the ratio is computed ONCE per group here and reused
    // both for the warning message immediately below and for
    // `PlanGroup.scale_ratio_to_reference` later (keyed by group key —
    // unique per build by construction, `groups.rs`'s own "one key ⇔ one
    // group" doc) — the two spellings of the same guard used to risk
    // drifting apart. R-T6-7: the group's own side of the ratio is now
    // `group_display_scale` (included-only median, all-member fallback for
    // a fully excluded group), not the raw all-member
    // `IntegrationGroup::pixel_scale_arcsec`.
    let mut group_scale_ratios: HashMap<String, f64> = HashMap::new();
    for g in &groups {
        let (display_scale, from_included) = group_display_scale(g, &excluded_set);
        let scale_ratio = display_scale
            .zip(reference_scale.filter(|&r| r > 0.0))
            .map(|(group_scale, ref_scale)| (group_scale, ref_scale, group_scale / ref_scale));
        if let Some((group_scale, ref_scale, ratio)) = scale_ratio {
            // R-FR-1: the ratio and the Scale column are reported for every
            // group, the WARNING only for one that still has an included
            // member with a scale of its own — a fully excluded group's
            // all-member fallback describes frames the run will never read.
            if from_included && (ratio > SCALE_TOLERANCE || ratio < 1.0 / SCALE_TOLERANCE) {
                warnings.push(format!(
                    "group `{}` is at {group_scale:.2} \"/px — \u{d7}{ratio:.1} the reference's {ref_scale:.2} \"/px; co-registered mode resamples it into the reference geometry, native mode keeps its own",
                    g.key
                ));
            }
            tracing::debug!(
                group_key = %g.key,
                pixel_scale_arcsec = group_scale,
                scale_ratio = ratio,
                "group pixel scale"
            );
            group_scale_ratios.insert(g.key.clone(), ratio);
        }

        // Fix round 1: only INCLUDED members — a group whose only
        // foreign-scale members are manually excluded must not warn
        // "mixes pixel scales" over frames the run will never touch.
        let member_scales: Vec<f64> = g
            .frames
            .iter()
            .filter(|f| !excluded_set.contains(&f.frame_id))
            .filter_map(|f| f.pixel_scale_arcsec)
            .collect();
        if member_scales.len() >= 2 {
            let min = member_scales.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = member_scales
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            if min > 0.0 && max / min > SCALE_TOLERANCE {
                warnings.push(format!(
                    "group `{}` mixes pixel scales ({min:.2}\u{2013}{max:.2} \"/px); frames outside \u{d7}{SCALE_TOLERANCE} of the group's reference are registered with a scale factor",
                    g.key
                ));
            }
        }
    }

    // Gate 3: folders — also resolves free space and the estimate's probe
    // target.
    let resolved_dirs = paths::resolve_dirs(conn, settings, &cfg.paths);
    let mut free_bytes: Option<u64> = None;
    let mut working_dir: Option<String> = None;
    let mut output_dir: Option<String> = None;
    match (resolved_dirs.working, resolved_dirs.output) {
        (Some(working), Some(output)) => {
            match paths::validate_dirs(conn, policy, &working, &output, paths::ValidateMode::Plan) {
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
        drizzle: cfg.drizzle.enabled.then_some((
            cfg.drizzle.scale,
            cfg.drizzle.write_weight_map,
            cfg.drizzle.use_rejection,
        )),
        // M4c Task 3: large-scale rejection creates the same per-frame
        // bitmaps drizzle's `useRejection` does, plus a processed sibling
        // of each — even with drizzle off.
        large_scale: cfg.integration.large_scale.enabled,
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
    let local_normalization_active = cfg.normalization.local.enabled
        || cfg.normalization.rejection == RejectionNormalization::Local;
    let mut normalize_stale = false;
    // Fix round 1, item 5: the SAME per-frame registration hash
    // `stacking::run`'s `GroupMember.registration_hash` reads (the STORED
    // `registration_results.config_hash`, never recomputed here) — fetched
    // once for the whole build, not per group/frame, same rationale as
    // `compute_register_stale`'s own `by_frame` map below.
    let ln_registration_by_frame: HashMap<i64, Option<String>> = if local_normalization_active {
        get_registration_for_frame_set(conn, frames_set_id)?
            .into_iter()
            .map(|r| (r.frame_id, r.config_hash))
            .collect()
    } else {
        HashMap::new()
    };
    // Carry-over (c), Task 5's re-review: a frame the LATEST run itself
    // runtime-excluded for a local-normalization reason (`TooFewMatches`, a
    // sidecar write failure, an unreadable cached sidecar — M2 Task 7's
    // `exclude_frame_and_persist` calls, `run.rs`) has no `ln` artifact by
    // design — its own `normalize_frame` call never produced one, and won't
    // for the same underlying reason until something upstream changes.
    // Without this, `normalize_stale` would stay true FOREVER for that
    // group: this one known-bad frame can never earn a fresh `ln` row on
    // its own. Every LN exclusion reason (`LnError`'s `Display`, and
    // `run.rs`'s own hand-built "writing .athln sidecar: reading it back
    // failed: …" post-write read-back text) starts with the literal
    // "local normalization:" prefix — checked, never assumed, so an
    // unrelated exclusion (registration failure, manual list, weight floor)
    // is never mistaken for one. Same "fetch once per build, look up per
    // frame" shape as `ln_registration_by_frame` above; `compute_register_stale`
    // below fetches the same last-run frame rows independently for its own
    // purpose — kept separate rather than threading a shared fetch through,
    // since the two staleness computations are otherwise unrelated.
    //
    // Fix round 1, item 4: gated on `last_run.config_hash == hash` (the
    // CURRENT build's own resolved config hash, already in hand) — a frame
    // excluded under an OLDER `normalization.local` config (a different
    // `scale`/`referenceFrames`/`psfModel`, say) must not stay "not owed"
    // forever once the user changes it; only an exclusion FROM A RUN THAT
    // USED THIS EXACT CONFIG is trusted as "will fail again for the same
    // reason". A config change makes the set empty, so the frame reverts to
    // ordinary "no fresh `ln` artifact" staleness until a real run either
    // re-excludes it (under the new config) or produces one.
    let ln_runtime_excluded_by_frame: HashSet<i64> = if local_normalization_active {
        match list_runs(conn, frames_set_id, 1)?.into_iter().next() {
            Some(last_run) if last_run.config_hash == hash => list_frame_rows(conn, last_run.id)?
                .into_iter()
                .filter(|row| {
                    !row.included
                        && row
                            .exclusion_reason
                            .as_deref()
                            .is_some_and(|r| r.starts_with("local normalization:"))
                })
                .map(|row| row.frame_id)
                .collect(),
            _ => HashSet::new(),
        }
    } else {
        HashSet::new()
    };

    for g in &groups {
        // Fix round 1, item 5: the group's LN reference artifact carries the
        // resolved `reference_member_ids` (weight-ordered) and its own
        // `reference_hash` in `payload_json` — read once per group so every
        // frame's `ln` row can be verified EXACTLY (`normalization_hash_for`)
        // without this DB-plus-cheap-FS-probe gate ever needing the stage-3
        // weight ranking that produced that member list. Residual: if a
        // fresh run would pick a DIFFERENT top-`referenceFrames` member set
        // (a weight change, not a config/registration one), this still
        // reports fresh against the OLD set until a real run updates it —
        // documented, not fixed, here (weights are pixel-phase data).
        let ln_reference_info: Option<LnReferencePayload> = if local_normalization_active {
            find_artifact(conn, frames_set_id, &g.key, "ln_reference", None)?
                .and_then(|row| row.payload_json)
                .and_then(|s| serde_json::from_str::<LnReferencePayload>(&s).ok())
        } else {
            None
        };
        let mut calibrated_cached = 0usize;
        let mut metrics_cached = 0usize;
        let mut ln_cached = 0usize;
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

            // Stage 6 (local normalization, M2, fix round 1 item 5): an
            // EXACT hash comparison, same rule `is_fresh` uses for every
            // other per-frame artifact — `normalization_hash_for` recomputed
            // from the group's stored reference info (above) and this
            // frame's own stored registration hash, compared against the
            // `ln` row's own `config_hash`. No artifact/no reference
            // info/no registration row/an unparseable payload are all
            // "can't verify" → stale, never assumed fresh.
            if local_normalization_active {
                let ln_artifact =
                    find_artifact(conn, frames_set_id, &g.key, "ln", Some(f.frame_id))?;
                let ln_fresh = match (&ln_reference_info, &ln_artifact) {
                    (Some(ref_info), Some(row)) => {
                        match ln_registration_by_frame.get(&f.frame_id) {
                            // M12 (final fix wave): a `NULL` stored
                            // `config_hash` must hash as `""` here, exactly
                            // the way `run.rs` writes it
                            // (`record.config_hash.clone().unwrap_or_default()`)
                            // — without this, a frame whose registration row
                            // genuinely has no `config_hash` would cache-hit
                            // correctly inside a real run but be reported
                            // permanently stale by this gate. Only the
                            // ABSENCE of a registration row at all (no entry
                            // in the map) stays "can't verify".
                            Some(hash_opt) => {
                                let frame_registration_hash =
                                    hash_opt.clone().unwrap_or_default();
                                let expected = normalization_hash_for(
                                    &cfg,
                                    &frame_registration_hash,
                                    &ref_info.reference_member_ids,
                                    &ref_info.reference_hash,
                                );
                                is_fresh(row, &expected)
                            }
                            None => false,
                        }
                    }
                    _ => false,
                };
                if ln_fresh {
                    ln_cached += 1;
                } else if !is_excluded && !ln_runtime_excluded_by_frame.contains(&f.frame_id) {
                    // Carry-over (c): a frame the latest run itself already
                    // excluded for a local-normalization reason has no `ln`
                    // artifact by design and is not owed one — see
                    // `ln_runtime_excluded_by_frame`'s own doc above.
                    normalize_stale = true;
                }
            }
        }

        if included_count >= 3 {
            any_group_has_three_included = true;
        }

        // Fix round 1, Minor M3: `group_anchor_geometry` (shared with
        // `run.rs`'s own `stacking_run_groups.width`/`height` DB write,
        // which keeps `Some(0)` — a DB column, not this struct's own "is it
        // known" contract) reports `GroupFrame.width`/`height` verbatim,
        // and `GroupFrame` stores an absent `NAXIS1`/`NAXIS2` as `0`
        // (`groups.rs`'s `unwrap_or(0)`), not `None`. `PlanGroup`'s own doc
        // promises `None` for "unknown", so `0` is mapped to `None` HERE,
        // scoped to this one call site, rather than changing the shared
        // helper's own (differently-contracted) DB-write behavior.
        let (anchor_width, anchor_height) = group_anchor_geometry(g);
        let anchor_width = anchor_width.filter(|&w| w > 0);
        let anchor_height = anchor_height.filter(|&h| h > 0);
        plan_groups.push(PlanGroup {
            key: g.key.clone(),
            instrume: g.instrume.clone(),
            color_mode: g.color_mode,
            filter: g.filter.clone(),
            binning: g.binning,
            cameras: g.cameras.clone(),
            exposure_s: g.exposure_s,
            frame_count: g.frames.len(),
            included_count,
            total_exposure_s: g.total_exposure_s,
            calibrated_cached,
            metrics_cached,
            ln_cached,
            anchor_width,
            anchor_height,
            // R-T6-7: included-only median, all-member fallback for a fully
            // excluded group (`group_display_scale`'s own doc). The Scale
            // column shows either; only the warning cares which (R-FR-1).
            pixel_scale_arcsec: group_display_scale(g, &excluded_set).0,
            // Fix round 1: reused from `group_scale_ratios`, computed once
            // alongside the warning above — never recomputed here.
            scale_ratio_to_reference: group_scale_ratios.get(&g.key).copied(),
            // F3: `Solve` when at least HALF of the INCLUDED members with a
            // scale measured their own (`2 * solve_count >= total`), else
            // `Header` — one header-sourced member out of many used to flip
            // a 95 %-solved group to `Header` ("~"), reading a group that
            // is almost entirely measured as merely implied. A manually
            // excluded member's source doesn't count either way — its
            // scale never entered `pixel_scale_arcsec` above.
            scale_source: {
                let sources: Vec<ScaleSource> = g
                    .frames
                    .iter()
                    .filter(|f| !excluded_set.contains(&f.frame_id))
                    .filter_map(|f| f.scale_source)
                    .collect();
                if sources.is_empty() {
                    None
                } else {
                    let solve_count =
                        sources.iter().filter(|s| **s == ScaleSource::Solve).count();
                    if solve_count * 2 >= sources.len() {
                        Some(ScaleSource::Solve)
                    } else {
                        Some(ScaleSource::Header)
                    }
                }
            },
        });
    }

    // Gate 5: frames.
    if !any_group_has_three_included {
        blockers.push(PlanBlocker {
            code: "frames".to_string(),
            message: "At least 3 included frames in one group".to_string(),
        });
    }

    let register_stale =
        compute_register_stale(conn, &cfg, frames_set_id, &groups, &reference, &mut memo)?;

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
    if local_normalization_active && normalize_stale {
        stale_stages.push(Stage::Normalize);
    }

    // Gate 6: unsupported — checked last, so a user only sees these once
    // everything more fundamental is already in order. Local normalization
    // (M2) is no longer blocked here — fix round 1: `integrate_group` (M2
    // Task 7) has a real `GroupInput.ln` conduit for both output and
    // rejection normalization now, so a plan with `normalization.local.
    // enabled` (or `rejection == "local"`) runs like any other. M3 Task 5:
    // drizzle itself is no longer blocked — only an out-of-range `scale`
    // is (ruling R-M3-10; `dropShrink` outside `[0.5, 1.0]` is a run-time
    // clamp, not a plan blocker).
    if cfg.drizzle.enabled && !(1..=3).contains(&cfg.drizzle.scale) {
        blockers.push(PlanBlocker {
            code: "unsupported".to_string(),
            message: "drizzle scale must be 1, 2 or 3".to_string(),
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
        masters_to_build,
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
    use crate::api::lights::compute_export_readiness;
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

    /// Owner decision 2026-09-10: groups are camera-agnostic — two lights
    /// from different cameras, same colour mode/filter/binning and an
    /// exposure within tolerance, land in ONE group; `cameras` lists both,
    /// `instrume` is the first member's (by `date_obs`) display camera.
    #[test]
    fn plan_groups_are_camera_agnostic() {
        let f = test_fixtures::frame_set("LDN 1272");
        test_fixtures::add_light(
            &f,
            &LightSpec {
                instrume: "ATR2600M",
                ..light_spec("a", "2025-01-01T00:00:00")
            },
        );
        test_fixtures::add_light(
            &f,
            &LightSpec {
                instrume: "Other",
                ..light_spec("b", "2025-01-01T00:05:00")
            },
        );

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert_eq!(
            plan.groups.len(),
            1,
            "two mono cameras, one exposure: one group: {:?}",
            plan.groups
        );
        assert_eq!(
            plan.groups[0].cameras,
            vec!["ATR2600M".to_string(), "Other".to_string()]
        );
        assert_eq!(
            plan.groups[0].instrume.as_deref(),
            Some("ATR2600M"),
            "the first member by date_obs anchors the display camera"
        );
    }

    /// A frame with no `EXPTIME` never blocks the plan — it gets its own
    /// "unknown" exposure cluster (`groups::cluster_indices`) and a
    /// warning naming it.
    #[test]
    fn plan_warns_about_frames_without_exptime() {
        let f = test_fixtures::frame_set("LDN 1272");
        let (no_exp_id, _) =
            test_fixtures::add_light(&f, &light_spec("noexp", "2025-01-01T00:00:00"));
        f.conn
            .execute(
                "UPDATE frames SET exptime = NULL WHERE id = ?1",
                params![no_exp_id],
            )
            .unwrap();
        // Three known-EXPTIME frames so the "frames" gate (>= 3 included in
        // ONE group) is satisfied by the known cluster alone — this test is
        // about the warning, not about also tripping that unrelated gate.
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("known{i}"), t));
        }

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(
            plan.warnings.iter().any(|w| w.contains("noexp.fits")),
            "{:?}",
            plan.warnings
        );
        assert!(
            !plan.blockers.iter().any(|b| b.code == "frames"),
            "a missing EXPTIME is a warning, never a blocker: {:?}",
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

    /// M4b (rulings R-M4b-1/7): a group whose pixel scale sits far from the
    /// resolved reference's own gets a warning naming it — never a blocker.
    /// The reference's own group (ratio ~1.0) gets no such warning, and
    /// both groups' `PlanGroup.pixelScaleArcsec`/`scaleRatioToReference`
    /// come back populated.
    #[test]
    fn scale_warning_group_far_from_reference() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ref_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("ref{i}");
            // Real file on disk: `Manual` mode's `resolve_reference` checks
            // `reference.on_disk` for the chosen frame.
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&stem, t));
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            ref_ids.push(id);
        }
        let mut other_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("other{i}");
            let mut spec = light_spec(&stem, t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 1.55);
            other_ids.push(id);
        }
        let mut all_ids = ref_ids.clone();
        all_ids.extend(&other_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);

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
        set_frame_set_reference(&f.conn, f.set_id, ref_ids[0]).unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 2, "{:?}", plan.groups);

        let other_group = plan
            .groups
            .iter()
            .find(|g| g.key.contains("Ha"))
            .expect("the Ha group exists");
        let far_warnings: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|w| w.contains("\u{d7}2.0"))
            .collect();
        assert_eq!(far_warnings.len(), 1, "{:?}", plan.warnings);
        assert!(
            far_warnings[0].contains(&other_group.key),
            "{:?}",
            far_warnings
        );
        assert_eq!(other_group.pixel_scale_arcsec, Some(1.55));
        assert_eq!(other_group.scale_source, Some(ScaleSource::Solve));
        let ratio = other_group.scale_ratio_to_reference.unwrap();
        assert!((ratio - 1.55 / 0.78).abs() < 1e-9, "{ratio}");

        let ref_group = plan
            .groups
            .iter()
            .find(|g| g.key != other_group.key)
            .expect("the reference's own group exists");
        assert_eq!(ref_group.pixel_scale_arcsec, Some(0.78));
        assert_eq!(ref_group.scale_source, Some(ScaleSource::Solve));
        assert!(
            (ref_group.scale_ratio_to_reference.unwrap() - 1.0).abs() < 1e-9,
            "{:?}",
            ref_group.scale_ratio_to_reference
        );
    }

    /// Ruling R-FR-1 (final review): the sibling of the test above. A
    /// group whose members are ALL manually excluded keeps its Scale
    /// column value and its `scaleRatioToReference` — both fall back to
    /// the group's own all-member median (R-T6-7) — but drives no
    /// far-from-reference warning: that fallback describes frames the run
    /// will never read.
    #[test]
    fn scale_warning_skips_a_fully_excluded_group() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ref_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("ref{i}");
            let (id, _path) = test_fixtures::add_light(&f, &light_spec_written(&stem, t));
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            ref_ids.push(id);
        }
        let mut other_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("other{i}");
            let mut spec = light_spec(&stem, t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 1.55);
            other_ids.push(id);
        }
        let mut all_ids = ref_ids.clone();
        all_ids.extend(&other_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);

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
        set_frame_set_reference(&f.conn, f.set_id, ref_ids[0]).unwrap();
        let settings = SettingsManager::new();

        // Premise: with the Ha group included, the warning fires.
        let plan_all = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg.clone()),
        )
        .unwrap();
        assert!(
            plan_all.warnings.iter().any(|w| w.contains("\u{d7}2.0")),
            "sanity: an INCLUDED group at 2x must warn: {:?}",
            plan_all.warnings
        );

        // Now exclude every member of it.
        set_set_config(&f.conn, f.set_id, "{}", &other_ids).unwrap();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        assert!(
            !plan.warnings.iter().any(|w| w.contains("\u{d7}2.0")),
            "a fully excluded group must not warn: {:?}",
            plan.warnings
        );
        let other_group = plan
            .groups
            .iter()
            .find(|g| g.key.contains("Ha"))
            .expect("the Ha group is still planned, just fully excluded");
        assert_eq!(other_group.included_count, 0);
        assert_eq!(
            other_group.pixel_scale_arcsec,
            Some(1.55),
            "the Scale column still shows the all-member median"
        );
        let ratio = other_group
            .scale_ratio_to_reference
            .expect("and so does the ratio");
        assert!((ratio - 1.55 / 0.78).abs() < 1e-9, "{ratio}");
    }

    /// M4b ruling R-T1-1, fix round 1: `Auto` reference mode (the default)
    /// never resolves `reference.frame_id` at plan time, so with NO
    /// previous run the pixel-scale comparison falls back to the median
    /// scale of the LARGEST group (most frames). Four frames at 0.78 "/px
    /// (the largest group) vs three at 1.55 "/px produces exactly one
    /// warning — for the SMALLER group, ×2.0 the largest group's own scale
    /// — and `scale_ratio_to_reference` comes back `Some` on BOTH groups
    /// (≈1.0 for the largest group itself, since it IS the comparison
    /// point; ≈1.99 for the other).
    #[test]
    fn scale_warning_auto_mode_falls_back_to_largest_group() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut big_ids = Vec::new();
        for i in 0..4 {
            let t = format!("2025-01-01T00:{:02}:00", i * 5);
            let stem = format!("big{i}");
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&stem, &t));
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            big_ids.push(id);
        }
        let mut small_ids = Vec::new();
        for i in 0..3 {
            let t = format!("2025-01-01T01:{:02}:00", i * 5);
            let stem = format!("small{i}");
            let mut spec = light_spec(&stem, &t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 1.55);
            small_ids.push(id);
        }
        let mut all_ids = big_ids.clone();
        all_ids.extend(&small_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);

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

        // Default config: `ReferenceMode::Auto`, no `set_frame_set_reference`
        // call and no previous run — the largest-group fallback is the
        // only source `reference_scale` can come from.
        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 2, "{:?}", plan.groups);

        let big_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 4)
            .expect("largest group exists");
        let small_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 3)
            .expect("smaller group exists");
        assert_eq!(big_group.pixel_scale_arcsec, Some(0.78));
        assert_eq!(small_group.pixel_scale_arcsec, Some(1.55));

        let far_warnings: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|w| w.contains("\u{d7}2.0"))
            .collect();
        assert_eq!(far_warnings.len(), 1, "{:?}", plan.warnings);
        assert!(far_warnings[0].contains(&small_group.key), "{:?}", far_warnings);

        let big_ratio = big_group
            .scale_ratio_to_reference
            .expect("the largest group IS the comparison point, so its own ratio is Some too");
        assert!((big_ratio - 1.0).abs() < 1e-9, "{big_ratio}");
        let small_ratio = small_group.scale_ratio_to_reference.unwrap();
        assert!((small_ratio - 1.55 / 0.78).abs() < 1e-9, "{small_ratio}");
    }

    /// M4b ruling R-T1-1: once a set has run at least once, `Auto` mode
    /// prefers the LAST RUN's own recorded reference over the
    /// largest-group fallback — even when that reference sits in the
    /// SMALLER group. Same 4-vs-3-frame fixture as
    /// `scale_warning_auto_mode_falls_back_to_largest_group`, but with a
    /// seeded run row pointing at a frame in the smaller (1.55 "/px) group:
    /// the warning now names the LARGER group instead (×0.5, not ×2.0).
    #[test]
    fn scale_warning_auto_mode_prefers_last_run_reference_over_largest_group() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut big_ids = Vec::new();
        for i in 0..4 {
            let t = format!("2025-01-01T00:{:02}:00", i * 5);
            let stem = format!("big{i}");
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&stem, &t));
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            big_ids.push(id);
        }
        let mut small_ids = Vec::new();
        for i in 0..3 {
            let t = format!("2025-01-01T01:{:02}:00", i * 5);
            let stem = format!("small{i}");
            let mut spec = light_spec(&stem, &t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 1.55);
            small_ids.push(id);
        }
        let mut all_ids = big_ids.clone();
        all_ids.extend(&small_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);

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

        // A previous run recorded its reference in the SMALLER group.
        seed_run_frame_rows(&f.conn, f.set_id, Some(small_ids[0]), "auto", &all_ids);

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 2, "{:?}", plan.groups);

        let big_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 4)
            .expect("largest group exists");
        let small_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 3)
            .expect("smaller group exists");

        assert!(
            !plan.warnings.iter().any(|w| w.contains("\u{d7}2.0")),
            "the largest-group fallback would have warned about the small \
             group (×2.0) — the last run's reference must win instead: {:?}",
            plan.warnings
        );
        let far_warnings: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|w| w.contains("\u{d7}0.5"))
            .collect();
        assert_eq!(far_warnings.len(), 1, "{:?}", plan.warnings);
        assert!(far_warnings[0].contains(&big_group.key), "{:?}", far_warnings);

        let small_ratio = small_group.scale_ratio_to_reference.unwrap();
        assert!(
            (small_ratio - 1.0).abs() < 1e-9,
            "the last run's own reference sits in the small group: ratio ~1.0, {small_ratio}"
        );
        let big_ratio = big_group.scale_ratio_to_reference.unwrap();
        assert!((big_ratio - 0.78 / 1.55).abs() < 1e-9, "{big_ratio}");
    }

    /// M4b ruling R-T6-7 (real-data finding: a 127-frame group, every
    /// member manually excluded, was picked over the 30 + 30 frames a run
    /// would actually integrate): `largest_group_scale` ranks by INCLUDED
    /// member count, never total membership, and a group's own
    /// `pixel_scale_arcsec` is the INCLUDED-only median. An 8-member group
    /// (5 excluded at 0.78 "/px, 3 included at 1.55 "/px) is the bigger
    /// group by TOTAL count, but only a 4-member, fully-included group
    /// wins the ranking — and the 8-member group's own scale reads 1.55,
    /// never corrupted by its excluded majority.
    #[test]
    fn scale_warning_auto_mode_largest_group_ranks_by_included_count() {
        let f = test_fixtures::frame_set("LDN 1272");

        // The "big" group: 8 total members, only 3 survive exclusion.
        let mut big_ids = Vec::new();
        for i in 0..8 {
            let t = format!("2025-01-01T00:{:02}:00", i * 5);
            let stem = format!("big{i}");
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&stem, &t));
            let scale = if i < 5 { 0.78 } else { 1.55 };
            test_fixtures::seed_plate_solve_scale(&f.conn, id, scale);
            big_ids.push(id);
        }
        let excluded_from_big: Vec<i64> = big_ids[..5].to_vec();

        // The "small" group: 4 total members, all of them included — fewer
        // frames overall, but MORE included frames than the big group's 3.
        let mut small_ids = Vec::new();
        for i in 0..4 {
            let t = format!("2025-01-01T02:{:02}:00", i * 5);
            let stem = format!("small{i}");
            let mut spec = light_spec(&stem, &t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            small_ids.push(id);
        }

        let mut all_ids = big_ids.clone();
        all_ids.extend(&small_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);

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

        set_set_config(&f.conn, f.set_id, "{}", &excluded_from_big).unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 2, "{:?}", plan.groups);

        let big_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 8)
            .expect("the 8-member group exists");
        let small_group = plan
            .groups
            .iter()
            .find(|g| g.frame_count == 4)
            .expect("the 4-member group exists");
        assert_eq!(big_group.included_count, 3);
        assert_eq!(small_group.included_count, 4);

        // Point 1: the big group's own scale is the INCLUDED-only median —
        // 1.55, not the all-member median the 5 excluded 0.78s would pull
        // it toward.
        assert_eq!(big_group.pixel_scale_arcsec, Some(1.55));
        assert_eq!(small_group.pixel_scale_arcsec, Some(0.78));

        // Point 2: the SMALL group (4 included > the big group's 3) wins
        // the largest-group-scale ranking, so the warning names the BIG
        // group (far from 0.78), never the small one.
        let far_warnings: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|w| w.contains("\u{d7}2.0"))
            .collect();
        assert_eq!(far_warnings.len(), 1, "{:?}", plan.warnings);
        assert!(far_warnings[0].contains(&big_group.key), "{:?}", far_warnings);

        assert!(
            (small_group.scale_ratio_to_reference.unwrap() - 1.0).abs() < 1e-9,
            "the small group IS the comparison point"
        );
        let big_ratio = big_group.scale_ratio_to_reference.unwrap();
        assert!((big_ratio - 1.55 / 0.78).abs() < 1e-9, "{big_ratio}");
    }

    /// M4b: a group whose OWN members span a wide pixel-scale range (mixed
    /// optics feeding one logical group) gets its own warning, independent
    /// of any reference — this fixture leaves the reference in `Auto` mode
    /// (unresolved at plan time) precisely to show the mixing warning needs
    /// none. F3: two of the three members' scales come from the header
    /// rather than a solve — only 1 of 3 (below the "at least half" bar) —
    /// so `PlanGroup.scale_source` reads `Header`.
    #[test]
    fn scale_warning_group_mixes_scales() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        // F3: two of the three members are header-sourced (i == 1, 2) and
        // only one is solve-sourced (i == 0) — 1 of 3 is below the "at
        // least half" bar, so the group reads `Header`.
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            if i == 0 {
                test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            } else if i == 1 {
                f.conn
                    .execute(
                        "UPDATE frames SET focallen = 1000.0, xpixsz = 3.76 WHERE id = ?1",
                        params![id],
                    )
                    .unwrap();
            } else {
                f.conn
                    .execute(
                        "UPDATE frames SET focallen = 1000.0, xpixsz = 7.5 WHERE id = ?1",
                        params![id],
                    )
                    .unwrap();
            }
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
        assert_eq!(plan.groups.len(), 1, "{:?}", plan.groups);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("mixes pixel scales")),
            "{:?}",
            plan.warnings
        );
        assert_eq!(
            plan.groups[0].scale_source,
            Some(ScaleSource::Header),
            "only 1 of 3 members is solve-sourced — below the \"at least half\" bar"
        );
    }

    /// Fix round 1 (finding 5): the "mixes pixel scales" spread must only
    /// consider INCLUDED members — a group whose only foreign-scale member
    /// is manually excluded must not warn about a spread the run will
    /// never actually see.
    #[test]
    fn scale_warning_mixes_ignores_manually_excluded_members() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            test_fixtures::seed_plate_solve_scale(&f.conn, id, 0.78);
            ids.push(id);
        }
        let (outlier_id, _path) =
            test_fixtures::add_light(&f, &light_spec("outlier", "2025-01-01T00:15:00"));
        test_fixtures::seed_plate_solve_scale(&f.conn, outlier_id, 1.55);
        ids.push(outlier_id);

        let settings = SettingsManager::new();
        let plan_all =
            build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert!(
            plan_all
                .warnings
                .iter()
                .any(|w| w.contains("mixes pixel scales")),
            "sanity: with the outlier included, the spread must warn: {:?}",
            plan_all.warnings
        );

        set_set_config(&f.conn, f.set_id, "{}", &[outlier_id]).unwrap();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert!(
            !plan.warnings.iter().any(|w| w.contains("mixes pixel scales")),
            "the outlier is manually excluded — no spread left to warn about: {:?}",
            plan.warnings
        );
    }

    /// M4b: when no frame in the set has any usable pixel scale (no header
    /// focal length/pixel size, no stored solve), the plan gate stays
    /// entirely silent about it — nothing to warn about, and both new
    /// `PlanGroup` fields come back `None`.
    #[test]
    fn no_scale_data_means_no_scale_warning() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
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
        assert_eq!(plan.groups[0].pixel_scale_arcsec, None);
        assert_eq!(plan.groups[0].scale_ratio_to_reference, None);
        assert_eq!(plan.groups[0].scale_source, None);
        assert!(
            !plan
                .warnings
                .iter()
                .any(|w| w.contains("pixel scale") || w.contains("\"/px")),
            "{:?}",
            plan.warnings
        );
    }

    /// Owner requirement 2026-09-09 ("the pipeline should build the
    /// calibration masters itself when they are missing"): a raw calibration
    /// set with every member frame on disk becomes planned work
    /// (`masters_to_build == [Build]`) instead of the `masters` blocker.
    #[test]
    fn plan_lists_buildable_raw_set_instead_of_blocking() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        let raw_dark = test_fixtures::add_raw_linked_dark(&f, &ids, 64, 48);

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let library_dir = f.dir.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();
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
        crate::db::set_setting(
            &f.conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &library_dir.to_string_lossy(),
        )
        .unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(
            !plan.blockers.iter().any(|b| b.code == "masters"),
            "a buildable raw set must not block: {:?}",
            plan.blockers
        );
        assert_eq!(
            plan.masters_to_build.len(),
            1,
            "{:?}",
            plan.masters_to_build
        );
        assert_eq!(plan.masters_to_build[0].set_id, raw_dark);
        assert_eq!(plan.masters_to_build[0].kind, MasterWork::Build);
        assert_eq!(plan.masters_to_build[0].imagetyp, "Dark");
        assert_eq!(plan.masters_to_build[0].frame_count, 3);
    }

    /// M4b ruling R-T6-6 (real-data finding on set 195 "Ghost Nebula", 495
    /// lights, 375 manually excluded): masters/links readiness — and the
    /// stage-0.5 build list it feeds (`mastersToBuild`) — must be judged
    /// over the frames a run will actually touch. A 1-frame raw flat
    /// (unbuildable — fewer than `MIN_MASTER_FRAMES`) linked to a light
    /// that turns out to be the ONLY consumer, and is manually excluded,
    /// must not block the plan or appear in `mastersToBuild`; with the
    /// exclusion lifted, the existing behaviour (a `masters` blocker) is
    /// back.
    #[test]
    fn masters_readiness_ignores_a_raw_set_only_an_excluded_light_needs() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        // A fourth light, calibrated ONLY by a 1-frame raw flat — the
        // finding's own shape.
        let (outlier_id, _path) =
            test_fixtures::add_light(&f, &light_spec("outlier", "2025-01-01T00:20:00"));
        let raw_flat = test_fixtures::add_raw_linked_calibration_with_count(
            &f,
            &[outlier_id],
            64,
            48,
            "Flat",
            1,
        );

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let library_dir = f.dir.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();
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
        crate::db::set_setting(
            &f.conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &library_dir.to_string_lossy(),
        )
        .unwrap();

        let settings = SettingsManager::new();

        // Sanity: with the outlier INCLUDED, the unbuildable flat blocks —
        // the existing behaviour this fix must not disturb.
        let plan_included =
            build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert!(
            plan_included.blockers.iter().any(|b| b.code == "masters"),
            "sanity: an unbuildable raw set must block while its only light is included: {:?}",
            plan_included.blockers
        );
        assert!(
            plan_included
                .masters_to_build
                .iter()
                .all(|m| m.set_id != raw_flat),
            "an unbuildable set is a blocker, never planned work: {:?}",
            plan_included.masters_to_build
        );

        // Manually exclude the ONLY light that needs the unbuildable flat.
        set_set_config(&f.conn, f.set_id, "{}", &[outlier_id]).unwrap();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert!(
            !plan.blockers.iter().any(|b| b.code == "masters"),
            "no included light needs this set any more: {:?}",
            plan.blockers
        );
        assert!(
            plan.masters_to_build.iter().all(|m| m.set_id != raw_flat),
            "an excluded-only set must not become planned work either: {:?}",
            plan.masters_to_build
        );
    }

    /// Fix round 1, item 1: with NO calibration library folder configured at
    /// all, an otherwise-buildable raw set is `raw_sets_unbuildable`
    /// (`api::lights::raw_set_unbuildable_without_a_library_folder_configured`
    /// pins the readiness side of this) — the plan gate therefore carries the
    /// `masters` blocker instead of listing the set as planned work.
    #[test]
    fn plan_blocks_a_buildable_raw_set_without_a_library_folder_configured() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_raw_linked_dark(&f, &ids, 64, 48);
        // Deliberately no `CALIBRATION_LIBRARY_DIR` setting.

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

        assert!(
            plan.masters_to_build.is_empty(),
            "{:?}",
            plan.masters_to_build
        );
        let blocker = plan
            .blockers
            .iter()
            .find(|b| b.code == "masters")
            .expect("masters blocker");
        assert!(
            blocker
                .message
                .contains("no calibration library folder configured"),
            "{:?}",
            blocker
        );
    }

    /// Fix round 1, item 5: pins the label format `calibration_set_label`
    /// actually produces for a real row (not the `"<imagetyp> · set <id>"`
    /// fallback, which only the "no row" branch takes).
    #[test]
    fn calibration_set_label_pins_the_format() {
        let f = test_fixtures::frame_set("LDN 1272");
        f.conn
            .execute(
                "INSERT INTO calibration_set (imagetyp, date, exptime, ccd_temp, instrume, frame_count)
                 VALUES ('Dark', '2025-01-01', 180.0, -10.0, 'ATR2600M', 3)",
                [],
            )
            .unwrap();
        let set_id = f.conn.last_insert_rowid();

        let label = calibration_set_label(&f.conn, set_id, "Dark").unwrap();
        assert_eq!(label, "Dark 180s -10°C ATR2600M");
    }

    /// Plan 5b final fix wave, review item A2: a sub-second exposure must
    /// not truncate to `0s` — `calibration_set_label` now reuses
    /// `calibration_library::paths::fmt_num`, the same trailing-zero-trimmed
    /// formatter the master filenames use.
    #[test]
    fn calibration_set_label_keeps_a_sub_second_flat_exposure() {
        let f = test_fixtures::frame_set("LDN 1272");
        f.conn
            .execute(
                "INSERT INTO calibration_set (imagetyp, date, exptime, ccd_temp, instrume, frame_count)
                 VALUES ('Flat', '2025-01-01', 0.39, 0.0, 'ATR2600M', 3)",
                [],
            )
            .unwrap();
        let set_id = f.conn.last_insert_rowid();

        let label = calibration_set_label(&f.conn, set_id, "Flat").unwrap();
        assert_eq!(label, "Flat 0.39s 0°C ATR2600M");
    }

    /// A built master whose FILE is missing but whose `master_provenance`
    /// row and raw source frames are intact becomes planned work
    /// (`masters_to_build == [Rebuild]`) instead of the `masterFiles`
    /// blocker.
    #[test]
    fn plan_lists_rebuildable_missing_master_instead_of_blocking() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        let (dark_set, _flat_set) = test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        // Delete the dark master's FILE from disk — provenance + real raw
        // source frames stay (the `master_provenance` row `add_master_dark_and_flat`
        // now writes via the real `register_master` path).
        let dark_path: String = f
            .conn
            .query_row(
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames fr ON fr.id = csf.frame_id
                 JOIN files fi ON fi.id = fr.file_id
                 WHERE csf.set_id = ?1",
                [dark_set],
                |r| r.get(0),
            )
            .unwrap();
        std::fs::remove_file(&dark_path).unwrap();

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

        assert!(
            !plan.blockers.iter().any(|b| b.code == "masterFiles"),
            "a rebuildable missing master must not block: {:?}",
            plan.blockers
        );
        assert_eq!(
            plan.masters_to_build.len(),
            1,
            "{:?}",
            plan.masters_to_build
        );
        assert_eq!(plan.masters_to_build[0].set_id, dark_set);
        assert_eq!(plan.masters_to_build[0].kind, MasterWork::Rebuild);
        assert_eq!(plan.masters_to_build[0].imagetyp, "Dark");
    }

    /// Plan 5b final fix wave, review finding B5: a master the readiness
    /// split just classified as rebuildable can still fail to resolve a
    /// moment later — its `master_provenance` row deleted between the
    /// readiness split and `collect_masters_to_build`'s own re-read (a real
    /// TOCTOU window, simulated here by deleting it in between the two
    /// calls). Before the fix such a master vanished from the plan
    /// entirely — neither `masters_to_build` nor a blocker — and the run
    /// would only discover it was missing once it reached Calibrate.
    #[test]
    fn collect_masters_to_build_drops_and_blocks_a_master_whose_provenance_vanished() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }
        let (dark_set, _flat_set) = test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);
        let dark_path: String = f
            .conn
            .query_row(
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames fr ON fr.id = csf.frame_id
                 JOIN files fi ON fi.id = fr.file_id
                 WHERE csf.set_id = ?1",
                [dark_set],
                |r| r.get(0),
            )
            .unwrap();
        std::fs::remove_file(&dark_path).unwrap();

        let readiness = compute_export_readiness(&f.conn, f.set_id).unwrap();
        assert!(
            readiness.masters_rebuildable.contains(&dark_set),
            "fixture must classify the dark master as rebuildable before the race: {:?}",
            readiness.masters_rebuildable
        );

        // The race: another actor removes the provenance row after the
        // readiness split already read it.
        f.conn
            .execute(
                "DELETE FROM master_provenance WHERE master_set_id = ?1",
                [dark_set],
            )
            .unwrap();

        let (masters_to_build, dropped) = collect_masters_to_build(&f.conn, &readiness);
        assert!(
            masters_to_build.iter().all(|m| m.set_id != dark_set),
            "a master whose provenance vanished must not be planned work: {:?}",
            masters_to_build
        );
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert_eq!(dropped[0].0, dark_set);

        let blocker = dropped_masters_blocker(&dropped).expect("dropped masters must block");
        assert_eq!(blocker.code, "masters");
        assert!(
            blocker.message.contains("could not be resolved"),
            "{:?}",
            blocker
        );
    }

    /// Task 8b: a missing MASTER FLAT's rebuild reads its own
    /// pre-calibration master (`select_flat_precal`'s choice) — when that
    /// precal master is ALSO missing, `masters_to_build` must list it BEFORE
    /// the flat, so the run's own dependency order (`type_build_rank`,
    /// bias/darkflat -> dark -> flat) rebuilds the dark first and the flat's
    /// rebuild finds it on disk. `test_fixtures` has no ready-made shape for
    /// "a raw flat source set with its own missing-master Dark sub-cal
    /// link", so this seeds inline — the same raw-SQL style
    /// `plan_blocks_missing_master_with_no_provenance` below already uses.
    #[test]
    fn masters_to_build_orders_precal_before_flat() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }

        // Missing MasterFlat 500, provenance -> raw flat source set 501.
        f.conn
            .execute(
                "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, exptime)
                 VALUES (500, 'MasterFlat', '2025-01-01', 1, 2.0)",
                [],
            )
            .unwrap();
        let flat_missing_path = f.dir.path().join("master_flat_500.fits");
        f.conn
            .execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 0, '2025-01-01', 'FITS')",
                rusqlite::params![flat_missing_path.to_string_lossy(), "master_flat_500.fits"],
            )
            .unwrap();
        let flat_file_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterFlat', 1)",
                [flat_file_id],
            )
            .unwrap();
        let flat_frame_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, ?1)",
                [flat_frame_id],
            )
            .unwrap();

        // Raw flat source set 501, real member frames on disk
        // (`check_rebuild_source_ready`'s precondition).
        f.conn
            .execute(
                "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, exptime, frame_count)
                 VALUES (501, 'Flat', '2025-01-01', 0, 2.0, 3)",
                [],
            )
            .unwrap();
        for i in 0..3 {
            let path = f.dir.path().join(format!("flat_501_{i}.fits"));
            std::fs::write(&path, b"flat sub-frame").unwrap();
            f.conn
                .execute(
                    "INSERT INTO files (path, filename, size, modified_at, format)
                     VALUES (?1, ?2, 0, '2025-01-01', 'FITS')",
                    rusqlite::params![path.to_string_lossy(), format!("flat_501_{i}.fits")],
                )
                .unwrap();
            let file_id = f.conn.last_insert_rowid();
            f.conn
                .execute(
                    "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Flat')",
                    [file_id],
                )
                .unwrap();
            let frame_id = f.conn.last_insert_rowid();
            f.conn
                .execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (501, ?1)",
                    [frame_id],
                )
                .unwrap();
        }
        crate::db::master_provenance::insert(
            &f.conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 500,
                source_set_id: Some(501),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2025-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        for &light_id in &ids {
            f.conn
                .execute(
                    "INSERT INTO calibration_set_to_frames
                        (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                     VALUES (?1, 'frame', 500, 'Flat', '2025-01-01T00:00:00Z')",
                    [light_id],
                )
                .unwrap();
        }

        // 501's own Dark sub-cal link -> missing MasterDark 502.
        f.conn
            .execute(
                "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, exptime)
                 VALUES (502, 'MasterDark', '2025-01-01', 1, 2.0)",
                [],
            )
            .unwrap();
        let dark_missing_path = f.dir.path().join("master_dark_502.fits");
        f.conn
            .execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 0, '2025-01-01', 'FITS')",
                rusqlite::params![dark_missing_path.to_string_lossy(), "master_dark_502.fits"],
            )
            .unwrap();
        let dark_file_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
                [dark_file_id],
            )
            .unwrap();
        let dark_frame_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (502, ?1)",
                [dark_frame_id],
            )
            .unwrap();

        // Raw dark source set 503, real member frames on disk.
        f.conn
            .execute(
                "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, frame_count)
                 VALUES (503, 'Dark', '2025-01-01', 0, 3)",
                [],
            )
            .unwrap();
        for i in 0..3 {
            let path = f.dir.path().join(format!("dark_503_{i}.fits"));
            std::fs::write(&path, b"dark sub-frame").unwrap();
            f.conn
                .execute(
                    "INSERT INTO files (path, filename, size, modified_at, format)
                     VALUES (?1, ?2, 0, '2025-01-01', 'FITS')",
                    rusqlite::params![path.to_string_lossy(), format!("dark_503_{i}.fits")],
                )
                .unwrap();
            let file_id = f.conn.last_insert_rowid();
            f.conn
                .execute(
                    "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Dark')",
                    [file_id],
                )
                .unwrap();
            let frame_id = f.conn.last_insert_rowid();
            f.conn
                .execute(
                    "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (503, ?1)",
                    [frame_id],
                )
                .unwrap();
        }
        crate::db::master_provenance::insert(
            &f.conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 502,
                source_set_id: Some(503),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2025-01-01T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        f.conn
            .execute(
                "INSERT INTO calibration_set_to_frames
                    (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                 VALUES (501, 'calibration_set', 502, 'Dark', '2025-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

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

        assert!(
            !plan
                .blockers
                .iter()
                .any(|b| b.code == "masterFiles" || b.code == "masters"),
            "both missing masters are rebuildable, neither should block: {:?}",
            plan.blockers
        );
        assert_eq!(
            plan.masters_to_build.len(),
            2,
            "{:?}",
            plan.masters_to_build
        );
        let set_ids: Vec<i64> = plan.masters_to_build.iter().map(|m| m.set_id).collect();
        assert_eq!(
            set_ids,
            vec![502, 500],
            "the precal dark must build before the flat that reads it: {:?}",
            plan.masters_to_build
        );
        assert_eq!(plan.masters_to_build[0].kind, MasterWork::Rebuild);
        assert_eq!(plan.masters_to_build[1].kind, MasterWork::Rebuild);
    }

    /// A built master whose FILE is missing AND has no `master_provenance`
    /// row (imported, or built outside the app) is not something stage 0.5
    /// can do anything about — it stays the `masterFiles` blocker, naming
    /// "no provenance".
    #[test]
    fn plan_blocks_missing_master_with_no_provenance() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
            ids.push(id);
        }

        // An "imported" master: a calibration_set + files/frames row, NO
        // master_provenance — the file is never written to disk (the
        // "archived or moved" shape).
        f.conn
            .execute(
                "INSERT INTO calibration_set (imagetyp, date, is_master_library, frame_count)
                 VALUES ('MasterDark', '2025-01-01', 1, 1)",
                [],
            )
            .unwrap();
        let master_set_id = f.conn.last_insert_rowid();
        let missing_path = f.dir.path().join("imported_master_dark.fits");
        f.conn
            .execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 0, '2025-01-01', 'FITS')",
                rusqlite::params![missing_path.to_string_lossy(), "imported_master_dark.fits"],
            )
            .unwrap();
        let file_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, 'MasterDark', 1)",
                [file_id],
            )
            .unwrap();
        let frame_id = f.conn.last_insert_rowid();
        f.conn
            .execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![master_set_id, frame_id],
            )
            .unwrap();
        for &light_id in &ids {
            f.conn
                .execute(
                    "INSERT INTO calibration_set_to_frames
                        (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                     VALUES (?1, 'frame', ?2, 'Dark', '2025-01-01T00:00:00Z')",
                    rusqlite::params![light_id, master_set_id],
                )
                .unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();

        assert!(
            plan.masters_to_build.is_empty(),
            "{:?}",
            plan.masters_to_build
        );
        let blocker = plan
            .blockers
            .iter()
            .find(|b| b.code == "masterFiles")
            .expect("masterFiles blocker");
        assert!(blocker.message.contains("no provenance"), "{:?}", blocker);
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

    /// M2 fix round 1, item 1(a): `build_plan` no longer refuses a plan with
    /// `normalization.local.enabled` (or `rejection == "local"`, checked
    /// below too) as `"unsupported"` — the M1-era Gate 6 predates
    /// `integrate_group` having a real `GroupInput.ln` conduit (M2 Task 7).
    #[test]
    fn local_normalization_is_allowed() {
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

        let mut cfg = StackingConfig::default();
        cfg.normalization.local.enabled = true;
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();
        assert!(
            !plan
                .blockers
                .iter()
                .any(|b| b.message == "Local normalization arrives in M2"),
            "{:?}",
            plan.blockers
        );

        // The asymmetry the fix closes: `rejection == "local"` alone (no
        // `local.enabled`) used to pass THIS gate and only die later inside
        // `integrate_group`'s own now-removed refusal.
        let mut cfg2 = StackingConfig::default();
        cfg2.normalization.rejection = RejectionNormalization::Local;
        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg2),
        )
        .unwrap();
        assert!(
            !plan2
                .blockers
                .iter()
                .any(|b| b.message == "Local normalization arrives in M2"),
            "{:?}",
            plan2.blockers
        );
    }

    #[test]
    fn stage_as_str_matches_the_wire_spelling() {
        for (s, name) in [
            (Stage::Masters, "masters"),
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

    /// Seed a `stacking_runs` row (a stand-in for "a previous run already
    /// happened") plus one `stacking_run_groups` row and one
    /// `stacking_run_frames` row per `frame_ids` entry, ALL `included =
    /// true` — [`compute_register_stale`]'s "follow the last run" rule
    /// (final fix wave item 3) reads exactly these rows. Returns the run id.
    fn seed_run_frame_rows(
        conn: &Connection,
        frames_set_id: i64,
        reference_frame_id: Option<i64>,
        reference_mode: &str,
        frame_ids: &[i64],
    ) -> i64 {
        let run_id = crate::db::stacking::insert_run(
            conn,
            &crate::db::stacking::NewRun {
                frames_set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id,
                reference_mode,
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        let group_id = crate::db::stacking::insert_group(
            conn,
            &crate::db::stacking::NewGroup {
                run_id,
                group_key: "g",
                instrume: None,
                color_mode: "mono",
                filter: None,
                binning: Some(1),
                width: Some(1),
                height: Some(1),
                exposure: None,
                frame_count: frame_ids.len() as i64,
                included_count: frame_ids.len() as i64,
            },
        )
        .unwrap();
        for &frame_id in frame_ids {
            crate::db::stacking::upsert_frame_row(
                conn,
                &crate::db::stacking::NewFrameRow {
                    run_id,
                    group_id,
                    frame_id,
                    included: true,
                    exclusion_reason: None,
                    weight: None,
                    weight_channels_json: None,
                    metrics_json: None,
                    reg_status: None,
                    reg_model: None,
                    reg_rms_px: None,
                    reg_inliers: None,
                    reg_inlier_ratio: None,
                    reg_flipped: None,
                    rejected_fraction: None,
                },
            )
            .unwrap();
        }
        run_id
    }

    /// The fresh path, end to end: every frame gets a matching `calibrated`
    /// artifact (real file, recorded size), a matching `metrics` artifact,
    /// a previous run whose `stacking_run_frames` rows include all three
    /// frames, and (manual reference mode) a matching `registration_results`
    /// row — `stale_stages` comes back empty and both cache counters read
    /// the group's full frame count (test scenario (a) of final fix wave
    /// item 3, manual mode). Then one non-reference frame's registration row
    /// is flipped to a hash that no longer matches, and ONLY `Register` goes
    /// stale — Calibrate/Measure are untouched by a change that is
    /// registration-only (scenario (b)).
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

        // A previous run that registered all three frames (the new rule's
        // "which frames matter" source, instead of the current group
        // membership).
        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "manual", &ids);

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

    /// Fix round 1, item 5: `ln_cached`/`stale_stages`'s `Normalize` entry
    /// must react to a REAL config change, not just artifact presence on
    /// disk — verified via the `ln_reference` artifact's stored
    /// `LnReferencePayload` (`reference_member_ids` + `reference_hash`),
    /// which lets `build_plan` recompute each `ln` row's expected hash
    /// (`normalization_hash_for`) without the stage-3 weight ranking that
    /// picked that member list in the first place. Same fixture shape as
    /// `fresh_artifacts_and_registration_leave_nothing_stale` (calibrated +
    /// metrics artifacts, a previous run's frame rows, matching
    /// registration rows for all three frames), plus hand-seeded
    /// `ln_reference`/`ln` artifacts at the hashes the FIRST config would
    /// produce.
    #[test]
    fn local_normalization_reacts_to_a_config_change() {
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
        cfg.normalization.local.enabled = true;
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

        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "manual", &ids);

        let reference_calib_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        let mut registration_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let frame_hash = calib_hashes.get(&gf.frame_id).unwrap();
            let expected = registration_hash_for(&cfg, ids[0], &reference_calib_hash, frame_hash);
            registration_hashes.insert(gf.frame_id, expected.clone());
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

        // The LN reference: all three frames are candidates (weight order
        // is irrelevant here — `build_plan` never re-derives it, it trusts
        // the stored list; that trust is exactly item 5's documented
        // residual).
        let reference_member_ids: Vec<i64> = ids.clone();
        let combined = {
            let mut hashes: Vec<&str> = reference_member_ids
                .iter()
                .map(|id| registration_hashes[id].as_str())
                .collect();
            hashes.sort_unstable();
            hashes.join(",")
        };
        let reference_hash = normalization_hash_for(&cfg, &combined, &reference_member_ids, "");
        let reference_payload = serde_json::to_string(&LnReferencePayload {
            reference_member_ids: reference_member_ids.clone(),
            reference_hash: reference_hash.clone(),
        })
        .unwrap();
        let reference_path = f.dir.path().join("reference.fits");
        std::fs::write(&reference_path, [0u8]).unwrap();
        let reference_size = std::fs::metadata(&reference_path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                frames_set_id: f.set_id,
                frame_id: None,
                group_key: &group_key,
                kind: "ln_reference",
                path: Some(reference_path.to_str().unwrap()),
                config_hash: &reference_hash,
                size: Some(reference_size),
                modified_at: None,
                payload_json: Some(&reference_payload),
            },
        )
        .unwrap();

        for gf in &groups[0].frames {
            let frame_hash = normalization_hash_for(
                &cfg,
                &registration_hashes[&gf.frame_id],
                &reference_member_ids,
                &reference_hash,
            );
            let sidecar_path = f.dir.path().join(format!("f{}.athln", gf.frame_id));
            std::fs::write(&sidecar_path, [0u8]).unwrap();
            let size = std::fs::metadata(&sidecar_path).unwrap().len() as i64;
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    frames_set_id: f.set_id,
                    frame_id: Some(gf.frame_id),
                    group_key: &group_key,
                    kind: "ln",
                    path: Some(sidecar_path.to_str().unwrap()),
                    config_hash: &frame_hash,
                    size: Some(size),
                    modified_at: None,
                    payload_json: None,
                },
            )
            .unwrap();
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
        assert!(
            !plan.stale_stages.contains(&Stage::Normalize),
            "{:?}",
            plan.stale_stages
        );
        assert_eq!(plan.groups[0].ln_cached, 3, "{:?}", plan.groups[0]);

        // A config change to `normalization.local.scale` invalidates every
        // stored `ln` row's hash (the subtree folds the WHOLE
        // `normalization` block in) without touching a single artifact on
        // disk.
        let mut cfg2 = cfg.clone();
        cfg2.normalization.local.scale += 256;
        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg2),
        )
        .unwrap();
        assert!(
            plan2.stale_stages.contains(&Stage::Normalize),
            "{:?}",
            plan2.stale_stages
        );
        assert_eq!(plan2.groups[0].ln_cached, 0, "{:?}", plan2.groups[0]);
    }

    /// M2 Task 7 carry-over (c) (Task 5's re-review): a frame the LATEST run
    /// itself runtime-excluded for a local-normalization reason
    /// (`TooFewMatches`, a sidecar write/read failure) has no `ln` artifact
    /// BY DESIGN — its own `normalize_frame` call never produced one, and
    /// won't for the same underlying reason until something upstream
    /// changes. Before the fix, `normalize_stale` stayed `true` FOREVER for
    /// such a group: this one known-bad frame could never earn a fresh `ln`
    /// row on its own. `ids[2]` gets no `ln` artifact and no registration
    /// row at all (same shape `register_stale_ignores_a_frame_the_run_excluded`
    /// uses for its own analogous case) — only a `stacking_run_frames` row
    /// saying `included = false` with a "local normalization: …" reason.
    #[test]
    fn normalize_stale_ignores_a_frame_the_run_excluded_for_local_normalization() {
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
        cfg.normalization.local.enabled = true;
        // Fix round 1, item 4: the seeded run's own `config_hash` must
        // match `cfg`'s for the "not owed" gate to trust it at all —
        // resolved once, before any later mutation of `cfg`.
        let run_config_hash = config_hash(&cfg);
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

        // Calibrated + metrics artifacts for all three — this test is about
        // Normalize staleness specifically, so Calibrate/Measure must read
        // as fresh for every frame, excluded one included (its file was
        // still calibrated/measured; only stage 6's own normalize_frame
        // call failed for it).
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

        // The run: ids[0]/ids[1] included and normalized; ids[2] excluded by
        // stage 6 itself, the same way `run_group_normalization`'s own
        // `exclude_frame_and_persist` call would leave it.
        let run_id = crate::db::stacking::insert_run(
            &f.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: f.set_id,
                config_json: "{}",
                config_hash: &run_config_hash,
                reference_frame_id: Some(ids[0]),
                reference_mode: "manual",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        let group_id = crate::db::stacking::insert_group(
            &f.conn,
            &crate::db::stacking::NewGroup {
                run_id,
                group_key: &group_key,
                instrume: None,
                color_mode: "mono",
                filter: None,
                binning: Some(1),
                width: Some(1),
                height: Some(1),
                exposure: None,
                frame_count: 3,
                included_count: 2,
            },
        )
        .unwrap();
        let seed_frame_row = |included: bool, reason: Option<&str>, frame_id: i64| {
            crate::db::stacking::upsert_frame_row(
                &f.conn,
                &crate::db::stacking::NewFrameRow {
                    run_id,
                    group_id,
                    frame_id,
                    included,
                    exclusion_reason: reason,
                    weight: None,
                    weight_channels_json: None,
                    metrics_json: None,
                    reg_status: None,
                    reg_model: None,
                    reg_rms_px: None,
                    reg_inliers: None,
                    reg_inlier_ratio: None,
                    reg_flipped: None,
                    rejected_fraction: None,
                },
            )
            .unwrap();
        };
        seed_frame_row(true, None, ids[0]);
        seed_frame_row(true, None, ids[1]);
        seed_frame_row(
            false,
            Some("local normalization: 5 matched stars (< 20)"),
            ids[2],
        );

        // Registration rows for the two INCLUDED-AND-NORMALIZED frames only
        // — none at all for ids[2], same as the run-excluded case elsewhere.
        let reference_calib_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        let mut registration_hashes: HashMap<i64, String> = HashMap::new();
        for &frame_id in &ids[..2] {
            let frame_hash = calib_hashes.get(&frame_id).unwrap();
            let expected = registration_hash_for(&cfg, ids[0], &reference_calib_hash, frame_hash);
            registration_hashes.insert(frame_id, expected.clone());
            let is_reference = frame_id == ids[0];
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id,
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

        // The LN reference: only the two normalized frames are candidates —
        // ids[2] was never eligible (it failed before ever contributing).
        let reference_member_ids: Vec<i64> = ids[..2].to_vec();
        let combined = {
            let mut hashes: Vec<&str> = reference_member_ids
                .iter()
                .map(|id| registration_hashes[id].as_str())
                .collect();
            hashes.sort_unstable();
            hashes.join(",")
        };
        let reference_hash = normalization_hash_for(&cfg, &combined, &reference_member_ids, "");
        let reference_payload = serde_json::to_string(&LnReferencePayload {
            reference_member_ids: reference_member_ids.clone(),
            reference_hash: reference_hash.clone(),
        })
        .unwrap();
        let reference_path = f.dir.path().join("reference.fits");
        std::fs::write(&reference_path, [0u8]).unwrap();
        let reference_size = std::fs::metadata(&reference_path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                frames_set_id: f.set_id,
                frame_id: None,
                group_key: &group_key,
                kind: "ln_reference",
                path: Some(reference_path.to_str().unwrap()),
                config_hash: &reference_hash,
                size: Some(reference_size),
                modified_at: None,
                payload_json: Some(&reference_payload),
            },
        )
        .unwrap();

        // `ln` artifacts for ids[0]/ids[1] only — none at all for ids[2].
        for &frame_id in &ids[..2] {
            let frame_hash = normalization_hash_for(
                &cfg,
                &registration_hashes[&frame_id],
                &reference_member_ids,
                &reference_hash,
            );
            let sidecar_path = f.dir.path().join(format!("f{frame_id}.athln"));
            std::fs::write(&sidecar_path, [0u8]).unwrap();
            let size = std::fs::metadata(&sidecar_path).unwrap().len() as i64;
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    frames_set_id: f.set_id,
                    frame_id: Some(frame_id),
                    group_key: &group_key,
                    kind: "ln",
                    path: Some(sidecar_path.to_str().unwrap()),
                    config_hash: &frame_hash,
                    size: Some(size),
                    modified_at: None,
                    payload_json: None,
                },
            )
            .unwrap();
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
        assert!(
            !plan.stale_stages.contains(&Stage::Normalize),
            "a run-excluded LN frame must not keep Normalize stale forever: {:?}",
            plan.stale_stages
        );
        assert_eq!(
            plan.groups[0].ln_cached, 2,
            "only the two normalized frames are cached: {:?}",
            plan.groups[0]
        );

        // Control: flip ids[2]'s row to `included = true` with no LN
        // exclusion reason (still no `ln` artifact — some OTHER, non-LN gap)
        // — this is the ordinary "missing artifact" case and MUST still
        // read as stale, proving the fix is scoped to the LN-exclusion
        // reason, not a blanket "ignore any run-excluded-or-not frame".
        seed_frame_row(true, None, ids[2]);
        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg.clone()),
        )
        .unwrap();
        assert!(
            plan2.stale_stages.contains(&Stage::Normalize),
            "an included frame with no ln artifact and no LN exclusion reason must still be stale: {:?}",
            plan2.stale_stages
        );

        // Fix round 1, item 4: re-exclude ids[2] the same way as the first
        // phase, but change a config field OUTSIDE the `normalization`/
        // `measurement.{psfModel,maxStars}` subtree (`normalization_hash_for`'s
        // own inputs — `local_normalization_reacts_to_a_config_change`
        // already pins that changing `normalization.local.*` itself
        // invalidates every frame's `ln` artifact, which would trivially
        // pass this assertion for an unrelated reason and prove nothing
        // about item 4 specifically). `integration.min_weight` changes the
        // WHOLE-config `config_hash` (the run-vs-current-build comparison
        // item 4 gates on) while leaving ids[0]/ids[1]'s own `ln` artifacts
        // reading exactly as fresh as in the first phase (`ln_cached == 2`
        // again below) — isolating that ids[2]'s reverted "not owed" status
        // is what actually drives `Normalize` stale here.
        seed_frame_row(
            false,
            Some("local normalization: 5 matched stars (< 20)"),
            ids[2],
        );
        let mut cfg3 = cfg;
        cfg3.integration.min_weight += 0.001;
        let plan3 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg3),
        )
        .unwrap();
        assert_eq!(
            plan3.groups[0].ln_cached, 2,
            "the config change must not itself invalidate ids[0]/ids[1]'s own ln artifacts: {:?}",
            plan3.groups[0]
        );
        assert!(
            plan3.stale_stages.contains(&Stage::Normalize),
            "an LN exclusion recorded under a DIFFERENT config must not be trusted: {:?}",
            plan3.stale_stages
        );
    }

    /// Final fix wave item 3, scenario (c): a frame the LATEST RUN itself
    /// excluded (`stacking_run_frames.included = false`, e.g. dropped by
    /// stage-3 selection) needs no registration row at all to leave
    /// Register not stale — only the frames the run actually kept matter.
    #[test]
    fn register_stale_ignores_a_frame_the_run_excluded() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        set_frame_set_reference(&f.conn, f.set_id, ids[0]).unwrap();

        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");

        let mut divisors = DivisorCache::new();
        let mut calib_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            calib_hashes.insert(gf.frame_id, hash);
        }

        // Run: ids[0]/ids[1] included, ids[2] excluded by the run itself
        // (e.g. dropped below the weight floor at stage 3).
        let run_id = crate::db::stacking::insert_run(
            &f.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: f.set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: Some(ids[0]),
                reference_mode: "manual",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        let group_id = crate::db::stacking::insert_group(
            &f.conn,
            &crate::db::stacking::NewGroup {
                run_id,
                group_key: "g",
                instrume: None,
                color_mode: "mono",
                filter: None,
                binning: Some(1),
                width: Some(1),
                height: Some(1),
                exposure: None,
                frame_count: 3,
                included_count: 2,
            },
        )
        .unwrap();
        for (i, &frame_id) in ids.iter().enumerate() {
            crate::db::stacking::upsert_frame_row(
                &f.conn,
                &crate::db::stacking::NewFrameRow {
                    run_id,
                    group_id,
                    frame_id,
                    included: i != 2,
                    exclusion_reason: if i == 2 {
                        Some("below the weight floor")
                    } else {
                        None
                    },
                    weight: None,
                    weight_channels_json: None,
                    metrics_json: None,
                    reg_status: None,
                    reg_model: None,
                    reg_rms_px: None,
                    reg_inliers: None,
                    reg_inlier_ratio: None,
                    reg_flipped: None,
                    rejected_fraction: None,
                },
            )
            .unwrap();
        }

        // Registration rows for the two INCLUDED frames only — none at all
        // for ids[2], the run-excluded one.
        let reference_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        for &frame_id in &ids[..2] {
            let frame_hash = calib_hashes.get(&frame_id).unwrap();
            let expected = registration_hash_for(&cfg, ids[0], &reference_hash, frame_hash);
            let is_reference = frame_id == ids[0];
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id,
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
            Some(cfg),
        )
        .unwrap();
        assert!(
            !plan.stale_stages.contains(&Stage::Register),
            "a frame the run itself excluded must not force Register stale: {:?}",
            plan.stale_stages
        );
    }

    /// Final fix wave item 3, scenarios (a) and (e) in Auto mode: with a
    /// previous run's OWN recorded `reference_frame_id` and matching
    /// registration rows, Register is not stale (a); a SECOND run choosing a
    /// DIFFERENT reference than the first, without the registration rows
    /// ever being updated to match, makes Register stale again (e) — even
    /// though every row's own `config_hash` still matches its own (now
    /// wrong) `reference_frame_id`.
    #[test]
    fn register_stale_auto_mode_follows_the_runs_own_reference() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let cfg = StackingConfig::default();
        assert_eq!(
            cfg.reference.mode,
            ReferenceMode::Auto,
            "the default is Auto"
        );

        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");

        let mut divisors = DivisorCache::new();
        let mut calib_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            calib_hashes.insert(gf.frame_id, hash);
        }

        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "auto", &ids);

        let reference_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        for &frame_id in &ids {
            let frame_hash = calib_hashes.get(&frame_id).unwrap();
            let expected = registration_hash_for(&cfg, ids[0], &reference_hash, frame_hash);
            let is_reference = frame_id == ids[0];
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id,
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
        assert!(
            !plan.stale_stages.contains(&Stage::Register),
            "{:?}",
            plan.stale_stages
        );

        // A second run recorded a DIFFERENT reference — the registration
        // rows still point at the OLD one.
        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[1]), "auto", &ids);

        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();
        assert!(
            plan2.stale_stages.contains(&Stage::Register),
            "a reference change the registration rows don't reflect must be stale: {:?}",
            plan2.stale_stages
        );
    }

    /// M4b ruling R-M4b-4: in `native` mode a group's expected reference is
    /// the one the LAST RUN itself recorded for that group
    /// (`SummaryGroup.reference_frame_id` in its stored `summary_json`) —
    /// there is no run-wide reference every frame can be judged against any
    /// more. Rows written against that reference read fresh; the same rows
    /// read stale the moment the summary names a different frame for the
    /// group.
    #[test]
    fn register_stale_native_mode_follows_each_groups_own_reference() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let mut cfg = StackingConfig::default();
        cfg.registration.geometry = RegistrationGeometry::Native;

        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");
        let group_key = groups[0].key.clone();

        let mut divisors = DivisorCache::new();
        let mut calib_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            calib_hashes.insert(gf.frame_id, hash);
        }

        // Every row registered onto `ids[1]` — deliberately NOT the run
        // row's own `reference_frame_id`, so only the per-group summary can
        // explain them.
        let group_reference = ids[1];
        let reference_hash = calib_hashes.get(&group_reference).unwrap().clone();
        for &frame_id in &ids {
            let frame_hash = calib_hashes.get(&frame_id).unwrap();
            let expected =
                registration_hash_for(&cfg, group_reference, &reference_hash, frame_hash);
            let is_reference = frame_id == group_reference;
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id,
                reference_frame_id: group_reference,
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
        let plan_with_summary = |summary_reference: i64| -> Vec<Stage> {
            let run_id = seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "auto", &ids);
            let summary = serde_json::json!({
                "groups": [{ "key": group_key, "referenceFrameId": summary_reference }]
            })
            .to_string();
            crate::db::stacking::finish_run(&f.conn, run_id, "done", Some(&summary), None).unwrap();
            build_plan(
                &f.conn,
                &settings,
                &PathPolicy::AllowAll,
                f.set_id,
                Some(cfg.clone()),
            )
            .unwrap()
            .stale_stages
        };

        let fresh = plan_with_summary(group_reference);
        assert!(
            !fresh.contains(&Stage::Register),
            "rows written against the group's own recorded reference must read fresh: {fresh:?}"
        );

        let moved = plan_with_summary(ids[2]);
        assert!(
            moved.contains(&Stage::Register),
            "a group whose recorded reference no longer matches its rows must be stale: {moved:?}"
        );
    }

    /// Ruling R-M4a-6: after a run whose two-pass pick MOVED the reference
    /// (the run row records the frame it switched TO, and stage 5's
    /// persisting pass wrote every `registration_results` row against that
    /// same frame), the next plan with the same config must NOT call
    /// Register stale — otherwise every plan after a switch would demand a
    /// re-registration and every run would pay two passes again.
    ///
    /// The rule needed no new code: [`compute_register_stale`] in `Auto`
    /// mode already takes the expected reference from the LAST RUN's own
    /// `stacking_runs.reference_frame_id`, which is exactly what
    /// `run.rs`'s switch updates (`set_run_reference`). This test is the
    /// pin on that.
    ///
    /// Flipping `reference.twoPass` off does NOT make Register stale
    /// either, and deliberately so: the registration stage hash folds in
    /// `cfg.registration` and the resolved `reference_frame_id`, never
    /// `cfg.reference` — the toggle says HOW a reference gets chosen, and
    /// the stored rows already record WHICH frame it was. (The Auto rule
    /// is an approximation in both directions — it cannot know what stage 4
    /// will pick next time without measuring the set first; a run that then
    /// resolves a different reference re-registers and the NEXT plan sees
    /// it, which is scenario (e) of
    /// `register_stale_auto_mode_follows_the_runs_own_reference`.)
    #[test]
    fn register_stale_accepts_a_two_pass_switched_reference() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let cfg = StackingConfig::default();
        assert_eq!(cfg.reference.mode, ReferenceMode::Auto);
        assert!(cfg.reference.two_pass, "the default is on");

        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");

        let mut divisors = DivisorCache::new();
        let mut calib_hashes: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            calib_hashes.insert(gf.frame_id, hash);
        }

        // The run STARTED on ids[0] (best weight) and the two-pass pick
        // switched it to ids[1]; `set_run_reference` left the row naming
        // ids[1], and pass 2 wrote every row against ids[1].
        let switched_to = ids[1];
        seed_run_frame_rows(&f.conn, f.set_id, Some(switched_to), "auto", &ids);

        let reference_hash = calib_hashes.get(&switched_to).unwrap().clone();
        for &frame_id in &ids {
            let frame_hash = calib_hashes.get(&frame_id).unwrap();
            let expected = registration_hash_for(&cfg, switched_to, &reference_hash, frame_hash);
            let is_reference = frame_id == switched_to;
            let rec = RegistrationRecord {
                frames_set_id: f.set_id,
                frame_id,
                reference_frame_id: switched_to,
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
        assert!(
            !plan.stale_stages.contains(&Stage::Register),
            "a switched reference the rows already name must not be stale: {:?}",
            plan.stale_stages
        );

        let mut off = cfg.clone();
        off.reference.two_pass = false;
        let plan_off = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(off),
        )
        .unwrap();
        assert!(
            !plan_off.stale_stages.contains(&Stage::Register),
            "reference.twoPass is not part of the registration stage hash: {:?}",
            plan_off.stale_stages
        );
    }

    /// Final fix wave item 3, scenario (d): with no previous run at all,
    /// Register is always stale — nothing to compare against, regardless of
    /// reference mode.
    #[test]
    fn register_stale_with_no_previous_run() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let settings = SettingsManager::new();
        let plan = build_plan(&f.conn, &settings, &PathPolicy::AllowAll, f.set_id, None).unwrap();
        assert!(
            plan.stale_stages.contains(&Stage::Register),
            "{:?}",
            plan.stale_stages
        );
    }

    /// Blocker order is stable and matches the gate order exactly (fix
    /// round 1 item 7): no calibration links (masters/links) fires first,
    /// then both folder sentences (in `working`, `output` order), then
    /// `frames` (fewer than 3 included) — nothing from gate 6 (M2 fix round
    /// 1 item 1(a) removed local normalization's own blocker,
    /// `cfg.normalization.local.enabled` is set below specifically to pin
    /// that it adds none of its own; M3 Task 5 removed drizzle's blanket
    /// "arrives in M3" one — `cfg.drizzle.enabled` at its default `scale`
    /// (2, in range) is pinned here to add none either, brief test (g)'s
    /// first half) — `reference`/`space` never appear here (Auto mode; free
    /// space is never probed once the folders themselves are blocked).
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
            vec!["links", "folders", "folders", "frames"],
            "{:?}",
            plan.blockers
        );
        assert_eq!(plan.blockers[1].message, "Choose a working folder");
        assert_eq!(plan.blockers[2].message, "Choose an output folder");
    }

    /// M3 Task 5, brief test (g), second half: an out-of-range drizzle
    /// `scale` (ruling R-M3-10: only 1, 2 or 3 are supported) is the ONE
    /// thing gate 6 still blocks on.
    #[test]
    fn drizzle_out_of_range_scale_is_the_one_gate_6_blocker() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in ["2025-01-01T00:00:00", "2025-01-01T00:05:00"]
            .iter()
            .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 4;

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            f.set_id,
            Some(cfg),
        )
        .unwrap();

        let unsupported: Vec<&str> = plan
            .blockers
            .iter()
            .filter(|b| b.code == "unsupported")
            .map(|b| b.message.as_str())
            .collect();
        assert_eq!(
            unsupported,
            vec!["drizzle scale must be 1, 2 or 3"],
            "{:?}",
            plan.blockers
        );
    }
}
