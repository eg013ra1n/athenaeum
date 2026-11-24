//! The `StackingConfig` tree (spec §9.2/§9.3): every stage's settings under
//! one versioned, camelCase, fully-defaulted document; the built-in presets;
//! whole-config precedence over a stored set/global override; and the
//! per-stage config hashes an artifact's reuse check keys off.
//!
//! Every sub-config here mirrors the same shape the spec's table lists:
//! `#[serde(rename_all = "camelCase", default)]` so `{}` (and any partial
//! JSON) decodes, an explicit `impl Default` where the defaults are not all
//! zero/`None`/the field type's own `#[default]` variant.

use serde::{Deserialize, Serialize};
use tracing::warn;

// Named by the tests below through `use super::*`.
#[cfg(test)]
use crate::resample::Interpolation;
#[cfg(test)]
use crate::stacking::fits_artifact::FITS_ARTIFACT_VERSION;
#[cfg(test)]
use crate::stacking::integrate::RejectionChoice;
#[cfg(test)]
use crate::stacking::register::align::ALIGN_VERSION;
#[cfg(test)]
use crate::stacking::register::DistortionChoice;
#[cfg(test)]
use crate::star_measure::MEASURE_VERSION;
#[cfg(test)]
use starloom::stacking::measure::MEASURE_STAGE_VERSION;

// The measurement and drizzle configs, the seed source, the algorithm sections with their presets and stage subtrees,
// and the kernel's bounds are the kernel's; this file keeps the resolution, the app's own presets lines and the
// stage hash that folds upstream and sources.
pub use starloom::stacking::config::{
    algorithm_preset, AlgorithmConfig, DrizzleConfig, DrizzleKernel, MeasurementConfig, SeedSource, StackingPreset,
    CALIBRATION_HASH_GROUPING_TOLERANCE_SEC, DRIZZLE_KERNEL_VERSION, MAX_GROWTH, MAX_PROTECTED_LAYERS,
    MIN_PROTECTED_LAYERS,
};

/// Bumped only when the `StackingConfig` shape changes in a way an old
/// stored document can't decode through `#[serde(default)]` alone (spec
/// §9.2).
pub const STACKING_CONFIG_VERSION: u32 = 1;

/// spec §9.2: the whole per-run configuration document. camelCase on the
/// wire, every field defaulted so `{}` (and any partial JSON) decodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct StackingConfig {
    pub version: u32,
    pub grouping: GroupingConfig,
    pub calibration: crate::export::CalibratedLightOptions,
    pub measurement: MeasurementConfig,
    pub selection: crate::stacking::weights::SelectionConfig,
    pub reference: ReferenceConfig,
    pub registration: crate::stacking::register::RegistrationConfig,
    pub normalization: crate::stacking::integrate::NormalizationConfig,
    pub integration: crate::stacking::integrate::IntegrationConfig,
    pub drizzle: DrizzleConfig,
    pub output: OutputConfig,
    pub paths: PathsConfig,
}

impl Default for StackingConfig {
    fn default() -> Self {
        StackingConfig {
            version: STACKING_CONFIG_VERSION,
            grouping: GroupingConfig::default(),
            calibration: crate::export::CalibratedLightOptions::default(),
            measurement: MeasurementConfig::default(),
            selection: crate::stacking::weights::SelectionConfig::default(),
            reference: ReferenceConfig::default(),
            registration: crate::stacking::register::RegistrationConfig::default(),
            normalization: crate::stacking::integrate::NormalizationConfig::default(),
            integration: crate::stacking::integrate::IntegrationConfig::default(),
            drizzle: DrizzleConfig::default(),
            output: OutputConfig::default(),
            paths: PathsConfig::default(),
        }
    }
}

impl StackingConfig {
    /// The seven algorithm sections (calibration, measurement, selection, registration, normalization, integration and
    /// drizzle), cloned: what the kernel's stage subtrees and presets read. Grouping, reference, output and paths are
    /// the app's own.
    pub fn algorithm(&self) -> AlgorithmConfig {
        AlgorithmConfig {
            calibration: self.calibration.clone(),
            measurement: self.measurement.clone(),
            selection: self.selection.clone(),
            registration: self.registration.clone(),
            normalization: self.normalization.clone(),
            integration: self.integration.clone(),
            drizzle: self.drizzle.clone(),
        }
    }
}

/// The floor `resolve_config` clamps [`GroupingConfig::exposure_tolerance_sec`]
/// to (fix round 1, minor 5). Below this, two distinct exposure clusters
/// (e.g. a genuine near-zero or negative stored value) can format to the
/// SAME [`crate::calibration_library::paths::fmt_num`] token and collide on
/// `stacking_run_groups`'s `UNIQUE(run_id, group_key)`.
pub const MIN_EXPOSURE_TOLERANCE_SEC: f64 = 0.01;

/// The range [`resolve_config`] clamps [`MeasurementConfig::detection_sigma`]
/// to — the same numbers `MeasurePanel`'s field offers, enforced on the
/// backend too because a stored or hand-edited document never went through
/// that field (M4a Task 2 fix round 3).
pub const MIN_DETECTION_SIGMA: f64 = 1.0;
pub const MAX_DETECTION_SIGMA: f64 = 100.0;

/// The range [`resolve_config`] clamps
/// [`crate::stacking::register::RegistrationConfig::tps_smoothing`] to —
/// the same 0–10 `RegisterPanel`'s field offers (M4c Task 4 fix round 1,
/// minor 5). Enforced on the backend for the same reason as
/// `detectionSigma`: a stored or hand-edited document never went through
/// that field, and a negative or non-finite λ would otherwise make
/// `ThinPlateSpline::fit` refuse EVERY frame's spline one at a time, with
/// a per-frame note instead of one honest warning about the config.
pub const MIN_TPS_SMOOTHING: f64 = 0.0;
pub const MAX_TPS_SMOOTHING: f64 = 10.0;

/// spec 2026-10-02 §8 (plan 6b-2 C2/C3): how a group's exposure classes
/// are formed. `seconds` is today's tolerance clustering — the frame-set
/// default, so an existing stack regroups nothing; `stops` takes every
/// exposure within `stopsWidth` stops of the class's shortest; `equivalent`
/// does the same on `t / N²` (`N = FOCALLEN / APTDIA`); `none` puts every
/// exposure of a colour × filter into one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ExposureMode {
    #[default]
    Seconds,
    Stops,
    Equivalent,
    // `none` on the wire; `NoSplit` in Rust so it never reads as `Option::None`.
    #[serde(rename = "none")]
    NoSplit,
}

/// The floor `resolve_config` clamps [`GroupingConfig::stops_width`] to
/// (plan 6b-2 C2). Below it two `stops`/`equivalent` class anchors could sit
/// so close that their tokens print the same (`groups::class_token_num` is
/// collision-safe only for anchors more than `2^MIN_STOPS_WIDTH` apart), and
/// a non-positive width would make a class exclude its own anchor.
pub const MIN_STOPS_WIDTH: f64 = 0.1;

/// spec §9.2 `grouping:` (owner decision 2026-09-10: groups are
/// camera-agnostic — colour mode, filter, binning and exposure form the
/// key; exposure ALWAYS splits a group now, so there is no toggle for it
/// any more). Deliberately does NOT reject an unknown field — no type in
/// this module opts into `#[serde(deny_unknown_fields)]` — so a per-set or
/// global config JSON stored by an M1 build (which still carries
/// `"splitByExposure": …`) decodes fine: serde silently drops a field this
/// struct no longer declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig {
    /// Clamped to a floor of [`MIN_EXPOSURE_TOLERANCE_SEC`] by
    /// [`resolve_config`] — never trusted raw from a stored document.
    pub exposure_tolerance_sec: f64,
    // How exposure classes are formed (spec 2026-10-02 §8). Enters no stage
    // hash: `calibration_subtree` writes a constant tolerance.
    pub exposure_mode: ExposureMode,
    // The class width of `stops` / `equivalent`, in stops. Clamped to
    // `MIN_STOPS_WIDTH` by `resolve_config`.
    pub stops_width: f64,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        GroupingConfig {
            exposure_tolerance_sec: 2.0,
            exposure_mode: ExposureMode::Seconds,
            stops_width: 1.0,
        }
    }
}

/// The smallest `measurement.fitStars` ([`resolve_config`] clamps up to `max(MIN_FIT_STARS, registration.maxStars)`):
/// mono Register takes Measure's fits whenever they exist and never falls back to detection, so a K under its
/// budget would starve it (FM-3).
pub const MIN_FIT_STARS: u32 = 1_000;

/// spec §9.2 `reference:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ReferenceConfig {
    pub mode: ReferenceMode,
    /// The two-pass registration reference (spec §4.4, ruling R-M4a-5):
    /// with `mode = Auto`, stage 5 registers the reference's OWN group once
    /// without persisting anything, then re-picks the reference among the
    /// top-weighted frames closest to that group's median transform before
    /// the real, persisting pass runs. Ignored in `Manual` mode — a pinned
    /// reference never moves. Default ON: a run whose reference happens to
    /// be the one frame the mount was nudged on rotates every master and
    /// loses its corners, and the dry pass costs one extra registration of
    /// one group.
    ///
    /// `#[serde(default = "default_two_pass")]` (ruling R-M4a-9): a config
    /// stored before M4a has no such field and must decode to `true`, not
    /// to `false`.
    #[serde(default = "default_two_pass")]
    pub two_pass: bool,
}

/// Serde default for [`ReferenceConfig::two_pass`] — see its doc comment.
fn default_two_pass() -> bool {
    true
}

