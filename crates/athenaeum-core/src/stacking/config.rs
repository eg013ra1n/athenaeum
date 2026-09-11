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

use crate::integration::stats::ScaleEstimator;
use crate::resample::Interpolation;
use crate::stacking::integrate::RejectionChoice;
use crate::stacking::measure::MeasureOptions;
use crate::stacking::prefilter::SeedPrefilter;
use crate::stacking::structure::SeedDetector;
use crate::stacking::psf_signal::{PsfModel, PSF_FIT_VERSION};
use crate::stacking::register::DistortionChoice;
use crate::stacking::weights::{FormulaWeights, WeightMode};

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
}

impl Default for GroupingConfig {
    fn default() -> Self {
        GroupingConfig {
            exposure_tolerance_sec: 2.0,
        }
    }
}

/// spec §9.2 `measurement:` — Plan 2's weighting/selection inputs, chosen
/// once per run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct MeasurementConfig {
    pub weight_mode: WeightMode,
    pub psf_model: PsfModel,
    /// Detection cap (spec §9.2 `maxStars`).
    pub max_stars: usize,
    pub formula: FormulaWeights,
    /// FITS keyword `WeightMode::Keyword` reads its value from.
    pub keyword: String,
    /// Star-detection threshold for the quality measurement, in sigma above
    /// the local background (spec §9.2). The measurement detector's two
    /// levels sit at `background + k*noise` and `background + (k/2)*noise`,
    /// so the seed population follows THIS frame's sky instead of a fixed
    /// bright-pixel budget (M4a Task 2, ruling R-M4a-1).
    #[serde(default = "default_detection_sigma")]
    pub detection_sigma: f64,
    /// What the seed detection runs on (math reference §5.1): the plane as
    /// it is, or its 3×3 median. Everything downstream of detection always
    /// measures the untouched plane. Read by the peak detector only.
    #[serde(default)]
    pub seed_prefilter: SeedPrefilter,
    /// WHICH detector finds the seeds (math reference §5.1, ruling
    /// R-M4c-11): the peak threshold `detectionSigma` steers, or the
    /// structure map. A stored config written before M4c Task 0 has no such
    /// field and decodes to the shipped default.
    #[serde(default)]
    pub seed_detector: SeedDetector,
}

/// Serde default for [`MeasurementConfig::detection_sigma`] — a stored
/// config written before M4a Task 2 has no such field and must decode to
/// the calibrated value, not to `0.0`.
fn default_detection_sigma() -> f64 {
    crate::stacking::measure::DEFAULT_DETECTION_SIGMA as f64
}

impl Default for MeasurementConfig {
    fn default() -> Self {
        MeasurementConfig {
            weight_mode: WeightMode::PsfSignalWeight,
            psf_model: PsfModel::Auto,
            max_stars: 24_576,
            formula: FormulaWeights::default(),
            keyword: "SSWEIGHT".to_string(),
            detection_sigma: default_detection_sigma(),
            seed_prefilter: crate::stacking::measure::DEFAULT_SEED_PREFILTER,
            seed_detector: crate::stacking::measure::DEFAULT_SEED_DETECTOR,
        }
    }
}

impl MeasurementConfig {
    /// [`MeasureOptions`] for Plan 2's `measure_frame`/`measure_plane`,
    /// built from this config's fields. `scale_estimator` comes from the
    /// caller (the orchestrator keeps it equal to
    /// `NormalizationConfig::scale_estimator`, spec §9.2 — the two are
    /// stored separately because normalization's estimator also drives
    /// stage 5, which never measures a frame). `min_snr` is not yet a
    /// `StackingConfig` field, so this takes the detector's own default
    /// sensitivity (`MeasureOptions::default().min_snr`).
    pub fn measure_options(&self, scale_estimator: ScaleEstimator) -> MeasureOptions {
        MeasureOptions {
            psf_model: self.psf_model,
            max_stars: self.max_stars,
            scale_estimator,
            min_snr: MeasureOptions::default().min_snr,
            detection_sigma: self.detection_sigma as f32,
            seed_prefilter: self.seed_prefilter,
            seed_detector: self.seed_detector,
            // Not a config field: a run picks the detector, not its dials
            // (M4c Task 0, ruling R-M4c-11).
            structure: MeasureOptions::default().structure,
        }
    }
}

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

