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

use crate::integration::stats::ScaleEstimator;
use crate::resample::Interpolation;
use crate::stacking::integrate::RejectionChoice;
use crate::stacking::measure::MeasureOptions;
use crate::stacking::psf_signal::PsfModel;
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

/// spec §9.2 `grouping:`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig {
    /// Split an otherwise-matching group by `EXPTIME` (within
    /// `exposure_tolerance_sec`) instead of merging mixed exposures.
    pub split_by_exposure: bool,
    pub exposure_tolerance_sec: f64,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        GroupingConfig {
            split_by_exposure: false,
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
}

impl Default for MeasurementConfig {
    fn default() -> Self {
        MeasurementConfig {
            weight_mode: WeightMode::PsfSignalWeight,
            psf_model: PsfModel::Auto,
            max_stars: 24_576,
            formula: FormulaWeights::default(),
            keyword: "SSWEIGHT".to_string(),
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
        }
    }
}

/// spec §9.2 `reference:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ReferenceConfig {
    pub mode: ReferenceMode,
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
/// `MaximumQuality` implements only the two knobs M1 actually has
/// (`distortion`, `writeRejectionMaps`): the spec's eventual text for this
/// preset also turns on local normalization and 2x drizzle, but
/// `integrate_group` refuses `RejectionNormalization::Local` today and the
/// drizzle stage does not exist yet (M2/M3) — turning either on here would
/// make the preset fail every run until those milestones land, so both stay
/// at the M1 default (off) until then.
pub fn preset(p: StackingPreset) -> StackingConfig {
    match p {
        StackingPreset::Default => StackingConfig::default(),
        StackingPreset::FastPreview => {
            let mut c = StackingConfig::default();
            c.registration.interpolation = Interpolation::Bilinear;
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
pub fn resolve_config(
    set_json: Option<&str>,
    global_json: Option<&str>,
) -> Result<StackingConfig, serde_json::Error> {
    if let Some(set) = set_json {
        return serde_json::from_str(set);
    }
    if let Some(global) = global_json {
        return serde_json::from_str(global);
    }
    Ok(StackingConfig::default())
}

/// xxh3 of the canonical JSON of the whole resolved config — a run-level
/// fingerprint, distinct from the per-stage [`stage_hash`] below.
pub fn config_hash(cfg: &StackingConfig) -> String {
    let json = serde_json::to_string(cfg).expect("StackingConfig always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}

/// Spec §9.3: `config_hash = xxh3(canonical JSON of the stage's config
/// subtree + the upstream hashes it depends on + the source file
/// identities)`. `config_subtree` is one of [`calibration_subtree`],
/// [`measurement_subtree`], [`registration_subtree`] (or a future stage's
/// equivalent) — a `serde_json::Value` map, which this workspace's
/// `serde_json` (no `preserve_order` feature) always serializes with its
/// keys sorted, making the JSON canonical regardless of field-declaration
/// order.
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
    #[derive(Serialize)]
    struct StageHashPayload<'a> {
        config: &'a serde_json::Value,
        upstream: &'a [&'a str],
        sources: &'a [SourceIdentity],
    }
    let payload = StageHashPayload {
        config: config_subtree,
        upstream,
        sources,
    };
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

/// Stage 3 (measurement) config subtree: the measurement settings plus the
/// scale estimator normalization keeps in lockstep with it (see
/// [`MeasurementConfig::measure_options`]).
pub fn measurement_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({
        "measurement": cfg.measurement,
        "normalization": { "scaleEstimator": cfg.normalization.scale_estimator },
    })
}

/// Stage 5 (registration) config subtree.
pub fn registration_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({ "registration": cfg.registration })
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
    }

    #[test]
    fn serde_names_follow_the_spec() {
        let s = serde_json::to_string(&StackingConfig::default()).unwrap();
        for needle in [
            "\"splitByExposure\":false",
            "\"exposureToleranceSec\":2.0",
            "\"weightMode\":\"psfSignalWeight\"",
            "\"psfModel\":\"auto\"",
            "\"minWeightFraction\":0.05",
            "\"excludeOnRegistrationFailure\":true",
            "\"reference\":{\"mode\":\"auto\"}",
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
        ] {
            assert!(s.contains(needle), "{needle} missing in {s}");
        }
    }

    #[test]
    fn presets() {
        let f = preset(StackingPreset::FastPreview);
        assert_eq!(f.registration.interpolation, Interpolation::Bilinear);
        assert!(matches!(
            f.integration.rejection,
            RejectionChoice::SigmaClip { .. }
        ));
        assert_eq!(f.output.cleanup, CleanupPolicy::DeleteIntermediates);
        let m = preset(StackingPreset::MaximumQuality);
        assert_eq!(m.registration.distortion, DistortionChoice::Polynomial3);
        assert!(m.integration.write_rejection_maps);
        assert!(!m.normalization.local.enabled);
        assert!(!m.drizzle.enabled);
        assert_eq!(preset(StackingPreset::Default), StackingConfig::default());
    }

    #[test]
    fn precedence_is_whole_config() {
        let set = Some("{\"measurement\":{\"maxStars\":100}}");
        let global =
            Some("{\"measurement\":{\"maxStars\":200},\"grouping\":{\"splitByExposure\":true}}");
        let c = resolve_config(set, global).unwrap();
        assert_eq!(c.measurement.max_stars, 100);
        assert!(!c.grouping.split_by_exposure, "no field-level merge");
        assert_eq!(
            resolve_config(None, global).unwrap().measurement.max_stars,
            200
        );
        assert_eq!(
            resolve_config(None, None).unwrap(),
            StackingConfig::default()
        );
        assert!(resolve_config(Some("{not json"), None).is_err());
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
}