impl Default for ReferenceConfig {
    fn default() -> Self {
        ReferenceConfig {
            mode: ReferenceMode::default(),
            two_pass: default_two_pass(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum ReferenceMode {
    /// The best-weighted frame in the whole set (spec §4.4).
    #[default]
    Auto,
    Manual,
}

/// spec §9.2 `output:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputConfig {
    pub format: OutputFormat,
    pub cleanup: CleanupPolicy,
    // Write the per-pixel frame count beside each master (spec §5.7): off for
    // a frame set, seeded on for a project. Enters no stage hash.
    pub coverage_map: bool,
    // Crop every output to the rectangle at least this many frames cover
    // (spec §5.9); 0 = no crop. Enters no stage hash.
    pub crop_to_coverage: u32,
}

/// The container the master, the drizzled master and the drizzle weight map
/// are written in (spec §6.4/§9.5). The rejection maps stay FITS either way
/// — they are diagnostic artifacts, not the product (ruling R-M4d-3).
///
/// One enum for every writer in the app (2026-09-18 plan, ruling R1) — moved
/// to `fits_writer::OutputFormat` and re-exported here so this path, the
/// `ts_export` registry line and the generated `stacking.ts` type are
/// unchanged.
pub use crate::fits_writer::OutputFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum CleanupPolicy {
    #[default]
    KeepAll,
    DeleteRegistered,
    DeleteIntermediates,
}

/// spec §9.2 `paths:`. `None` = the global default (`stacking.working_dir`
/// / `stacking.output_dir`), resolved by the run orchestration, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct PathsConfig {
    pub working_dir: Option<String>,
    pub output_dir: Option<String>,
}

/// Resolves a [`StackingPreset`] to its `StackingConfig`.
///
/// `MaximumQuality` (M3 Task 5, spec §9.2): now turns on everything the spec
/// text always described — `distortion`, `writeRejectionMaps`, LOCAL
/// NORMALIZATION and 2x DRIZZLE. M1/M2 kept the latter two off here because
/// `integrate_group` refused `RejectionNormalization::Local` and the
/// drizzle stage did not exist yet; both landed (M2's acceptance run, this
/// plan's own Tasks 1-4) and pass with the SAME preset config, so the
/// preset no longer needs to hide them.
///
/// `FastPreview` (M4a Task 4, ruling R-M4a-18) turns the two-pass reference
/// pick OFF: the dry pass is a whole extra registration of the reference's
/// group, and a preset whose entire promise is "fast" must not pay for a
/// refinement a preview does not need. For the same reason it pins the
/// rejection normalization to `ScaleZeroOffset` (owner ruling 2026-11-01,
/// OD-3): the shipped default is `Local` now, whose Normalize stage costs
/// about as much as the integration itself, and a preview has no use for it.
/// `Default` and `MaximumQuality` inherit `Local`.
pub fn preset(p: StackingPreset) -> StackingConfig {
    // The algorithm sections are the kernel's (`algorithm_preset`); the app's own sections keep their defaults,
    // except where a preset says otherwise below.
    let algorithm = algorithm_preset(p);
    let mut c = StackingConfig {
        calibration: algorithm.calibration,
        measurement: algorithm.measurement,
        selection: algorithm.selection,
        registration: algorithm.registration,
        normalization: algorithm.normalization,
        integration: algorithm.integration,
        drizzle: algorithm.drizzle,
        ..StackingConfig::default()
    };
    if p == StackingPreset::FastPreview {
        c.reference.two_pass = false;
        c.output.cleanup = CleanupPolicy::DeleteIntermediates;
    }
    c
}

/// Whole-config precedence (spec §9.2): a stored per-set config, when
/// present, IS the run's config — any field it omits falls back to
/// `StackingConfig`'s own default, never to the global default's value for
/// that field. There is no field-level merge between the two documents.
/// Only when no set override exists does the global default JSON apply the
/// same way; with neither, the built-in default.
///
/// A decoded document whose `version` differs from
/// [`STACKING_CONFIG_VERSION`] still decodes fine (every field defaults
/// through `#[serde(default)]`), but the drift is worth a `warn!` rather
/// than silently reusing an old/foreign shape's semantics — the in-memory
/// `StackingConfig` always carries the current version afterward.
pub fn resolve_config(
    set_json: Option<&str>,
    global_json: Option<&str>,
) -> Result<StackingConfig, serde_json::Error> {
    let mut config: StackingConfig = if let Some(set) = set_json {
        serde_json::from_str(set)?
    } else if let Some(global) = global_json {
        serde_json::from_str(global)?
    } else {
        StackingConfig::default()
    };
    if config.version != STACKING_CONFIG_VERSION {
        warn!(
            version = config.version,
            expected = STACKING_CONFIG_VERSION,
            "stacking config version differs; decoding with the current defaults"
        );
        config.version = STACKING_CONFIG_VERSION;
    }
    // Fix round 1, minor 5: `!(x >= MIN)` rather than `x < MIN` so a NaN
    // (still possible through a hand-edited/foreign JSON document — floats
    // decode from any JSON number) clamps too; `x < MIN` would leave NaN
    // unclamped, since every comparison against NaN is false.
    if !(config.grouping.exposure_tolerance_sec >= MIN_EXPOSURE_TOLERANCE_SEC) {
        warn!(
            value = config.grouping.exposure_tolerance_sec,
            min = MIN_EXPOSURE_TOLERANCE_SEC,
            "stacking config: exposureToleranceSec below the minimum; clamped"
        );
        config.grouping.exposure_tolerance_sec = MIN_EXPOSURE_TOLERANCE_SEC;
    }
    if !(config.grouping.stops_width >= MIN_STOPS_WIDTH) {
        warn!(
            value = config.grouping.stops_width,
            min = MIN_STOPS_WIDTH,
            "stacking config: stopsWidth below the minimum; clamped"
        );
        config.grouping.stops_width = MIN_STOPS_WIDTH;
    }
    config.registration.tps_smoothing = clamp_tps_smoothing(config.registration.tps_smoothing);
    // M4c Task 3: the same backend guard for the large-scale filter's two
    // integers. Out of range these are not preferences but a broken filter
    // — the window doubles with every layer, so a hand-edited
    // `protectedLayers: 40` would ask for a 2^41-wide window (and shift a
    // `usize` past its width), and a large `growth` grows every structure
    // by a disc of that radius. The UI's own fields already offer exactly
    // these ranges.
    if !(MIN_PROTECTED_LAYERS..=MAX_PROTECTED_LAYERS)
        .contains(&config.integration.large_scale.protected_layers)
    {
        let clamped = config
            .integration
            .large_scale
            .protected_layers
            .clamp(MIN_PROTECTED_LAYERS, MAX_PROTECTED_LAYERS);
        warn!(
            value = config.integration.large_scale.protected_layers,
            clamped, "stacking config: protectedLayers out of range; clamped"
        );
        config.integration.large_scale.protected_layers = clamped;
    }
    if config.integration.large_scale.growth > MAX_GROWTH {
        warn!(
            value = config.integration.large_scale.growth,
            clamped = MAX_GROWTH,
            "stacking config: growth above the maximum; clamped"
        );
        config.integration.large_scale.growth = MAX_GROWTH;
    }
    resolve_ln_fields(&mut config);
    Ok(config)
}

/// L-9 and the spec §6 clamps for the LN fields (and the two seed clamps
/// `detectionSigma` / `seedThreshold`), on any config that came from
/// storage or from a caller (resolve_config, a plan override, a decoded user
/// preset), and the `measurement.fitStars` clamp ([`resolve_fit_stars`]). Idempotent.
pub(crate) fn resolve_ln_fields(config: &mut StackingConfig) {
    // The two seed clamps ride the same shared resolver, so a plan override or
    // a decoded preset cannot carry an out-of-range value past resolve_config.
    config.measurement.detection_sigma = clamp_detection_sigma(config.measurement.detection_sigma);
    config.measurement.seed_threshold = clamp_seed_threshold(config.measurement.seed_threshold);
    resolve_fit_stars(config);
    use crate::stacking::integrate::LnScaleMode;
    use crate::stacking::ln::{effective_scale_mode, snap_ln_scale, MAX_GRID_SIZE, MIN_GRID_SIZE};
    let local = &mut config.normalization.local;
    if local.scale_mode.is_none() {
        let mode = effective_scale_mode(local);
        tracing::debug!(
            scale = local.scale,
            ln_scale_mode = mode.as_str(),
            "stacking config: legacy LN scale resolved"
        );
        local.scale_mode = Some(mode);
    }
    if local.scale_mode == Some(LnScaleMode::Fixed) {
        let snapped = snap_ln_scale(local.scale as f64);
        if snapped != local.scale {
            warn!(
                value = local.scale,
                clamped = snapped,
                "stacking config: LN scale is not a supported size; snapped"
            );
            local.scale = snapped;
        }
    }
    if !(MIN_GRID_SIZE..=MAX_GRID_SIZE).contains(&local.grid_size) {
        let clamped = local.grid_size.clamp(MIN_GRID_SIZE, MAX_GRID_SIZE);
        warn!(
            value = local.grid_size,
            clamped, "stacking config: LN gridSize out of range; clamped"
        );
        local.grid_size = clamped;
    }
    let sel = &mut local.star_selection;
    // `!(x >= MIN)` so a NaN clamps to the minimum too.
    if !(sel.min_peak_snr >= 5.0) || sel.min_peak_snr > 200.0 {
        let clamped = if sel.min_peak_snr > 200.0 { 200.0 } else { 5.0 };
        warn!(
            value = sel.min_peak_snr,
            clamped, "stacking config: LN starSelection.minPeakSnr out of range; clamped"
        );
        sel.min_peak_snr = clamped;
    }
    if !(20..=5000).contains(&sel.max_stars) {
        let clamped = sel.max_stars.clamp(20, 5000);
        warn!(
            value = sel.max_stars,
            clamped, "stacking config: LN starSelection.maxStars out of range; clamped"
        );
        sel.max_stars = clamped;
    }
    if !(sel.saturation >= 0.5) || sel.saturation > 1.0 {
        let clamped = if sel.saturation > 1.0 { 1.0 } else { 0.5 };
        warn!(
            value = sel.saturation,
            clamped, "stacking config: LN starSelection.saturation out of range; clamped"
        );
        sel.saturation = clamped;
    }
}

/// `measurement.fitStars` (FM-3): a K under the floor `max(`[`MIN_FIT_STARS`]`, registration.maxStars)` is clamped up
/// to it with a `warn!`; a K at or above `measurement.maxStars` is All and is stored as `None` (equivalent configs
/// share hashes); a `selection.minStars` above the effective K warns, since selection reads `stars_fitted` and every
/// frame would be excluded. Idempotent. The probe passes its K straight to the module and never comes through here.
fn resolve_fit_stars(config: &mut StackingConfig) {
    let Some(k) = config.measurement.fit_stars else {
        return;
    };
    let floor = MIN_FIT_STARS.max(u32::try_from(config.registration.max_stars).unwrap_or(u32::MAX));
    let resolves_to_all = |k: u32| k as usize >= config.measurement.max_stars;
    let k = if k < floor {
        if resolves_to_all(floor) {
            warn!(
                value = k,
                clamped = floor,
                "stacking config: fitStars below the floor; resolves to all"
            );
        } else {
            warn!(
                value = k,
                clamped = floor,
                "stacking config: fitStars below the floor; clamped"
            );
        }
        floor
    } else {
        k
    };
    if resolves_to_all(k) {
        config.measurement.fit_stars = None;
        return;
    }
    config.measurement.fit_stars = Some(k);
    if let Some(min) = config.selection.min_stars {
        if min > k as usize {
            warn!(
                value = min,
                count = k,
                "stacking config: selection.minStars above fitStars; every frame would be excluded"
            );
        }
    }
}

/// [`MeasurementConfig::detection_sigma`] into
/// `[MIN_DETECTION_SIGMA, MAX_DETECTION_SIGMA]`, warning when it moves.
///
/// The UI's own field already offers exactly that range; this is the
/// backend's guard for a value that never went through it — a stored
/// document, a hand-edited one, or a caller building the struct directly.
/// Out of range it is not a preference but a broken detector: at `0` or
/// below, every pixel clears the level, and at an infinity none does.
///
/// `!(x >= MIN)` rather than `x < MIN` so a NaN clamps too (every
/// comparison against NaN is false) — the same shape the exposure-tolerance
/// floor uses. JSON cannot express NaN or an infinity (`serde_json` rejects
/// both the literals and out-of-range magnitudes such as `1e309`), so that
/// arm only ever fires for a programmatic caller; it is cheap and it means
/// the value handed to the detector is finite by construction.
fn clamp_detection_sigma(value: f64) -> f64 {
    if !(value >= MIN_DETECTION_SIGMA) {
        warn!(
            detection_sigma = value,
            min = MIN_DETECTION_SIGMA,
            "stacking config: detectionSigma below the minimum; clamped"
        );
        return MIN_DETECTION_SIGMA;
    }
    if value > MAX_DETECTION_SIGMA {
        warn!(
            detection_sigma = value,
            max = MAX_DETECTION_SIGMA,
            "stacking config: detectionSigma above the maximum; clamped"
        );
        return MAX_DETECTION_SIGMA;
    }
    value
}

/// [`MeasurementConfig::seed_threshold`]'s range (ruling R-OSP-2).
pub const MIN_SEED_THRESHOLD: f64 = crate::star_measure::seeds::MIN_THRESHOLD;
pub const MAX_SEED_THRESHOLD: f64 = crate::star_measure::seeds::MAX_THRESHOLD;

/// [`MeasurementConfig::seed_threshold`] into [`MIN_SEED_THRESHOLD`]..=[`MAX_SEED_THRESHOLD`], warning when it
/// moves. `!(x >= MIN)` so a NaN clamps too.
fn clamp_seed_threshold(value: f64) -> f64 {
    if !(value >= MIN_SEED_THRESHOLD) {
        warn!(
            seed_threshold = value,
            min = MIN_SEED_THRESHOLD,
            "stacking config: seedThreshold below the minimum; clamped"
        );
        return MIN_SEED_THRESHOLD;
    }
    if value > MAX_SEED_THRESHOLD {
        warn!(
            seed_threshold = value,
            max = MAX_SEED_THRESHOLD,
            "stacking config: seedThreshold above the maximum; clamped"
        );
        return MAX_SEED_THRESHOLD;
    }
    value
}

/// [`MIN_TPS_SMOOTHING`]..=[`MAX_TPS_SMOOTHING`], with the one-time
/// `warn!` the other clamps use. `!(value >= MIN)` rather than
/// `value < MIN` so a NaN clamps too — every comparison against NaN is
/// false, and a NaN λ would sink every spline in the run.
fn clamp_tps_smoothing(value: f64) -> f64 {
    if !(value >= MIN_TPS_SMOOTHING) {
        warn!(
            value,
            min = MIN_TPS_SMOOTHING,
            "stacking config: tpsSmoothing below the minimum; clamped"
        );
        return MIN_TPS_SMOOTHING;
    }
    if value > MAX_TPS_SMOOTHING {
        warn!(
            value,
            max = MAX_TPS_SMOOTHING,
            "stacking config: tpsSmoothing above the maximum; clamped"
        );
        return MAX_TPS_SMOOTHING;
    }
    value
}

/// xxh3 of the canonical JSON of the whole resolved config — a run-level
/// fingerprint, distinct from the per-stage [`stage_hash`] below. Goes
/// through `serde_json::to_value` before stringifying (the same
/// sorted-object-keys canonicalization `stage_hash` relies on — this
/// workspace's `serde_json` has no `preserve_order` feature), so a field
/// reorder inside any config type never changes the fingerprint.
///
/// Carries [`DRIZZLE_KERNEL_VERSION`] beside the config (see that
/// constant's doc for why it belongs here and in no stage hash).
pub fn config_hash(cfg: &StackingConfig) -> String {
    let value = serde_json::to_value(cfg).expect("StackingConfig always serializes");
    let payload = serde_json::json!({
        "config": value,
        "drizzleKernelVersion": DRIZZLE_KERNEL_VERSION,
    });
    let json = serde_json::to_string(&payload).expect("a serde_json::Value always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}

/// Spec §9.3: `config_hash = xxh3(canonical JSON of the stage's config
/// subtree + the upstream hashes it depends on + the source file
/// identities)`. `config_subtree` is one of [`calibration_subtree`],
/// [`measurement_subtree`], [`registration_subtree`] (or a future stage's
/// equivalent) — a `serde_json::Value` map. Canonical means every object's
/// keys sorted, at every level: this workspace's `serde_json` (no
/// `preserve_order` feature) always serializes a map with sorted keys, and
/// the envelope this function builds around `config_subtree` is a
/// `serde_json::Value` too (`serde_json::json!`, not a
/// `#[derive(Serialize)]` struct, which would serialize in field-declaration
/// order instead).
///
/// **Order-insensitive on both inputs.** `upstream` is sorted lexically and
/// `sources` by `(file_id, size, third field)` before hashing, so a caller
/// that assembles either from an unordered source (a `HashMap`, a query
/// with no `ORDER BY`) still gets the same hash every run — an
/// order-sensitive hash would make every artifact look stale and get
/// silently recomputed. Duplicate entries are kept, not deduped: a
/// duplicate is the caller's bug, and a stable hash for it is still the
/// right answer.
///
/// A source is named by its content, never by a modification time (FI-1): a catalog light by its
/// file id, size and sampled hash ([`SourceIdentity::Sampled`]); a prepared project frame keeps its
/// old shape ([`SourceIdentity::Project`]: the frame's own byte size and no time), so a project's
/// caches do not move.
#[derive(Serialize)]
#[serde(untagged)]
pub enum SourceIdentity {
    /// A catalog light: its `files.id`, size and the sample (`"zip:<token>"` for a zip entry).
    Sampled {
        file_id: i64,
        size: i64,
        sample: String,
    },
    /// A prepared project frame: no catalog identity (`file_id` 0, empty time), its byte size.
    Project {
        file_id: i64,
        size: i64,
        modified_at: String,
    },
}

impl SourceIdentity {
    fn file_id(&self) -> i64 {
        match self {
            Self::Sampled { file_id, .. } | Self::Project { file_id, .. } => *file_id,
        }
    }

    fn size(&self) -> i64 {
        match self {
            Self::Sampled { size, .. } | Self::Project { size, .. } => *size,
        }
    }

    /// The field that follows the size in the sort: the sample, or the project's (empty) time.
    fn third(&self) -> &str {
        match self {
            Self::Sampled { sample, .. } => sample,
            Self::Project { modified_at, .. } => modified_at,
        }
    }
}

pub fn stage_hash(
    config_subtree: &serde_json::Value,
    upstream: &[&str],
    sources: &[SourceIdentity],
) -> String {
    let mut sorted_upstream: Vec<&str> = upstream.to_vec();
    sorted_upstream.sort_unstable();

    let mut sorted_sources: Vec<&SourceIdentity> = sources.iter().collect();
    sorted_sources.sort_by(|a, b| {
        a.file_id()
            .cmp(&b.file_id())
            .then(a.size().cmp(&b.size()))
            .then_with(|| a.third().cmp(b.third()))
    });

    let payload = serde_json::json!({
        "config": config_subtree,
        "upstream": sorted_upstream,
        "sources": sorted_sources,
    });
    let json = serde_json::to_string(&payload).expect("stage hash payload always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}

/// Bumped when Prepare's output for an unchanged input changes — the
/// in-place rule, a conversion's value domain, the debayer (plan 6b-2 C12).
pub const PREPARE_VERSION: u32 = 1;

/// Stage 1's subtree for a PREPARED frame (spec §5.1, plan 6b-2 C12): of
/// the calibration options only `debayerOsc` applies (a prepared frame is
/// never calibrated), plus the debayer method — VNG, the only one — and
/// [`PREPARE_VERSION`]. Deliberately not `calibration_subtree`, whose
/// golden literals a frame set's caches rest on.
pub fn prepare_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({
        "debayerOsc": cfg.calibration.debayer_osc,
        "debayerMethod": "VNG",
        "prepareVersion": PREPARE_VERSION,
    })
}

/// Stage 1 (calibration) config subtree (spec §9.3): the kernel's [`starloom::stacking::config::calibration_subtree`]
/// over this config's algorithm sections.
pub fn calibration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    starloom::stacking::config::calibration_subtree(&cfg.algorithm())
}

/// Stage 3 (measurement) config subtree: the kernel's [`starloom::stacking::config::measurement_subtree`] (the
/// measurement settings, the scale estimator, [`MEASURE_VERSION`] and the `.athf` format version).
pub fn measurement_subtree(cfg: &StackingConfig) -> serde_json::Value {
    starloom::stacking::config::measurement_subtree(&cfg.algorithm())
}

/// [`measurement_subtree`] with the `.athf` format version supplied (Plan D D2c r1); nothing else should call this
/// with anything but [`FITS_ARTIFACT_VERSION`].
#[cfg(test)]
pub(crate) fn measurement_subtree_with_fits_artifact_version(
    cfg: &StackingConfig,
    fits_artifact_version: u16,
) -> serde_json::Value {
    starloom::stacking::config::measurement_subtree_with_fits_artifact_version(
        &cfg.algorithm(),
        fits_artifact_version,
    )
}

// Folds starloom's `MEASURE_STAGE_VERSION` into a subtree, only when it is not 1 (starloom phase 1, P1-R1).
#[cfg(test)]
pub(crate) use starloom::stacking::config::fold_measure_stage_version;

/// [`measurement_subtree`] with the measure version supplied, so a test can prove the hash actually follows it.
#[cfg(test)]
fn measurement_subtree_with_measure_version(cfg: &StackingConfig, measure_version: u32) -> serde_json::Value {
    starloom::stacking::config::measurement_subtree_with_measure_version(&cfg.algorithm(), measure_version)
}

/// Stage 5 (registration) config subtree: the kernel's [`starloom::stacking::config::registration_subtree`]
/// (the registration settings, [`MEASURE_VERSION`], `alignVersion`).
pub fn registration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    starloom::stacking::config::registration_subtree(&cfg.algorithm())
}

/// [`registration_subtree`] with both versions supplied, so a test can prove the hash follows each.
#[cfg(test)]
fn registration_subtree_with_versions(
    cfg: &StackingConfig,
    measure_version: u32,
    align_version: u32,
) -> serde_json::Value {
    starloom::stacking::config::registration_subtree_with_versions(&cfg.algorithm(), measure_version, align_version)
}

/// [`registration_subtree`] with the measure version supplied and no `alignVersion`.
#[cfg(test)]
fn registration_subtree_with_measure_version(cfg: &StackingConfig, measure_version: u32) -> serde_json::Value {
    starloom::stacking::config::registration_subtree_with_measure_version(&cfg.algorithm(), measure_version)
}

/// Stage 6 (local normalization) config subtree: the kernel's [`starloom::stacking::config::normalization_subtree`]
/// (the whole `normalization` block, `measurement.psfModel` and `maxStars`, [`MEASURE_VERSION`], `lnModelVersion`).
pub fn normalization_subtree(cfg: &StackingConfig) -> serde_json::Value {
    starloom::stacking::config::normalization_subtree(&cfg.algorithm())
}

/// [`normalization_subtree`] with the measure version supplied.
#[cfg(test)]
fn normalization_subtree_with_measure_version(cfg: &StackingConfig, measure_version: u32) -> serde_json::Value {
    starloom::stacking::config::normalization_subtree_with_measure_version(&cfg.algorithm(), measure_version)
}

/// The document a project's first plan stores (spec §9.4): the global
/// default with `grouping.exposureMode = stops` and `output.coverageMap = true`.
pub fn project_seed_config(global: StackingConfig) -> StackingConfig {
    let mut config = global;
    config.grouping.exposure_mode = ExposureMode::Stops;
    config.output.coverage_map = true;
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::stats::{RejectionNormalization, ScaleEstimator};
    use crate::stacking::prefilter::SeedPrefilter;
    use crate::stacking::structure::SeedDetector;
    use crate::stacking::weights::WeightMode;

    #[test]
    fn a_legacy_document_reads_scale_1024_as_auto_and_any_other_as_fixed() {
        use crate::stacking::integrate::LnScaleMode;
        let read = |local: &str| {
            resolve_config(
                Some(&format!(r#"{{"normalization":{{"local":{local}}}}}"#)),
                None,
            )
            .unwrap()
            .normalization
            .local
        };
        let a = read(r#"{"enabled":true,"scale":1024}"#);
        assert_eq!((a.scale_mode, a.scale), (Some(LnScaleMode::Auto), 1024));
        let b = read(r#"{"enabled":true,"scale":2048}"#);
        assert_eq!((b.scale_mode, b.scale), (Some(LnScaleMode::Fixed), 2048));
        let c = read(r#"{"enabled":true,"scale":1300}"#);
        assert_eq!(
            (c.scale_mode, c.scale),
            (Some(LnScaleMode::Fixed), 1536),
            "fixed is snapped"
        );
        let d = read(r#"{"enabled":true,"scale":2048,"scaleMode":"auto"}"#);
        assert_eq!(
            d.scale_mode,
            Some(LnScaleMode::Auto),
            "a stored mode is never overridden"
        );
        assert_eq!(read(r#"{"gridSize":1}"#).grid_size, 2);
        assert_eq!(read(r#"{"gridSize":9}"#).grid_size, 8);
    }

    #[test]
    fn a_stored_document_without_scale_mode_decodes_to_none_before_resolution() {
        let c: StackingConfig =
            serde_json::from_str(r#"{"normalization":{"local":{"scale":2048}}}"#).unwrap();
        assert_eq!(c.normalization.local.scale_mode, None);
        assert_eq!(
            StackingConfig::default().normalization.local.scale_mode,
            Some(crate::stacking::integrate::LnScaleMode::Auto)
        );
    }

    /// L-19: scale, scaleMode and gridSize leave the subtree; the resolved
    /// scale reaches the hashes as a token instead.
    #[test]
    fn auto_and_fixed_at_the_same_resolved_scale_share_the_normalization_subtree() {
        use crate::stacking::integrate::LnScaleMode;
        let mut auto = StackingConfig::default();
        auto.normalization.local.enabled = true;
        let mut fixed = auto.clone();
        fixed.normalization.local.scale_mode = Some(LnScaleMode::Fixed);
        fixed.normalization.local.scale = 1536;
        fixed.normalization.local.grid_size = 6;
        assert_eq!(normalization_subtree(&auto), normalization_subtree(&fixed));
        let local = &normalization_subtree(&auto)["normalization"]["local"];
        for key in ["scale", "scaleMode", "gridSize"] {
            assert!(
                local.get(key).is_none(),
                "{key} is still in the subtree: {local}"
            );
        }
    }

    #[test]
    fn the_ln_model_version_rides_the_normalization_subtree_only() {
        let cfg = StackingConfig::default();
        assert_eq!(
            normalization_subtree(&cfg)["lnModelVersion"],
            serde_json::json!(crate::stacking::ln::LN_MODEL_VERSION)
        );
        for other in [measurement_subtree(&cfg), registration_subtree(&cfg)] {
            assert!(other.get("lnModelVersion").is_none(), "{other}");
        }
    }

    /// Robust quad seed RQ-5: what `align` returns for an unchanged star list is not expressible in the config, so
    /// [`ALIGN_VERSION`] rides the registration subtree beside `measureVersion` (LN_MODEL_VERSION's precedent) and no
    /// other stage's own subtree.
    #[test]
    fn the_align_version_rides_the_registration_subtree_only() {
        let cfg = StackingConfig::default();
        assert_eq!(
            registration_subtree(&cfg)["alignVersion"],
            serde_json::json!(ALIGN_VERSION)
        );
        for other in [measurement_subtree(&cfg), normalization_subtree(&cfg)] {
            assert!(other.get("alignVersion").is_none(), "{other}");
        }
    }

    /// Robust quad seed RQ-5: another `align` version is another registration hash (so a cached row misses by key and
    /// the set re-registers once) and it moves no measurement hash (Calibrate and Measure are not recomputed).
    #[test]
    fn a_new_align_version_moves_the_registration_hash_and_not_the_measurement_hash() {
        let cfg = StackingConfig::default();
        let shipped = stage_hash(&registration_subtree(&cfg), &[], &[]);
        assert_eq!(
            shipped,
            stage_hash(
                &registration_subtree_with_versions(&cfg, MEASURE_VERSION, ALIGN_VERSION),
                &[],
                &[]
            ),
            "the shipped helper is the versioned one at the current constants"
        );
        assert_ne!(
            stage_hash(
                &registration_subtree_with_versions(&cfg, MEASURE_VERSION, 0),
                &[],
                &[]
            ),
            shipped,
            "another align version is another registration hash"
        );
        // The measurement subtree takes no align version: the same default
        // config hashes the same as at the unmoved version-8 literal.
        // 2026-11-08 frame-level cache Task 11 (spec §5.8, owner ruling; no truth bar run): measurement.maxStars default 24576 -> 9000 moves it (was "ef32610c8173c47f").
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            "1a598ed663844a86"
        );
    }

    /// Plan 6b-2 C2: the four modes' wire names are the spec's.
    #[test]
    fn exposure_modes_use_the_spec_names() {
        for (mode, name) in [
            (ExposureMode::Seconds, "\"seconds\""),
            (ExposureMode::Stops, "\"stops\""),
            (ExposureMode::Equivalent, "\"equivalent\""),
            (ExposureMode::NoSplit, "\"none\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), name);
            assert_eq!(serde_json::from_str::<ExposureMode>(name).unwrap(), mode);
        }
    }

    /// Plan 6b-2 C2: a document written before the fields existed decodes to
    /// their defaults — a frame set's stored config regroups nothing.
    #[test]
    fn a_document_without_the_wave6_fields_decodes_to_their_defaults() {
        let cfg = resolve_config(
            Some(r#"{"grouping":{"exposureToleranceSec":3.0},"output":{"format":"fits"}}"#),
            None,
        )
        .unwrap();
        assert_eq!(cfg.grouping.exposure_tolerance_sec, 3.0);
        assert_eq!(cfg.grouping.exposure_mode, ExposureMode::Seconds);
        assert_eq!(cfg.grouping.stops_width, 1.0);
        assert!(!cfg.output.coverage_map);
        assert_eq!(cfg.output.crop_to_coverage, 0);
    }

    /// Plan 6b-2 C2/C3: `stopsWidth` at or below zero would make a class
    /// exclude its own anchor; it is clamped to `MIN_STOPS_WIDTH`.
    #[test]
    fn stops_width_is_clamped_to_its_floor() {
        for raw in ["0", "-1", "0.05"] {
            let json = format!(r#"{{"grouping":{{"stopsWidth":{raw}}}}}"#);
            let cfg = resolve_config(Some(&json), None).unwrap();
            assert_eq!(cfg.grouping.stops_width, MIN_STOPS_WIDTH, "{raw}");
        }
        let cfg = resolve_config(Some(r#"{"grouping":{"stopsWidth":2.0}}"#), None).unwrap();
        assert_eq!(cfg.grouping.stops_width, 2.0);
    }

    #[test]
    fn empty_json_is_the_default_config() {
        let c: StackingConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, StackingConfig::default());
        assert_eq!(c.version, 1);
        assert_eq!(c.measurement.max_stars, 9000);
        assert_eq!(c.measurement.keyword, "SSWEIGHT");
        assert!(c.selection.exclude_on_registration_failure);
        assert_eq!(c.registration.max_stars, 2000);
        assert!((c.registration.clamping_threshold - 0.30).abs() < 1e-9);
        assert!(!c.normalization.local.enabled);
        assert_eq!(c.normalization.local.scale, 1024);
        assert!((c.integration.min_weight - 0.005).abs() < 1e-12);
        assert!(!c.drizzle.enabled);
        assert_eq!(c.output.cleanup, CleanupPolicy::KeepAll);
        assert!(c.paths.working_dir.is_none());
        assert!(c.reference.two_pass);
    }

    /// Ruling R-M4a-9: a `reference` block stored before M4a carries only
    /// `mode`, and it must decode to the shipped default (`twoPass: true`)
    /// rather than to `false` — the whole point of `#[serde(default =
    /// "default_two_pass")]`. The wire name is `twoPass`.
    #[test]
    fn a_pre_m4a_reference_block_decodes_with_two_pass_on() {
        let c: StackingConfig = serde_json::from_str(r#"{"reference":{"mode":"manual"}}"#).unwrap();
        assert_eq!(c.reference.mode, ReferenceMode::Manual);
        assert!(c.reference.two_pass);

        let off: StackingConfig =
            serde_json::from_str(r#"{"reference":{"mode":"auto","twoPass":false}}"#).unwrap();
        assert!(!off.reference.two_pass);

        let json = serde_json::to_value(StackingConfig::default()).unwrap();
        assert_eq!(json["reference"]["twoPass"], serde_json::json!(true));
    }

    /// `reference.twoPass` changes the RUN fingerprint (a different run,
    /// recorded as such) but no STAGE hash: nothing about a stored
    /// calibrated/measured/registered artifact depends on how the
    /// reference was chosen, only on WHICH frame it is — and that already
    /// rides `registration_hash_for`'s own `reference_frame_id` argument.
    #[test]
    fn two_pass_moves_the_config_hash_but_no_stage_hash() {
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.reference.two_pass = !cfg.reference.two_pass;
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_eq!(
            stage_hash(&registration_subtree(&cfg), &[], &[]),
            stage_hash(&registration_subtree(&other), &[], &[]),
            "the registration stage hash must not follow reference.twoPass"
        );
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            stage_hash(&calibration_subtree(&other), &[], &[]),
        );
    }

    /// Spec 2026-11-08 §5.4: the weights are computed every run from stored
    /// measurements and no fit reads the weight fields, so changing them
    /// must not re-measure; a field a fit does read still moves the hash.
    #[test]
    fn measurement_hash_ignores_the_weight_settings() {
        let default = StackingConfig::default();
        let h = |c: &StackingConfig| stage_hash(&measurement_subtree(c), &[], &[]);
        let base = h(&default);

        let mut noise = default.clone();
        noise.measurement.weight_mode = WeightMode::Noise;
        assert_eq!(h(&noise), base, "weightMode Noise must not move it");

        let mut formula = default.clone();
        formula.measurement.weight_mode = WeightMode::Formula;
        formula.measurement.formula.fwhm = 40.0;
        assert_eq!(h(&formula), base, "weightMode Formula and formula.fwhm must not move it");

        let mut keyword = default.clone();
        keyword.measurement.weight_mode = WeightMode::Keyword;
        keyword.measurement.keyword = "WEIGHT".to_string();
        assert_eq!(h(&keyword), base, "weightMode Keyword and its keyword must not move it");

        let mut max_stars = default.clone();
        max_stars.measurement.max_stars += 1;
        assert_ne!(h(&max_stars), base, "maxStars still moves it");

        let mut seed = default.clone();
        seed.measurement.seed_threshold += 1.0;
        assert_ne!(h(&seed), base, "seedThreshold still moves it");
    }

    /// Spec 2026-11-08 §5.2: the grouping tolerance decides which frames share
    /// a group, never what a calibrated frame contains; a set at another
    /// tolerance or exposure mode keeps its calibration hash.
    #[test]
    fn calibration_hash_ignores_the_grouping_tolerance() {
        let default = StackingConfig::default();
        let mut wide = default.clone();
        wide.grouping.exposure_tolerance_sec = 200.0;
        let mut stops = default.clone();
        stops.grouping.exposure_mode = ExposureMode::Stops;
        let h = |c: &StackingConfig| stage_hash(&calibration_subtree(c), &["m"], &[]);
        assert_eq!(h(&default), h(&wide), "a wider tolerance must not move it");
        assert_eq!(h(&default), h(&stops), "the exposure mode must not move it");
    }

    /// Ruling R-M4c-8's own "the `.athln` config hash folds `localScale`
    /// in (it already hashes the whole `local` block — verify)": flipping
    /// it changes what a `.athln` sidecar CONTAINS (a spatially varying
    /// `A` grid instead of a constant one), so every cached sidecar
    /// written under the other setting must be recomputed. It rides
    /// `cfg.normalization`, which `normalization_subtree` serializes
    /// whole, so this holds by construction — pinned here so a future
    /// hand-written subtree cannot quietly drop it. Nothing upstream of
    /// normalization follows it.
    #[test]
    fn local_scale_moves_the_normalization_stage_hash() {
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.normalization.local.local_scale = !cfg.normalization.local.local_scale;
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_ne!(
            stage_hash(&normalization_subtree(&cfg), &[], &[]),
            stage_hash(&normalization_subtree(&other), &[], &[]),
            "the LN artifact hash must follow normalization.local.localScale"
        );
        assert_eq!(
            stage_hash(&registration_subtree(&cfg), &[], &[]),
            stage_hash(&registration_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            stage_hash(&calibration_subtree(&other), &[], &[]),
        );
    }

    /// M4c rulings R-M4c-5/7: the two new registration fields change what
    /// a stored registration artifact IS — a different distortion surface
    /// under the same linear model, or a linear model a corrector was
    /// composed into — so both must move the registration stage hash and
    /// re-register the set on purpose, exactly as `registration.geometry`
    /// does. They ride `cfg.registration`, so they move the run
    /// fingerprint too, and nothing upstream of registration follows
    /// them.
    #[test]
    fn the_tps_fields_move_the_registration_stage_hash() {
        let cfg = StackingConfig::default();
        for other in [
            {
                let mut c = cfg.clone();
                c.registration.distortion = crate::stacking::register::DistortionChoice::Tps;
                c
            },
            {
                let mut c = cfg.clone();
                c.registration.tps_smoothing = 2.0;
                c
            },
            {
                let mut c = cfg.clone();
                c.registration.local_distortion = true;
                c
            },
        ] {
            assert_ne!(config_hash(&cfg), config_hash(&other));
            assert_ne!(
                stage_hash(&registration_subtree(&cfg), &[], &[]),
                stage_hash(&registration_subtree(&other), &[], &[]),
                "the registration stage hash must follow every registration field"
            );
            assert_eq!(
                stage_hash(&measurement_subtree(&cfg), &[], &[]),
                stage_hash(&measurement_subtree(&other), &[], &[]),
            );
            assert_eq!(
                stage_hash(&calibration_subtree(&cfg), &[], &[]),
                stage_hash(&calibration_subtree(&other), &[], &[]),
            );
        }
    }

    /// M4b ruling R-M4b-4/5: flipping `registration.geometry` changes what
    /// every stored registration artifact MEANS (which reference the frame
    /// was warped onto, and into which geometry), so it must move the
    /// registration stage hash — a set switched to native mode
    /// re-registers instead of reusing co-registered rows. It rides
    /// `cfg.registration`, so it moves the run fingerprint too.
    #[test]
    fn geometry_moves_the_registration_stage_hash() {
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.registration.geometry = crate::stacking::register::RegistrationGeometry::Native;
        assert_ne!(cfg.registration.geometry, other.registration.geometry);
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_ne!(
            stage_hash(&registration_subtree(&cfg), &[], &[]),
            stage_hash(&registration_subtree(&other), &[], &[]),
            "the registration stage hash must follow registration.geometry"
        );
        // Nothing upstream of registration depends on it.
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            stage_hash(&calibration_subtree(&other), &[], &[]),
        );
    }

    /// M4d Task 1 (ruling R-M4d-1/2): `drizzle.bayer` moves the run
    /// fingerprint (it changes what the run produces) but must leave every
    /// PER-STAGE hash alone — above all the calibration one. The debayered
    /// artifact is byte-identical whether or not the mosaic is kept beside
    /// it, so flipping this toggle must not throw away a set's whole
    /// calibrated cache; the run notices the missing `calibrated_mosaic`
    /// artifact instead and regenerates the pair, frame by frame.
    #[test]
    fn bayer_moves_the_run_fingerprint_but_no_stage_hash() {
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.drizzle.bayer = true;
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            stage_hash(&calibration_subtree(&other), &[], &[]),
            "drizzle.bayer must not invalidate a calibrated frame"
        );
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&registration_subtree(&cfg), &[], &[]),
            stage_hash(&registration_subtree(&other), &[], &[]),
        );
        assert_eq!(
            stage_hash(&normalization_subtree(&cfg), &[], &[]),
            stage_hash(&normalization_subtree(&other), &[], &[]),
        );
    }

    #[test]
    fn format_moves_the_run_fingerprint_but_no_stage_hash() {
        // The XISF plan's `calibration.format` (2026-09-18, ruling R6): the
        // run writes FITS intermediates whatever the field says, so a
        // cached calibrated frame stays valid across the field's arrival
        // AND across a user flipping it — only the whole-config fingerprint
        // moves, exactly as `drizzle.bayer` above.
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.calibration.format = crate::fits_writer::OutputFormat::Xisf;
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            stage_hash(&calibration_subtree(&other), &[], &[]),
            "calibration.format must not invalidate a calibrated frame"
        );
        assert!(
            calibration_subtree(&cfg)["calibration"]
                .get("format")
                .is_none(),
            "the subtree must not carry the field at all"
        );
    }

    #[test]
    fn star_selection_is_clamped_to_its_ranges() {
        let read = |sel: &str| {
            resolve_config(
                Some(&format!(
                    r#"{{"normalization":{{"local":{{"starSelection":{sel}}}}}}}"#
                )),
                None,
            )
            .unwrap()
            .normalization
            .local
            .star_selection
        };
        let s = read(r#"{"minPeakSnr":1,"maxStars":5,"saturation":0.2}"#);
        assert_eq!((s.min_peak_snr, s.max_stars, s.saturation), (5.0, 20, 0.5));
        let s = read(r#"{"minPeakSnr":900,"maxStars":90000,"saturation":2}"#);
        assert_eq!(
            (s.min_peak_snr, s.max_stars, s.saturation),
            (200.0, 5000, 1.0)
        );
        assert_eq!(
            read("{}"),
            crate::stacking::integrate::LnStarSelection::default()
        );
    }

    #[test]
    fn the_aperture_is_in_the_subtree_only_under_the_common_aperture() {
        use crate::stacking::integrate::{LnAperture, LnFluxEstimator};
        // 2026-10-06: F1 growth curve is the default (L-24); this test starts from PSF signal.
        let mut a = StackingConfig::default();
        a.normalization.local.flux_estimator = LnFluxEstimator::PsfSignal;
        let mut b = a.clone();
        b.normalization.local.aperture = LnAperture::Fwhm2;
        assert_eq!(
            normalization_subtree(&a),
            normalization_subtree(&b),
            "PSF signal ignores the aperture"
        );
        a.normalization.local.flux_estimator = LnFluxEstimator::CommonAperture;
        b.normalization.local.flux_estimator = LnFluxEstimator::CommonAperture;
        assert_ne!(normalization_subtree(&a), normalization_subtree(&b));
        let mut c = StackingConfig::default();
        c.normalization.local.flux_estimator = LnFluxEstimator::PsfSignal;
        assert_ne!(
            normalization_subtree(&StackingConfig::default()),
            normalization_subtree(&c),
            "the estimator is always in"
        );
    }

    #[test]
    fn serde_names_follow_the_spec() {
        let s = serde_json::to_string(&StackingConfig::default()).unwrap();
        assert!(
            !s.contains("splitByExposure"),
            "the toggle is gone — exposure always splits: {s}"
        );
        // 2026-10-05 LN model fixes Task 11: starSelection + lnModelVersion 3.
        // 2026-10-05 LN model fixes Task 13: fluxEstimator + aperture (aperture only under commonAperture)
        // 2026-10-06: F1 growth curve is the default (L-24).
        for needle in [
            "\"exposureToleranceSec\":2.0",
            "\"exposureMode\":\"seconds\"",
            "\"stopsWidth\":1.0",
            "\"coverageMap\":false",
            "\"cropToCoverage\":0",
            "\"weightMode\":\"psfSignalWeight\"",
            "\"detectionSigma\":20.0",
            "\"seedPrefilter\":\"none\"",
            "\"seedDetector\":\"peak\"",
            "\"psfModel\":\"auto\"",
            "\"minWeightFraction\":0.05",
            "\"excludeOnRegistrationFailure\":true",
            "\"reference\":{\"mode\":\"auto\",\"twoPass\":true}",
            "\"interpolation\":\"bicubicBSpline\"",
            "\"clampingThreshold\":0.3",
            "\"output\":\"additiveWithScaling\"",
            // Owner ruling 2026-11-01 (OD-1a): `local` is the shipped default.
            "\"rejection\":\"local\"",
            "\"scaleEstimator\":\"bwmv\"",
            "\"local\":{\"enabled\":false,\"scale\":1024,\"scaleMode\":\"auto\",\"gridSize\":4,\"referenceFrames\":20,\"psfModel\":\"auto\",\"localScale\":false,\"starSelection\":{\"minPeakSnr\":40.0,\"maxStars\":500,\"saturation\":0.75},\"fluxEstimator\":\"commonAperture\",\"aperture\":\"growthCurve\"}",
            "\"rejection\":{\"method\":\"auto\"}",
            "\"writeRejectionMaps\":false",
            "\"dropShrink\":0.9",
            "\"kernel\":\"square\"",
            "\"format\":\"fits\"",
            "\"cleanup\":\"keepAll\"",
            "\"paths\":{\"workingDir\":null,\"outputDir\":null}",
            "\"combination\":\"average\"",
            "\"minWeight\":0.005",
            "\"rangeLow\":0.0",
            "\"rangeHigh\":null",
            "\"drizzle\":{\"enabled\":false,\"scale\":2,\"dropShrink\":0.9,\"kernel\":\"square\",\"useRejection\":true,\"useWeights\":true,\"useLocalNormalization\":true,\"writeWeightMap\":false,\"bayer\":false}",
        ] {
            assert!(s.contains(needle), "{needle} missing in {s}");
        }
    }

    #[test]
    fn presets() {
        let f = preset(StackingPreset::FastPreview);
        assert_eq!(f.registration.interpolation, Interpolation::Bilinear);
        assert_eq!(
            f.integration.rejection,
            RejectionChoice::SigmaClip {
                sigma_low: 4.0,
                sigma_high: 3.0,
            }
        );
        assert_eq!(f.output.cleanup, CleanupPolicy::DeleteIntermediates);
        // M4a Task 4, ruling R-M4a-18: a "fast" preset does not pay for the
        // two-pass reference's dry registration pass.
        assert!(!f.reference.two_pass);
        // OD-3: Fast preview keeps the global pair (no Normalize stage).
        assert_eq!(
            f.normalization.rejection,
            RejectionNormalization::ScaleZeroOffset
        );
        let m = preset(StackingPreset::MaximumQuality);
        assert_eq!(m.registration.distortion, DistortionChoice::Polynomial3);
        assert!(m.integration.write_rejection_maps);
        // M3 Task 5, brief test (h): MaximumQuality turns on drizzle 2x AND
        // local normalization (spec §9.2) — both hidden in M1/M2 only
        // because neither stage existed yet.
        assert!(m.normalization.local.enabled);
        assert!(m.drizzle.enabled);
        assert_eq!(m.drizzle.scale, 2);
        // …and the quality preset keeps the two-pass pick the default
        // already has: only FastPreview opts out (R-M4a-18).
        assert!(m.reference.two_pass);
        assert_eq!(preset(StackingPreset::Default), StackingConfig::default());
        assert!(preset(StackingPreset::Default).reference.two_pass);
    }

    /// Owner ruling 2026-11-01 (OD-1a, OD-3): the rejection normalization of
    /// Default / Maximum quality / Fast preview resolves to Local / Local /
    /// ScaleZeroOffset, and the shipped default is the Default preset.
    #[test]
    fn the_three_presets_resolve_their_rejection_normalization() {
        assert_eq!(
            preset(StackingPreset::Default).normalization.rejection,
            RejectionNormalization::Local
        );
        assert_eq!(
            preset(StackingPreset::MaximumQuality)
                .normalization
                .rejection,
            RejectionNormalization::Local
        );
        assert_eq!(
            preset(StackingPreset::FastPreview).normalization.rejection,
            RejectionNormalization::ScaleZeroOffset
        );
        assert_eq!(
            StackingConfig::default().normalization.rejection,
            RejectionNormalization::Local
        );
    }

    #[test]
    fn precedence_is_whole_config() {
        let set = Some("{\"measurement\":{\"maxStars\":100}}");
        let global = Some(
            "{\"measurement\":{\"maxStars\":200},\"grouping\":{\"exposureToleranceSec\":9.0}}",
        );
        let c = resolve_config(set, global).unwrap();
        assert_eq!(c.measurement.max_stars, 100);
        assert_eq!(
            c.grouping.exposure_tolerance_sec, 2.0,
            "no field-level merge — the set's own default, not the global's 9.0"
        );
        assert_eq!(
            resolve_config(None, global).unwrap().measurement.max_stars,
            200
        );
        assert_eq!(
            resolve_config(None, global)
                .unwrap()
                .grouping
                .exposure_tolerance_sec,
            9.0
        );
        assert_eq!(
            resolve_config(None, None).unwrap(),
            StackingConfig::default()
        );
        assert!(resolve_config(Some("{not json"), None).is_err());
    }

    /// An M1-stored per-set or global config JSON still carries
    /// `"splitByExposure"` — the toggle it once turned. That field must
    /// still deserialize (silently ignored, `GroupingConfig` no longer
    /// declares it) and never reappear on re-serialization.
    #[test]
    fn legacy_split_by_exposure_field_is_ignored() {
        let json = r#"{"grouping":{"splitByExposure":true,"exposureToleranceSec":5.0}}"#;
        let c: StackingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.grouping.exposure_tolerance_sec, 5.0);
        let out = serde_json::to_string(&c).unwrap();
        assert!(!out.contains("splitByExposure"), "{out}");

        // Same via the real precedence entry point, both roles.
        let via_set = resolve_config(Some(json), None).unwrap();
        assert_eq!(via_set.grouping.exposure_tolerance_sec, 5.0);
        let via_global = resolve_config(None, Some(json)).unwrap();
        assert_eq!(via_global.grouping.exposure_tolerance_sec, 5.0);
    }

    /// Fix round 1, minor 5: zero, negative and sub-floor values all clamp
    /// to [`MIN_EXPOSURE_TOLERANCE_SEC`] — a real value above the floor is
    /// untouched. Below the floor, two distinct exposure clusters could
    /// format to the same `fmt_num` token and collide on
    /// `stacking_run_groups`'s `UNIQUE(run_id, group_key)`.
    #[test]
    fn exposure_tolerance_is_clamped_to_a_floor() {
        let clamp = |v: &str| {
            resolve_config(
                Some(&format!(r#"{{"grouping":{{"exposureToleranceSec":{v}}}}}"#)),
                None,
            )
            .unwrap()
            .grouping
            .exposure_tolerance_sec
        };
        assert_eq!(clamp("0.0"), MIN_EXPOSURE_TOLERANCE_SEC);
        assert_eq!(clamp("-5.0"), MIN_EXPOSURE_TOLERANCE_SEC);
        assert_eq!(clamp("0.005"), MIN_EXPOSURE_TOLERANCE_SEC);
        assert_eq!(
            clamp("1.5"),
            1.5,
            "a legitimate value above the floor is untouched"
        );
    }

    #[test]
    fn resolve_config_normalizes_a_foreign_version() {
        let c = resolve_config(Some("{\"version\":99}"), None).unwrap();
        assert_eq!(c.version, STACKING_CONFIG_VERSION);
        // Every other field still decodes to the current defaults — a
        // foreign version does not otherwise change how the document is
        // read (`#[serde(default)]` already tolerates a missing/renamed
        // field on its own).
        assert_eq!(
            c,
            StackingConfig {
                version: STACKING_CONFIG_VERSION,
                ..StackingConfig::default()
            }
        );
    }

    /// A project frame's source keeps the bytes of the struct it replaced (the hash goes through a
    /// `serde_json::Value`, whose map is sorted), so a project keeps its caches; a catalog light's is
    /// the three fields of its identity.
    #[test]
    fn a_sources_two_shapes_serialize_as_pinned() {
        let project = SourceIdentity::Project {
            file_id: 0,
            size: 4096,
            modified_at: String::new(),
        };
        assert_eq!(
            serde_json::to_string(&serde_json::to_value(&project).unwrap()).unwrap(),
            r#"{"file_id":0,"modified_at":"","size":4096}"#
        );
        let sampled = SourceIdentity::Sampled {
            file_id: 42,
            size: 7,
            sample: "0000000000004242".into(),
        };
        assert_eq!(
            serde_json::to_string(&serde_json::to_value(&sampled).unwrap()).unwrap(),
            r#"{"file_id":42,"sample":"0000000000004242","size":7}"#
        );
    }

    #[test]
    fn stage_hash_is_stable_and_sensitive() {
        let cfg = StackingConfig::default();
        let src = [SourceIdentity::Sampled {
            file_id: 1,
            size: 10,
            sample: "t".into(),
        }];
        let a = stage_hash(&calibration_subtree(&cfg), &["up"], &src);
        assert_eq!(a, stage_hash(&calibration_subtree(&cfg), &["up"], &src));
        assert_eq!(a.len(), 16);
        assert_ne!(a, stage_hash(&calibration_subtree(&cfg), &["other"], &src));
        assert_ne!(
            a,
            stage_hash(
                &calibration_subtree(&cfg),
                &["up"],
                &[SourceIdentity::Sampled {
                    file_id: 1,
                    size: 11,
                    sample: "t".into(),
                }]
            )
        );
        let mut cfg2 = cfg.clone();
        cfg2.calibration.hot_pixel_correction = false;
        assert_ne!(a, stage_hash(&calibration_subtree(&cfg2), &["up"], &src));
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&cfg2), &[], &[]),
            "calibration change does not touch the measurement hash"
        );
        let v: serde_json::Value = serde_json::from_str("{\"b\":1,\"a\":2}").unwrap();
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            "{\"a\":2,\"b\":1}",
            "serde_json orders keys — the canonical form relies on it"
        );
    }

    /// The seed threshold has to be part of stage 3's fingerprint: a run
    /// with a different `detectionSigma` measured a different star
    /// population, so a cached `stacking_artifacts` row from the other
    /// value must not be reused (spec §9.3).
    #[test]
    fn config_hash_changes_when_the_seed_population_rules_change() {
        let cfg = StackingConfig::default();
        let mut other = cfg.clone();
        other.measurement.detection_sigma += 1.0;
        assert_ne!(config_hash(&cfg), config_hash(&other));
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&other), &[], &[]),
            "the measurement stage hash must follow detectionSigma"
        );
        // Same contract for the seed pre-filter: a different detection image
        // is a different star population.
        let mut filtered = cfg.clone();
        filtered.measurement.seed_prefilter = SeedPrefilter::Median3;
        assert_ne!(config_hash(&cfg), config_hash(&filtered));
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&filtered), &[], &[]),
            "the measurement stage hash must follow seedPrefilter"
        );
        // And for the detector itself (M4c Task 0, ruling R-M4c-11) — the
        // strongest form of the same contract: a different detector is a
        // different star population, whatever the other dials say.
        let mut structure = cfg.clone();
        structure.measurement.seed_source = SeedSource::Structure;
        structure.measurement.seed_detector = SeedDetector::Structure;
        assert_ne!(config_hash(&cfg), config_hash(&structure));
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&structure), &[], &[]),
            "the measurement stage hash must follow seedSource"
        );
        assert_eq!(
            structure
                .measurement
                .measure_options(ScaleEstimator::Bwmv)
                .seed_source,
            SeedSource::Structure,
            "and the resolved measure options carry it"
        );
    }

    /// Ruling R-M4a-15 (registration folded in by Tier C Task 3, ruling
    /// C-6; the constant became `star_measure::MEASURE_VERSION` in Plan A
    /// Task 3, ruling R-OSP-1): the measurement path's own behaviour is not
    /// expressible in the config, so the artifact hashes that store
    /// fit-derived numbers fold in [`MEASURE_VERSION`] instead. All three
    /// must: Measure stores PSFSW/TFlux/star counts, local normalization's
    /// `.athln` stores a PSF-flux scale measured with the SAME fitter, and
    /// registration builds a mono frame's star list from the fitter's own
    /// accepted fits. Replaces
    /// `all_three_fit_derived_subtree_hashes_follow_the_psf_fit_version`.
    #[test]
    fn every_fit_reading_stage_hash_follows_measure_version() {
        let cfg = StackingConfig::default();
        for (name, a, b) in [
            (
                "measurement",
                measurement_subtree_with_measure_version(&cfg, MEASURE_VERSION),
                measurement_subtree_with_measure_version(&cfg, MEASURE_VERSION + 1),
            ),
            (
                "registration",
                registration_subtree_with_versions(&cfg, MEASURE_VERSION, ALIGN_VERSION),
                registration_subtree_with_versions(&cfg, MEASURE_VERSION + 1, ALIGN_VERSION),
            ),
            (
                "normalization",
                normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION),
                normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION + 1),
            ),
        ] {
            assert_ne!(stage_hash(&a, &[], &[]), stage_hash(&b, &[], &[]), "{name}");
            assert!(
                a.get("measureVersion").is_some() && a.get("psfFitVersion").is_none(),
                "{name}: one key, the new one"
            );
        }
        // The shipped helpers are the versioned ones at the current constant
        // - not a second, drifting copy of the JSON shape.
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(
                &measurement_subtree_with_fits_artifact_version(&cfg, FITS_ARTIFACT_VERSION),
                &[],
                &[]
            ),
            "the shipped measurement helper is the fits-versioned one at the current constant"
        );
        assert_eq!(
            stage_hash(&registration_subtree(&cfg), &[], &[]),
            stage_hash(
                &registration_subtree_with_versions(&cfg, MEASURE_VERSION, ALIGN_VERSION),
                &[],
                &[]
            )
        );
        assert_eq!(
            stage_hash(&normalization_subtree(&cfg), &[], &[]),
            stage_hash(
                &normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION),
                &[],
                &[]
            )
        );
        // the calibration and prepare subtrees carry no measure version: a
        // bump must not recalibrate
        assert!(calibration_subtree(&cfg).get("measureVersion").is_none());
        assert!(prepare_subtree(&cfg).get("measureVersion").is_none());
    }

