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
    active_run_for_subject, find_artifact, get_set_config, list_artifacts, list_frame_rows,
    list_runs, upsert_artifact, NewArtifact, StackSubject, StackingArtifactRow,
};
use crate::export::{resolve_generation_cached, resolved_master_paths, DivisorCache};
use crate::integration::stats::RejectionNormalization;
use crate::registration::db::{
    get_frame_set_reference, get_registration_for_frame_set, upsert_registration,
    RegistrationRecord,
};
use crate::settings::{keys, SettingsManager};
use crate::stacking::config::{
    calibration_subtree, config_hash, measurement_subtree, normalization_subtree,
    registration_subtree, resolve_config, stage_hash, CleanupPolicy, ReferenceMode, SourceIdentity,
    StackingConfig,
};
use crate::stacking::fits_artifact;
use crate::stacking::groups::{
    frame_source, median_f64, ColorMode, GroupFrame, IntegrationGroup, ScaleSource,
};
use crate::stacking::ln::SeedsCalibration;
use crate::stacking::measure::FrameMeasurement;
use crate::stacking::origin::{origin_of, FrameOrigin, FrameOrigins};
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
    // The group's exposure-class token (spec §8) — what the Groups table
    // shows beside the seconds value.
    pub exposure_class: String,
    pub frame_count: usize,
    pub included_count: usize,
    pub total_exposure_s: f64,
    pub calibrated_cached: usize,
    pub metrics_cached: usize,
    /// Tier C Task 1: frames (of `frame_count`) whose `metrics` row AND
    /// every one of its planes' `fits` rows are all fresh — a subset of
    /// `metrics_cached` in practice (see [`build_plan`]'s own doc on the
    /// per-frame loop), never counted when `metrics` itself is stale.
    pub fits_cached: usize,
    /// Frames (of `included_count`) whose `.athln` sidecar already exists on
    /// disk (spec §9.3, M2) — `0` when local normalization is off. A
    /// PRESENCE check, not a hash-verified freshness one: see the doc on
    /// this field's computation in [`build_plan`] for why (the LN reference
    /// member list is a stage-3 weight quantity, unavailable at plan time).
    pub ln_cached: usize,
    // The LN scale this group will run at (spec §2.3), `None` while LN is
    // inactive; `ln_scale_estimated` when the geometry is not yet the one
    // the sidecars were written in (no stored LN reference). `ln_width` /
    // `ln_height` are that geometry (P-5).
    pub ln_scale_px: Option<u32>,
    pub ln_scale_estimated: bool,
    pub ln_width: Option<u32>,
    pub ln_height: Option<u32>,
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

/// A project plan's grid state (spec §7): `unset` — no confirmed grid and
/// no proposal (no accepted frame has a pixel scale); `proposed` — a
/// proposal, not confirmed; `confirmed` — the hub has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum GridState {
    Unset,
    Proposed,
    Confirmed,
}

/// A project plan's grid (spec §6, §7).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanGrid {
    pub state: GridState,
    pub current: Option<crate::collab::grid::CanonicalGrid>,
    pub proposal: Option<crate::collab::grid::CanonicalGrid>,
    // Stackable frames with a WCS at least half inside the grid in use
    // (the confirmed one, else the proposal).
    pub frames_inside: usize,
    pub can_edit: bool,
}

/// One row of a project plan's Frames table (spec §7): every published,
/// accepted frame of the project, stackable on this device or not.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanProjectFrame {
    // The frame's handle — `None` for a frame not stackable here (a handle
    // is minted only when a plan includes the frame).
    pub handle: Option<i64>,
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    pub filter_canonical: String,
    pub exptime_sec: f64,
    pub color_mode: ColorMode,
    // The manifest carries a WCS (else the run plate-solves the frame).
    pub has_wcs: bool,
    // Stackable on this device.
    pub on_disk: bool,
    // Why the frame is not stacked here; `None` when it is.
    pub reason: Option<String>,
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
    // Exactly one of `set_id` / `project_id` is set (plan 6b-2 C1).
    pub set_id: Option<i64>,
    pub project_id: Option<String>,
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
    // A project plan's grid (spec §7); `None` for a frame set.
    pub grid: Option<PlanGrid>,
    // A project plan's Frames rows (spec §7); empty for a frame set.
    pub project_frames: Vec<PlanProjectFrame>,
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

/// Stage 1's hash for an ATTESTED light (spec §13, plan 6b-2 C11): the
/// Prepare subtree and the light's own `files` identity, with no master
/// upstream, because an attested light is never calibrated.
pub(crate) fn prepare_hash_for_catalog(cfg: &StackingConfig, frame: &GroupFrame) -> String {
    stage_hash(
        &crate::stacking::config::prepare_subtree(cfg),
        &[],
        &[SourceIdentity {
            file_id: frame.file_id,
            size: frame.size,
            modified_at: frame.modified_at.clone(),
        }],
    )
}

/// Stage 1's hash for a prepared PROJECT frame (spec §5.1, plan 6b-2 C12):
/// the Prepare subtree, the frame's content (`blake3`) and byte size —
/// never a catalog identity. Every later stage hash chains on it exactly as
/// on a calibration hash.
pub(crate) fn prepare_hash_for_project(
    cfg: &StackingConfig,
    blake3: &str,
    byte_size: i64,
) -> String {
    stage_hash(
        &crate::stacking::config::prepare_subtree(cfg),
        &[&format!("blake3:{blake3}")],
        &[SourceIdentity {
            file_id: 0,
            size: byte_size,
            modified_at: String::new(),
        }],
    )
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
    /// Where each frame comes from; the stage-1 hash dispatches on it. A
    /// frame with no origin is an error, never a silent catalog read.
    origins: FrameOrigins,
}

impl HashMemo {
    pub(crate) fn new(origins: FrameOrigins) -> Self {
        HashMemo {
            divisors: DivisorCache::new(),
            by_frame: HashMap::new(),
            origins,
        }
    }