/// spec §9.2 `drizzle:` (M3 — carried here, defaulted off, so a stored
/// config round-trips before the stage exists).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct DrizzleConfig {
    pub enabled: bool,
    pub scale: u32,
    pub drop_shrink: f64,
    pub kernel: DrizzleKernel,
    pub use_rejection: bool,
    pub use_weights: bool,
    /// B8 (M3 final fix wave, M5 ruling): a no-op whenever local
    /// normalization is off for the run (`normalization.local.enabled ==
    /// false` and `normalization.rejection != "local"`) — the drizzle
    /// driver already falls back to each included frame's own global
    /// output-normalization pair per frame in that case, exactly as
    /// [`super::integrate`]'s own engine does; this toggle only has an
    /// effect when local normalization is actually driving output
    /// normalization for the group.
    pub use_local_normalization: bool,
    pub write_weight_map: bool,
}

impl Default for DrizzleConfig {
    fn default() -> Self {
        DrizzleConfig {
            enabled: false,
            scale: 2,
            drop_shrink: 0.9,
            kernel: DrizzleKernel::Square,
            use_rejection: true,
            use_weights: true,
            use_local_normalization: true,
            write_weight_map: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum DrizzleKernel {
    #[default]
    Square,
    Circle,
    Gaussian,
}

/// spec §9.2 `output:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputConfig {
    pub format: OutputFormat,
    pub cleanup: CleanupPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    #[default]
    Fits,
}

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

/// spec §9.2: built-in config transforms. Editing any field afterward makes
/// the effective config "Custom" — that bookkeeping belongs to the caller
/// (the Stacking tab / run orchestration), not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum StackingPreset {
    Default,
    FastPreview,
    MaximumQuality,
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
/// refinement a preview does not need.
pub fn preset(p: StackingPreset) -> StackingConfig {
    match p {
        StackingPreset::Default => StackingConfig::default(),
        StackingPreset::FastPreview => {
            let mut c = StackingConfig::default();
            c.registration.interpolation = Interpolation::Bilinear;
            c.reference.two_pass = false;
            c.integration.rejection = RejectionChoice::SigmaClip {
                sigma_low: 4.0,
                sigma_high: 3.0,
            };
            c.output.cleanup = CleanupPolicy::DeleteIntermediates;
            c
        }
        StackingPreset::MaximumQuality => {
            let mut c = StackingConfig::default();
            c.registration.distortion = DistortionChoice::Polynomial3;
            c.integration.write_rejection_maps = true;
            c.normalization.local.enabled = true;
            c.drizzle.enabled = true;
            c.drizzle.scale = 2;
            c
        }
    }
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
    config.measurement.detection_sigma = clamp_detection_sigma(config.measurement.detection_sigma);
    Ok(config)
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

/// xxh3 of the canonical JSON of the whole resolved config — a run-level
/// fingerprint, distinct from the per-stage [`stage_hash`] below. Goes
/// through `serde_json::to_value` before stringifying (the same
/// sorted-object-keys canonicalization `stage_hash` relies on — this
/// workspace's `serde_json` has no `preserve_order` feature), so a field
/// reorder inside any config type never changes the fingerprint.
pub fn config_hash(cfg: &StackingConfig) -> String {
    let value = serde_json::to_value(cfg).expect("StackingConfig always serializes");
    let json = serde_json::to_string(&value).expect("a serde_json::Value always serializes");
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
/// `sources` by `(file_id, size, modified_at)` before hashing, so a caller
/// that assembles either from an unordered source (a `HashMap`, a query
/// with no `ORDER BY`) still gets the same hash every run — an
/// order-sensitive hash would make every artifact look stale and get
/// silently recomputed. Duplicate entries are kept, not deduped: a
/// duplicate is the caller's bug, and a stable hash for it is still the
/// right answer.
#[derive(Serialize)]
pub struct SourceIdentity {
    pub file_id: i64,
    pub size: i64,
    pub modified_at: String,
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
        a.file_id
            .cmp(&b.file_id)
            .then(a.size.cmp(&b.size))
            .then_with(|| a.modified_at.cmp(&b.modified_at))
    });

    let payload = serde_json::json!({
        "config": config_subtree,
        "upstream": sorted_upstream,
        "sources": sorted_sources,
    });
    let json = serde_json::to_string(&payload).expect("stage hash payload always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}

/// Stage 1 (calibration) config subtree (spec §9.3): the frame's
/// calibration options plus the grouping rule that decided which frames
/// share a group.
pub fn calibration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({
        "calibration": cfg.calibration,
        "grouping": cfg.grouping,
    })
}

/// Stage 3 (measurement) config subtree: the measurement settings, the
/// scale estimator normalization keeps in lockstep with it (see
/// [`MeasurementConfig::measure_options`]), and
/// [`crate::stacking::psf_signal::PSF_FIT_VERSION`] — the config alone
/// cannot express "the fitter itself now accepts different stars", which is
/// what M4a Task 2 did to every stored measurement (ruling R-M4a-15).
pub fn measurement_subtree(cfg: &StackingConfig) -> serde_json::Value {
    measurement_subtree_with_fit_version(cfg, PSF_FIT_VERSION)
}

/// [`measurement_subtree`] with the fitter version supplied, so a test can
/// prove the hash actually follows it (nothing else should call this with
/// anything but [`PSF_FIT_VERSION`]).
fn measurement_subtree_with_fit_version(
    cfg: &StackingConfig,
    psf_fit_version: u32,
) -> serde_json::Value {
    serde_json::json!({
        "measurement": cfg.measurement,
        "normalization": { "scaleEstimator": cfg.normalization.scale_estimator },
        "psfFitVersion": psf_fit_version,
    })
}

/// Stage 5 (registration) config subtree.
pub fn registration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({ "registration": cfg.registration })
}

/// Stage 6 (local normalization, M2) config subtree: the whole
/// `normalization` block (global output/rejection choice AND the `local`
/// block — a change to either invalidates a stored `.athln`/reference; this
/// already covers the PSF model [`crate::stacking::ln::normalize_frame`]
/// actually hands [`crate::stacking::ln::scale::relative_scale`], which is
/// `normalization.local.psfModel`, never `measurement.psfModel`) plus
/// `measurement.maxStars` — the ONE genuinely extra fold-in, since it is the
/// detection/PSF-fit budget `relative_scale` uses for both planes but lives
/// under `measurement`, not `normalization`. `measurement.psfModel` rides
/// along too (harmless — it just widens what invalidates a sidecar — but
/// plays no role in `relative_scale`'s own model choice; fix round 1, item
/// 11: an earlier version of this comment claimed otherwise), plus
/// [`crate::stacking::psf_signal::PSF_FIT_VERSION`]: `relative_scale` fits
/// its matched stars through the same `fit_stars` /
/// `FitParams::default()` the measurement uses, so a change to what the
/// fitter accepts silently changes every `.athln` — and no config field
/// moves when it does (ruling R-M4a-15).
pub fn normalization_subtree(cfg: &StackingConfig) -> serde_json::Value {
    normalization_subtree_with_fit_version(cfg, PSF_FIT_VERSION)
}

/// [`normalization_subtree`] with the fitter version supplied — see
/// [`measurement_subtree_with_fit_version`].
fn normalization_subtree_with_fit_version(
    cfg: &StackingConfig,
    psf_fit_version: u32,
) -> serde_json::Value {
    serde_json::json!({
        "normalization": cfg.normalization,
        "measurement": {
            "psfModel": cfg.measurement.psf_model,
            "maxStars": cfg.measurement.max_stars,
        },
        "psfFitVersion": psf_fit_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_is_the_default_config() {
        let c: StackingConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, StackingConfig::default());
        assert_eq!(c.version, 1);
        assert_eq!(c.measurement.max_stars, 24576);
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

    #[test]
    fn serde_names_follow_the_spec() {
        let s = serde_json::to_string(&StackingConfig::default()).unwrap();
        assert!(
            !s.contains("splitByExposure"),
            "the toggle is gone — exposure always splits: {s}"
        );
        for needle in [
            "\"exposureToleranceSec\":2.0",
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
            "\"rejection\":\"scaleZeroOffset\"",
            "\"scaleEstimator\":\"bwmv\"",
            "\"local\":{\"enabled\":false,\"scale\":1024,\"referenceFrames\":20,\"psfModel\":\"auto\",\"localScale\":false}",
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
            "\"drizzle\":{\"enabled\":false,\"scale\":2,\"dropShrink\":0.9,\"kernel\":\"square\",\"useRejection\":true,\"useWeights\":true,\"useLocalNormalization\":true,\"writeWeightMap\":false}",
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

    #[test]
    fn stage_hash_is_stable_and_sensitive() {
        let cfg = StackingConfig::default();
        let src = [SourceIdentity {
            file_id: 1,
            size: 10,
            modified_at: "t".into(),
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
                &[SourceIdentity {
                    file_id: 1,
                    size: 11,
                    modified_at: "t".into(),
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
        structure.measurement.seed_detector = SeedDetector::Structure;
        assert_ne!(config_hash(&cfg), config_hash(&structure));
        assert_ne!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            stage_hash(&measurement_subtree(&structure), &[], &[]),
            "the measurement stage hash must follow seedDetector"
        );
        assert_eq!(
            structure.measurement.measure_options(ScaleEstimator::Bwmv).seed_detector,
            SeedDetector::Structure,
            "and the resolved measure options carry it"
        );
    }

    /// Ruling R-M4a-15: the PSF fitter's own behaviour is not expressible
    /// in the config, so the artifact hashes that store fit-derived numbers
    /// fold in [`PSF_FIT_VERSION`] instead. Both of them must: the
    /// measurement stage stores PSFSW/TFlux/star counts, and local
    /// normalization's `.athln` stores a PSF-flux scale measured with the
    /// SAME fitter.
    #[test]
    fn both_fit_derived_subtree_hashes_follow_the_psf_fit_version() {
        let cfg = StackingConfig::default();
        let h = |v: u32| {
            (
                stage_hash(&measurement_subtree_with_fit_version(&cfg, v), &[], &[]),
                stage_hash(&normalization_subtree_with_fit_version(&cfg, v), &[], &[]),
            )
        };
        let (m_now, n_now) = h(PSF_FIT_VERSION);
        let (m_old, n_old) = h(PSF_FIT_VERSION - 1);
        assert_ne!(m_now, m_old, "the measurement hash must follow the fitter");
        assert_ne!(n_now, n_old, "the LN hash must follow the fitter");
        // And the shipped helpers are the versioned ones at the current
        // constant — not a second, drifting copy of the JSON shape.
        assert_eq!(
            stage_hash(&measurement_subtree(&cfg), &[], &[]),
            m_now,
            "measurement_subtree must hash as the current fitter version"
        );
        assert_eq!(
            stage_hash(&normalization_subtree(&cfg), &[], &[]),
            n_now,
            "normalization_subtree must hash as the current fitter version"
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

    #[test]
    fn stage_hash_is_order_insensitive() {
        let cfg = StackingConfig::default();
        let subtree = calibration_subtree(&cfg);
        let s1 = SourceIdentity {
            file_id: 1,
            size: 10,
            modified_at: "t".into(),
        };
        let s2 = SourceIdentity {
            file_id: 2,
            size: 20,
            modified_at: "u".into(),
        };
        let forward = stage_hash(
            &subtree,
            &["a", "b"],
            &[
                SourceIdentity {
                    file_id: s1.file_id,
                    size: s1.size,
                    modified_at: s1.modified_at.clone(),
                },
                SourceIdentity {
                    file_id: s2.file_id,
                    size: s2.size,
                    modified_at: s2.modified_at.clone(),
                },
            ],
        );
        let reversed = stage_hash(&subtree, &["b", "a"], &[s2, s1]);
        assert_eq!(
            forward, reversed,
            "sources/upstream in either order must hash the same"
        );

        let changed = stage_hash(
            &subtree,
            &["a", "b"],
            &[
                SourceIdentity {
                    file_id: 3,
                    size: 10,
                    modified_at: "t".into(),
                },
                SourceIdentity {
                    file_id: 2,
                    size: 20,
                    modified_at: "u".into(),
                },
            ],
        );
        assert_ne!(forward, changed, "a changed file_id still changes the hash");
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
        // calibrate/measure/register stage hashes never fold in
        // `grouping`), only this whole-config fingerprint.
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
        assert_eq!(default_hash, "7b2463cf8463926d");
    }
}