    /// Plan D D2 (`c`'s colour sample, PD-9; ruling F28): the one bump of Plan D (D3's Blend refusal on the stacking
    /// Measure ships under it). `MEASURE_VERSION` is 8, the three fit-reading stage hashes of the default config are
    /// the values below and are none of the version-5, burnt version-6 or version-7 ones, and the calibration subtree
    /// hashes as it did before the bump (a bump must not recalibrate). The measurement literal was re-recorded 2026-11-02
    /// (Plan D D2c r1: `FITS_ARTIFACT_VERSION` 3 folded into that subtree, `the_measurement_hash_follows_the_fits_artifact_version`;
    /// was `972c028e1512fdd0`); the registration one stands, and the normalization one was re-recorded 2026-11-02 at the
    /// merge of `main` (the `local` rejection default). The registration one is read at the shape from before
    /// `alignVersion` joined that subtree (robust quad seed RQ-5): the literal pins MEASURE_VERSION 8's contribution, and
    /// the shipped registration hash (with `alignVersion`) is pinned by `plan.rs`'s stage-hash goldens.
    #[test]
    fn measure_version_eight_moves_the_three_fit_reading_subtrees_only() {
        let cfg = StackingConfig::default();
        assert_eq!(
            (
                MEASURE_VERSION,
                stage_hash(&measurement_subtree(&cfg), &[], &[]),
                stage_hash(
                    &registration_subtree_with_measure_version(&cfg, MEASURE_VERSION),
                    &[],
                    &[]
                ),
                stage_hash(&normalization_subtree(&cfg), &[], &[]),
                stage_hash(&calibration_subtree(&cfg), &[], &[]),
            ),
            (
                8,
                // 2026-11-08 frame-level cache Task 11 (spec §5.8, owner ruling; no truth bar run): measurement.maxStars default 24576 -> 9000 moves measurement and normalization (was "ef32610c8173c47f" and "48ebdb2f76923cba").
                "1a598ed663844a86".to_string(),
                "e6c1ddfad3f06299".to_string(),
                // 2026-11-02 Plan D DF-a (main merged into plan-d): rejection defaults to `local` (pcores Task 1, OD-1a), which the normalization subtree folds; was "f37aa67a46c48a56". The measurement, registration and calibration literals stand.
                "3e2dfb49bb8448fb".to_string(),
                "9653a478603091dc".to_string(),
            ),
            "version, measurement, registration, normalization, calibration"
        );
        for earlier in [5, 6, 7] {
            for (name, old, new) in [
                (
                    "measurement",
                    measurement_subtree_with_measure_version(&cfg, earlier),
                    measurement_subtree(&cfg),
                ),
                (
                    "registration",
                    registration_subtree_with_measure_version(&cfg, earlier),
                    // Like shapes: the registration row of this literal is read without `alignVersion`, so the
                    // pair differs by the measure version alone.
                    registration_subtree_with_measure_version(&cfg, MEASURE_VERSION),
                ),
                (
                    "normalization",
                    normalization_subtree_with_measure_version(&cfg, earlier),
                    normalization_subtree(&cfg),
                ),
            ] {
                assert_ne!(
                    stage_hash(&old, &[], &[]),
                    stage_hash(&new, &[], &[]),
                    "{name} moved with the bump from {earlier}"
                );
            }
        }
    }