    /// The stage-1 hash of one frame, dispatched by its origin.
    fn hash_by_origin(
        &mut self,
        conn: &Connection,
        cfg: &StackingConfig,
        frame: &GroupFrame,
    ) -> Result<String, ApiError> {
        match origin_of(&self.origins, frame.frame_id)? {
            FrameOrigin::Catalog {
                attested: false, ..
            } => calibration_hash_for(conn, cfg, frame, &mut self.divisors),
            // Spec §13 (R3): an attested light is prepared, never calibrated.
            FrameOrigin::Catalog { attested: true, .. } => Ok(prepare_hash_for_catalog(cfg, frame)),
            FrameOrigin::Project {
                blake3, byte_size, ..
            } => Ok(prepare_hash_for_project(cfg, blake3, *byte_size)),
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
        let result = match self.hash_by_origin(conn, cfg, frame) {
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
        let result = self.hash_by_origin(conn, cfg, frame);
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
/// own calibration merely changed), the reference's own stage-1 hash, this
/// frame's stage-1 hash, and — Tier C Task 3 fix round 1 (ruling C-18) —
/// this frame's own MEASUREMENT hash.
///
/// Ruling C-18: Register now reads a mono frame's `fits.0` artifact
/// (`stacking::register::detect::stars_from_fits`), and that artifact's
/// CONTENT moves with `measurement.maxStars`/`detectionSigma`/
/// `seedDetector`/`seedPrefilter`/`structure` — none of which
/// `registration_subtree` carries (it serializes only `cfg.registration` +
/// the measure version, formerly `PSF_FIT_VERSION`). Without this fold-in, changing `measurement.maxStars`
/// rewrote every `.athf` (Measure's own hash follows it) while the plan
/// gate still reported Register cached and a run reused stale registration
/// rows built from fits the file on disk no longer matched — exactly the
/// failure ruling C-17 fixed for Normalize's own read of the same artifact
/// (`normalization_hash_for`), fixed here identically: the SAME
/// `measurement_hash_for(cfg, frame_calibrated_hash)` value, empty-string
/// skipped as "not recorded" rather than folded in as some other frame's.
///
/// The invalidation is uniform across BOTH mono and OSC frames, even though
/// only mono frames ever actually read the `fits` artifact (OSC keeps the
/// luminance detection, ruling C-6): a per-plane-count hash (folding the
/// measurement hash in for mono only) would split this rule into two cases
/// every caller has to get right for the SAME field, in exchange for
/// sparing an OSC frame's `registration_results` row an invalidation it
/// never needed — a far smaller cost than a second hash-construction path.
pub(crate) fn registration_hash_for(
    cfg: &StackingConfig,
    reference_frame_id: i64,
    reference_calibrated_hash: &str,
    frame_calibrated_hash: &str,
    frame_measurement_hash: &str,
) -> String {
    let reference_token = format!("ref:{reference_frame_id}");
    let mut upstream: Vec<String> = vec![
        reference_token,
        reference_calibrated_hash.to_string(),
        frame_calibrated_hash.to_string(),
    ];
    if !frame_measurement_hash.is_empty() {
        upstream.push(format!("measure:{frame_measurement_hash}"));
    }
    let upstream_refs: Vec<&str> = upstream.iter().map(String::as_str).collect();
    stage_hash(&registration_subtree(cfg), &upstream_refs, &[])
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
    // The LN geometry the reference (and every sidecar against it) lives in,
    // and the scale of the run that built it. 0 on a payload written before
    // 2026-10 (the plan then estimates, spec §2.3 case 3).
    #[serde(default)]
    pub width: usize,
    #[serde(default)]
    pub height: usize,
    #[serde(default)]
    pub scale_px: u32,
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
///
/// `beta` (final fix wave, ruling C-30, the whole-branch review's M1) is the
/// group's own β — [`crate::stacking::psf_signal::group_beta`] over every
/// member Measure actually measured, the value `stacking_run_groups.beta`
/// records. It is a real LN INPUT: with `normalization.local.psfModel =
/// Auto`, stage 6 fits the LN reference at `PsfModel::Fixed(group_beta)` and
/// `relative_scale_from_seeds` re-fits every member's seeds at that same β,
/// so two runs whose β differ produce different `A`/`B` grids from byte-
/// identical inputs otherwise. It was the ONE such input no hash folded:
/// `normalization_subtree` carries `psfModel` (the CHOICE) but never the
/// value `Auto` resolves to, which moves with the group's measured
/// population, not with `cfg`. Folded at `{:.6}` for the reason
/// [`SeedsCalibration`]'s `k` is: the rounding keeps the two sides'
/// formatting agreement exact. β was picked from a small fixed ladder
/// (`psf_signal::AUTO_BETAS`) until Plan A Task 9; it is continuous since
/// (F-4), and both sides still fold the same stored `f64`, so a β that moves
/// by less than 5e-7 — nothing LN can see — is the only change the rounding
/// hides.
///
/// `None` hashes as "not recorded" (nothing folded), never as some default
/// value — which is what keeps the run and the gate in step for a group
/// that measured nothing at all. Both sides must resolve it the same way:
/// the run passes what stage 3 put in `RunContext::measured_groups`, the
/// gate passes `stacking_run_groups.beta` of the LAST run for that group.
/// That read carries the same "one run's optimism" residual the stored
/// `ln_calibration` already has — the gate is asking "what were the
/// sidecars on disk written under", and the last run is who wrote them; a
/// newer run that never reached stage 3 for this group reports `NULL`, the
/// hash then differs, and the group reads STALE. Conservative in the safe
/// direction (a needless re-normalize, never a false cache hit).
///
/// Folded UNCONDITIONALLY, not only when `psfModel` is `Auto`: splitting
/// this into two cases every caller has to get right for the SAME field is
/// what [`registration_hash_for`]'s own doc argues against just above, and
/// the cost of the conservative rule is one extra re-normalize for a group
/// pinned to an explicit β whose measured population moved anyway.
///
/// This fold invalidates every `.athln` sidecar once, on the first run after
/// it ships — no template on disk carries any, and the cost is one LN
/// fan-out per group.
///
/// `ln_scale_px` is the resolved LN scale (spec 2026-10-04 §2, L-19): the
/// reference hash passes `None` (the reference is scale-independent), every
/// per-frame hash passes the scale the frame was normalized at.
pub(crate) fn normalization_hash_for(
    cfg: &StackingConfig,
    registration_hash: &str,
    measurement_hash: &str,
    reference_member_ids: &[i64],
    reference_hash: &str,
    calibration: Option<&SeedsCalibration>,
    beta: Option<f64>,
    ln_scale_px: Option<u32>,
) -> String {
    let mut upstream: Vec<String> = vec!["register".to_string(), registration_hash.to_string()];
    // C-17: the frame's own MEASUREMENT hash (`measurement_hash_for`, the
    // same value its `fits` artifact is keyed on), because stage 6 reads
    // that artifact. `normalization_subtree` carries only
    // `measurement.psfModel`/`maxStars` — NOT `detectionSigma`,
    // `seedDetector`, `seedPrefilter` or `structure`, every one of which
    // reshapes the `.athf` seed population that
    // `scale::relative_scale_from_seeds` fits and that the group's `k` is
    // derived from. Without this, flipping `seedDetector` to `structure`
    // rewrote every `.athf` while the plan gate still reported Normalize
    // cached and the run reused both the sidecars and the stored `k`. The
    // caller-supplied string is the ONE seam: the run and the gate must
    // pass the same value for the same frame (empty when a frame's
    // measurement hash cannot be resolved at all, which hashes as "not
    // recorded" rather than silently as some other frame's).
    if !measurement_hash.is_empty() {
        upstream.push(format!("measure:{measurement_hash}"));
    }
    upstream.extend(reference_member_ids.iter().map(|id| format!("ln_ref:{id}")));
    if !reference_hash.is_empty() {
        upstream.push(format!("ln_reference_hash:{reference_hash}"));
    }
    if let Some(c) = calibration {
        upstream.extend(c.frame_ids.iter().map(|id| format!("ln_calib_frame:{id}")));
        // Rounded to 1e-6 (ruling C-14 item 2): `k` is a median of measured
        // ratios, so the last bits of an f64 carry no meaning — but the
        // hash must still move when `k` moves by anything a master's level
        // could notice (1e-6 is four orders below the ±0.1 % median row the
        // whole calibration exists to protect).
        upstream.extend(
            c.k.iter()
                .enumerate()
                .map(|(channel, k)| format!("ln_calib_k:{channel}:{:.6}", k)),
        );
    }
    // M1 (final fix wave, ruling C-30): the group β stage 6 fits at — see
    // this function's own doc for why it is an LN input and why `None`
    // folds nothing.
    if let Some(b) = beta {
        upstream.push(format!("ln_beta:{:.6}", b));
    }
    if let Some(s) = ln_scale_px {
        upstream.push(format!("ln_scale_px:{s}"));
    }
    let upstream_refs: Vec<&str> = upstream.iter().map(String::as_str).collect();
    stage_hash(&normalization_subtree(cfg), &upstream_refs, &[])
}

/// The group's `ln_calibration` artifact hash (ruling C-14 item 2): what
/// the seeds calibration was MEASURED from, as opposed to what it produced.
/// Folding the calibration frames' own registration hashes in is what makes
/// a re-registered calibration frame re-measure `k` — and, through
/// [`normalization_hash_for`] above, invalidate the whole group's sidecars
/// rather than leaving a group with frames normalized at two different `k`
/// (the review's I3). `reference_hash` is the group's LN reference: `k` is
/// a ratio measured AGAINST that reference, so a rebuilt reference is a
/// different measurement.
///
/// `calibration_frames` is `(frame_id, registration hash, measurement
/// hash)` in the sample's own order. The measurement hash is there for the
/// same reason [`normalization_hash_for`] folds it (C-17): `k` is derived
/// from the calibration frames' `.athf` seeds, so a Measure seed-config
/// change has to re-measure it.
///
/// `beta` (final fix wave, ruling C-30, review M1) is folded here for the
/// same reason and in the same shape as in [`normalization_hash_for`] —
/// read that function's doc for the full argument. It matters twice over
/// here: `k` is measured by running BOTH the seeds path and the detected
/// path over the sample and taking their ratio, and the seeds path fits at
/// exactly this β, so a different β is a different measurement of `k`, not
/// merely a different way of applying it.
pub(crate) fn seeds_calibration_hash_for(
    cfg: &StackingConfig,
    reference_hash: &str,
    calibration_frames: &[(i64, &str, &str)],
    beta: Option<f64>,
    ln_scale_px: u32,
) -> String {
    let mut upstream: Vec<String> = vec![format!("ln_reference_hash:{reference_hash}")];
    upstream.extend(
        calibration_frames
            .iter()
            .enumerate()
            .map(|(rank, (id, reg, meas))| format!("ln_calib:{rank}:{id}:{reg}:{meas}")),
    );
    if let Some(b) = beta {
        upstream.push(format!("ln_beta:{:.6}", b));
    }
    upstream.push(format!("ln_scale_px:{ln_scale_px}"));
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
/// alone". A `metrics` artifact has no equivalent disk check of its own: it
/// may be `payload_json`-only with no file to verify — its sibling `fits`
/// rows are what carries a file for Measure to check (see
/// [`fits_artifact_fresh`]; the `Measure` staleness check in [`build_plan`]
/// requires both).
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

/// Whether every one of a frame's `fits.<plane>` rows is fresh against
/// `expected_hash` (Tier C Task 1 fix round 1) — the plane count is decoded
/// from the frame's cached `metrics` payload (a JSON parse, never pixel
/// I/O), never resolved when there is no payload to read one from.
/// `pub(crate)`: shared by this module's own per-frame staleness loop
/// (`build_plan`) AND `stage_measure`'s reuse decision (`stacking::run`) —
/// one rule, two callers, the same pattern [`is_fresh`] itself follows for
/// `calibrated`. A frame whose `metrics` payload is missing, unparseable, or
/// reports zero channels reads as NOT fresh — there is nothing to check
/// plane counts against.
pub(crate) fn fits_artifact_fresh(
    conn: &Connection,
    subject: &StackSubject,
    group_key: &str,
    frame_id: i64,
    metrics_payload: Option<&str>,
    expected_hash: &str,
) -> Result<bool, ApiError> {
    let Some(planes) = metrics_payload
        .and_then(|json| serde_json::from_str::<FrameMeasurement>(json).ok())
        .map(|m| m.channels.len())
    else {
        return Ok(false);
    };
    fits_artifact_fresh_for_planes(conn, subject, group_key, frame_id, planes, expected_hash)
}

/// [`fits_artifact_fresh`]'s own per-plane freshness loop, with the plane
/// count supplied directly instead of decoded from a `metrics` payload —
/// pulled out (Tier C Task 3 fix round 1, ruling C-18, item 8) so a caller
/// that already knows a frame's plane count as a plain `usize` (stage 5's
/// `resolve_register_fits` in `stacking::run`, from `MeasuredFrame::planes`
/// — never from a stored JSON payload) can reuse the EXACT freshness rule
/// the plan gate applies, rather than a hand-rolled `find_artifact` +
/// `is_fresh` pair that could quietly drift from it.
pub(crate) fn fits_artifact_fresh_for_planes(
    conn: &Connection,
    subject: &StackSubject,
    group_key: &str,
    frame_id: i64,
    planes: usize,
    expected_hash: &str,
) -> Result<bool, ApiError> {
    if planes == 0 {
        return Ok(false);
    }
    for plane in 0..planes {
        let kind = fits_artifact::artifact_kind(plane);
        let plane_fresh = find_artifact(conn, subject, group_key, &kind, Some(frame_id))?
            .is_some_and(|row| is_fresh(&row, expected_hash));
        if !plane_fresh {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether this group's stage 1 also keeps the calibrated CFA mosaic beside
/// each debayered frame (M4d Task 1, rulings R-M4d-1/2): an OSC group whose
/// drizzle is on, asked to deposit each colour's own samples, AND whose
/// lights are actually being debayered. A mono group ignores
/// `drizzle.bayer` silently — there is no mosaic to keep and nothing about
/// the run changes.
///
/// `calibration.debayer_osc` is load-bearing, not belt-and-braces (fix round
/// 1, I1): the generator only writes a mosaic for a frame it is DEBAYERING
/// (`spec.debayer = debayer_osc && cfa_geometry.is_some()`), because an OSC
/// frame with the debayer off already writes the mosaic itself as its
/// primary output under that very name. Without this term, a run with
/// `bayer` on and the debayer off would want a mosaic no generation can
/// produce, find the artifact missing every time, and recalibrate the whole
/// group on every run for ever.
///
/// `pub(crate)`, and THE one place the condition is spelled out: this gate
/// reports what is cached, stage 1 decides what to write, and stage 8
/// decides what to deposit from — all three read this, so they cannot
/// disagree about whether a mosaic should exist. The remaining per-FRAME
/// half of `spec.debayer` (a `BAYERPAT` the catalog cannot parse) is
/// deliberately not folded in here: resolving it would cost a catalog read
/// per frame in both this gate and the run's freshness check, and such a
/// frame is reported as stale by both and warned about by the run.
pub(crate) fn wants_cfa_mosaic(cfg: &StackingConfig, group: &IntegrationGroup) -> bool {
    cfg.drizzle.enabled
        && cfg.drizzle.bayer
        && cfg.calibration.debayer_osc
        && group.color_mode == ColorMode::Osc
}

/// The plane count every member of `group` has after stage 1, and so the
/// channel count of its LN reference, `.athln` grids and seeds calibration
/// (plan 6b-3 D15; one rule where 6b-2 had two that disagreed). Calibrate
/// (a frame set): three for a debayered OSC group, one otherwise. Prepare
/// (a project or an attested set, plan 6b-2 C13): one for mono; three for
/// OSC with the debayer on; with it off, three for an `osc__` group
/// (three-plane files used as they are) and one for a `cfa__` group
/// (mosaics kept as mosaics).
///
/// C-17: the plan gate needs this to apply the SAME `k.len()` filter the
/// run applies to a stored `ln_calibration` payload (`run.rs` compares
/// against `ln_reference.planes.len()`, which it has because it has the
/// reference).
pub(crate) fn stage_one_planes(
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    prepare_path: bool,
) -> usize {
    match group.color_mode {
        ColorMode::Mono => 1,
        ColorMode::Osc if cfg.calibration.debayer_osc => 3,
        ColorMode::Osc if prepare_path && group.key.starts_with("osc__") => 3,
        ColorMode::Osc => 1,
    }
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

/// The subject's display label: a frame set's name, a project's title.
pub(crate) fn subject_label(conn: &Connection, subject: &StackSubject) -> Result<String, ApiError> {
    match subject {
        StackSubject::FrameSet(id) => frame_set_name(conn, *id),
        StackSubject::Project(id) => conn
            .query_row(
                "SELECT title FROM collab_projects WHERE project_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| ApiError::NotFound(format!("project {id} not found"))),
    }
}

/// The subject's stored manual reference frame, as an id in the run's own
/// namespace (a catalog frame id for a frame set, a project-frame handle for
/// a project).
pub(crate) fn stored_reference_frame(
    conn: &Connection,
    subject: &StackSubject,
) -> anyhow::Result<Option<i64>> {
    match subject {
        StackSubject::FrameSet(id) => {
            Ok(get_frame_set_reference(conn, *id)?.map(|r| r.reference_frame_id))
        }
        StackSubject::Project(id) => Ok(conn
            .query_row(
                "SELECT h.id FROM stacking_project_config c \
                 JOIN stack_project_frames h ON h.project_id = c.project_id \
                 AND h.frame_uuid = c.reference_frame_uuid \
                 WHERE c.project_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?),
    }
}

/// The payload-only artifact a project run caches each frame's star map in
/// (spec §4.2, plan 6b-3 D2): one row per handle, `group_key = ""`, keyed
/// on the frame's registration hash, the `RegistrationRecord` as JSON with
/// `frames_set_id = 0`. Not in `paths::INTERMEDIATE_ARTIFACT_KINDS`, so only
/// `CleanupWhat::All` drops it: a cleanup that removes the registered
/// frames keeps the star matching. A project never writes
/// `registration_results` (catalog-keyed).
pub(crate) const REGISTRATION_ARTIFACT_KIND: &str = "registration";

/// The subject's stored registration rows: `registration_results` for a
/// frame set, the `registration` artifacts for a project. A project row
/// whose payload does not parse is skipped with a warning, and the frame
/// registers again.
pub(crate) fn stored_registrations(
    conn: &Connection,
    subject: &StackSubject,
) -> anyhow::Result<Vec<RegistrationRecord>> {
    match subject {
        StackSubject::FrameSet(id) => get_registration_for_frame_set(conn, *id),
        StackSubject::Project(project_id) => {
            let mut out = Vec::new();
            for row in list_artifacts(conn, subject, Some(REGISTRATION_ARTIFACT_KIND))? {
                match row
                    .payload_json
                    .as_deref()
                    .map(serde_json::from_str::<RegistrationRecord>)
                {
                    Some(Ok(record)) => out.push(record),
                    Some(Err(e)) => tracing::warn!(
                        project_id = %project_id,
                        frame_id = ?row.frame_id,
                        error = %e,
                        "stored star map unreadable; the frame registers again"
                    ),
                    None => tracing::warn!(
                        project_id = %project_id,
                        frame_id = ?row.frame_id,
                        "stored star map has no payload; the frame registers again"
                    ),
                }
            }
            Ok(out)
        }
    }
}

/// Upsert one registration row for the subject; `build` receives the frame
/// set id the record carries (0 for a project).
pub(crate) fn store_registration(
    conn: &Connection,
    subject: &StackSubject,
    build: impl FnOnce(i64) -> RegistrationRecord,
) -> anyhow::Result<()> {
    match subject {
        StackSubject::FrameSet(id) => upsert_registration(conn, &build(*id)).map(|_| ()),
        StackSubject::Project(_) => {
            let record = build(0);
            let payload = serde_json::to_string(&record)?;
            upsert_artifact(
                conn,
                &NewArtifact {
                    subject,
                    frame_id: Some(record.frame_id),
                    group_key: "",
                    kind: REGISTRATION_ARTIFACT_KIND,
                    path: None,
                    config_hash: record.config_hash.as_deref().unwrap_or_default(),
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )?;
            Ok(())
        }
    }
}

/// The hash a frame's `registered` artifact is cached under (spec §5.5,
/// plan 6b-3 D2): its registration hash for a frame set; on a grid,
/// `grid_registered_hash` over the reference's WCS and the grid, so a grid
/// move or a re-solved reference rewrites the frame without star matching.
pub(crate) fn registered_hash_for(registration_hash: &str, grid: Option<(&str, &str)>) -> String {
    match grid {
        None => registration_hash.to_string(),
        Some((reference_wcs_hash, grid_hash)) => crate::stacking::grid_map::grid_registered_hash(
            registration_hash,
            reference_wcs_hash,
            grid_hash,
        ),
    }
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
        Some(mut cfg) => {
            crate::stacking::config::resolve_ln_fields(&mut cfg);
            cfg
        }
        None => {
            let set_json = set_row.as_ref().map(|r| r.config_json.as_str());
            let global_json = read_global_config_json(conn, settings);
            resolve_config(set_json, global_json.as_deref())
                .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?
        }
    };

    Ok((config, excluded_frame_ids))
}

/// The document a project's first plan stores (spec §9.4): the global
/// default with the project seed applied.
fn project_seed(conn: &Connection, settings: &SettingsManager) -> Result<StackingConfig, ApiError> {
    let global_json = read_global_config_json(conn, settings);
    let global = resolve_config(None, global_json.as_deref())
        .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?;
    Ok(crate::stacking::config::project_seed_config(global))
}

/// A project's stacking document, read only (plan 6b-3 D12): the stored one,
/// resolved whole, or — with no stored row — the seed the first plan would
/// store. Never writes. The config view, usage and cleanup read through
/// here; only a plan seeds.
pub(crate) fn read_project_config(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
) -> Result<(StackingConfig, Option<crate::db::stacking::ProjectConfigRow>), ApiError> {
    match crate::db::stacking::get_project_config(conn, project_id)? {
        Some(row) => {
            let config = resolve_config(Some(&row.config_json), None)
                .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?;
            Ok((config, Some(row)))
        }
        None => Ok((project_seed(conn, settings)?, None)),
    }
}

/// A project's effective config and manual exclusions (frame uuids) — plan
/// 6b-2 C10, spec §9.4. Seeds the stored document on the first plan (a
/// documented exception to "the plan reads only"; `seed_project_config` is
/// one IMMEDIATE transaction), then resolves whole-config like a frame set:
/// an override wins outright and is never stored, else the stored document
/// IS the config. `pub` so the lib build carries it before the project plan
/// arm (Task 7); the read-only config view, usage and cleanup read through
/// [`read_project_config`] instead.
pub fn resolve_project_config_and_exclusions(
    conn: &Connection,
    settings: &SettingsManager,
    project_id: &str,
    config_override: Option<StackingConfig>,
) -> Result<(StackingConfig, Vec<String>), ApiError> {
    // Read first: a stored document needs neither the global defaults nor
    // the write lock. Only a first plan builds the seed (the one place that
    // needs the global); a racing first plan loses to `DO NOTHING` and reads
    // the winner's document back.
    let row = match crate::db::stacking::get_project_config(conn, project_id)? {
        Some(row) => row,
        None => {
            let seed = serde_json::to_string(&project_seed(conn, settings)?).map_err(|e| {
                ApiError::Internal(format!("failed to serialize the project seed config: {e}"))
            })?;
            crate::db::stacking::seed_project_config(conn, project_id, &seed)?
        }
    };
    let config = match config_override {
        Some(mut cfg) => {
            crate::stacking::config::resolve_ln_fields(&mut cfg);
            cfg
        }
        None => resolve_config(Some(&row.config_json), None)
            .map_err(|e| ApiError::Invalid(format!("invalid stacking config: {e}")))?,
    };
    Ok((config, row.excluded_frame_uuids))
}

/// Resolve the plan's reference frame (gate step 2). `Auto` never blocks.
/// `Manual` requires a `frame_set_reference` row whose frame still resolves
/// to an on-disk file; either miss pushes a `reference` blocker and returns
/// an empty [`PlanReference`].
fn resolve_reference(
    conn: &Connection,
    subject: &StackSubject,
    origins: &FrameOrigins,
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

    let Some(reference_frame_id) = stored_reference_frame(conn, subject)? else {
        return Ok(no_reference_chosen(blockers));
    };

    // A manual reference whose frame is not in the plan's groups has no
    // origin and stays "no reference chosen", as before. The match has no
    // wildcard arm: every origin gets its own lookup.
    let file_row: Option<(String, String)> = match origins.get(&reference_frame_id) {
        Some(FrameOrigin::Catalog { .. }) => conn
            .query_row(
                "SELECT fi.filename, fi.path FROM frames f JOIN files fi ON fi.id = f.file_id \
                 WHERE f.id = ?1",
                params![reference_frame_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        // A project frame's file is its landed replica; the landing writes
        // `<publisher folder>/<file name>`, so the base name is the file name.
        Some(FrameOrigin::Project { landed_path, .. }) => Some((
            Path::new(landed_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| landed_path.clone()),
            landed_path.clone(),
        )),
        None => None,
    };

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
        frame_id: Some(reference_frame_id),
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
    subject: &StackSubject,
    groups: &[IntegrationGroup],
) -> Result<Option<f64>, ApiError> {
    let Some(last_run) = list_runs(conn, subject, 1)?.into_iter().next() else {
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
    included_median_scale(largest_group(groups, excluded)?, excluded)
}

/// The group with the most included frames (ties to the smaller key).
fn largest_group<'a>(
    groups: &'a [IntegrationGroup],
    excluded: &HashSet<i64>,
) -> Option<&'a IntegrationGroup> {
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
    best.map(|(g, _)| g)
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

/// Why a stage is stale, when the reason is the last run's OWN cleanup
/// (v0.6.3): `deleteIntermediates` removes every calibrated and registered
/// frame (and their rows) after a successful run, `deleteRegistered` the
/// registered ones — so the next run, whatever "Re-run from" it was asked
/// for, has to start at Calibrate (or Register). Returns the warning line
/// naming the run and the policy, or `None` when the staleness has another
/// cause (a config change, a first run, an interrupted run), which the
/// stage rows already show as `Stale` without needing a story.
///
/// `metrics` rows survive `deleteIntermediates` since v0.6.3
/// (`paths::INTERMEDIATE_ARTIFACT_KINDS`), so the line says whether the
/// measurements are still cached — read off the plan's own counters, not
/// assumed, because a set cleaned by v0.6.2 lost them too.
fn cleanup_stale_note(
    conn: &Connection,
    subject: &StackSubject,
    plan_groups: &[PlanGroup],
    stale_stages: &[Stage],
) -> Result<Option<String>, ApiError> {
    let calibrate_stale = stale_stages.contains(&Stage::Calibrate);
    let register_stale = stale_stages.contains(&Stage::Register);
    if !calibrate_stale && !register_stale {
        return Ok(None);
    }
    let Some(last) = list_runs(conn, subject, 1)?.into_iter().next() else {
        return Ok(None);
    };
    if last.status != "done" {
        return Ok(None);
    }
    let Some(cleanup) = serde_json::from_str::<StackingConfig>(&last.config_json)
        .ok()
        .map(|c| c.output.cleanup)
    else {
        return Ok(None);
    };
    let nothing_calibrated = plan_groups.iter().all(|g| g.calibrated_cached == 0);
    let metrics_kept = plan_groups.iter().any(|g| g.metrics_cached > 0);
    let note = match cleanup {
        CleanupPolicy::DeleteIntermediates if calibrate_stale && nothing_calibrated => {
            let (what, tail) = if metrics_kept {
                (
                    "no calibrated or registered frame is cached",
                    "; the measurements are still cached and will be reused",
                )
            } else {
                ("nothing is cached", "")
            };
            format!(
                "Run #{} deleted its calibrated and registered frames afterwards (Output › \
                 cleanup: delete intermediates) — {what}, so the next run starts from \
                 Calibrate whichever stage it is re-run from{tail}",
                last.id
            )
        }
        CleanupPolicy::DeleteRegistered if register_stale && !calibrate_stale => format!(
            "Run #{} deleted its registered frames afterwards (Output › cleanup: delete \
             registered) — the next run re-registers every frame whichever stage it is \
             re-run from",
            last.id
        ),
        _ => return Ok(None),
    };
    Ok(Some(note))
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
    subject: &StackSubject,
    groups: &[IntegrationGroup],
    reference: &PlanReference,
    memo: &mut HashMemo,
    origins: &FrameOrigins,
    grid: Option<&crate::collab::grid::CanonicalGrid>,
) -> Result<bool, ApiError> {
    let Some(last_run) = list_runs(conn, subject, 1)?.into_iter().next() else {
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

    // Plan 6b-3 D2: on a grid, the registered artifact also folds each
    // reference's WCS geometry and the grid; a reference without a WCS
    // cannot have produced one, so the stage reads stale.
    let grid_hash = grid.map(|g| g.hash());
    let mut reference_wcs_hashes: HashMap<i64, String> = HashMap::new();
    if grid_hash.is_some() {
        for &reference_frame_id in expected_reference.values() {
            if reference_wcs_hashes.contains_key(&reference_frame_id) {
                continue;
            }
            if !origins.contains_key(&reference_frame_id) {
                return Ok(true);
            }
            match crate::stacking::grid_map::reference_wcs_hash_of(
                conn,
                origins,
                reference_frame_id,
            )
            .map_err(|e| ApiError::Internal(format!("{e:#}")))?
            {
                Some(h) => {
                    reference_wcs_hashes.insert(reference_frame_id, h);
                }
                None => return Ok(true),
            }
        }
    }

    let rows = stored_registrations(conn, subject)?;
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
        // Ruling C-18: the same measurement hash `stacking::run` folds in
        // when it builds this frame's registration row.
        let frame_measurement_hash = measurement_hash_for(cfg, &frame_calib_hash);
        let expected = registration_hash_for(
            cfg,
            reference_frame_id,
            reference_calib_hash,
            &frame_calib_hash,
            &frame_measurement_hash,
        );
        let Some(row) = by_frame.get(&frame_id) else {
            return Ok(true);
        };
        if !registration_row_is_fresh(row, reference_frame_id, &expected) {
            return Ok(true);
        }
        // Fix round 2, ruling R-TA-6 I2: a fresh REGISTRATION row only
        // means the alignment itself does not need to be redone — perf
        // tier A Task 6a's materialized `registered` artifact is a
        // separate on-disk file the run rewrites per frame whenever it is
        // missing or stale (`registered_artifact_is_fresh` in `run.rs`;
        // same rule, mirrored here so the plan and the run can never
        // disagree about what "cached" means). Without this, an existing
        // catalog's first run after Task 6a — real registration rows, zero
        // `registered` artifacts yet — reported Register as cached while
        // the run actually warped and wrote every included frame; the
        // same was true after any cleanup that deletes the `registered/`
        // directory (`CleanupPolicy::DeleteRegistered`/`DeleteIntermediates`).
        let artifact_hash = match &grid_hash {
            None => expected.clone(),
            Some(g) => match reference_wcs_hashes.get(&reference_frame_id) {
                Some(w) => registered_hash_for(&expected, Some((w, g))),
                None => return Ok(true),
            },
        };
        let artifact = find_artifact(conn, subject, &group.key, "registered", Some(frame_id))?;
        if !artifact.is_some_and(|row| is_fresh(&row, &artifact_hash)) {
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

/// What a subject's own arm resolves before the shared gates (plan 6b-2 C10).
struct SubjectInputs {
    cfg: StackingConfig,
    hash: String,
    groups: Vec<IntegrationGroup>,
    origins: FrameOrigins,
    excluded_frame_ids: Vec<i64>,
    readiness: ExportReadiness,
    masters_to_build: Vec<PlanMaster>,
    /// Blockers ahead of every shared gate: a frame set's gate 1
    /// (masters / links / masterFiles), a project's `grid`.
    blockers: Vec<PlanBlocker>,
    /// A project's own warnings (spec §7); empty for a frame set.
    warnings: Vec<String>,
    project: Option<ProjectInputs>,
}

/// What only a project's plan carries.
struct ProjectInputs {
    grid: PlanGrid,
    frames: Vec<PlanProjectFrame>,
    /// The scale the M4b spread is measured against (spec §7): the
    /// confirmed grid's, else the proposal's.
    grid_scale: Option<f64>,
    /// The size of the grid in use — the space estimate's registered and
    /// master geometry. `None` when there is neither grid nor proposal.
    grid_dims: Option<(u64, u64)>,
    /// What Prepare writes (conversions and debayers).
    prepared_bytes: u64,
    /// The handles of the frames Prepare cannot read (review I1): they leave
    /// the plan's included counts and the ≥ 3 rule, not `excluded_frame_ids`.
    unsupported_ids: HashSet<i64>,
}

/// The frame-set arm of [`build_plan`] — today's code, moved: the stored or
/// global config, the catalog frame source, export readiness (over the
/// frames a run will touch), the masters to build and gate 1. Read-only.
/// An attested set (spec §13, R3) is prepared, never calibrated: it needs
/// no master, so it gets no gate 1 and builds nothing.
fn frame_set_inputs(
    conn: &Connection,
    settings: &SettingsManager,
    subject: &StackSubject,
    frames_set_id: i64,
    config_override: Option<StackingConfig>,
) -> Result<SubjectInputs, ApiError> {
    let (cfg, excluded_frame_ids) =
        resolve_config_and_exclusions(conn, settings, frames_set_id, config_override)?;
    let hash = config_hash(&cfg);

    let (groups, origins) = frame_source(conn, subject, &cfg)?;

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

    let mut blockers: Vec<PlanBlocker> = Vec::new();
    let masters_to_build = if readiness.attested {
        Vec::new()
    } else {
        let (masters_to_build, dropped_masters) = collect_masters_to_build(conn, &readiness);

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
        masters_to_build
    };
    Ok(SubjectInputs {
        cfg,
        hash,
        groups,
        origins,
        excluded_frame_ids,
        readiness,
        masters_to_build,
        blockers,
        warnings: Vec::new(),
        project: None,
    })
}

/// The project arm of [`build_plan`] (spec §7, plan 6b-2 C10). It WRITES —
/// a documented exception to "the plan reads only": the config seed
/// (`resolve_project_config_and_exclusions`) and the handle mint (the
/// project frame source), each one IMMEDIATE transaction; never call it with
/// a transaction open on `conn`. The masters / links / masterFiles gate never
/// applies to a project (spec §5.0): readiness is empty, nothing is built.
fn project_inputs(
    conn: &Connection,
    settings: &SettingsManager,
    subject: &StackSubject,
    project_id: &str,
    config_override: Option<StackingConfig>,
) -> Result<SubjectInputs, ApiError> {
    use crate::collab::grid_geometry::{
        count_inside, fraction_inside, grid_input_of, propose_grid, GridInput, GridTarget,
    };
    use crate::db::collab_frames::NotStackable;
    use crate::stacking::prepare::{prepare_action, prepared_bytes, probe_file, PrepareAction};

    // A project this device left fails readably here, before the config
    // seed: no stray seed row is written for a project this device left.
    let project = crate::api::collab_exchange::live_project(conn, project_id)?;
    let (cfg, excluded_uuids) =
        resolve_project_config_and_exclusions(conn, settings, project_id, config_override)?;
    let hash = config_hash(&cfg);
    let source = crate::stacking::project_source::project_frame_source(conn, project_id, &cfg)?;
    let handle_of: HashMap<&str, i64> = source
        .frames
        .iter()
        .filter_map(|f| f.handle.map(|h| (f.frame_uuid.as_str(), h)))
        .collect();
    let excluded_frame_ids: Vec<i64> = excluded_uuids
        .iter()
        .filter_map(|u| handle_of.get(u.as_str()).copied())
        .collect();
    let excluded_set: HashSet<i64> = excluded_frame_ids.iter().copied().collect();
    // Spec §5.1 / §7: what Prepare will write, and the files it cannot read.
    // Probed before anything is counted: an unsupported frame is out of the
    // plan (review I1), so it must leave every count below too.
    let mut prepared = 0u64;
    let mut unsupported: Vec<&str> = Vec::new();
    let mut unsupported_ids: HashSet<i64> = HashSet::new();
    let mut unsupported_why: HashMap<i64, String> = HashMap::new();
    for g in &source.groups {
        for f in g
            .frames
            .iter()
            .filter(|f| !excluded_set.contains(&f.frame_id))
        {
            let shape = probe_file(Path::new(&f.path));
            match prepare_action(&shape, cfg.calibration.debayer_osc) {
                PrepareAction::Unsupported(why) => {
                    tracing::warn!(project_id = %project_id, frame_id = f.frame_id, path = %f.path, error = %why, "project frame unsupported; left out of the plan");
                    unsupported.push(f.filename.as_str());
                    unsupported_ids.insert(f.frame_id);
                    unsupported_why.insert(f.frame_id, why);
                }
                action => prepared += prepared_bytes(&shape, &action),
            }
        }
    }
    let in_scope = |f: &&crate::stacking::project_source::SourceFrame| {
        f.handle
            .is_some_and(|h| !excluded_set.contains(&h) && !unsupported_ids.contains(&h))
    };

    // The grid (spec §6.2, §6.3).
    let can_edit = crate::api::collab::has_cap(
        project.is_coordinator,
        &project.gov_caps_json,
        "project.edit",
    );
    let current = crate::api::collab_grid::stored_grid(conn, project_id)?;
    let all_inputs: Vec<GridInput> = source
        .frames
        .iter()
        .filter_map(|f| f.meta.as_ref())
        .map(grid_input_of)
        .collect();
    let target = GridTarget {
        ra_deg: project.target_ra_deg,
        dec_deg: project.target_dec_deg,
        radius_deg: project.target_radius_deg,
    };
    let proposal = propose_grid(&all_inputs, &target);
    let state = match (&current, &proposal) {
        (Some(_), _) => GridState::Confirmed,
        (None, Some(_)) => GridState::Proposed,
        (None, None) => GridState::Unset,
    };
    let in_use = current.or(proposal);
    let in_scope_inputs: Vec<GridInput> = source
        .frames
        .iter()
        .filter(in_scope)
        .filter_map(|f| f.meta.as_ref())
        .map(grid_input_of)
        .collect();
    let frames_inside = in_use
        .as_ref()
        .map(|g| count_inside(&in_scope_inputs, g))
        .unwrap_or(0);

    let mut blockers = Vec::new();
    if state != GridState::Confirmed {
        blockers.push(PlanBlocker {
            code: "grid".to_string(),
            message: if can_edit {
                "Confirm the project's grid in the Stacking tab".to_string()
            } else {
                "Waiting for the coordinator to confirm the grid".to_string()
            },
        });
    }

    let mut warnings = Vec::new();
    // Spec §7: frames of the project not on this device, per canonical filter.
    let mut missing: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for f in source
        .frames
        .iter()
        .filter(|f| f.not_stackable.as_deref() == Some(NotStackable::NotOnDevice.reason()))
    {
        *missing.entry(f.filter_canonical.as_str()).or_default() += 1;
    }
    let n_missing: usize = missing.values().sum();
    if n_missing > 0 {
        let list = missing
            .iter()
            .map(|(filter, n)| format!("{filter}: {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        warnings.push(if n_missing == 1 {
            format!("1 frame of the project is not on this device and is left out ({list})")
        } else {
            format!("{n_missing} frames of the project are not on this device and are left out ({list})")
        });
    }
    // Every other reason a frame is left out (re-staged, a new version on
    // its way, unreadable metadata), one line per reason.
    let mut others: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for reason in source
        .frames
        .iter()
        .filter_map(|f| f.not_stackable.as_deref())
        .filter(|r| *r != NotStackable::NotOnDevice.reason())
    {
        *others.entry(reason).or_default() += 1;
    }
    for (reason, n) in others {
        warnings.push(format!(
            "{n} frame{} left out: {reason}",
            if n == 1 { "" } else { "s" }
        ));
    }
    // Spec §5.2, plan 6b-3 D14: the solver identity is read once, so every
    // verdict below is the one the run's Solve sub-step will reach. A cached
    // failure or refusal counts as unsolvable only while it stands for this
    // solver and catalog (a release may carry a solver that rescues it, so
    // one from another app version is tried again, review I7), and a
    // manifest or cached WCS the run cannot build a reference on counts as
    // one with no usable WCS.
    let identity = match crate::stacking::solve::installed_solver_identity(conn) {
        Ok(identity) => Some(identity),
        Err(why) if crate::stacking::solve::is_not_installed(&why) => {
            tracing::debug!(project_id = %project_id, error = %why, "no solver identity for the reference rule");
            None
        }
        Err(why) => {
            tracing::warn!(project_id = %project_id, error = %why, "solver identity unavailable for the reference rule");
            None
        }
    };
    let (mut to_solve, mut unsolvable, mut unusable) = (0usize, 0usize, 0usize);
    for f in source
        .frames
        .iter()
        .filter(in_scope)
        .filter(|f| f.meta.is_some())
    {
        let Some(handle) = f.handle else { continue };
        match crate::stacking::origin::wcs_outlook(
            conn,
            origin_of(&source.origins, handle)?,
            handle,
            identity.as_deref(),
        )
        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
        {
            crate::stacking::origin::WcsOutlook::ToSolve => to_solve += 1,
            crate::stacking::origin::WcsOutlook::Unsolvable => {
                if f.meta.as_ref().is_some_and(|m| m.wcs.is_some()) {
                    unusable += 1;
                } else {
                    unsolvable += 1;
                }
            }
            crate::stacking::origin::WcsOutlook::NotApplicable
            | crate::stacking::origin::WcsOutlook::Manifest
            | crate::stacking::origin::WcsOutlook::Solved => {}
        }
    }
    if to_solve > 0 {
        warnings.push(format!(
            "{to_solve} frame{} no WCS in the manifest and will be plate-solved locally",
            if to_solve == 1 { " has" } else { "s have" }
        ));
    }
    if unsolvable > 0 {
        warnings.push(format!(
            "{unsolvable} frame{} could not be plate-solved and cannot anchor the grid",
            if unsolvable == 1 { "" } else { "s" }
        ));
    }
    if unusable > 0 {
        warnings.push(format!(
            "{unusable} frame{} a WCS that cannot be used and cannot anchor the grid",
            if unusable == 1 { " has" } else { "s have" }
        ));
    }
    // Plan 6b-3 D14 (spec §5.4): the grid needs a reference with a WCS.
    let pinned = if cfg.reference.mode == ReferenceMode::Manual {
        stored_reference_frame(conn, subject).map_err(|e| ApiError::Internal(format!("{e:#}")))?
    } else {
        None
    };
    let in_scope_handles: Vec<i64> = source
        .frames
        .iter()
        .filter(in_scope)
        .filter_map(|f| f.handle)
        .collect();
    let check = crate::stacking::origin::reference_wcs_check(
        conn,
        &source.origins,
        &in_scope_handles,
        pinned,
        identity.as_deref(),
    )
    .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
    if let Some(message) = check.blocker {
        blockers.push(PlanBlocker {
            code: "reference".to_string(),
            message,
        });
    }
    if let Some(note) = check.warning {
        warnings.push(note);
    }
    // Spec §6.3: in-scope frames a confirmed grid leaves outside.
    if let Some(grid) = &current {
        let outside = source
            .frames
            .iter()
            .filter(in_scope)
            .filter_map(|f| f.meta.as_ref())
            .filter_map(|m| grid_input_of(m).footprint)
            .filter(|fp| fraction_inside(fp, grid) < 0.5)
            .count();
        if outside > 0 {
            warnings.push(format!(
                "{outside} frame{} outside the grid",
                if outside == 1 { " lies" } else { "s lie" }
            ));
        }
    }
    // Spec §5.1 / §7: the files Prepare cannot read.
    if !unsupported.is_empty() {
        const NAME_CAP: usize = 5;
        let shown = unsupported
            .iter()
            .take(NAME_CAP)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = unsupported.len().saturating_sub(NAME_CAP);
        let suffix = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        let n = unsupported.len();
        warnings.push(format!(
            "{n} file{} cannot be read and {} left out (unsupported file): {shown}{suffix}",
            if n == 1 { "" } else { "s" },
            if n == 1 { "is" } else { "are" },
        ));
    }

    let frames: Vec<PlanProjectFrame> = source
        .frames
        .iter()
        .map(|f| PlanProjectFrame {
            handle: f.handle,
            frame_uuid: f.frame_uuid.clone(),
            file_name: f.file_name.clone(),
            publisher: f.publisher.clone(),
            filter_canonical: f.filter_canonical.clone(),
            exptime_sec: f.exptime_sec,
            color_mode: f.color_mode,
            has_wcs: f.meta.as_ref().is_some_and(|m| m.wcs.is_some()),
            on_disk: f.not_stackable.is_none(),
            reason: f.not_stackable.clone().or_else(|| {
                f.handle
                    .and_then(|h| unsupported_why.get(&h))
                    .map(|w| format!("unsupported file: {w}"))
            }),
        })
        .collect();

    Ok(SubjectInputs {
        cfg,
        hash,
        groups: source.groups,
        origins: source.origins,
        excluded_frame_ids,
        readiness: ExportReadiness::default(),
        masters_to_build: Vec::new(),
        blockers,
        warnings,
        project: Some(ProjectInputs {
            grid: PlanGrid {
                state,
                current,
                proposal,
                frames_inside,
                can_edit,
            },
            frames,
            grid_scale: in_use.map(|g| g.scale_arcsec),
            grid_dims: in_use.map(|g| (g.width_px as u64, g.height_px as u64)),
            prepared_bytes: prepared,
            unsupported_ids,
        }),
    })
}

/// The token a project's LN hashes fold beside each frame's registered
/// hash (plan 6b-3 D6): masked LN is a different computation from the
/// frame-set one, so the two never share a sidecar. Bump it when masked
/// LN's arithmetic changes.
pub(crate) const LN_MASKED_TOKEN: &str = "lnmask:1";

/// A frame's registration upstream as every LN hash folds it: a frame
/// set's registration hash unchanged; on a grid, the registered hash plus
/// [`LN_MASKED_TOKEN`].
pub(crate) fn ln_registration_upstream(registration_hash: &str, masked: bool) -> String {
    if masked {
        format!("{registration_hash}+{LN_MASKED_TOKEN}")
    } else {
        registration_hash.to_string()
    }
}

/// A grid member's LN upstream with its group's field hash folded in (plan
/// 6b-3 D8, review I6): the masked-LN token, then `lnfield:<hash>`. The plan
/// and the run both build the field with `origin::group_ln_field`, so a
/// field change moves every member's LN hash on both sides at once.
pub(crate) fn ln_upstream_on_field(registration_hash: &str, field_hash: &str) -> String {
    format!(
        "{}+lnfield:{field_hash}",
        ln_registration_upstream(registration_hash, true)
    )
}

/// The LN geometry the plan expects for `g` and whether it is an estimate
/// (spec §2.3, L-10): a project's grid (exact once confirmed); the stored
/// LN reference's own dims (exact); otherwise the run's reference frame in
/// co-registered mode (a manual pin, else the last run's reference), the
/// group's per-group reference from the last run in native mode, else the
/// group's anchor, else the largest group's anchor - all estimates.
#[allow(clippy::too_many_arguments)]
fn plan_ln_geometry(
    g: &IntegrationGroup,
    groups: &[IntegrationGroup],
    excluded: &HashSet<i64>,
    ln_grid_dims: Option<(u64, u64, bool)>,
    stored_reference: Option<&LnReferencePayload>,
    native: bool,
    run_reference: Option<i64>,
    native_references: &HashMap<String, i64>,
) -> Option<(usize, usize, bool)> {
    if let Some((w, h, confirmed)) = ln_grid_dims {
        return Some((w as usize, h as usize, !confirmed));
    }
    if let Some(p) = stored_reference.filter(|p| p.width > 0 && p.height > 0) {
        return Some((p.width, p.height, false));
    }
    let dims_of = |id: i64| {
        find_group_frame(groups, id).and_then(|f| {
            (f.width > 0 && f.height > 0).then_some((f.width as usize, f.height as usize, true))
        })
    };
    let pick = if native {
        native_references.get(&g.key).copied()
    } else {
        run_reference
    };
    if let Some(found) = pick.and_then(dims_of) {
        return Some(found);
    }
    let anchor = |grp: &IntegrationGroup| match group_anchor_geometry(grp) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as usize, h as usize, true)),
        _ => None,
    };
    if native {
        if let Some(found) = anchor(g) {
            return Some(found);
        }
    }
    largest_group(groups, excluded).and_then(anchor)
}

/// Build the plan for a subject (a frame set or a project): its groups, the
/// resolved config, every gate blocker (spec §2, checked in a fixed order so the first blocker a
/// user sees is always the most fundamental one), and which of the three
/// cacheable per-frame stages a run would still have to redo. Pure DB reads
/// plus cheap filesystem probes for a frame set — a project's plan also seeds
/// its config and mints frame handles (plan 6b-2 C10) — never starts a run, never touches
/// [`crate::services::ServiceContext`].
pub fn build_plan(
    conn: &Connection,
    settings: &SettingsManager,
    policy: &PathPolicy,
    subject: &StackSubject,
    config_override: Option<StackingConfig>,
) -> Result<StackingPlan, ApiError> {
    let set_name = subject_label(conn, subject)?;
    let SubjectInputs {
        cfg,
        hash,
        groups,
        origins,
        excluded_frame_ids,
        readiness,
        masters_to_build,
        blockers: arm_blockers,
        warnings: arm_warnings,
        project,
    } = match subject {
        StackSubject::FrameSet(frames_set_id) => {
            frame_set_inputs(conn, settings, subject, *frames_set_id, config_override)?
        }
        StackSubject::Project(project_id) => {
            project_inputs(conn, settings, subject, project_id, config_override)?
        }
    };
    let project_grid: Option<crate::collab::grid::CanonicalGrid> =
        project.as_ref().and_then(|p| p.grid.current);
    // Review I1: a project's unsupported files leave the included counts
    // and the ≥ 3 rule (Prepare would exclude them), but not
    // `excluded_frame_ids`, which stays the user's manual list.
    let excluded_set: HashSet<i64> = excluded_frame_ids
        .iter()
        .copied()
        .chain(
            project
                .iter()
                .flat_map(|p| p.unsupported_ids.iter().copied()),
        )
        .collect();
    let mut blockers: Vec<PlanBlocker> = arm_blockers;
    let mut warnings: Vec<String> = Vec::new();

    // Owner decision 2026-09-10: exposure always splits a group now, and a
    // frame with no EXPTIME lands in its own "unknown" cluster
    // (`groups::seconds_classes`) rather than being silently folded in with
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

    // Spec §8: in `equivalent` mode a frame with no known focal ratio is
    // classed by seconds — say how many, once.
    if cfg.grouping.exposure_mode == crate::stacking::config::ExposureMode::Equivalent {
        let n: usize = groups
            .iter()
            .filter(|g| crate::stacking::groups::is_seconds_token(&g.exposure_class))
            .map(|g| g.frames.len())
            .sum();
        if n > 0 {
            warnings.push(format!(
                "exposure equivalence unknown for {n} frame{} — grouped by seconds",
                if n == 1 { "" } else { "s" }
            ));
        }
    }

    warnings.extend(arm_warnings);

    // Spec 2026-10-04 §5.2 (P-11): one advisory sentence per group whose
    // included lights carry a flat but no dark or bias. Frame sets only; an
    // attested set's lights are already calibrated. Not logged here — the
    // plan is rebuilt on every debounced edit.
    if project.is_none() && !readiness.attested {
        for g in &groups {
            let ids: Vec<i64> = g
                .frames
                .iter()
                .map(|f| f.frame_id)
                .filter(|id| !excluded_set.contains(id))
                .collect();
            let t = crate::api::lights::flat_only_lights(conn, &ids)?;
            if !t.frame_ids.is_empty() {
                warnings.push(crate::api::lights::pedestal_plan_sentence(
                    &g.key,
                    t.frame_ids.len(),
                    ids.len(),
                    &crate::api::lights::offset_gain_text(&t),
                ));
            }
        }
    }

    // Gate 2: reference.
    let reference = resolve_reference(conn, subject, &origins, &cfg, &mut blockers)?;

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
    let reference_scale: Option<f64> = match (&project, reference.frame_id) {
        (Some(p), _) => p.grid_scale,
        (None, Some(id)) => find_group_frame(&groups, id).and_then(|gf| gf.pixel_scale_arcsec),
        (None, None) => last_run_reference_scale(conn, subject, &groups)?
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
                warnings.push(if project.is_some() {
                    format!(
                        "group `{}` is at {group_scale:.2} \"/px — \u{d7}{ratio:.1} the grid's {ref_scale:.2} \"/px; it is resampled onto the grid",
                        g.key
                    )
                } else {
                    format!(
                        "group `{}` is at {group_scale:.2} \"/px — \u{d7}{ratio:.1} the reference's {ref_scale:.2} \"/px; co-registered mode resamples it into the reference geometry, native mode keeps its own",
                        g.key
                    )
                });
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
                    // Plan 6b-2 C6: a project's working root must not overlap
                    // any scan root (the Collaboration root is one).
                    if project.is_some() {
                        let root = paths::subject_layout(conn, &validated.working, subject)
                            .map_err(|e| ApiError::Internal(format!("{e:#}")))?
                            .root;
                        if let Some(message) = paths::project_root_overlap(conn, &root)? {
                            blockers.push(PlanBlocker {
                                code: "folders".to_string(),
                                message,
                            });
                        }
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
    let estimate_inputs = EstimateInputs {
        groups: &estimate_groups,
        // Perf tier A Task 6a: the registered artifact is now the run's
        // REQUIRED per-frame artifact (Normalize/Integrate always read it),
        // not the optional QC dump `write_registered_frames` used to gate —
        // the footprint estimate must count it unconditionally, the same
        // way the run itself now writes it unconditionally.
        write_registered: true,
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
        // M4d Task 1: Bayer drizzle keeps one extra single-plane artifact
        // per OSC frame (the calibrated CFA mosaic) — under the same
        // condition `wants_cfa_mosaic` applies, minus its per-group colour
        // test, which `estimate_bytes` makes itself (fix round 1, I1: with
        // the debayer off no mosaic is kept, so the term must not be added).
        drizzle_bayer: cfg.drizzle.bayer && cfg.calibration.debayer_osc,
        crop_scratch: cfg.output.crop_to_coverage > 0,
    };
    let estimate_bytes = match project
        .as_ref()
        .and_then(|p| p.grid_dims.map(|d| (d, p.prepared_bytes)))
    {
        Some((grid, prepared)) => paths::estimate_project_bytes(&estimate_inputs, grid, prepared),
        None => paths::estimate_bytes(&estimate_inputs),
    };

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
    let mut memo = HashMemo::new(origins.clone());
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
        let rows = stored_registrations(conn, subject)?;
        match project_grid.as_ref().map(|g| g.hash()) {
            None => rows
                .into_iter()
                .map(|r| (r.frame_id, r.config_hash))
                .collect(),
            // Plan 6b-3 D2: on a grid, LN's registration upstream is the
            // registered (composed) hash the run folds in.
            Some(grid_hash) => {
                let mut wcs: HashMap<i64, Option<String>> = HashMap::new();
                let mut out = HashMap::with_capacity(rows.len());
                for r in rows {
                    // A stored star map outlives its reference's replica (gone,
                    // re-staged, excluded, unsupported): that frame has no
                    // registered hash, so Normalize reads stale; never an error.
                    if !origins.contains_key(&r.reference_frame_id) {
                        out.insert(r.frame_id, None);
                        continue;
                    }
                    let w = match wcs.get(&r.reference_frame_id) {
                        Some(w) => w.clone(),
                        None => {
                            let w = crate::stacking::grid_map::reference_wcs_hash_of(
                                conn,
                                &origins,
                                r.reference_frame_id,
                            )
                            .map_err(|e| ApiError::Internal(format!("{e:#}")))?;
                            wcs.insert(r.reference_frame_id, w.clone());
                            w
                        }
                    };
                    let registered = match (r.config_hash, w) {
                        (Some(h), Some(w)) => Some(registered_hash_for(&h, Some((&w, &grid_hash)))),
                        _ => None,
                    };
                    out.insert(r.frame_id, registered);
                }
                out
            }
        }
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
        match list_runs(conn, subject, 1)?.into_iter().next() {
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
    // M1 (final fix wave, ruling C-30): the group β every `ln` row on disk
    // was written under — `stacking_run_groups.beta` of the LAST run,
    // keyed by group. It is a real LN input (stage 6 fits the reference and
    // re-fits every member's seeds at it), so `normalization_hash_for` and
    // `seeds_calibration_hash_for` both fold it in, and this gate has to
    // resolve the same value the run will. It cannot: β comes out of stage
    // 3's own measurements, which this DB-plus-cheap-FS-probe gate never
    // runs — so it reads what the last run RECORDED, the same "one run's
    // optimism" residual the stored `ln_calibration` above already carries,
    // and for the same reason: the question is what the sidecars on disk
    // were written under, and the last run is who wrote them. A newer run
    // that never reached stage 3 for a group leaves `NULL`, which hashes as
    // "not recorded", the hash differs and the group reads STALE — a
    // needless re-normalize, never a false cache hit. Fetched once for the
    // whole build, like `ln_registration_by_frame` above.
    let ln_beta_by_group: HashMap<String, f64> = if local_normalization_active {
        match list_runs(conn, subject, 1)?.into_iter().next() {
            Some(last_run) => crate::db::stacking::list_groups(conn, last_run.id)?
                .into_iter()
                .filter_map(|row| row.beta.map(|b| (row.group_key, b)))
                .collect(),
            None => HashMap::new(),
        }
    } else {
        HashMap::new()
    };
    // Spec §2.3: the geometry the plan resolves the LN scale against. The
    // run reference (co-registered), the per-group references (native) and
    // a project's grid dims; `ln_grid_dims` is not `project_grid` (the
    // canonical grid itself, bound above).
    let ln_native = cfg.registration.geometry == RegistrationGeometry::Native;
    let (run_reference, native_references): (Option<i64>, HashMap<String, i64>) =
        if local_normalization_active {
            let last_run = list_runs(conn, subject, 1)?.into_iter().next();
            let native_refs = match (&last_run, ln_native) {
                (Some(r), true) => summary_group_references(r.summary_json.as_deref()),
                _ => HashMap::new(),
            };
            (
                reference
                    .frame_id
                    .or(last_run.as_ref().and_then(|r| r.reference_frame_id)),
                native_refs,
            )
        } else {
            (None, HashMap::new())
        };
    let ln_grid_dims: Option<(u64, u64, bool)> = project.as_ref().and_then(|p| {
        p.grid_dims
            .map(|(w, h)| (w, h, p.grid.current.is_some()))
    });

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
            find_artifact(conn, subject, &g.key, "ln_reference", None)?
                .and_then(|row| row.payload_json)
                .and_then(|s| serde_json::from_str::<LnReferencePayload>(&s).ok())
        } else {
            None
        };
        // Plan 6b-3 D8: the group's LN field hash on a grid — the in-scope
        // rule is the run's own (manual list plus unsupported files).
        let ln_field_hash: Option<String> = match project_grid.as_ref() {
            // Without a stored LN reference no hash is compared (the
            // per-frame check below needs `ln_reference_info`), so the
            // field is not built.
            Some(grid) if local_normalization_active && ln_reference_info.is_some() => {
                let in_scope: Vec<i64> = g
                    .frames
                    .iter()
                    .map(|f| f.frame_id)
                    .filter(|id| !excluded_set.contains(id))
                    .collect();
                Some(
                    crate::stacking::origin::group_ln_field(conn, &origins, &in_scope, grid)
                        .map_err(|e| ApiError::Internal(format!("{e:#}")))?
                        .hash,
                )
            }
            _ => None,
        };
        // Perf tier C Task 2 fix round 4 (ruling C-14 item 2): the group's
        // measured seeds calibration is part of every member's own LN hash,
        // so this gate has to read the SAME stored value the run will use.
        // A group with no `ln_calibration` row yet (a catalog normalized
        // before this existed, or one whose calibration could not be
        // measured) hashes with `None`, which is exactly what `run.rs` does
        // in that case — the two sides stay in step without this gate ever
        // running a measurement of its own. The row's own freshness is NOT
        // re-derived here (it keys on the calibration frames' registration
        // hashes, which this gate would have to rank by weight to know):
        // whatever the last run stored is what its sidecars were written
        // under, which is the question this gate is asking.
        let ln_calibration: Option<SeedsCalibration> = if local_normalization_active {
            let ln_channels = stage_one_planes(&cfg, g, project.is_some() || readiness.attested);
            find_artifact(conn, subject, &g.key, "ln_calibration", None)?
                .and_then(|row| row.payload_json)
                .and_then(|s| serde_json::from_str::<SeedsCalibration>(&s).ok())
                // C-17: the same shape filter the run applies
                // (`run.rs`'s `stored_calibration`) — a payload whose `k`
                // does not match the group's channel count is unusable
                // THERE, so it must read as absent HERE too.
                .filter(|c| c.k.len() == ln_channels)
        } else {
            None
        };
        let ln_geometry = if local_normalization_active {
            plan_ln_geometry(
                g,
                &groups,
                &excluded_set,
                ln_grid_dims,
                ln_reference_info.as_ref(),
                ln_native,
                run_reference,
                &native_references,
            )
        } else {
            None
        };
        let ln_scale_px = ln_geometry
            .map(|(w, h, _)| crate::stacking::ln::resolve_scale(&cfg.normalization.local, w, h));
        let mut calibrated_cached = 0usize;
        let mut metrics_cached = 0usize;
        let mut fits_cached = 0usize;
        let mut ln_cached = 0usize;
        let mut included_count = 0usize;
        // M4d Task 1 (fix round 1, I2): with Bayer drizzle on, stage 1 owes
        // this group's frames a SECOND file, and a frame whose mosaic is
        // missing is regenerated as a pair — see `wants_cfa_mosaic` and
        // `run::calibrate_one_frame`. Reporting such a frame as cached here
        // would promise work the run is about to do all over again.
        let group_wants_mosaic = wants_cfa_mosaic(&cfg, g);

        for f in &g.frames {
            let is_excluded = excluded_set.contains(&f.frame_id);
            if !is_excluded {
                included_count += 1;
            }

            let calib_artifact =
                find_artifact(conn, subject, &g.key, "calibrated", Some(f.frame_id))?;
            let metrics_artifact =
                find_artifact(conn, subject, &g.key, "metrics", Some(f.frame_id))?;

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
            // The mosaic half of stage 1's freshness — the same rule
            // `run::calibrate_one_frame` applies, keyed on the frame's own
            // calibration hash (one generation, one hash). Only asked when
            // the calibrated half already passed, so a group without Bayer
            // drizzle costs no extra query at all.
            let calib_fresh = if calib_fresh && group_wants_mosaic {
                let mosaic_artifact =
                    find_artifact(conn, subject, &g.key, "calibrated_mosaic", Some(f.frame_id))?;
                match (&mosaic_artifact, &current_calib_hash) {
                    (Some(row), Some(hash)) => is_fresh(row, hash),
                    _ => false,
                }
            } else {
                calib_fresh
            };
            if calib_fresh {
                calibrated_cached += 1;
            } else if !is_excluded {
                calibrate_stale = true;
            }

            // Computed once and reused for both the `metrics` hash compare
            // and the `fits` check below (fix round 1 — this used to call
            // `measurement_hash_for` a second time for `fits_fresh`).
            let expected_measurement_hash: Option<String> = current_calib_hash
                .as_deref()
                .map(|hash| measurement_hash_for(&cfg, hash));
            let metrics_fresh = match (&metrics_artifact, &expected_measurement_hash) {
                (Some(row), Some(hash)) => &row.config_hash == hash,
                _ => false,
            };
            if metrics_fresh {
                metrics_cached += 1;
            }

            // Tier C Task 1 (fix round 1): the frame's Measure output is
            // fully cached only when its `metrics` row AND every one of its
            // planes' `fits` rows are fresh — the two are written together
            // from the measure version 3 (formerly `PSF_FIT_VERSION`) on, so a `metrics`-only hit (an old
            // catalog re-measured before this task shipped, or a `fits`
            // file removed by hand) is honestly reported as needing Measure
            // again, the same way Task 6a's `registered` artifact is
            // checked independently of `calibrated`. `fits_artifact_fresh`
            // is the SAME rule `stage_measure`'s own reuse decision calls.
            let fits_fresh = if metrics_fresh {
                fits_artifact_fresh(
                    conn,
                    subject,
                    &g.key,
                    f.frame_id,
                    metrics_artifact
                        .as_ref()
                        .and_then(|row| row.payload_json.as_deref()),
                    expected_measurement_hash
                        .as_deref()
                        .expect("metrics_fresh implies a resolved hash"),
                )?
            } else {
                false
            };
            if fits_fresh {
                fits_cached += 1;
            }
            if !(metrics_fresh && fits_fresh) && !is_excluded {
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
                let expected_ln: Option<String> = match (
                    &ln_reference_info,
                    ln_registration_by_frame.get(&f.frame_id),
                ) {
                    (Some(ref_info), Some(hash_opt)) => {
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
                        let registration = hash_opt.clone().unwrap_or_default();
                        let frame_registration_hash = match &ln_field_hash {
                            Some(field) => ln_upstream_on_field(&registration, field),
                            None => ln_registration_upstream(&registration, project_grid.is_some()),
                        };
                        // C-17: the same measurement hash the Measure
                        // check above resolved for this frame, and the
                        // same value `run.rs` folds in. `None` (no group
                        // frame, no calibration hash) hashes as "" on
                        // BOTH sides, so the gate and the run still
                        // agree.
                        let frame_measurement_hash =
                            expected_measurement_hash.clone().unwrap_or_default();
                        Some(normalization_hash_for(
                            &cfg,
                            &frame_registration_hash,
                            &frame_measurement_hash,
                            &ref_info.reference_member_ids,
                            &ref_info.reference_hash,
                            ln_calibration.as_ref(),
                            // M1: the SAME value the run folds in — the
                            // last run's recorded β for THIS group,
                            // `None` when it has none.
                            ln_beta_by_group.get(&g.key).copied(),
                            ln_scale_px,
                        ))
                    }
                    _ => None,
                };
                let ln_fresh = match &expected_ln {
                    None => false,
                    Some(expected) => {
                        find_artifact(conn, subject, &g.key, "ln", Some(f.frame_id))?
                            .is_some_and(|row| is_fresh(&row, expected))
                            // Plan 6b-3 D7: a member that keeps global
                            // normalization (under 25 % overlap) is cached
                            // through its payload-only marker.
                            || find_artifact(
                                conn,
                                subject,
                                &g.key,
                                crate::stacking::run::LN_GLOBAL_ARTIFACT_KIND,
                                Some(f.frame_id),
                            )?
                            .is_some_and(|row| row.config_hash == *expected)
                    }
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
            exposure_class: g.exposure_class.clone(),
            frame_count: g.frames.len(),
            included_count,
            total_exposure_s: g.total_exposure_s,
            calibrated_cached,
            metrics_cached,
            fits_cached,
            ln_cached,
            ln_scale_px,
            ln_scale_estimated: ln_geometry.is_some_and(|g| g.2),
            ln_width: ln_geometry.map(|g| g.0 as u32),
            ln_height: ln_geometry.map(|g| g.1 as u32),
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
                    let solve_count = sources.iter().filter(|s| **s == ScaleSource::Solve).count();
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
            message: if project.is_some() && groups.is_empty() {
                "No frame of this project is on this device yet".to_string()
            } else {
                "At least 3 included frames in one group".to_string()
            },
        });
    }

    let register_stale = compute_register_stale(
        conn,
        &cfg,
        subject,
        &groups,
        &reference,
        &mut memo,
        &origins,
        project_grid.as_ref(),
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
    if local_normalization_active && normalize_stale {
        stale_stages.push(Stage::Normalize);
    }
    // v0.6.3: when the caches a re-run would reuse are gone because the LAST
    // run's own cleanup policy removed them, say so — a "Re-run from
    // Integrate" that silently starts at Calibrate was the owner's report.
    if let Some(note) = cleanup_stale_note(conn, subject, &plan_groups, &stale_stages)? {
        warnings.push(note);
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
    let active_run_id = active_run_for_subject(conn, subject)?;

    match subject {
        StackSubject::FrameSet(id) => tracing::debug!(
            set_id = *id,
            groups = plan_groups.len(),
            frame_count,
            included_count,
            blockers = blockers.len(),
            stale_stages = stale_stages.len(),
            "stacking plan built"
        ),
        StackSubject::Project(id) => tracing::debug!(
            project_id = %id,
            groups = plan_groups.len(),
            frame_count,
            included_count,
            blockers = blockers.len(),
            stale_stages = stale_stages.len(),
            "stacking plan built"
        ),
    }

    let (set_id, project_id) = crate::stacking::run::wire_subject(subject);
    Ok(StackingPlan {
        set_id,
        project_id,
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
        grid: project.as_ref().map(|p| p.grid.clone()),
        project_frames: project.map(|p| p.frames).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::lights::compute_export_readiness;
    use crate::db::stacking::set_set_config;
    use crate::registration::db::set_frame_set_reference;
    use crate::stacking::groups::{group_frames, is_seconds_token};
    use crate::stacking::test_fixtures::{self, LightSpec};

    /// Plan 6b-3 D6: a frame set's LN upstream is its registration hash,
    /// byte for byte; a project's carries the masked-LN token.
    #[test]
    fn the_ln_upstream_is_unchanged_for_a_frame_set() {
        assert_eq!(ln_registration_upstream("h", false), "h");
        assert_eq!(
            ln_registration_upstream("h", true),
            format!("h+{LN_MASKED_TOKEN}")
        );
    }

    /// Plan 6b-3 D2: a project's star map is a payload-only `registration`
    /// artifact — read back by `stored_registrations`, kept by an
    /// `Intermediates` cleanup, dropped only by `All`.
    #[test]
    fn the_star_map_cache_survives_intermediates_cleanup() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let handle =
            crate::db::stacking::project_frame_handles(&conn, "p1", &["u1"]).unwrap()["u1"];
        let subject = StackSubject::Project("p1".into());
        store_registration(&conn, &subject, |set_id| RegistrationRecord {
            frames_set_id: set_id,
            frame_id: handle,
            reference_frame_id: handle,
            is_reference: true,
            status: "reference".into(),
            config_hash: Some("reghash".into()),
            transform_json: Some(
                crate::geometry::PixelMap::linear(crate::geometry::Linear::identity())
                    .unwrap()
                    .to_json(),
            ),
            ..Default::default()
        })
        .unwrap();
        let rows = stored_registrations(&conn, &subject).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].frame_id, rows[0].frames_set_id), (handle, 0));
        assert_eq!(rows[0].config_hash.as_deref(), Some("reghash"));
        let row = find_artifact(
            &conn,
            &subject,
            "",
            REGISTRATION_ARTIFACT_KIND,
            Some(handle),
        )
        .unwrap()
        .unwrap();
        assert_eq!((row.path, row.config_hash.as_str()), (None, "reghash"));

        let tmp = tempfile::tempdir().unwrap();
        let layout = crate::stacking::paths::WorkingLayout::new(tmp.path(), "p1");
        crate::stacking::paths::cleanup_work(
            &conn,
            &subject,
            &layout,
            crate::stacking::paths::CleanupWhat::Intermediates,
        )
        .unwrap();
        assert_eq!(
            stored_registrations(&conn, &subject).unwrap().len(),
            1,
            "Intermediates keeps the star map"
        );
        crate::stacking::paths::cleanup_work(
            &conn,
            &subject,
            &layout,
            crate::stacking::paths::CleanupWhat::All,
        )
        .unwrap();
        assert!(
            stored_registrations(&conn, &subject).unwrap().is_empty(),
            "All drops it"
        );
    }

    /// Plan 6b-3 D2: a stored star map whose reference is no longer among the
    /// run's frames never fails the plan; the frame reads Normalize-stale.
    #[test]
    fn a_star_map_with_a_vanished_reference_leaves_the_plan_ok_and_normalize_stale() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let grid = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 128,
            height_px: 128,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        };
        crate::db::collab::set_canonical_grid(&f.conn, "p1", Some(&grid.canonical_json())).unwrap();
        let handles =
            crate::db::stacking::project_frame_handles(&f.conn, "p1", &["r1", "r2", "r3"]).unwrap();
        let subject = StackSubject::Project("p1".into());
        let gone = 9_999_999;
        for uuid in ["r1", "r2", "r3"] {
            store_registration(&f.conn, &subject, |set_id| RegistrationRecord {
                frames_set_id: set_id,
                frame_id: handles[uuid],
                reference_frame_id: gone,
                config_hash: Some("reghash".into()),
                ..Default::default()
            })
            .unwrap();
        }
        let mut cfg = with_folders(&working, &output);
        cfg.normalization.local.enabled = true;
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &subject,
            Some(cfg),
        )
        .expect("a vanished reference must not fail the plan");
        assert!(
            plan.stale_stages.contains(&Stage::Normalize),
            "{:?}",
            plan.stale_stages
        );
    }

    /// Plan 6b-3 D2: on a grid the registered artifact is keyed on the
    /// composed hash. A composed-hash artifact reads cached, a plain-hash one
    /// stale, and a reference with no WCS stale.
    #[test]
    fn register_stale_on_a_grid_keys_the_registered_artifact_on_the_composed_hash() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let grid = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 128,
            height_px: 128,
            crval1: 83.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        };
        let subject = StackSubject::Project("p1".into());
        let cfg = with_folders(&working, &output);
        let inputs = project_inputs(
            &f.conn,
            &SettingsManager::new(),
            &StackSubject::Project("p1".into()),
            "p1",
            Some(cfg.clone()),
        )
        .unwrap();
        let groups = inputs.groups;
        let origins = inputs.origins;
        let handles =
            crate::db::stacking::project_frame_handles(&f.conn, "p1", &["r1", "r2", "r3", "o1"])
                .unwrap();
        let mut memo = HashMemo::new(origins.clone());

        // Cases: (reference uuid, artifact keyed composed?) -> stale?
        let mut stale_for = |reference_uuid: &str, composed: bool| -> bool {
            let reference_id = handles[reference_uuid];
            // A fresh run row and fresh rows per case.
            crate::db::stacking::delete_all_artifacts(&f.conn, &subject).unwrap();
            f.conn.execute("DELETE FROM stacking_runs", []).unwrap();
            let run_id = crate::db::stacking::insert_run(
                &f.conn,
                &crate::db::stacking::NewRun {
                    subject: &subject,
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: Some(reference_id),
                    reference_mode: "auto",
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
                    exposure_class: None,
                    frame_count: 3,
                    included_count: 3,
                },
            )
            .unwrap();
            let reference_calib = {
                let (_, gf) = find_group_of(&groups, reference_id).unwrap();
                memo.calibration_hash(&f.conn, &cfg, gf).unwrap()
            };
            let wcs =
                crate::stacking::grid_map::reference_wcs_hash_of(&f.conn, &origins, reference_id)
                    .unwrap();
            for uuid in ["r1", "r2", "r3"] {
                let frame_id = handles[uuid];
                crate::db::stacking::upsert_frame_row(
                    &f.conn,
                    &subject,
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
                let (group, gf) = find_group_of(&groups, frame_id).unwrap();
                let frame_calib = memo.calibration_hash(&f.conn, &cfg, gf).unwrap();
                let expected = registration_hash_for(
                    &cfg,
                    reference_id,
                    &reference_calib,
                    &frame_calib,
                    &measurement_hash_for(&cfg, &frame_calib),
                );
                store_registration(&f.conn, &subject, |set_id| RegistrationRecord {
                    frames_set_id: set_id,
                    frame_id,
                    reference_frame_id: reference_id,
                    is_reference: frame_id == reference_id,
                    status: if frame_id == reference_id {
                        "reference"
                    } else {
                        "aligned"
                    }
                    .to_string(),
                    config_hash: Some(expected.clone()),
                    ..Default::default()
                })
                .unwrap();
                let artifact_hash = match (&wcs, composed) {
                    (Some(w), true) => registered_hash_for(&expected, Some((w, &grid.hash()))),
                    _ => expected.clone(),
                };
                let path = f.dir.path().join(format!("registered_{frame_id}.fits"));
                std::fs::write(&path, [0u8]).unwrap();
                crate::db::stacking::upsert_artifact(
                    &f.conn,
                    &crate::db::stacking::NewArtifact {
                        subject: &subject,
                        frame_id: Some(frame_id),
                        group_key: &group.key,
                        kind: "registered",
                        path: Some(path.to_str().unwrap()),
                        config_hash: &artifact_hash,
                        size: Some(1),
                        modified_at: None,
                        payload_json: None,
                    },
                )
                .unwrap();
            }
            let reference = PlanReference {
                mode: ReferenceMode::Auto,
                frame_id: Some(reference_id),
                filename: None,
                on_disk: true,
            };
            compute_register_stale(
                &f.conn,
                &cfg,
                &subject,
                &groups,
                &reference,
                &mut memo,
                &origins,
                Some(&grid),
            )
            .unwrap()
        };
        assert!(!stale_for("r1", true), "a composed-hash artifact is cached");
        assert!(
            stale_for("r1", false),
            "a plain-hash artifact is stale on a grid"
        );
        assert!(stale_for("o1", true), "a reference with no WCS is stale");
    }

    /// Plan 6b-3 D2: a frame set's registered artifact is keyed on its
    /// registration hash exactly as before; on a grid it is the composed hash.
    #[test]
    fn a_frame_sets_registered_hash_is_its_registration_hash() {
        assert_eq!(registered_hash_for("abc", None), "abc");
        assert_eq!(
            registered_hash_for("abc", Some(("w", "g"))),
            crate::stacking::grid_map::grid_registered_hash("abc", "w", "g")
        );
    }

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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

    /// Spec §8: in `equivalent` mode, frames whose focal ratio is unknown are
    /// classed by seconds, and the plan says how many. The fixture's lights
    /// carry no `fits_header` blob, so `load_apertures` finds no aperture and
    /// every frame classes by seconds; a fixture that wrote a header blob with
    /// APTDIA would change what this test sees (review M5).
    #[test]
    fn plan_warns_when_exposure_equivalence_is_unknown() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        let mut cfg = StackingConfig::default();
        cfg.grouping.exposure_mode = crate::stacking::config::ExposureMode::Equivalent;
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            Some(cfg),
        )
        .unwrap();
        assert!(
            plan.warnings
                .iter()
                .any(|w| w == "exposure equivalence unknown for 3 frames — grouped by seconds"),
            "{:?}",
            plan.warnings
        );
        assert!(plan
            .groups
            .iter()
            .all(|g| is_seconds_token(&g.exposure_class)));
    }

    /// A frame with no `EXPTIME` never blocks the plan — it gets its own
    /// "unknown" exposure cluster (`groups::seconds_classes`) and a
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(
            plan.stale_stages,
            vec![Stage::Calibrate, Stage::Measure, Stage::Register]
        );
        assert!(plan.estimate_bytes > 0);
    }

    /// Spec §5.2: one plan warning per affected group, counted over the
    /// group's included lights; the readiness sentence is not copied in.
    #[test]
    fn the_plan_warns_per_group_for_flat_only_lights() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut a_ids = Vec::new();
        for (i, t) in [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
            "2025-01-01T00:15:00",
        ]
        .iter()
        .enumerate()
        {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("a{i}"), t));
            a_ids.push(id);
        }
        let mut b_ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("b{i}");
            let mut spec = light_spec(&stem, t);
            spec.filter = Some("Ha");
            let (id, _path) = test_fixtures::add_light(&f, &spec);
            b_ids.push(id);
        }
        let mut all_ids = a_ids.clone();
        all_ids.extend(&b_ids);
        test_fixtures::add_master_dark_and_flat(&f, &all_ids, 64, 48);
        // Group A: Flat only (its Dark links are removed). Group B keeps both.
        for &id in &a_ids {
            f.conn
                .execute(
                    "DELETE FROM calibration_set_to_frames
                     WHERE source_id = ?1 AND calibration_type = 'Dark'",
                    [id],
                )
                .unwrap();
        }

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
        // One of A's four lights is manually excluded: m = 3, n = 3.
        set_set_config(&f.conn, f.set_id, "{}", &[a_ids[3]]).unwrap();

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

        assert_eq!(plan.groups.len(), 2, "{:?}", plan.groups);
        let key_b = plan
            .groups
            .iter()
            .find(|g| g.key.contains("Ha"))
            .expect("the Ha group exists")
            .key
            .clone();
        let key_a = plan
            .groups
            .iter()
            .find(|g| g.key != key_b)
            .expect("the other group exists")
            .key
            .clone();
        let pedestal: Vec<&String> = plan
            .warnings
            .iter()
            .filter(|w| w.contains("have a flat but no dark or bias"))
            .collect();
        assert_eq!(pedestal.len(), 1, "{:?}", plan.warnings);
        assert!(
            pedestal[0].starts_with(&format!("group {key_a}: 3 of 3 lights have a flat")),
            "{:?}",
            pedestal[0]
        );
        assert!(!plan.warnings.iter().any(|w| w.contains(&key_b) && w.contains("flat but no")));
        // The set-level sentence rides on the readiness only.
        assert_eq!(plan.readiness.calibration_warnings.len(), 1);
        assert!(!plan
            .warnings
            .contains(&plan.readiness.calibration_warnings[0]));
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        assert!(
            far_warnings[0].contains(&small_group.key),
            "{:?}",
            far_warnings
        );

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        assert!(
            far_warnings[0].contains(&big_group.key),
            "{:?}",
            far_warnings
        );

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        assert!(
            far_warnings[0].contains(&big_group.key),
            "{:?}",
            far_warnings
        );

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan_all = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(
            plan_all
                .warnings
                .iter()
                .any(|w| w.contains("mixes pixel scales")),
            "sanity: with the outlier included, the spread must warn: {:?}",
            plan_all.warnings
        );

        set_set_config(&f.conn, f.set_id, "{}", &[outlier_id]).unwrap();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(
            !plan
                .warnings
                .iter()
                .any(|w| w.contains("mixes pixel scales")),
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan_included = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
            &StackSubject::FrameSet(f.set_id),
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
        for (i, t) in ["2025-01-01T00:00:00", "2025-01-01T00:05:00"].iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

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
        let plan_all = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert_eq!(plan_all.included_count, 4);

        set_set_config(&f.conn, f.set_id, "{}", &[ids[0]]).unwrap();

        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();

        assert_eq!(plan.included_count, 3);
        assert_eq!(plan.excluded_frame_ids, vec![ids[0]]);
        assert!(
            plan.estimate_bytes < plan_all.estimate_bytes,
            "the estimate must count only in-scope frames: {} vs {}",
            plan.estimate_bytes,
            plan_all.estimate_bytes
        );
    }

    /// Plan 6b-3 D10, review M6: a crop config's estimate is larger than the
    /// K = 0 one — `build_plan` sets `EstimateInputs.crop_scratch`.
    #[test]
    fn a_crop_config_enlarges_the_estimate() {
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in ["2025-01-01T00:00:00", "2025-01-01T00:05:00"]
            .iter()
            .enumerate()
        {
            test_fixtures::add_light(&f, &light_spec(&format!("f{i}"), t));
        }
        let settings = SettingsManager::new();
        let estimate = || {
            build_plan(
                &f.conn,
                &settings,
                &PathPolicy::AllowAll,
                &StackSubject::FrameSet(f.set_id),
                None,
            )
            .unwrap()
            .estimate_bytes
        };
        let plain = estimate();
        set_set_config(&f.conn, f.set_id, r#"{"output":{"cropToCoverage":3}}"#, &[]).unwrap();
        let crop = estimate();
        assert!(crop > plain, "{crop} vs {plain}");
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
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
                subject: &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
                subject: &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
                subject: &StackSubject::FrameSet(frames_set_id),
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
                exposure_class: None,
                frame_count: frame_ids.len() as i64,
                included_count: frame_ids.len() as i64,
            },
        )
        .unwrap();
        for &frame_id in frame_ids {
            crate::db::stacking::upsert_frame_row(
                conn,
                &StackSubject::FrameSet(frames_set_id),
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

    /// Fix round 2, ruling R-TA-6 I2: writes a fresh `registered` artifact
    /// row for one frame — a real file on disk whose size matches the row,
    /// so `find_artifact` + [`is_fresh`] (now folded into
    /// [`compute_register_stale`]) reads it as usable. `hash` is the SAME
    /// registration hash the caller already computed for this frame
    /// (`registration_hash_for`) — the registered artifact's freshness
    /// rides the registration row's own hash, exactly as `run.rs`'s
    /// `registered_artifact_is_fresh` call site does.
    fn seed_registered_artifact(
        f: &test_fixtures::Fixture,
        group_key: &str,
        frame_id: i64,
        hash: &str,
    ) {
        let path = f.dir.path().join(format!("registered_{frame_id}.fits"));
        std::fs::write(&path, [0u8]).unwrap();
        let size = std::fs::metadata(&path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                subject: &StackSubject::FrameSet(f.set_id),
                frame_id: Some(frame_id),
                group_key,
                kind: "registered",
                path: Some(path.to_str().unwrap()),
                config_hash: hash,
                size: Some(size),
                modified_at: None,
                payload_json: None,
            },
        )
        .unwrap();
    }

    /// A minimal, genuinely-parseable `metrics` artifact payload (Tier C
    /// Task 1): the plan gate now decodes it to learn a frame's plane count
    /// before checking its `fits` rows, so a placeholder `"{}"` (which
    /// `FrameMeasurement` — no `#[serde(default)]` on the struct itself —
    /// fails to parse) would read as "can't tell, so stale" no matter what
    /// `fits` rows exist. Values beyond the channel count are irrelevant to
    /// every test that uses this: `build_plan` never opens a calibrated or
    /// measured file's actual pixels.
    fn minimal_metrics_payload(channels: usize) -> String {
        serde_json::to_string(&FrameMeasurement {
            width: 1,
            height: 1,
            channels: vec![crate::stacking::measure::ChannelMeasurement::default(); channels],
            duration_ms: 0,
        })
        .expect("FrameMeasurement always serializes")
    }

    /// One (frame, plane)'s `fits` artifact (Tier C Task 1) — a real,
    /// genuinely-written `.athf` file (empty fit list; `build_plan` never
    /// reads its content, only `hash` + `size` via `is_fresh`) so a fixture
    /// that wants Measure to read as fresh has one to point at, the same
    /// convention [`seed_registered_artifact`] follows for `registered`.
    fn seed_fits_artifact(
        f: &test_fixtures::Fixture,
        group_key: &str,
        frame_id: i64,
        plane: usize,
        hash: &str,
    ) {
        let path = f.dir.path().join(format!("f{frame_id}.p{plane}.athf"));
        fits_artifact::write_fits(&path, &[]).expect("fits artifact write");
        let size = std::fs::metadata(&path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                subject: &StackSubject::FrameSet(f.set_id),
                frame_id: Some(frame_id),
                group_key,
                kind: &fits_artifact::artifact_kind(plane),
                path: Some(path.to_str().unwrap()),
                config_hash: hash,
                size: Some(size),
                modified_at: None,
                payload_json: None,
            },
        )
        .unwrap();
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
        // C-17: the LN hash folds the frame's measurement hash in too.
        let mut metrics_hashes: HashMap<i64, String> = HashMap::new();
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
                    subject: &StackSubject::FrameSet(f.set_id),
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
            metrics_hashes.insert(gf.frame_id, metrics_hash.clone());
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
                    frame_id: Some(gf.frame_id),
                    group_key: &group_key,
                    kind: "metrics",
                    path: None,
                    config_hash: &metrics_hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some(&minimal_metrics_payload(1)),
                },
            )
            .unwrap();
            // Tier C Task 1: the plan gate now requires the matching `fits`
            // row(s) too — a `metrics`-only hit reads as stale (see
            // `build_plan`'s per-frame loop) — so this "nothing stale" fixture
            // needs one to stay fresh.
            seed_fits_artifact(&f, &group_key, gf.frame_id, 0, &metrics_hash);
        }

        // A previous run that registered all three frames (the new rule's
        // "which frames matter" source, instead of the current group
        // membership).
        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "manual", &ids);

        let reference_hash = calib_hashes.get(&ids[0]).unwrap().clone();
        for gf in &groups[0].frames {
            let frame_hash = calib_hashes.get(&gf.frame_id).unwrap();
            let frame_measurement_hash = metrics_hashes.get(&gf.frame_id).unwrap();
            let expected = registration_hash_for(
                &cfg,
                ids[0],
                &reference_hash,
                frame_hash,
                frame_measurement_hash,
            );
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
            seed_registered_artifact(
                &f,
                &group_key,
                gf.frame_id,
                rec.config_hash.as_deref().unwrap(),
            );
            crate::registration::db::upsert_registration(&f.conn, &rec).unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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

    /// C-17: both LN hashes must follow the MEASURE seed config, because
    /// stage 6 reads the `.athf` artifact that config shapes.
    /// `normalization_subtree` carries only `measurement.psfModel` and
    /// `maxStars`, so before this fix flipping `seedDetector` to
    /// `structure` rewrote every `.athf` while the plan gate still reported
    /// Normalize cached and the run reused both the sidecars and the stored
    /// `k`. The fix folds `measurement_hash_for`'s value into
    /// `normalization_hash_for` and `seeds_calibration_hash_for` alike, so
    /// this pin drives BOTH from the same config change.
    #[test]
    fn the_ln_hashes_follow_the_measure_seed_config() {
        use crate::stacking::config::SeedSource;

        let cfg = StackingConfig::default();
        let calibrated = "calibrated-hash";
        let reference_ids = [11i64, 22, 33];
        let reference_hash = "ln-reference-hash";
        let measurement = measurement_hash_for(&cfg, calibrated);
        let calibration = SeedsCalibration {
            frame_ids: vec![11, 22],
            k: vec![0.994],
        };

        // M1 (final fix wave): the group β is an input of both hashes now
        // — held fixed here, so this pin still isolates the MEASURE seed
        // config. `the_ln_hashes_follow_the_group_beta` below pins β's own
        // effect.
        let beta = Some(4.0);
        let ln = |cfg: &StackingConfig, measurement: &str| {
            normalization_hash_for(
                cfg,
                "registration-hash",
                measurement,
                &reference_ids,
                reference_hash,
                Some(&calibration),
                beta,
                Some(1024),
            )
        };
        let seeds = |cfg: &StackingConfig, measurement: &str| {
            seeds_calibration_hash_for(
                cfg,
                reference_hash,
                &[(11, "registration-hash", measurement)],
                beta,
                1024,
            )
        };

        // Same config, same inputs -> the same hash, twice.
        assert_eq!(ln(&cfg, &measurement), ln(&cfg, &measurement));
        assert_eq!(seeds(&cfg, &measurement), seeds(&cfg, &measurement));

        // A changed Measure SEED config moves the measurement hash ...
        let mut seed_cfg = cfg.clone();
        seed_cfg.measurement.seed_source = match cfg.measurement.seed_source {
            SeedSource::MatchedFilter => SeedSource::Peak,
            _ => SeedSource::MatchedFilter,
        };
        let seed_measurement = measurement_hash_for(&seed_cfg, calibrated);
        assert_ne!(
            measurement, seed_measurement,
            "sanity: the measurement hash itself must follow seedSource"
        );

        // ... and both LN hashes must move with it.
        assert_ne!(
            ln(&seed_cfg, &seed_measurement),
            ln(&cfg, &measurement),
            "a changed seedSource must invalidate the per-frame LN hash"
        );
        assert_ne!(
            seeds(&seed_cfg, &seed_measurement),
            seeds(&cfg, &measurement),
            "a changed seedSource must invalidate the seeds-calibration hash"
        );

        // The seam is the folded VALUE, not the subtree: handing the same
        // config a different measurement hash has to move them too, which
        // is what makes the gate and the run agree only when they pass the
        // same per-frame value.
        assert_ne!(ln(&cfg, "other-measurement"), ln(&cfg, &measurement));
        assert_ne!(seeds(&cfg, "other-measurement"), seeds(&cfg, &measurement));

        // And an EMPTY measurement hash ("not recorded") is its own state,
        // shared by the gate's `None` and the run's `String::new()`.
        assert_eq!(ln(&cfg, ""), ln(&cfg, ""));
        assert_ne!(ln(&cfg, ""), ln(&cfg, &measurement));
    }

    #[test]
    fn the_ln_reference_hash_ignores_the_scale_and_the_frame_hashes_follow_it() {
        let cfg = StackingConfig::default();
        let reference =
            |s: Option<u32>| normalization_hash_for(&cfg, "r", "m", &[1, 2], "", None, Some(4.0), s);
        // The run passes no token for the reference (L-19); a token would move it.
        assert_ne!(reference(None), reference(Some(1024)));
        let frame = |s: u32| {
            normalization_hash_for(&cfg, "r", "m", &[1, 2], "ref", None, Some(4.0), Some(s))
        };
        assert_ne!(frame(1024), frame(1536));
        let cal = |s: u32| seeds_calibration_hash_for(&cfg, "ref", &[(1, "r", "m")], Some(4.0), s);
        assert_ne!(cal(1024), cal(1536));
    }

    /// M1 (final fix wave, ruling C-30): the group β — the value
    /// `psf_signal::group_beta` resolves for a group and
    /// `stacking_run_groups.beta` records — is an LN INPUT, so both LN
    /// hashes must follow it. With `normalization.local.psfModel = Auto`,
    /// stage 6 fits the LN reference at `PsfModel::Fixed(group_beta)` and
    /// `relative_scale_from_seeds` re-fits every member's seeds at the
    /// same β; before this fold, two runs whose β differed reused each
    /// other's `.athln` sidecars and the stored `k`, because
    /// `normalization_subtree` carries `psfModel` (the CHOICE) but never
    /// the value `Auto` resolves to.
    #[test]
    fn the_ln_hashes_follow_the_group_beta() {
        let cfg = StackingConfig::default();
        let reference_ids = [11i64, 22, 33];
        let reference_hash = "ln-reference-hash";
        let measurement = measurement_hash_for(&cfg, "calibrated-hash");
        let calibration = SeedsCalibration {
            frame_ids: vec![11, 22],
            k: vec![0.994],
        };

        let ln = |beta: Option<f64>| {
            normalization_hash_for(
                &cfg,
                "registration-hash",
                &measurement,
                &reference_ids,
                reference_hash,
                Some(&calibration),
                beta,
                Some(1024),
            )
        };
        let seeds = |beta: Option<f64>| {
            seeds_calibration_hash_for(
                &cfg,
                reference_hash,
                &[(11, "registration-hash", measurement.as_str())],
                beta,
                1024,
            )
        };

        // Same β, same hash, twice.
        assert_eq!(ln(Some(4.0)), ln(Some(4.0)));
        assert_eq!(seeds(Some(4.0)), seeds(Some(4.0)));

        // Two different β the `Auto` ladder can actually produce must not
        // share a sidecar or a stored `k`.
        for other in [3.0f64, 6.0, 10.0] {
            assert_ne!(
                ln(Some(other)),
                ln(Some(4.0)),
                "beta {other} must invalidate the per-frame LN hash"
            );
            assert_ne!(
                seeds(Some(other)),
                seeds(Some(4.0)),
                "beta {other} must invalidate the seeds-calibration hash"
            );
        }

        // `None` is "not recorded" — its own state, shared by the run's
        // unmeasured group and the gate's NULL `stacking_run_groups.beta`,
        // and never equal to any recorded β (least of all the 4.0 the run
        // FALLS BACK to when it fits, which must still read as a different
        // cache key from a group that genuinely measured 4.0).
        assert_eq!(ln(None), ln(None));
        assert_eq!(seeds(None), seeds(None));
        assert_ne!(ln(None), ln(Some(4.0)));
        assert_ne!(seeds(None), seeds(Some(4.0)));

        // Rounded to 1e-6, like `k`: a difference far below what the
        // `Auto` ladder can express does not churn every sidecar, while
        // anything at or above it does.
        assert_eq!(ln(Some(4.0)), ln(Some(4.0 + 1e-9)));
        assert_ne!(ln(Some(4.0)), ln(Some(4.000_01)));
    }

    /// Ruling C-18 (Tier C Task 3 fix round 1): `registration_hash_for`
    /// must follow the MEASURE seed config too, for exactly the reason
    /// `the_ln_hashes_follow_the_measure_seed_config` above pins for
    /// Normalize — Register now reads the same `.athf` artifact for a mono
    /// frame (`stacking::register::detect::stars_from_fits`), and
    /// `registration_subtree` carries only `cfg.registration` +
    /// the measure version (formerly `PSF_FIT_VERSION`), neither of which reacts to `measurement.
    /// seedDetector`/`maxStars`/`detectionSigma`/`seedPrefilter`/
    /// `structure`.
    #[test]
    fn the_registration_hash_follows_the_measure_seed_config() {
        use crate::stacking::config::SeedSource;

        let cfg = StackingConfig::default();
        let calibrated = "calibrated-hash";
        let reference_calibrated = "reference-calibrated-hash";
        let measurement = measurement_hash_for(&cfg, calibrated);

        let reg = |cfg: &StackingConfig, measurement: &str| {
            registration_hash_for(cfg, 7, reference_calibrated, calibrated, measurement)
        };

        // Same config, same inputs -> the same hash, twice.
        assert_eq!(reg(&cfg, &measurement), reg(&cfg, &measurement));

        // A changed Measure SEED config moves the measurement hash ...
        let mut seed_cfg = cfg.clone();
        seed_cfg.measurement.seed_source = match cfg.measurement.seed_source {
            SeedSource::MatchedFilter => SeedSource::Peak,
            _ => SeedSource::MatchedFilter,
        };
        let seed_measurement = measurement_hash_for(&seed_cfg, calibrated);
        assert_ne!(
            measurement, seed_measurement,
            "sanity: the measurement hash itself must follow seedSource"
        );

        // ... and the registration hash must move with it, even though
        // `cfg.registration` itself is untouched.
        assert_ne!(
            reg(&seed_cfg, &seed_measurement),
            reg(&cfg, &measurement),
            "a changed seedSource must invalidate the registration hash"
        );

        // The seam is the folded VALUE, not the subtree.
        assert_ne!(reg(&cfg, "other-measurement"), reg(&cfg, &measurement));

        // An EMPTY measurement hash ("not recorded") is its own state,
        // shared by the gate's `None` and the run's `String::new()`.
        assert_eq!(reg(&cfg, ""), reg(&cfg, ""));
        assert_ne!(reg(&cfg, ""), reg(&cfg, &measurement));
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
        // C-17: the LN hash folds the frame's measurement hash in too.
        let mut metrics_hashes: HashMap<i64, String> = HashMap::new();
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
                    subject: &StackSubject::FrameSet(f.set_id),
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
            metrics_hashes.insert(gf.frame_id, metrics_hash.clone());
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = metrics_hashes.get(&gf.frame_id).unwrap();
            let expected = registration_hash_for(
                &cfg,
                ids[0],
                &reference_calib_hash,
                frame_hash,
                frame_measurement_hash,
            );
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
        // M1: `None` for β on both sides — these fixtures record no run
        // group with a β, so the gate resolves `None` too.
        let reference_hash =
            normalization_hash_for(&cfg, &combined, "", &reference_member_ids, "", None, None, None);
        let reference_payload = serde_json::to_string(&LnReferencePayload {
            reference_member_ids: reference_member_ids.clone(),
            reference_hash: reference_hash.clone(),
            width: 0,
            height: 0,
            scale_px: 0,
        })
        .unwrap();
        let reference_path = f.dir.path().join("reference.fits");
        std::fs::write(&reference_path, [0u8]).unwrap();
        let reference_size = std::fs::metadata(&reference_path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                subject: &StackSubject::FrameSet(f.set_id),
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

        // The scale the plan resolves for this fixture's geometry (the
        // stored payload carries no dims, so it estimates from the anchor).
        let ln_scale = crate::stacking::ln::resolve_scale(
            &cfg.normalization.local,
            groups[0].frames[0].width as usize,
            groups[0].frames[0].height as usize,
        );
        for gf in &groups[0].frames {
            let frame_hash = normalization_hash_for(
                &cfg,
                &registration_hashes[&gf.frame_id],
                &metrics_hashes[&gf.frame_id],
                &reference_member_ids,
                &reference_hash,
                None,
                None,
                Some(ln_scale),
            );
            let sidecar_path = f.dir.path().join(format!("f{}.athln", gf.frame_id));
            std::fs::write(&sidecar_path, [0u8]).unwrap();
            let size = std::fs::metadata(&sidecar_path).unwrap().len() as i64;
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
        // Under auto mode a `scale` edit no longer changes the resolved
        // scale (spec 2026-10-04 §2), so the edit pins a fixed scale that
        // differs from the auto one.
        let mut cfg2 = cfg.clone();
        cfg2.normalization.local.scale_mode =
            Some(crate::stacking::integrate::LnScaleMode::Fixed);
        cfg2.normalization.local.scale = 2048;
        let plan2 = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
        // C-17: the LN hash folds the frame's measurement hash in too.
        let mut metrics_hashes: HashMap<i64, String> = HashMap::new();
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
                    subject: &StackSubject::FrameSet(f.set_id),
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
            metrics_hashes.insert(gf.frame_id, metrics_hash.clone());
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
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
                subject: &StackSubject::FrameSet(f.set_id),
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
                exposure_class: None,
                frame_count: 3,
                included_count: 2,
            },
        )
        .unwrap();
        let seed_frame_row = |included: bool, reason: Option<&str>, frame_id: i64| {
            crate::db::stacking::upsert_frame_row(
                &f.conn,
                &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = measurement_hash_for(&cfg, frame_hash);
            let expected = registration_hash_for(
                &cfg,
                ids[0],
                &reference_calib_hash,
                frame_hash,
                &frame_measurement_hash,
            );
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
        // M1: `None` for β on both sides — these fixtures record no run
        // group with a β, so the gate resolves `None` too.
        let reference_hash =
            normalization_hash_for(&cfg, &combined, "", &reference_member_ids, "", None, None, None);
        let reference_payload = serde_json::to_string(&LnReferencePayload {
            reference_member_ids: reference_member_ids.clone(),
            reference_hash: reference_hash.clone(),
            width: 0,
            height: 0,
            scale_px: 0,
        })
        .unwrap();
        let reference_path = f.dir.path().join("reference.fits");
        std::fs::write(&reference_path, [0u8]).unwrap();
        let reference_size = std::fs::metadata(&reference_path).unwrap().len() as i64;
        crate::db::stacking::upsert_artifact(
            &f.conn,
            &crate::db::stacking::NewArtifact {
                subject: &StackSubject::FrameSet(f.set_id),
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
        let ln_scale = crate::stacking::ln::resolve_scale(
            &cfg.normalization.local,
            groups[0].frames[0].width as usize,
            groups[0].frames[0].height as usize,
        );
        for &frame_id in &ids[..2] {
            let frame_hash = normalization_hash_for(
                &cfg,
                &registration_hashes[&frame_id],
                &metrics_hashes[&frame_id],
                &reference_member_ids,
                &reference_hash,
                None,
                None,
                Some(ln_scale),
            );
            let sidecar_path = f.dir.path().join(format!("f{frame_id}.athln"));
            std::fs::write(&sidecar_path, [0u8]).unwrap();
            let size = std::fs::metadata(&sidecar_path).unwrap().len() as i64;
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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

    /// v0.6.3: a set whose caches were wiped by the last run's own cleanup
    /// policy gets a warning that NAMES the run and the policy, and says the
    /// next run starts from Calibrate whichever stage it is re-run from —
    /// the owner's "re-run from Integrate doesn't work" was exactly this
    /// case with nothing telling them. A `keepAll` run, a first run and an
    /// interrupted run get no such line: their staleness has other causes
    /// the stage rows already show.
    #[test]
    fn a_cleanup_wiped_cache_is_explained_by_the_plan() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);
        let settings = SettingsManager::new();

        let finished_run = |cleanup: CleanupPolicy, status: &str| {
            let mut cfg = StackingConfig::default();
            cfg.output.cleanup = cleanup;
            let run_id = crate::db::stacking::insert_run(
                &f.conn,
                &crate::db::stacking::NewRun {
                    subject: &StackSubject::FrameSet(f.set_id),
                    config_json: &serde_json::to_string(&cfg).unwrap(),
                    config_hash: "h",
                    reference_frame_id: Some(ids[0]),
                    reference_mode: "auto",
                    working_dir: "/w",
                    output_dir: "/o",
                },
            )
            .unwrap();
            crate::db::stacking::finish_run(&f.conn, run_id, status, None, None).unwrap();
            run_id
        };

        // No run at all: stale for the ordinary reason, no story.
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(
            plan.stale_stages.contains(&Stage::Calibrate),
            "{:?}",
            plan.stale_stages
        );
        assert!(
            !plan.warnings.iter().any(|w| w.contains("cleanup")),
            "{:?}",
            plan.warnings
        );

        // A finished keepAll run: still no story.
        finished_run(CleanupPolicy::KeepAll, "done");
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(
            !plan.warnings.iter().any(|w| w.contains("cleanup")),
            "{:?}",
            plan.warnings
        );

        // A finished deleteIntermediates run with nothing cached: named.
        let run_id = finished_run(CleanupPolicy::DeleteIntermediates, "done");
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        let note = plan
            .warnings
            .iter()
            .find(|w| w.contains("delete intermediates"))
            .unwrap_or_else(|| panic!("no cleanup note in {:?}", plan.warnings));
        assert!(note.contains(&format!("Run #{run_id}")), "{note}");
        assert!(note.contains("starts from Calibrate"), "{note}");
        assert!(
            !note.contains("measurements are still cached"),
            "no metrics row exists here: {note}"
        );

        // With a measurement still cached (a v0.6.3 cleanup keeps the rows),
        // the line says so instead of claiming nothing is cached.
        {
            let cfg = StackingConfig::default();
            let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
            let gf = &groups[0].frames[0];
            let mut divisors = DivisorCache::new();
            let calib_hash = calibration_hash_for(&f.conn, &cfg, gf, &mut divisors).unwrap();
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &StackSubject::FrameSet(f.set_id),
                    frame_id: Some(gf.frame_id),
                    group_key: &groups[0].key,
                    kind: "metrics",
                    path: None,
                    config_hash: &measurement_hash_for(&cfg, &calib_hash),
                    size: None,
                    modified_at: None,
                    payload_json: Some("{}"),
                },
            )
            .unwrap();
        }
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        let note = plan
            .warnings
            .iter()
            .find(|w| w.contains("delete intermediates"))
            .unwrap_or_else(|| panic!("no cleanup note in {:?}", plan.warnings));
        assert!(
            note.contains("no calibrated or registered frame is cached"),
            "{note}"
        );
        assert!(note.contains("measurements are still cached"), "{note}");
        assert!(!note.contains("nothing is cached"), "{note}");
        crate::db::stacking::delete_artifacts(
            &f.conn,
            &StackSubject::FrameSet(f.set_id),
            &["metrics"],
        )
        .unwrap();

        // An INTERRUPTED deleteIntermediates run never ran its cleanup —
        // no story either.
        finished_run(CleanupPolicy::DeleteIntermediates, "failed");
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
        assert!(
            !plan.warnings.iter().any(|w| w.contains("cleanup")),
            "{:?}",
            plan.warnings
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
                subject: &StackSubject::FrameSet(f.set_id),
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
                exposure_class: None,
                frame_count: 3,
                included_count: 2,
            },
        )
        .unwrap();
        for (i, &frame_id) in ids.iter().enumerate() {
            crate::db::stacking::upsert_frame_row(
                &f.conn,
                &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = measurement_hash_for(&cfg, frame_hash);
            let expected = registration_hash_for(
                &cfg,
                ids[0],
                &reference_hash,
                frame_hash,
                &frame_measurement_hash,
            );
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
            seed_registered_artifact(
                &f,
                &groups[0].key,
                frame_id,
                rec.config_hash.as_deref().unwrap(),
            );
            crate::registration::db::upsert_registration(&f.conn, &rec).unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = measurement_hash_for(&cfg, frame_hash);
            let expected = registration_hash_for(
                &cfg,
                ids[0],
                &reference_hash,
                frame_hash,
                &frame_measurement_hash,
            );
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
            seed_registered_artifact(
                &f,
                &groups[0].key,
                frame_id,
                rec.config_hash.as_deref().unwrap(),
            );
            crate::registration::db::upsert_registration(&f.conn, &rec).unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = measurement_hash_for(&cfg, frame_hash);
            let expected = registration_hash_for(
                &cfg,
                group_reference,
                &reference_hash,
                frame_hash,
                &frame_measurement_hash,
            );
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
            seed_registered_artifact(
                &f,
                &group_key,
                frame_id,
                rec.config_hash.as_deref().unwrap(),
            );
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
                &StackSubject::FrameSet(f.set_id),
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
            let frame_measurement_hash = measurement_hash_for(&cfg, frame_hash);
            let expected = registration_hash_for(
                &cfg,
                switched_to,
                &reference_hash,
                frame_hash,
                &frame_measurement_hash,
            );
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
            seed_registered_artifact(
                &f,
                &groups[0].key,
                frame_id,
                rec.config_hash.as_deref().unwrap(),
            );
            crate::registration::db::upsert_registration(&f.conn, &rec).unwrap();
        }

        let settings = SettingsManager::new();
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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
        let plan = build_plan(
            &f.conn,
            &settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(f.set_id),
            None,
        )
        .unwrap();
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
            &StackSubject::FrameSet(f.set_id),
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
            &StackSubject::FrameSet(f.set_id),
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

    /// Spec 2026-10-02 §8 / plan 6b-1 Task 1 (fable C1): every stage hash of
    /// fixed configs, computed through the SAME pure formula the plan uses,
    /// pinned to literals recorded on `main` BEFORE `calibration_subtree`
    /// stopped serializing `grouping` whole. A later config field that leaks
    /// into a stage subtree moves one of these and fails here, instead of
    /// silently recomputing every frame set's cache on its next run.
    #[test]
    fn stage_hashes_are_pinned_to_golden_literals() {
        use crate::stacking::config::{
            calibration_subtree, preset, stage_hash, ExposureMode, SourceIdentity, StackingPreset,
        };
        let mut custom = StackingConfig::default();
        custom.grouping.exposure_tolerance_sec = 5.0;
        custom.normalization.local.enabled = true;
        custom.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
        let configs = [
            ("default", StackingConfig::default()),
            ("fastPreview", preset(StackingPreset::FastPreview)),
            ("maximumQuality", preset(StackingPreset::MaximumQuality)),
            ("custom", custom),
        ];
        let masters = [
            "/m/dark.fits|100|2026-01-01T00:00:00+00:00",
            "/m/flat.fits|200|2026-01-01T00:00:00+00:00",
        ];
        let src = || {
            [SourceIdentity {
                file_id: 42,
                size: 123_456,
                modified_at: "2026-01-01T00:00:00Z".into(),
            }]
        };
        let calib_k = crate::stacking::ln::SeedsCalibration {
            frame_ids: vec![11, 22],
            k: vec![0.994],
        };
        let line = |name: &str, cfg: &StackingConfig| {
            let calib = stage_hash(&calibration_subtree(cfg), &masters, &src());
            let meas = measurement_hash_for(cfg, &calib);
            let reg = registration_hash_for(cfg, 7, "ref-calib", &calib, &meas);
            let ln_ref =
                normalization_hash_for(cfg, "r1,r2", "m1,m2", &[11, 22], "", None, Some(1.5), None);
            let ln = normalization_hash_for(
                cfg,
                &reg,
                &meas,
                &[11, 22],
                &ln_ref,
                Some(&calib_k),
                Some(1.5),
                Some(1024),
            );
            let ln_cal = seeds_calibration_hash_for(
                cfg,
                &ln_ref,
                &[(11, "r1", "m1"), (22, "r2", "m2")],
                Some(1.5),
                1024,
            );
            format!("{name}: calib={calib} meas={meas} reg={reg} ln_ref={ln_ref} ln={ln} ln_cal={ln_cal}")
        };
        let got: Vec<String> = configs.iter().map(|(name, cfg)| line(name, cfg)).collect();
        // GOLDEN: recorded on main 17e2e96e before the Task 1 edit.
        // 2026-10-05 LN model fixes Task 2: lnModelVersion 2 joins the normalization subtree (spec §7, L-19); calib/meas/reg unchanged.
        // 2026-10-05 LN model fixes Task 3: scale/scaleMode/gridSize leave the subtree, ln_scale_px token (L-19).
        // 2026-10-05 LN model fixes Task 11: starSelection + lnModelVersion 3 (ln_* only; calib/meas/reg unchanged).
        // 2026-10-05 LN model fixes Task 13: fluxEstimator + aperture (aperture only under commonAperture)
        // 2026-10-06: LN_MODEL_VERSION 4 (L-23) - the reference-frames saturation map (ln_* only; calib/meas/reg unchanged).
        // 2026-10-06: F1 growth curve is the default (L-24) - ln_* only; calib/meas/reg unchanged.
        // 2026-10-08 one star-measurement path Plan A Task 3 (R-OSP-1): MEASURE_VERSION 4 replaces the old fit-version constant (3) - meas, reg, ln_ref, ln and ln_cal move on every line; calib is UNCHANGED (the calibration subtree carries no measure version).
        // 2026-10-08 one star-measurement path Plan A Task 7 (R-OSP-2): `measurement.seedSource` (matchedFilter) and `seedThreshold` join the measurement subtree (hand edit) - meas moves on every line, and reg, ln and (custom) follow; calib, ln_ref and ln_cal are UNCHANGED.
        assert_eq!(
            got,
            vec![
                "default: calib=f3cf4be2b2178358 meas=22e028e28274e94d reg=5304aff46f287eb2 ln_ref=2676a6e017eebe13 ln=c280caa7a7b6c3bd ln_cal=f6195d69cd7bbaee",
                "fastPreview: calib=f3cf4be2b2178358 meas=22e028e28274e94d reg=59572d1effaa50d4 ln_ref=2676a6e017eebe13 ln=a338b80e89bc6061 ln_cal=f6195d69cd7bbaee",
                "maximumQuality: calib=f3cf4be2b2178358 meas=22e028e28274e94d reg=9c0b40285429d40b ln_ref=1237e3e6fdf2cf1c ln=ed547a4e794451e0 ln_cal=6bb6c12538e4e622",
                "custom: calib=2b725e5e99c87f49 meas=e1f010e0202a1502 reg=96c5de182db4a56c ln_ref=1237e3e6fdf2cf1c ln=e6e439531f8f82ab ln_cal=6bb6c12538e4e622",
            ]
        );
        // Plan 6b-2 C2: the four wave-6 fields at non-default values move no
        // stage hash — an existing frame set keeps every cache.
        let with_wave6_fields: Vec<String> = configs
            .iter()
            .map(|(name, cfg)| {
                let mut cfg = cfg.clone();
                cfg.grouping.exposure_mode = ExposureMode::Stops;
                cfg.grouping.stops_width = 2.0;
                cfg.output.coverage_map = true;
                cfg.output.crop_to_coverage = 3;
                line(name, &cfg)
            })
            .collect();
        assert_eq!(with_wave6_fields, got);
        assert_eq!(
            serde_json::to_string(&calibration_subtree(&StackingConfig::default())["grouping"])
                .unwrap(),
            r#"{"exposureToleranceSec":2.0}"#
        );
    }

    /// Plan 6b-1 Task 1 (B12): the golden test above pins `stage_hash` over
    /// `calibration_subtree`; this proves `calibration_hash_for`, which a run
    /// and the plan actually call, is exactly that formula, so the golden
    /// literals guard the real cache keys. The master paths are read through
    /// the catalog, independently of `calibration_hash_for`'s own resolution.
    #[test]
    fn calibration_hash_for_is_the_pinned_formula() {
        use crate::stacking::config::{calibration_subtree, stage_hash, SourceIdentity};
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) =
                test_fixtures::add_light(&f, &light_spec_written(&format!("f{i}"), t));
            ids.push(id);
        }
        let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(&f, &ids, 64, 48);

        let mut master_paths: Vec<std::path::PathBuf> = Vec::new();
        for set_id in [dark_set, flat_set] {
            let path: String = f
                .conn
                .query_row(
                    "SELECT fi.path FROM calibration_set_frames csf
                     JOIN frames fr ON fr.id = csf.frame_id
                     JOIN files fi ON fi.id = fr.file_id
                     WHERE csf.set_id = ?1",
                    [set_id],
                    |r| r.get(0),
                )
                .unwrap();
            master_paths.push(std::path::PathBuf::from(path));
        }

        let cfg = StackingConfig::default();
        let groups = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(groups.len(), 1, "one group expected for this fixture");
        let frame = &groups[0].frames[0];

        let identities: Vec<String> = master_paths.iter().map(|p| master_identity(p)).collect();
        let identity_refs: Vec<&str> = identities.iter().map(String::as_str).collect();
        let expected = stage_hash(
            &calibration_subtree(&cfg),
            &identity_refs,
            &[SourceIdentity {
                file_id: frame.file_id,
                size: frame.size,
                modified_at: frame.modified_at.clone(),
            }],
        );
        let mut divisors = DivisorCache::new();
        assert_eq!(
            calibration_hash_for(&f.conn, &cfg, frame, &mut divisors).unwrap(),
            expected
        );
    }

    #[test]
    fn subject_label_and_stored_reference_by_subject() {
        // Frame set: the frames_set name and the frame_set_reference row.
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
            ids.push(id);
        }
        let set = StackSubject::FrameSet(f.set_id);
        assert_eq!(subject_label(&f.conn, &set).unwrap(), "LDN 1272");
        assert_eq!(stored_reference_frame(&f.conn, &set).unwrap(), None);
        set_frame_set_reference(&f.conn, f.set_id, ids[1]).unwrap();
        assert_eq!(stored_reference_frame(&f.conn, &set).unwrap(), Some(ids[1]));
        assert!(matches!(
            subject_label(&f.conn, &StackSubject::FrameSet(f.set_id + 999)),
            Err(ApiError::NotFound(_))
        ));

        // Project: the collab_projects title and the handle of the stored uuid.
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        f.conn
            .execute(
                "UPDATE collab_projects SET title = 'Orion Mosaic' WHERE project_id = 'p1'",
                [],
            )
            .unwrap();
        let project = StackSubject::Project("p1".into());
        assert_eq!(subject_label(&f.conn, &project).unwrap(), "Orion Mosaic");
        assert!(matches!(
            subject_label(&f.conn, &StackSubject::Project("nope".into())),
            Err(ApiError::NotFound(_))
        ));
        let handles =
            crate::db::stacking::project_frame_handles(&f.conn, "p1", &["u1", "u2"]).unwrap();
        assert_eq!(stored_reference_frame(&f.conn, &project).unwrap(), None);
        f.conn
            .execute(
                "INSERT INTO stacking_project_config (project_id, config_json, reference_frame_uuid, updated_at)
                 VALUES ('p1', '{}', 'u2', '2026-10-02T00:00:00Z')",
                [],
            )
            .unwrap();
        assert_eq!(
            stored_reference_frame(&f.conn, &project).unwrap(),
            Some(handles["u2"])
        );
        assert!(stored_registrations(&f.conn, &project).unwrap().is_empty());
        // Plan 6b-3 D2 (was B10: "a project stores nothing"): a project keeps
        // its star map as a payload-only artifact, with no set id.
        store_registration(&f.conn, &project, |set_id| RegistrationRecord {
            frames_set_id: set_id,
            frame_id: handles["u1"],
            ..Default::default()
        })
        .unwrap();
        let stored = stored_registrations(&f.conn, &project).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            (stored[0].frame_id, stored[0].frames_set_id),
            (handles["u1"], 0)
        );
    }

    #[test]
    fn the_hash_memo_refuses_a_frame_it_has_no_origin_for() {
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
        let frame = groups[0].frames[0].clone();

        let mut divisors = DivisorCache::new();
        let expected = calibration_hash_for(&f.conn, &cfg, &frame, &mut divisors).unwrap();

        // No origin: an error naming the frame, never a silent catalog read.
        let mut memo = HashMemo::new(std::sync::Arc::new(HashMap::new()));
        let err = memo
            .calibration_hash_checked(&f.conn, &cfg, &frame)
            .unwrap_err();
        assert!(
            err.to_string().contains(&frame.frame_id.to_string()),
            "{err}"
        );
        assert_eq!(memo.calibration_hash(&f.conn, &cfg, &frame), None);

        // With the frame's Catalog origin: the same hash calibration_hash_for returns.
        let origins: crate::stacking::origin::FrameOrigins = std::sync::Arc::new(
            [(
                frame.frame_id,
                crate::stacking::origin::FrameOrigin::Catalog {
                    file_id: frame.file_id,
                    attested: false,
                },
            )]
            .into_iter()
            .collect(),
        );
        let mut memo = HashMemo::new(origins);
        assert_eq!(
            memo.calibration_hash_checked(&f.conn, &cfg, &frame)
                .unwrap(),
            expected
        );
        assert_eq!(memo.calibration_hash(&f.conn, &cfg, &frame), Some(expected));
    }

    #[test]
    fn resolve_reference_reads_the_catalog_only_for_a_frame_with_an_origin() {
        use crate::stacking::groups::frame_source;
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        let mut first_path = None;
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, path) = test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
            ids.push(id);
            first_path.get_or_insert(path);
        }
        // The fixture's rows carry paths but no file; the reference must be on disk.
        std::fs::write(first_path.unwrap(), b"x").unwrap();
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let subject = StackSubject::FrameSet(f.set_id);
        let (_, origins) = frame_source(&f.conn, &subject, &cfg).unwrap();

        // A frame with an origin resolves its filename and path.
        set_frame_set_reference(&f.conn, f.set_id, ids[0]).unwrap();
        let mut blockers = Vec::new();
        let reference =
            resolve_reference(&f.conn, &subject, &origins, &cfg, &mut blockers).unwrap();
        assert_eq!(reference.frame_id, Some(ids[0]));
        assert!(reference.filename.is_some());
        assert!(reference.on_disk, "{blockers:?}");
        assert!(blockers.is_empty(), "{blockers:?}");

        // A reference the plan's groups do not hold has no origin: the same
        // "no reference chosen" outcome as before, not a failed plan.
        let empty: FrameOrigins = std::sync::Arc::new(HashMap::new());
        let mut blockers = Vec::new();
        let reference = resolve_reference(&f.conn, &subject, &empty, &cfg, &mut blockers).unwrap();
        assert_eq!(reference.frame_id, None);
        assert_eq!(reference.filename, None);
        assert!(!reference.on_disk);
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code, "reference");
        assert_eq!(blockers[0].message, "Choose a reference frame in Analysis");
    }

    #[test]
    fn frame_source_returns_one_catalog_origin_per_frame() {
        use crate::stacking::groups::frame_source;
        use crate::stacking::origin::FrameOrigin;
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        let cfg = StackingConfig::default();
        let (groups, origins) =
            frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg).unwrap();
        let direct = group_frames(&f.conn, f.set_id, &cfg.grouping).unwrap();
        assert_eq!(format!("{groups:?}"), format!("{direct:?}"));

        let frames: Vec<&GroupFrame> = groups.iter().flat_map(|g| g.frames.iter()).collect();
        assert_eq!(frames.len(), 3);
        assert_eq!(origins.len(), frames.len());
        for fr in frames {
            assert_eq!(
                origins.get(&fr.frame_id),
                Some(&FrameOrigin::Catalog {
                    file_id: fr.file_id,
                    attested: false
                })
            );
        }

        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let (groups, origins) =
            frame_source(&f.conn, &StackSubject::Project("p1".into()), &cfg).unwrap();
        assert!(
            groups.is_empty() && origins.is_empty(),
            "a project with no landed frame"
        );

        // Spec §13 (R3): an attested set's origins carry the flag.
        f.conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [f.set_id],
            )
            .unwrap();
        let (_, origins) = frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg).unwrap();
        assert_eq!(origins.len(), 3, "the set's three lights");
        assert!(origins
            .values()
            .all(|o| matches!(o, FrameOrigin::Catalog { attested: true, .. })));
    }

    /// Spec §13 (R3), plan 6b-2 C11: an attested light's stage-1 hash is the
    /// Prepare hash over its catalog identity. No master is resolved, so a
    /// light with no calibration link still hashes.
    #[test]
    fn an_attested_lights_hash_is_the_prepare_hash_over_its_catalog_identity() {
        use crate::stacking::config::{prepare_subtree, stage_hash, SourceIdentity};
        use crate::stacking::groups::frame_source;
        let f = test_fixtures::frame_set("LDN 1272");
        for (i, t) in THREE_TIMES.iter().enumerate() {
            test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
        }
        f.conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [f.set_id],
            )
            .unwrap();
        let cfg = StackingConfig::default();
        let (groups, origins) =
            frame_source(&f.conn, &StackSubject::FrameSet(f.set_id), &cfg).unwrap();
        let frame = groups[0].frames[0].clone();
        let expected = stage_hash(
            &prepare_subtree(&cfg),
            &[],
            &[SourceIdentity {
                file_id: frame.file_id,
                size: frame.size,
                modified_at: frame.modified_at.clone(),
            }],
        );
        assert_eq!(prepare_hash_for_catalog(&cfg, &frame), expected);
        let mut memo = HashMemo::new(origins);
        assert_eq!(
            memo.calibration_hash_checked(&f.conn, &cfg, &frame)
                .unwrap(),
            expected
        );
    }

    /// Plan 6b-2 C12: the Prepare subtree's exact shape, and the formula of
    /// the project stage-1 hash — content and size, never a catalog identity;
    /// the debayer toggle moves it, an unrelated stage's config does not. The
    /// dispatch through `HashMemo` is covered by the project frame source's
    /// decoy test.
    #[test]
    fn the_prepare_subtree_and_the_project_hash_are_pinned() {
        use crate::stacking::config::{prepare_subtree, stage_hash, SourceIdentity};
        let cfg = StackingConfig::default();
        assert_eq!(
            serde_json::to_string(&prepare_subtree(&cfg)).unwrap(),
            r#"{"debayerMethod":"VNG","debayerOsc":true,"prepareVersion":1}"#
        );
        let blake3 = "ab".repeat(32);
        let expected = stage_hash(
            &prepare_subtree(&cfg),
            &[&format!("blake3:{blake3}")],
            &[SourceIdentity {
                file_id: 0,
                size: 4096,
                modified_at: String::new(),
            }],
        );
        assert_eq!(prepare_hash_for_project(&cfg, &blake3, 4096), expected);
        let mut other = cfg.clone();
        other.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
        assert_eq!(prepare_hash_for_project(&other, &blake3, 4096), expected);
        other.calibration.debayer_osc = false;
        assert_ne!(prepare_hash_for_project(&other, &blake3, 4096), expected);
    }

    /// Plan 6b-2 C7: a project frame's manual reference resolves its file
    /// name and landed path from the origin, never the catalog.
    #[test]
    fn a_projects_manual_reference_resolves_from_its_origin() {
        use crate::stacking::groups::frame_source;
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        for uuid in ["r1", "r2", "r3"] {
            add_project_frame(&f.conn, f.dir.path(), "p1", &project_frame_spec(uuid));
        }
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        let subject = StackSubject::Project("p1".into());
        let (_, origins) = frame_source(&f.conn, &subject, &cfg).unwrap();
        crate::db::stacking::set_project_config(&f.conn, "p1", "{}", &[], Some("r2")).unwrap();
        let mut blockers = Vec::new();
        let reference =
            resolve_reference(&f.conn, &subject, &origins, &cfg, &mut blockers).unwrap();
        assert!(blockers.is_empty(), "{blockers:?}");
        assert_eq!(reference.filename.as_deref(), Some("c_r2.fits"));
        assert!(reference.on_disk);
    }

    /// Spec §9.4 / plan 6b-2 C10: the first plan of a project stores the
    /// global default with `stops` and the coverage map on; a stored document
    /// is whole-config; an override wins without being stored.
    #[test]
    fn a_project_config_is_seeded_then_resolved_whole() {
        use crate::stacking::config::ExposureMode;
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let settings = SettingsManager::new();
        let (cfg, excluded) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", None).unwrap();
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::Stops);
        assert!(cfg.output.coverage_map);
        assert!(excluded.is_empty());
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1")
            .unwrap()
            .unwrap();
        let stored_cfg: StackingConfig = serde_json::from_str(&stored.config_json).unwrap();
        assert_eq!(stored_cfg, cfg, "the seed is what was stored");

        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            r#"{"grouping":{"exposureMode":"none"}}"#,
            &["u7".to_string()],
            None,
        )
        .unwrap();
        let (cfg, excluded) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", None).unwrap();
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::NoSplit);
        assert!(
            !cfg.output.coverage_map,
            "whole-config: no field-level merge with the seed"
        );
        assert_eq!(excluded, vec!["u7".to_string()]);

        // A stored document is returned without the global being read: a
        // corrupt global default does not matter once a project has its own.
        f.conn
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, 'not json', '')",
                [crate::settings::keys::STACKING_DEFAULTS],
            )
            .unwrap();
        let (cfg, _) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", None).unwrap();
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::NoSplit);
        crate::db::stacking::insert_test_project(&f.conn, "p2");
        assert!(
            resolve_project_config_and_exclusions(&f.conn, &settings, "p2", None).is_err(),
            "a first plan does read the (corrupt) global"
        );
        f.conn
            .execute(
                "DELETE FROM settings WHERE key = ?1",
                [crate::settings::keys::STACKING_DEFAULTS],
            )
            .unwrap();

        let mut over = StackingConfig::default();
        over.output.crop_to_coverage = 5;
        let (cfg, _) =
            resolve_project_config_and_exclusions(&f.conn, &settings, "p1", Some(over.clone()))
                .unwrap();
        assert_eq!(cfg, over);
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1")
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.config_json, r#"{"grouping":{"exposureMode":"none"}}"#,
            "an override is never stored"
        );
    }

    /// A project `p1` on `f.conn` (target 83.8, −5.4, radius 1°; not
    /// coordinator, no caps):
    /// - `r1..r3`: filter R, WCS;
    /// - `g1, g2`: filter G, WCS, plus `g3`, which never landed;
    /// - `o1`: filter L, an OSC mosaic with no WCS;
    /// - `x1`: coordinator-excluded.
    /// Returns the working and output folders a test may use.
    fn seed_project(f: &test_fixtures::Fixture) -> (std::path::PathBuf, std::path::PathBuf) {
        use crate::stacking::test_fixtures::{
            add_project_frame, manifest_wcs_json, project_frame_spec, ProjectFile,
        };
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        let wcs = || Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        for (uuid, filter) in [
            ("r1", "R"),
            ("r2", "R"),
            ("r3", "R"),
            ("g1", "G"),
            ("g2", "G"),
        ] {
            let mut spec = project_frame_spec(uuid);
            spec.filter = filter;
            spec.wcs = wcs();
            add_project_frame(&f.conn, &landed, "p1", &spec);
        }
        let mut g3 = project_frame_spec("g3");
        g3.filter = "G";
        g3.file = ProjectFile::Missing;
        add_project_frame(&f.conn, &landed, "p1", &g3);
        let mut o1 = project_frame_spec("o1");
        o1.file = ProjectFile::Mosaic32;
        o1.bayerpat = Some("RGGB");
        add_project_frame(&f.conn, &landed, "p1", &o1);
        let mut x1 = project_frame_spec("x1");
        x1.accepted = false;
        add_project_frame(&f.conn, &landed, "p1", &x1);
        (f.dir.path().join("work"), f.dir.path().join("out"))
    }

    fn with_folders(working: &std::path::Path, output: &std::path::Path) -> StackingConfig {
        let mut cfg = StackingConfig::default();
        cfg.paths.working_dir = Some(working.to_string_lossy().into_owned());
        cfg.paths.output_dir = Some(output.to_string_lossy().into_owned());
        cfg
    }

    /// Spec §7: the project plan — groups by colour × canonical filter ×
    /// class (no binning), the grid blocker for a member who cannot edit it,
    /// the not-on-this-device and solved-locally warnings, the Frames rows
    /// (a coordinator-excluded frame absent), empty readiness and no masters.
    #[test]
    fn a_project_plan_reports_its_groups_frames_grid_and_warnings() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        assert_eq!(
            (plan.set_id, plan.project_id.as_deref()),
            (None, Some("p1"))
        );
        // `insert_test_project` sets `title = project_id` (review M6).
        assert_eq!(plan.set_name, "p1");
        let mut keys: Vec<(&str, usize)> = plan
            .groups
            .iter()
            .map(|g| (g.key.as_str(), g.frame_count))
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                ("mono__G__300s", 2),
                ("mono__R__300s", 3),
                ("osc__L__300s", 1)
            ]
        );
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(codes, vec!["grid"], "{:?}", plan.blockers);
        assert_eq!(
            plan.blockers[0].message,
            "Waiting for the coordinator to confirm the grid"
        );
        for expected in [
            "1 frame of the project is not on this device and is left out (G: 1)",
            // No catalog is installed here, so nothing can be solved (plan
            // 6b-3 D14: the plan judges with the installed solver).
            "1 frame could not be plate-solved and cannot anchor the grid",
        ] {
            assert!(
                plan.warnings.iter().any(|w| w == expected),
                "{expected} not in {:?}",
                plan.warnings
            );
        }
        let grid = plan.grid.as_ref().unwrap();
        assert_eq!(grid.state, GridState::Proposed);
        assert!(!grid.can_edit);
        assert_eq!(grid.proposal.unwrap().scale_arcsec, 1.5);
        assert_eq!(grid.frames_inside, 5);
        assert_eq!(
            plan.project_frames.len(),
            7,
            "x1 is excluded by the coordinator, so absent"
        );
        let g3 = plan
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "g3")
            .unwrap();
        assert!(!g3.on_disk && g3.handle.is_none());
        assert_eq!(g3.reason.as_deref(), Some("not on this device"));
        let o1 = plan
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "o1")
            .unwrap();
        assert!(!o1.has_wcs && o1.on_disk && o1.handle.is_some());
        assert_eq!(o1.color_mode, ColorMode::Osc);
        assert!(plan.masters_to_build.is_empty());
        assert_eq!(plan.readiness.total, 0);
    }

    /// Spec §6.3: a confirmed grid lifts the blocker; frames it leaves
    /// outside are counted.
    #[test]
    fn a_confirmed_grid_lifts_the_blocker_and_counts_frames_outside() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let far = crate::collab::grid::CanonicalGrid {
            scale_arcsec: 1.5,
            width_px: 128,
            height_px: 128,
            crval1: 93.8,
            crval2: -5.4,
            rotation_deg: 0.0,
        };
        crate::db::collab::set_canonical_grid(&f.conn, "p1", Some(&far.canonical_json())).unwrap();
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        assert!(
            plan.blockers.iter().all(|b| b.code != "grid"),
            "{:?}",
            plan.blockers
        );
        let grid = plan.grid.as_ref().unwrap();
        assert_eq!(
            (grid.state, grid.current),
            (GridState::Confirmed, Some(far))
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w == "5 frames lie outside the grid"),
            "{:?}",
            plan.warnings
        );
    }

    /// Spec §9.4: the first plan seeds the config (`stops`), and the stored
    /// exclusions (uuids) become handles.
    #[test]
    fn a_project_plan_seeds_its_config_and_maps_exclusions_to_handles() {
        let f = test_fixtures::frame_set("unused");
        seed_project(&f);
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                None,
            )
            .unwrap()
        };
        let first = plan(&f);
        assert_eq!(
            first.config.grouping.exposure_mode,
            crate::stacking::config::ExposureMode::Stops
        );
        assert!(
            first.groups.iter().any(|g| g.key == "mono__R__st300"),
            "{:?}",
            first.groups
        );
        let stored = crate::db::stacking::get_project_config(&f.conn, "p1")
            .unwrap()
            .unwrap();
        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            &stored.config_json,
            &["r1".to_string()],
            None,
        )
        .unwrap();
        let second = plan(&f);
        let r1 = second
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "r1")
            .unwrap()
            .handle
            .unwrap();
        assert_eq!(second.excluded_frame_ids, vec![r1]);
        let r = second
            .groups
            .iter()
            .find(|g| g.key == "mono__R__st300")
            .unwrap();
        assert_eq!((r.frame_count, r.included_count), (3, 2));
        let again = plan(&f);
        assert_eq!(
            again
                .project_frames
                .iter()
                .find(|r| r.frame_uuid == "r1")
                .unwrap()
                .handle,
            Some(r1),
            "the handle is stable across plans"
        );
    }

    /// Plan 6b-2 C6: a project working root inside a scan root is a
    /// `folders` blocker (for a frame set the overlap only warns, at save).
    #[test]
    fn a_project_working_folder_inside_a_scan_root_is_blocked() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        std::fs::create_dir_all(&working).unwrap();
        f.conn
            .execute(
                "INSERT INTO scan_roots (path) VALUES (?1)",
                [working.to_str().unwrap()],
            )
            .unwrap();
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(with_folders(&working, &output)),
        )
        .unwrap();
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(codes, vec!["grid", "folders"], "{:?}", plan.blockers);
        assert!(plan.blockers[1]
            .message
            .contains("overlaps a monitored folder"));
    }

    /// Plan 6b-2 C10: a project's blockers come in a fixed order.
    #[test]
    fn a_projects_blocker_order_is_stable() {
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec, ProjectFile};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let mut gone = project_frame_spec("a");
        gone.file = ProjectFile::Missing;
        add_project_frame(&f.conn, f.dir.path(), "p1", &gone);
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 4;
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(cfg),
        )
        .unwrap();
        let codes: Vec<&str> = plan.blockers.iter().map(|b| b.code.as_str()).collect();
        assert_eq!(
            codes,
            vec![
                "grid",
                "reference",
                "folders",
                "folders",
                "frames",
                "unsupported"
            ]
        );
        assert_eq!(
            plan.blockers[4].message,
            "No frame of this project is on this device yet"
        );
    }

    /// Review I1 (plan 6b-2 C10): an unsupported file leaves the plan's
    /// included counts, so a group that Prepare would leave under three frames
    /// is blocked at plan time, not at run time. The file is not a manual
    /// exclusion.
    #[test]
    fn an_unsupported_file_does_not_count_toward_three_frames() {
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec, ProjectFile};
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        for uuid in ["a", "b"] {
            add_project_frame(&f.conn, &landed, "p1", &project_frame_spec(uuid));
        }
        let mut tile = project_frame_spec("t");
        tile.file = ProjectFile::EmptyPrimary;
        add_project_frame(&f.conn, &landed, "p1", &tile);
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            None,
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1, "{:?}", plan.groups);
        let g = &plan.groups[0];
        assert_eq!((g.frame_count, g.included_count), (3, 2));
        assert!(
            plan.excluded_frame_ids.is_empty(),
            "an unsupported file is not a manual exclusion"
        );
        let row = plan
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "t")
            .unwrap();
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.starts_with("unsupported file: ")),
            "{:?}",
            row.reason
        );
        assert!(
            plan.blockers.iter().any(|b| b.code == "frames"),
            "{:?}",
            plan.blockers
        );
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.ends_with("(unsupported file): c_t.fits")),
            "{:?}",
            plan.warnings
        );
    }

    /// Spec §13 (R3): an attested set needs no masters — no masters / links
    /// / masterFiles blocker and nothing to build; its unattested twin does.
    #[test]
    fn an_attested_set_plans_without_masters() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let (id, _path) = test_fixtures::add_light(&f, &light_spec(&format!("l{i}"), t));
            ids.push(id);
        }
        // Review I11: a buildable raw linked dark, so "nothing to build" is
        // a real claim about the attested plan.
        test_fixtures::add_raw_linked_dark(&f, &ids, 64, 48);
        let library_dir = f.dir.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();
        crate::db::set_setting(
            &f.conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &library_dir.to_string_lossy(),
        )
        .unwrap();
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::FrameSet(f.set_id),
                None,
            )
            .unwrap()
        };
        let calibration_codes = ["masters", "links", "masterFiles"];
        let unattested = plan(&f);
        assert_eq!(
            unattested.masters_to_build.len(),
            1,
            "the unattested twin owes a master"
        );
        f.conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [f.set_id],
            )
            .unwrap();
        let attested = plan(&f);
        assert!(attested.readiness.attested);
        assert!(attested.masters_to_build.is_empty());
        assert!(
            attested
                .blockers
                .iter()
                .all(|b| !calibration_codes.contains(&b.code.as_str())),
            "{:?}",
            attested.blockers
        );
    }

    /// A project this device left fails readably and the plan writes no seed
    /// row for it.
    #[test]
    fn a_left_project_plan_fails_readably_and_seeds_nothing() {
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        crate::db::collab::mark_lost(&f.conn, "p1").unwrap();
        let err = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::NotFound(ref m) if m == "project no longer joined"),
            "{err:?}"
        );
        assert!(crate::db::stacking::get_project_config(&f.conn, "p1")
            .unwrap()
            .is_none());
    }

    /// A project this device does not know fails readably, never as a raw
    /// foreign-key error from the config seed.
    #[test]
    fn an_unknown_project_plan_fails_readably() {
        let f = test_fixtures::frame_set("unused");
        let err = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("nope".into()),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::NotFound(ref m) if m.contains("nope")),
            "{err:?}"
        );
    }

    /// Plan 6b-2 C14: the plan reads the run's last cached solve outcome, so
    /// a frame that could not be solved is reported as such, not as "will
    /// be plate-solved".
    #[test]
    fn a_project_plan_reports_frames_that_could_not_be_solved() {
        let f = test_fixtures::frame_set("unused");
        let (working, output) = seed_project(&f);
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                Some(with_folders(&working, &output)),
            )
            .unwrap()
        };
        let first = plan(&f);
        let o1 = first
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "o1")
            .unwrap()
            .handle
            .unwrap();
        let blake3: String = f
            .conn
            .query_row(
                "SELECT blake3 FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        crate::stacking::solve::write_outcome(
            &f.conn,
            "p1",
            o1,
            &blake3,
            "h",
            crate::stacking::solve::SolveOutcome::Failed("no match".into()),
        )
        .unwrap();
        let second = plan(&f);
        assert!(
            second
                .warnings
                .iter()
                .any(|w| w == "1 frame could not be plate-solved and cannot anchor the grid"),
            "{:?}",
            second.warnings
        );
        assert!(
            second
                .warnings
                .iter()
                .all(|w| !w.contains("will be plate-solved locally")),
            "{:?}",
            second.warnings
        );
    }

    /// Plan 6b-3 D14 (spec §5.4): with no installed catalog nothing can be
    /// solved, so a project none of whose in-scope frames carries a manifest
    /// WCS is blocked at plan time — the run would fail at Reference. One
    /// frame with a manifest WCS lifts the block.
    #[test]
    fn a_project_with_no_possible_wcs_is_blocked_at_plan_time() {
        use crate::stacking::test_fixtures::{
            add_project_frame, manifest_wcs_json, project_frame_spec,
        };
        let f = test_fixtures::frame_set("unused");
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        for uuid in ["a", "b", "c"] {
            add_project_frame(&f.conn, &landed, "p1", &project_frame_spec(uuid));
        }
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                None,
            )
            .unwrap()
        };
        let blocked = plan(&f);
        let reference: Vec<&PlanBlocker> = blocked
            .blockers
            .iter()
            .filter(|b| b.code == "reference")
            .collect();
        assert_eq!(reference.len(), 1, "{:?}", blocked.blockers);
        assert_eq!(
            reference[0].message,
            crate::stacking::origin::NO_FRAME_WITH_WCS_NO_CATALOG
        );

        let mut d = project_frame_spec("d");
        d.wcs = Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
        add_project_frame(&f.conn, &landed, "p1", &d);
        let open = plan(&f);
        assert!(
            open.blockers.iter().all(|b| b.code != "reference"),
            "{:?}",
            open.blockers
        );
    }

    /// Plan 6b-3 D14: a manual pin without a manifest WCS is a warning while
    /// the run still has to try to solve it, and a `reference` blocker once
    /// its cached failure stands for this very solver and catalog. A failure
    /// cached under another solver identity is tried again, so it warns.
    #[test]
    fn a_pinned_frame_without_a_wcs_warns_then_blocks_on_a_standing_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let f = test_fixtures::frame_set_with_conn(
            rusqlite::Connection::open(&db_path).unwrap(),
            "unused",
        );
        test_fixtures::install_stand_in_catalog(dir.path());
        let (working, output) = seed_project(&f);
        let mut cfg = with_folders(&working, &output);
        let first = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &StackSubject::Project("p1".into()),
            Some(cfg.clone()),
        )
        .unwrap();
        let o1 = first
            .project_frames
            .iter()
            .find(|r| r.frame_uuid == "o1")
            .unwrap()
            .handle
            .unwrap();
        cfg.reference.mode = ReferenceMode::Manual;
        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            &serde_json::to_string(&cfg).unwrap(),
            &[],
            Some("o1"),
        )
        .unwrap();
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                None,
            )
            .unwrap()
        };
        let pin_blocked = |p: &StackingPlan| {
            p.blockers.iter().any(|b| {
                b.code == "reference" && b.message == crate::stacking::origin::PIN_WITHOUT_WCS
            })
        };

        let to_solve = plan(&f);
        assert!(
            to_solve
                .warnings
                .iter()
                .any(|w| w == crate::stacking::origin::PIN_TO_SOLVE),
            "{:?}",
            to_solve.warnings
        );
        assert!(!pin_blocked(&to_solve), "{:?}", to_solve.blockers);

        let blake3: String = f
            .conn
            .query_row(
                "SELECT blake3 FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = 'o1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let identity = crate::stacking::solve::installed_solver_identity(&f.conn).unwrap();
        let failed = crate::stacking::solve::SolveOutcome::Failed("no match".into());
        crate::stacking::solve::write_outcome(
            &f.conn,
            "p1",
            o1,
            &blake3,
            &crate::stacking::solve::solve_hash(&blake3, &identity),
            failed.clone(),
        )
        .unwrap();
        let blocked = plan(&f);
        assert!(pin_blocked(&blocked), "{:?}", blocked.blockers);

        crate::stacking::solve::write_outcome(&f.conn, "p1", o1, &blake3, "another-solver", failed)
            .unwrap();
        let retried = plan(&f);
        assert!(!pin_blocked(&retried), "{:?}", retried.blockers);
        assert!(retried
            .warnings
            .iter()
            .any(|w| w == crate::stacking::origin::PIN_TO_SOLVE));
    }

    /// Plan 6b-3 D14 (review): a cached solve whose SIP tables cannot be read
    /// is no WCS at all — `ReferenceWcs::from_record` refuses it, so the run
    /// would fail at Reference with "no usable WCS". The plan keeps such a
    /// reference out: as a pin, and as the only candidate.
    #[test]
    fn a_cached_solve_with_unreadable_sip_cannot_be_a_reference() {
        use crate::stacking::test_fixtures::{add_project_frame, project_frame_spec};
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let f = test_fixtures::frame_set_with_conn(
            rusqlite::Connection::open(&db_path).unwrap(),
            "unused",
        );
        test_fixtures::install_stand_in_catalog(dir.path());
        crate::db::stacking::insert_test_project(&f.conn, "p1");
        let landed = f.dir.path().join("collab");
        for uuid in ["a", "b"] {
            add_project_frame(&f.conn, &landed, "p1", &project_frame_spec(uuid));
        }
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            &serde_json::to_string(&cfg).unwrap(),
            &[],
            Some("a"),
        )
        .unwrap();
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                None,
            )
            .unwrap()
        };
        let reference_messages = |p: &StackingPlan| -> Vec<String> {
            p.blockers
                .iter()
                .filter(|b| b.code == "reference")
                .map(|b| b.message.clone())
                .collect()
        };
        let identity = crate::stacking::solve::installed_solver_identity(&f.conn).unwrap();
        let seeded = plan(&f);
        let mut bad = crate::stacking::solve::FakeSolver::fixture_record();
        bad.sip_order = Some(2);
        bad.sip_a_coeffs = Some("not json".into());
        bad.sip_b_coeffs = Some("not json".into());
        assert!(bad.to_solution().is_none());
        for row in &seeded.project_frames {
            let blake3: String = f
                .conn
                .query_row(
                    "SELECT blake3 FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = ?1",
                    [&row.frame_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            crate::stacking::solve::write_outcome(
                &f.conn,
                "p1",
                row.handle.unwrap(),
                &blake3,
                &crate::stacking::solve::solve_hash(&blake3, &identity),
                crate::stacking::solve::SolveOutcome::Solved(bad.clone()),
            )
            .unwrap();
        }
        let blocked = reference_messages(&plan(&f));
        assert_eq!(
            blocked,
            vec![crate::stacking::origin::PIN_WITHOUT_WCS.to_string()]
        );

        // With the pin back on Auto, no candidate has a usable WCS either.
        cfg.reference.mode = ReferenceMode::Auto;
        crate::db::stacking::set_project_config(
            &f.conn,
            "p1",
            &serde_json::to_string(&cfg).unwrap(),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(
            reference_messages(&plan(&f)),
            vec![crate::stacking::origin::NO_FRAME_WITH_WCS.to_string()]
        );
    }

    /// Plan 6b-3 D16: a project's steady-state plan writes nothing. With the
    /// config seeded and every handle minted by a first plan, a second plan
    /// succeeds while another connection holds the write lock and this one
    /// will not wait for it.
    #[test]
    fn a_steady_state_project_plan_takes_no_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("catalog.db");
        let f = test_fixtures::frame_set_with_conn(
            rusqlite::Connection::open(&db_path).unwrap(),
            "unused",
        );
        let (working, output) = seed_project(&f);
        let plan = |f: &test_fixtures::Fixture| {
            build_plan(
                &f.conn,
                &SettingsManager::new(),
                &PathPolicy::AllowAll,
                &StackSubject::Project("p1".into()),
                Some(with_folders(&working, &output)),
            )
        };
        let first = plan(&f).unwrap();
        let writer = rusqlite::Connection::open(&db_path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        f.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let second = plan(&f).expect("a steady-state plan only reads");
        assert_eq!(second.project_frames.len(), first.project_frames.len());
        assert_eq!(second.config_hash, first.config_hash);
        writer.execute_batch("ROLLBACK").unwrap();
    }

    /// Plan 6b-3 D15 (the 6b-2 leftover): one rule for stage 1 and LN — an
    /// `osc__` group on the Prepare path with the debayer off keeps three
    /// planes (and three LN channels); on the Calibrate path a mosaic stays
    /// one plane.
    #[test]
    fn stage_one_planes_follow_the_prepare_rule() {
        let mut cfg = StackingConfig::default();
        cfg.calibration.debayer_osc = false;
        let group = |key: &str, color_mode| IntegrationGroup {
            key: key.into(),
            instrume: None,
            color_mode,
            filter: None,
            binning: 1,
            cameras: Vec::new(),
            exposure_s: Some(300.0),
            exposure_class: "st300".into(),
            frames: Vec::new(),
            total_exposure_s: 0.0,
            pixel_scale_arcsec: None,
        };
        assert_eq!(
            stage_one_planes(&cfg, &group("osc__None__st300", ColorMode::Osc), true),
            3
        );
        assert_eq!(
            stage_one_planes(&cfg, &group("cfa__None__st300", ColorMode::Osc), true),
            1
        );
        assert_eq!(
            stage_one_planes(&cfg, &group("osc__None__bin1__300s", ColorMode::Osc), false),
            1
        );
        assert_eq!(
            stage_one_planes(&cfg, &group("mono__L__st300", ColorMode::Mono), true),
            1
        );
        cfg.calibration.debayer_osc = true;
        assert_eq!(
            stage_one_planes(&cfg, &group("osc__None__bin1__300s", ColorMode::Osc), false),
            3
        );
    }

    /// Plan 6b-3 D15 (review fix 5): an attested `osc__` group with the
    /// debayer off is three-plane after stage 1, so the plan's C-17 shape
    /// filter keeps a three-channel `ln_calibration` — its `ln` rows read
    /// fresh at hashes folding it — and drops a one-channel one.
    #[test]
    fn an_attested_osc_group_counts_a_three_channel_ln_calibration() {
        let f = test_fixtures::frame_set("LDN 1272");
        let mut ids = Vec::new();
        for (i, t) in THREE_TIMES.iter().enumerate() {
            let stem = format!("f{i}");
            let mut spec = light_spec(&stem, t);
            spec.bayerpat = Some("RGGB");
            let (id, path) = test_fixtures::add_light(&f, &spec);
            // Three planes on disk: an `osc__` group, not a `cfa__` one.
            crate::fits_writer::write_fits_f32(&path, 64, 48, 3, &vec![0.5f32; 64 * 48 * 3], &[])
                .unwrap();
            ids.push(id);
        }
        f.conn
            .execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [f.set_id],
            )
            .unwrap();
        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        for (key, dir) in [
            (keys::STACKING_WORKING_DIR, working.path()),
            (keys::STACKING_OUTPUT_DIR, output.path()),
        ] {
            crate::db::set_setting(&f.conn, key, dir.to_str().unwrap()).unwrap();
        }
        let mut cfg = StackingConfig::default();
        cfg.reference.mode = ReferenceMode::Manual;
        cfg.normalization.local.enabled = true;
        cfg.calibration.debayer_osc = false;
        set_frame_set_reference(&f.conn, f.set_id, ids[0]).unwrap();
        let subject = StackSubject::FrameSet(f.set_id);
        let (groups, _origins) = frame_source(&f.conn, &subject, &cfg).unwrap();
        assert_eq!(groups.len(), 1);
        let group_key = groups[0].key.clone();
        assert!(group_key.starts_with("osc__"), "{group_key}");
        let up = |kind: &str,
                  frame_id: Option<i64>,
                  hash: &str,
                  path: Option<&str>,
                  payload: Option<&str>| {
            let size = path.map(|p| std::fs::metadata(p).unwrap().len() as i64);
            crate::db::stacking::upsert_artifact(
                &f.conn,
                &crate::db::stacking::NewArtifact {
                    subject: &subject,
                    frame_id,
                    group_key: &group_key,
                    kind,
                    path,
                    config_hash: hash,
                    size,
                    modified_at: None,
                    payload_json: payload,
                },
            )
            .unwrap();
        };
        let mut calib: HashMap<i64, String> = HashMap::new();
        let mut metrics: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let c = prepare_hash_for_catalog(&cfg, gf);
            let m = measurement_hash_for(&cfg, &c);
            up("metrics", Some(gf.frame_id), &m, None, Some("{}"));
            calib.insert(gf.frame_id, c);
            metrics.insert(gf.frame_id, m);
        }
        seed_run_frame_rows(&f.conn, f.set_id, Some(ids[0]), "manual", &ids);
        let mut registration: HashMap<i64, String> = HashMap::new();
        for gf in &groups[0].frames {
            let h = registration_hash_for(
                &cfg,
                ids[0],
                &calib[&ids[0]],
                &calib[&gf.frame_id],
                &metrics[&gf.frame_id],
            );
            registration.insert(gf.frame_id, h.clone());
            let is_ref = gf.frame_id == ids[0];
            crate::registration::db::upsert_registration(
                &f.conn,
                &RegistrationRecord {
                    frames_set_id: f.set_id,
                    frame_id: gf.frame_id,
                    reference_frame_id: ids[0],
                    is_reference: is_ref,
                    status: if is_ref { "reference" } else { "aligned" }.to_string(),
                    compute_time_ms: 0,
                    registered_at: "2025-01-01T00:00:00Z".to_string(),
                    config_hash: Some(h),
                    source_kind: Some("calibrated".to_string()),
                    ..RegistrationRecord::default()
                },
            )
            .unwrap();
        }
        let combined = {
            let mut hs: Vec<&str> = ids.iter().map(|id| registration[id].as_str()).collect();
            hs.sort_unstable();
            hs.join(",")
        };
        let reference_hash = normalization_hash_for(&cfg, &combined, "", &ids, "", None, None, None);
        let ref_payload = serde_json::to_string(&LnReferencePayload {
            reference_member_ids: ids.clone(),
            reference_hash: reference_hash.clone(),
            width: 0,
            height: 0,
            scale_px: 0,
        })
        .unwrap();
        let ref_path = f.dir.path().join("reference.fits");
        std::fs::write(&ref_path, [0u8]).unwrap();
        up(
            "ln_reference",
            None,
            &reference_hash,
            ref_path.to_str(),
            Some(&ref_payload),
        );
        let calibration = SeedsCalibration {
            frame_ids: ids.clone(),
            k: vec![0.99, 0.98, 0.97],
        };
        let calibration_json = serde_json::to_string(&calibration).unwrap();
        up("ln_calibration", None, "x", None, Some(&calibration_json));
        let ln_scale = crate::stacking::ln::resolve_scale(
            &cfg.normalization.local,
            groups[0].frames[0].width as usize,
            groups[0].frames[0].height as usize,
        );
        for gf in &groups[0].frames {
            let h = normalization_hash_for(
                &cfg,
                &registration[&gf.frame_id],
                &metrics[&gf.frame_id],
                &ids,
                &reference_hash,
                Some(&calibration),
                None,
                Some(ln_scale),
            );
            let p = f.dir.path().join(format!("f{}.athln", gf.frame_id));
            std::fs::write(&p, [0u8]).unwrap();
            up("ln", Some(gf.frame_id), &h, p.to_str(), None);
        }
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &subject,
            Some(cfg.clone()),
        )
        .unwrap();
        assert_eq!(plan.groups[0].ln_cached, 3, "{:?}", plan.groups[0]);
        // A one-channel payload is the wrong shape for a three-plane group:
        // it reads absent, the hashes move, nothing is cached.
        let one = serde_json::to_string(&SeedsCalibration {
            frame_ids: ids.clone(),
            k: vec![0.99],
        })
        .unwrap();
        up("ln_calibration", None, "x", None, Some(&one));
        let plan = build_plan(
            &f.conn,
            &SettingsManager::new(),
            &PathPolicy::AllowAll,
            &subject,
            Some(cfg),
        )
        .unwrap();
        assert_eq!(plan.groups[0].ln_cached, 0, "{:?}", plan.groups[0]);
    }
}