    /// Plan D D2c r1 (review M1 of D2b, the owner's speed condition): the `.athf` format version rides in the measurement
    /// stage hash, so a `fits.<plane>` row written at another format version is keyed on another hash and the plan gate
    /// needs no read of the file. Only the measurement subtree carries it: the later stages take it through their upstream
    /// measurement hash, and the calibration subtree does not move (a format bump must not recalibrate).
    #[test]
    fn the_measurement_hash_follows_the_fits_artifact_version() {
        let cfg = StackingConfig::default();
        let at = |v: u16| {
            stage_hash(
                &measurement_subtree_with_fits_artifact_version(&cfg, v),
                &[],
                &[],
            )
        };
        assert_ne!(
            at(FITS_ARTIFACT_VERSION),
            at(FITS_ARTIFACT_VERSION - 1),
            "a row measured at the previous format version is another hash"
        );
        assert_eq!(
            at(FITS_ARTIFACT_VERSION),
            at(FITS_ARTIFACT_VERSION),
            "and the same version is the same hash"
        );
        let shipped = measurement_subtree(&cfg);
        assert_eq!(
            shipped.get("fitsArtifactVersion"),
            Some(&serde_json::json!(FITS_ARTIFACT_VERSION)),
            "the key, once, in the shipped subtree"
        );
        assert!(
            shipped.get("measureVersion").is_some(),
            "beside the measure version"
        );
        for (name, subtree) in [
            ("registration", registration_subtree(&cfg)),
            ("normalization", normalization_subtree(&cfg)),
            ("calibration", calibration_subtree(&cfg)),
            ("prepare", prepare_subtree(&cfg)),
        ] {
            assert!(subtree.get("fitsArtifactVersion").is_none(), "{name}");
        }
        assert_eq!(
            stage_hash(&calibration_subtree(&cfg), &[], &[]),
            "9653a478603091dc",
            "the calibration hash is as before the fold"
        );
    }

    /// M4c Task 3: the large-scale filter's two integers are clamped to the
    /// ranges the Integrate panel offers — an unclamped `protectedLayers`
    /// would ask for a window of `2^(n+1)` pixels (and shift a `usize` past
    /// its own width on the way).
    #[test]
    fn the_large_scale_integers_are_clamped_to_their_ranges() {
        let large = |doc: &str| {
            resolve_config(Some(doc), None)
                .unwrap()
                .integration
                .large_scale
        };
        assert_eq!(
            large("{\"integration\":{\"largeScale\":{\"protectedLayers\":0}}}").protected_layers,
            MIN_PROTECTED_LAYERS
        );
        assert_eq!(
            large("{\"integration\":{\"largeScale\":{\"protectedLayers\":40}}}").protected_layers,
            MAX_PROTECTED_LAYERS
        );
        assert_eq!(
            large("{\"integration\":{\"largeScale\":{\"growth\":9}}}").growth,
            MAX_GROWTH
        );
        // In range, untouched — including `growth: 0` (no dilation).
        let ok = large("{\"integration\":{\"largeScale\":{\"protectedLayers\":6,\"growth\":0}}}");
        assert_eq!((ok.protected_layers, ok.growth), (6, 0));
        let shipped = resolve_config(None, None).unwrap().integration.large_scale;
        assert_eq!(
            shipped,
            crate::stacking::integrate::LargeScaleRejection::default(),
            "the shipped default is inside both ranges and must not move"
        );
    }

    /// M4c Task 4 fix round 1, minor 5: `tpsSmoothing` is clamped to the
    /// range its own UI field offers, for the same reason
    /// `detectionSigma` is — a stored or hand-edited document never went
    /// through that field, and `ThinPlateSpline::fit` refuses a negative
    /// or non-finite λ, which would degrade EVERY frame one per-frame
    /// note at a time instead of saying once that the config is wrong.
    #[test]
    fn tps_smoothing_is_clamped_to_its_range() {
        let lambda = |doc: &str| {
            resolve_config(Some(doc), None)
                .unwrap()
                .registration
                .tps_smoothing
        };
        assert_eq!(
            lambda("{\"registration\":{\"tpsSmoothing\":-1}}"),
            MIN_TPS_SMOOTHING
        );
        assert_eq!(
            lambda("{\"registration\":{\"tpsSmoothing\":1e9}}"),
            MAX_TPS_SMOOTHING
        );
        // In range, untouched — both ends and the shipped default.
        for v in ["0", "0.01", "2.5", "10"] {
            assert_eq!(
                lambda(&format!("{{\"registration\":{{\"tpsSmoothing\":{v}}}}}")),
                v.parse::<f64>().unwrap()
            );
        }
        // A non-finite λ clamps rather than surviving (`!(x >= MIN)`).
        // `serde_json` refuses `1e400` outright ("number out of range"),
        // so this half of the guard is exercised on the clamp itself —
        // it is defence in depth, not a reachable JSON shape.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let got = clamp_tps_smoothing(bad);
            assert!(
                got.is_finite() && (MIN_TPS_SMOOTHING..=MAX_TPS_SMOOTHING).contains(&got),
                "{bad} → {got}"
            );
        }
        // The shipped default (ruling R-T7-1) is inside the range and must
        // not move without a deliberate re-measurement.
        assert_eq!(
            resolve_config(None, None)
                .unwrap()
                .registration
                .tps_smoothing,
            0.5
        );
    }

    /// `detectionSigma` reaches the detector as a raw level multiplier, so a
    /// stored or hand-edited document must not be able to hand it a zero, a
    /// negative or a non-finite value (fix round 3, Important 3).
    #[test]
    fn detection_sigma_is_clamped_to_its_range() {
        let sigma = |doc: &str| {
            resolve_config(Some(doc), None)
                .unwrap()
                .measurement
                .detection_sigma
        };
        assert_eq!(
            sigma("{\"measurement\":{\"detectionSigma\":-5}}"),
            MIN_DETECTION_SIGMA
        );
        assert_eq!(
            sigma("{\"measurement\":{\"detectionSigma\":0}}"),
            MIN_DETECTION_SIGMA
        );
        assert_eq!(
            sigma("{\"measurement\":{\"detectionSigma\":250}}"),
            MAX_DETECTION_SIGMA
        );
        // In range, untouched — including both ends and the shipped default.
        for v in ["1", "20", "100"] {
            assert_eq!(
                sigma(&format!("{{\"measurement\":{{\"detectionSigma\":{v}}}}}")),
                v.parse::<f64>().unwrap()
            );
        }
        assert_eq!(
            resolve_config(None, None)
                .unwrap()
                .measurement
                .detection_sigma,
            MeasurementConfig::default().detection_sigma,
            "the shipped default is inside the range and must not move"
        );

        // A non-finite value cannot travel through JSON at all — serde_json
        // rejects the out-of-range magnitude rather than decoding it to an
        // infinity — so the clamp's NaN-safe shape is exercised directly.
        assert!(
            resolve_config(Some("{\"measurement\":{\"detectionSigma\":1e309}}"), None).is_err(),
            "an out-of-range JSON number is refused before the clamp sees it"
        );
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0, 0.9999] {
            assert_eq!(
                clamp_detection_sigma(bad),
                if bad > MAX_DETECTION_SIGMA {
                    MAX_DETECTION_SIGMA
                } else {
                    MIN_DETECTION_SIGMA
                },
                "clamping {bad}"
            );
        }
    }

    /// Starloom phase 1 (P1-R1): `MEASURE_STAGE_VERSION` enters the three fit-reading subtrees only past 1, so no pinned
    /// hash moves at 1 and a bump moves each of the three. The shipped subtrees are judged against the unfolded
    /// helpers, so a future bump of the constant moves no assert here.
    #[test]
    fn measure_stage_version_folds_only_past_1() {
        let cfg = StackingConfig::default();
        let mut measurement = measurement_subtree_with_measure_version(&cfg, MEASURE_VERSION);
        measurement["fitsArtifactVersion"] = serde_json::json!(FITS_ARTIFACT_VERSION);
        for (name, shipped, unfolded) in [
            ("measurement", measurement_subtree(&cfg), measurement),
            (
                "registration",
                registration_subtree(&cfg),
                registration_subtree_with_versions(&cfg, MEASURE_VERSION, ALIGN_VERSION),
            ),
            (
                "normalization",
                normalization_subtree(&cfg),
                normalization_subtree_with_measure_version(&cfg, MEASURE_VERSION),
            ),
        ] {
            assert!(
                unfolded.get("measureStageVersion").is_none(),
                "{name}: the unfolded helper holds no measure-stage key"
            );
            let mut expected = unfolded.clone();
            fold_measure_stage_version(&mut expected, MEASURE_STAGE_VERSION);
            assert_eq!(shipped, expected, "{name}: the shipped subtree is the unfolded one with the fold");
            let mut at_one = unfolded.clone();
            fold_measure_stage_version(&mut at_one, 1);
            assert_eq!(at_one, unfolded, "{name}: folding 1 changes nothing");
            let mut at_two = unfolded.clone();
            fold_measure_stage_version(&mut at_two, 2);
            assert_eq!(at_two["measureStageVersion"], serde_json::json!(2), "{name}");
            assert_ne!(
                stage_hash(&at_two, &[], &[]),
                stage_hash(&unfolded, &[], &[]),
                "{name}: folding 2 moves the stage hash"
            );
        }
    }

    #[test]
    fn stage_hash_is_order_insensitive() {
        let cfg = StackingConfig::default();
        let subtree = calibration_subtree(&cfg);
        let src = |file_id: i64, size: i64, sample: &str| SourceIdentity::Sampled {
            file_id,
            size,
            sample: sample.into(),
        };
        let (s1, s2) = (src(1, 10, "t"), src(2, 20, "u"));
        let forward = stage_hash(&subtree, &["a", "b"], &[src(1, 10, "t"), src(2, 20, "u")]);
        let reversed = stage_hash(&subtree, &["b", "a"], &[s2, s1]);
        assert_eq!(
            forward, reversed,
            "sources/upstream in either order must hash the same"
        );

        let changed = stage_hash(
            &subtree,
            &["a", "b"],
            &[
                src(3, 10, "t"),
                src(2, 20, "u"),
            ],
        );
        assert_ne!(forward, changed, "a changed file_id still changes the hash");
    }

    #[test]
    fn every_older_measurement_shape_decodes_to_the_spec_seed_source() {
        use crate::api::stacking::NamedPreset;
        let cases = [
            (r#"{"measurement":{}}"#, SeedSource::MatchedFilter), // pre-M4a
            (
                r#"{"measurement":{"detectionSigma":20.0,"seedPrefilter":"none"}}"#,
                SeedSource::MatchedFilter,
            ), // the old default
            (
                r#"{"measurement":{"detectionSigma":16.0}}"#,
                SeedSource::Peak,
            ), // an explicit sigma
            (
                r#"{"measurement":{"seedPrefilter":"median3"}}"#,
                SeedSource::Peak,
            ), // the prefilter
            (
                r#"{"measurement":{"seedDetector":"structure","seedPrefilter":"median3"}}"#,
                SeedSource::Structure,
            ), // Median3 is Peak's only
            (
                r#"{"measurement":{"seedSource":"peak","detectionSigma":20.0}}"#,
                SeedSource::Peak,
            ), // a new document says so
        ];
        for (json, want) in cases {
            let c = resolve_config(Some(json), None).unwrap();
            assert_eq!(c.measurement.seed_source, want, "{json}");
            assert_eq!(
                c.measurement.detection_sigma,
                if json.contains("16.0") { 16.0 } else { 20.0 },
                "never reinterpreted: {json}"
            );
        }
        // the global document and a user preset go through the same decode
        assert_eq!(
            resolve_config(None, Some(r#"{"measurement":{"detectionSigma":20.0}}"#))
                .unwrap()
                .measurement
                .seed_source,
            SeedSource::MatchedFilter
        );
        let p: NamedPreset = serde_json::from_str(
            r#"{"name":"x","config":{"measurement":{"seedDetector":"structure"}}}"#,
        )
        .unwrap();
        assert_eq!(p.config.measurement.seed_source, SeedSource::Structure);
        // `seedSource` wins over a contradicting `seedDetector`; the wire detector mirrors the source.
        let c = resolve_config(
            Some(r#"{"measurement":{"seedSource":"matchedFilter","seedDetector":"structure"}}"#),
            None,
        )
        .unwrap();
        assert_eq!(c.measurement.seed_source, SeedSource::MatchedFilter);
        assert_eq!(c.measurement.seed_detector, SeedDetector::Peak);
    }

    #[test]
    fn seed_source_moves_the_measurement_stage_hash_and_the_default_is_the_matched_filter() {
        let cfg = StackingConfig::default();
        assert_eq!(cfg.measurement.seed_source, SeedSource::MatchedFilter);
        assert_eq!(
            cfg.measurement.seed_threshold,
            crate::star_measure::seeds::DEFAULT_THRESHOLD
        );
        let mut peak = cfg.clone();
        peak.measurement.seed_source = SeedSource::Peak;
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&peak), &[], &[])
        );
        let mut k = cfg.clone();
        k.measurement.seed_threshold += 0.5;
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&k), &[], &[])
        );
        assert_eq!(
            cfg.measurement
                .measure_options(ScaleEstimator::Bwmv)
                .seed_source,
            SeedSource::MatchedFilter
        );
    }

    #[test]
    fn seed_threshold_is_clamped_with_a_warn() {
        let lo = resolve_config(Some(r#"{"measurement":{"seedThreshold":0.5}}"#), None).unwrap();
        assert_eq!(lo.measurement.seed_threshold, 2.0);
        let hi = resolve_config(Some(r#"{"measurement":{"seedThreshold":99.0}}"#), None).unwrap();
        assert_eq!(hi.measurement.seed_threshold, 20.0);
        let ok = resolve_config(Some(r#"{"measurement":{"seedThreshold":4.5}}"#), None).unwrap();
        assert_eq!(ok.measurement.seed_threshold, 4.5);
    }

    #[test]
    fn seed_clamps_ride_the_shared_resolver() {
        // A decoded preset / plan override never passes resolve_config; the
        // shared resolver must clamp both seed dials there too.
        let mut cfg: StackingConfig =
            serde_json::from_str(r#"{"measurement":{"seedThreshold":0.5,"detectionSigma":0.1}}"#)
                .unwrap();
        resolve_ln_fields(&mut cfg);
        assert_eq!(cfg.measurement.seed_threshold, 2.0);
        assert_eq!(cfg.measurement.detection_sigma, MIN_DETECTION_SIGMA);
        let mut ok = StackingConfig::default();
        let before = ok.measurement.clone();
        resolve_ln_fields(&mut ok);
        assert_eq!(ok.measurement.seed_threshold, before.seed_threshold);
        assert_eq!(ok.measurement.detection_sigma, before.detection_sigma);
    }

    #[test]
    fn config_hash_is_canonical_and_pinned() {
        let default_hash = config_hash(&StackingConfig::default());
        let value = serde_json::to_value(StackingConfig::default()).unwrap();
        let roundtripped: StackingConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            config_hash(&roundtripped),
            default_hash,
            "a Value round-trip must not change the fingerprint"
        );
        // Pinned once, on this task's implementation — guards every future
        // field reorder/rename in any config type. If this literal must
        // change in a later task, that task says why.
        //
        // Changed here (M2 Task 10, owner decision 2026-09-10): dropping
        // `GroupingConfig.splitByExposure` changes the canonical JSON of
        // every stored `StackingConfig`, so `config_hash` moves — no
        // per-frame `stacking_artifacts` row goes stale over this (the
        // calibration subtree folds in `grouping.exposureToleranceSec`
        // only, in a fixed shape, and every other grouping field stays
        // out of every stage hash), only this whole-config fingerprint.
        //
        // Changed again here (M4a Task 2, ruling R-M4a-1/R-M4a-9): adding
        // `MeasurementConfig.detectionSigma` changes the canonical JSON of
        // every stored `StackingConfig`. Unlike the M2 change this one DOES
        // go stale on purpose — `measurement_subtree` serializes the whole
        // `MeasurementConfig`, so every cached per-frame measure artifact
        // is recomputed once, which is the point: the old ones were
        // measured with the rank-budget seed population.
        //
        // And again in that task's fix round 1 (R-M4a-13): `seedPrefilter`
        // joins the same subtree. It ships `none`, i.e. today's behaviour,
        // so nothing about a run changes — but the stored JSON does, so the
        // fingerprint and the measure-stage hash move once more.
        //
        // Moved once more here (M4a Task 4, rulings R-M4a-5/R-M4a-9):
        // `reference.twoPass` joins `ReferenceConfig`. No STAGE hash folds
        // in `reference` (`registration_subtree` is `cfg.registration`
        // alone), so not one cached per-frame artifact goes stale over it —
        // only this whole-config fingerprint moves.
        //
        // And again here (M4c Task 0, ruling R-M4c-11): `seedDetector`
        // joins `MeasurementConfig`. It ships `peak`, i.e. today's
        // behaviour, so no run changes — but it DOES ride
        // `measurement_subtree`, so every set's cached measure artifact
        // goes stale once more, exactly as `seedPrefilter` did.
        //
        // And once more here (M4b Task 3, rulings R-M4b-4/R-M4b-5):
        // `registration.geometry` joins `RegistrationConfig`. It ships
        // `coRegistered`, i.e. today's behaviour, but it DOES ride
        // `registration_subtree`, so every set's cached registration rows
        // go stale once — deliberately: a row records which reference a
        // frame was warped onto, which is exactly what the mode decides.
        //
        // And here (M4c Task 3, ruling R-M4c-4): `integration.largeScale`
        // joins `IntegrationConfig`. It ships disabled, i.e. today's
        // behaviour, and NO stage subtree folds in `integration` at all
        // (`calibration`/`measurement`/`registration`/`normalization` are
        // the four), so not one cached per-frame artifact goes stale over
        // it — only this whole-config fingerprint moves.
        //
        // And once more here (M4c Task 4, rulings R-M4c-5/R-M4c-7):
        // `registration.tpsSmoothing` and `registration.localDistortion`
        // join `RegistrationConfig`. `distortion` still defaults to `off`,
        // so neither is read by a default run at all, but they DO ride
        // `registration_subtree`, so every set's cached registration rows
        // go stale once — deliberately, per the same reasoning as
        // `geometry` above: the fields decide what surface a stored
        // `transform_json` describes. The final fix wave then moved
        // `tpsSmoothing`'s own default from `0.0` to `0.5` (ruling
        // R-T7-1); that re-pins this literal but costs no SECOND
        // invalidation in practice — the field's arrival above already
        // spent the one re-registration, and a stored document that
        // spells `tpsSmoothing` out keeps its own value either way.
        //
        // And here (M4d Task 1, rulings R-M4d-1/R-M4d-2): `drizzle.bayer`
        // joins `DrizzleConfig`. It ships `false`, i.e. today's behaviour,
        // and no stage subtree folds in `drizzle` at all — deliberately,
        // since a debayered calibrated frame is byte-identical whether or
        // not the mosaic is kept beside it (see
        // `bayer_moves_the_run_fingerprint_but_no_stage_hash`) — so not one
        // cached per-frame artifact goes stale over it; only this
        // whole-config fingerprint moves.
        //
        // And here again (the XISF plan, 2026-09-18, ruling R6):
        // `calibration.format` joins `CalibratedLightOptions`, shipping
        // `fits`. It is an export/send choice the run overrides, so
        // `calibration_subtree` strips it and no calibrated artifact goes
        // stale (see `format_moves_the_run_fingerprint_but_no_stage_hash`);
        // only this whole-config fingerprint moves. This pin was NOT run
        // locally before the push that moved it — CI caught it — which is
        // the reminder to run `stacking::config` whenever a config struct
        // gains a field.
        //
        // And here (Tier C item C2, ruling C-3): `DRIZZLE_KERNEL_VERSION`
        // joins the hashed payload as a sibling of the config itself, so
        // the literal moves once for the phase-table deposit even though
        // no config FIELD changed. No stored ARTIFACT goes stale over it —
        // the drizzle stage has no cache and no stage subtree folds in
        // `drizzle` — but two runs of the same settings across this change
        // do produce different drizzled pixels, and the run-level
        // fingerprint is what says so. The one non-artifact consequence,
        // the plan gate's LN-exclusion memory emptying for one plan, is
        // written out on `DRIZZLE_KERNEL_VERSION`'s own doc.
        // And here (collab v3 wave 6b-2 Task 2, plan ruling C2):
        // `grouping.exposureMode`/`stopsWidth` and `output.coverageMap`/
        // `cropToCoverage` join the config, all `#[serde(default)]` with no
        // version bump. No stage subtree folds them in —
        // `stage_hashes_are_pinned_to_golden_literals` proves it with every
        // field at a non-default value — so not one cached artifact goes
        // stale; only this whole-config fingerprint moves (and, once, the
        // plan gate's LN-exclusion memory, see `DRIZZLE_KERNEL_VERSION`).
        //
        // And here (2026-10-05 LN model fixes Task 3): `normalization.local`
        // gains `scaleMode` and `gridSize` in the serialized config (the
        // fingerprint hashes the whole document).
        //
        // And here (2026-10-05 LN model fixes Task 11): `normalization.local`
        // gains `starSelection` in the serialized config.
        //
        // And here (2026-10-05 LN model fixes Task 13): `normalization.local`
        // gains `fluxEstimator` and `aperture` in the serialized config.
        //
        // And here (2026-10-06): F1 growth curve is the default (L-24) -
        // `fluxEstimator` and `aperture` take their new default values.
        //
        // And here (2026-10-08 one star-measurement path Plan A Task 7, R-OSP-2): `measurement` gains `seedSource`
        // (default matchedFilter) and `seedThreshold` (default `seeds::DEFAULT_THRESHOLD`) in the serialized config,
        // and `seedDetector` now mirrors the source. Was "03af478211b610de".
        //
        // And here (2026-11-01 pcores Task 1, OD-1a): `normalization.rejection` defaults to `local`. Was
        // "1de1a3136b8dc4ac".
        //
        // And here (2026-11-08 frame-level cache Task 11 (spec §5.8, owner ruling; no truth bar run): measurement.maxStars default 24576 -> 9000): the serialized default config carries `maxStars` 9000. Was "8bb369a197d1a3f4".
        assert_eq!(default_hash, "936429b7aeb6c9c4");
    }

    /// Tier C item C2 (ruling C-3): the drizzle kernel generation is part
    /// of the run-level fingerprint and of NO stage hash — a future reader
    /// must not be able to drop it from `config_hash` without a red test,
    /// and must not be able to sneak it into a stage subtree either (which
    /// would invalidate every cached calibrated/measured/registered frame
    /// for a change that touches none of them).
    #[test]
    fn the_drizzle_kernel_version_rides_the_fingerprint_and_no_stage_hash() {
        let cfg = StackingConfig::default();
        let bare = serde_json::to_string(
            &serde_json::to_value(&cfg).expect("StackingConfig always serializes"),
        )
        .unwrap();
        let bare_hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bare.as_bytes()));
        assert_ne!(
            config_hash(&cfg),
            bare_hash,
            "config_hash must fold in DRIZZLE_KERNEL_VERSION, not hash the config alone"
        );
        assert_eq!(DRIZZLE_KERNEL_VERSION, 2, "the phase-table generation");

        // All FOUR cacheable stages' subtrees (fix round 2, ruling C-22 —
        // `normalization_subtree` was missing, and LN is the stage whose
        // hash already folds in a version constant, so it is the one a
        // future reader is likeliest to reach for).
        for subtree in [
            calibration_subtree(&cfg),
            measurement_subtree(&cfg),
            registration_subtree(&cfg),
            normalization_subtree(&cfg),
        ] {
            let json = serde_json::to_string(&subtree).unwrap();
            assert!(
                !json.contains("drizzleKernelVersion"),
                "no stage subtree may carry the drizzle kernel version: {json}"
            );
        }
    }

    // ---- fitStars (FM-2, FM-3) ----

    /// Resolve `json` as a set document and count the WARN events it logged.
    fn resolve_counting_warns(json: &str) -> (StackingConfig, usize) {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tracing_subscriber::layer::SubscriberExt;
        struct Count(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() == tracing::Level::WARN {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let n = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(n.clone()));
        let cfg = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            resolve_config(Some(json), None).unwrap()
        });
        (cfg, n.load(Ordering::SeqCst))
    }

    #[test]
    fn all_serializes_without_the_key() {
        let v = serde_json::to_value(StackingConfig::default()).unwrap();
        assert!(v["measurement"].get("fitStars").is_none());
        let mut c = StackingConfig::default();
        c.measurement.fit_stars = Some(4000);
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["measurement"]["fitStars"], 4000);
    }

    #[test]
    fn document_without_fit_stars_decodes_all() {
        let c = resolve_config(Some(r#"{"measurement":{"maxStars":24576}}"#), None).unwrap();
        assert_eq!(c.measurement.fit_stars, None);
        let c = resolve_config(Some("{}"), None).unwrap();
        assert_eq!(c.measurement.fit_stars, None);
    }

    /// Spec 2026-11-08 §5.8 (owner ruling): 9 000 is the default for NEW documents only; a stored document carries
    /// its `maxStars` explicitly and keeps it, so its caches stay valid.
    #[test]
    fn a_stored_document_keeps_its_max_stars() {
        let c = resolve_config(Some(r#"{"measurement":{"maxStars":24576}}"#), None).unwrap();
        assert_eq!(c.measurement.max_stars, 24_576);
        let c = resolve_config(Some("{}"), None).unwrap();
        assert_eq!(c.measurement.max_stars, 9_000);
    }

    #[test]
    fn fit_stars_at_or_above_max_stars_is_none() {
        for k in [24_576, 30_000] {
            let (c, warns) =
                resolve_counting_warns(&format!(r#"{{"measurement":{{"fitStars":{k}}}}}"#));
            assert_eq!(c.measurement.fit_stars, None, "k {k}");
            assert_eq!(warns, 0);
        }
        let (c, _) = resolve_counting_warns(r#"{"measurement":{"fitStars":4000}}"#);
        assert_eq!(c.measurement.fit_stars, Some(4000));
    }

    #[test]
    fn fit_stars_below_min_is_clamped_with_a_warning() {
        // registration.maxStars default 2000 > MIN_FIT_STARS: use a registration budget under it.
        let (c, warns) = resolve_counting_warns(
            r#"{"measurement":{"fitStars":500},"registration":{"maxStars":600}}"#,
        );
        assert_eq!(c.measurement.fit_stars, Some(MIN_FIT_STARS));
        assert_eq!(warns, 1);
    }

    #[test]
    fn fit_stars_below_register_max_stars_is_clamped_to_it_with_a_warning() {
        let (c, warns) = resolve_counting_warns(
            r#"{"measurement":{"fitStars":1500},"registration":{"maxStars":3000}}"#,
        );
        assert_eq!(c.measurement.fit_stars, Some(3000));
        assert_eq!(warns, 1);
        // The default registration budget (2000) is the floor at the defaults.
        let (c, _) = resolve_counting_warns(r#"{"measurement":{"fitStars":1200}}"#);
        assert_eq!(c.measurement.fit_stars, Some(2000));
    }

    #[test]
    fn fit_stars_with_a_floor_at_max_stars_resolves_to_all() {
        // registration.maxStars 30 000 lifts the floor above measurement.maxStars: the clamp lands on All.
        let (c, warns) = resolve_counting_warns(
            r#"{"measurement":{"fitStars":5000},"registration":{"maxStars":30000}}"#,
        );
        assert_eq!(c.measurement.fit_stars, None);
        assert_eq!(warns, 1);
    }

    #[test]
    fn min_stars_above_fit_stars_warns() {
        let (c, warns) = resolve_counting_warns(
            r#"{"measurement":{"fitStars":4000},"selection":{"minStars":5000}}"#,
        );
        assert_eq!(c.measurement.fit_stars, Some(4000));
        assert_eq!(warns, 1);
        let (_, warns) = resolve_counting_warns(
            r#"{"measurement":{"fitStars":4000},"selection":{"minStars":3000}}"#,
        );
        assert_eq!(warns, 0);
    }

    /// The canonical config text sorts every object's keys because `serde_json` keeps its default map. A crate
    /// anywhere in the build that turns on `preserve_order` would reorder them and change every `config_hash`.
    #[test]
    fn serde_json_keys_stay_sorted() {
        let text = serde_json::to_string(&serde_json::json!({"b": 1, "a": 2})).unwrap();
        assert_eq!(text, r#"{"a":2,"b":1}"#, "a crate in the build turned on serde_json preserve_order");
    }

    /// The three presets, with the names the pins print under.
    fn every_preset() -> [(&'static str, StackingPreset); 3] {
        [
            ("Default", StackingPreset::Default),
            ("FastPreview", StackingPreset::FastPreview),
            ("MaximumQuality", StackingPreset::MaximumQuality),
        ]
    }

    /// xxh3-64 of a subtree's compact JSON, as 16 lowercase hex digits (starloom phase 2, P2-R9: the value a
    /// manifest entry's stage hash is made of).
    fn subtree_xxh3(subtree: &serde_json::Value) -> String {
        let json = serde_json::to_string(subtree).expect("a subtree serializes");
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
    }

    /// Starloom phase 2 (Task 6): the whole-config fingerprint of every built-in preset, pinned before the preset
    /// definitions move into starloom. A move that changes one byte of a preset is a red test here.
    #[test]
    fn every_preset_config_hash_is_pinned() {
        let expected = [
            ("Default", "936429b7aeb6c9c4"),
            ("FastPreview", "7e61164f16c92c82"),
            ("MaximumQuality", "84146c93127ef713"),
        ];
        for ((name, p), (pinned_name, pinned)) in every_preset().into_iter().zip(expected) {
            assert_eq!(name, pinned_name);
            assert_eq!(config_hash(&preset(p)), pinned, "{name}: config_hash moved");
        }
    }

    /// Starloom phase 2 (Task 6, P2-R9): xxh3-64 of each stage subtree of every preset, pinned before the subtree
    /// builders move into starloom. 12 literals, some repeated: a preset differs from the default in only some stages.
    #[test]
    fn every_preset_stage_subtree_is_pinned() {
        // (preset, calibration, measurement, registration, normalization)
        let expected = [
            ("Default", "0f3177998fbce0da", "3e6550bbf1c99012", "0a4a72469833a578", "135f0143c278b3e6"),
            ("FastPreview", "0f3177998fbce0da", "3e6550bbf1c99012", "e8bf9794120c4ab5", "06ab9d4517b16ba5"),
            ("MaximumQuality", "0f3177998fbce0da", "3e6550bbf1c99012", "dd272ff14698a2da", "eed22cdb5e2a2d25"),
        ];
        for ((name, p), (pinned_name, cal, mea, reg, nrm)) in every_preset().into_iter().zip(expected) {
            assert_eq!(name, pinned_name);
            let cfg = preset(p);
            assert_eq!(subtree_xxh3(&calibration_subtree(&cfg)), cal, "{name}: calibration subtree moved");
            assert_eq!(subtree_xxh3(&measurement_subtree(&cfg)), mea, "{name}: measurement subtree moved");
            assert_eq!(subtree_xxh3(&registration_subtree(&cfg)), reg, "{name}: registration subtree moved");
            assert_eq!(subtree_xxh3(&normalization_subtree(&cfg)), nrm, "{name}: normalization subtree moved");
        }
    }

    /// Starloom phase 2 (Task 7): the algorithm sections of every preset are starloom's `algorithm_preset`, key for
    /// key: a preset that moved must not drift from the copy the kernel's own tools read.
    #[test]
    fn the_algorithm_subtrees_of_every_preset_equal_starloom_s() {
        const KEYS: [&str; 7] = [
            "calibration",
            "measurement",
            "selection",
            "registration",
            "normalization",
            "integration",
            "drizzle",
        ];
        for (name, p) in every_preset() {
            let whole = serde_json::to_value(preset(p)).unwrap();
            let kernel = serde_json::to_value(algorithm_preset(p)).unwrap();
            for key in KEYS {
                assert_eq!(whole[key], kernel[key], "{name}: the {key} section differs");
            }
            assert_eq!(
                kernel.as_object().unwrap().len(),
                KEYS.len(),
                "{name}: the kernel's algorithm config carries exactly the seven sections"
            );
            assert_eq!(preset(p).algorithm(), algorithm_preset(p), "{name}: algorithm() is the same value");
        }
    }

    /// Starloom phase 2 (Task 7): the default `StackingConfig`, as a document, is the fixture starloom's own tests
    /// parse (`starloom/tests/fixtures/stacking-config-default.json`), so a change to a default that the kernel's
    /// tests do not see is red here.
    #[test]
    fn the_starloom_fixture_is_the_default_stacking_config() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("starloom")
            .join("tests")
            .join("fixtures")
            .join("stacking-config-default.json");
        let text = std::fs::read_to_string(&path).expect("the starloom fixture is readable");
        let fixture: serde_json::Value = serde_json::from_str(&text).expect("the fixture is JSON");
        // Both sides come from text: an `f32` field prints with its own shortest digits, which `to_value` would widen.
        let current: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&StackingConfig::default()).unwrap()).unwrap();
        assert_eq!(
            fixture, current,
            "regenerate starloom/tests/fixtures/stacking-config-default.json from StackingConfig::default()"
        );
    }
}
