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

/// The floor `resolve_config` clamps [`GroupingConfig::exposure_tolerance_sec`]
/// to (fix round 1, minor 5). Below this, two distinct exposure clusters
/// (e.g. a genuine near-zero or negative stored value) can format to the
/// SAME [`crate::calibration_library::paths::fmt_num`] token and collide on
/// `stacking_run_groups`'s `UNIQUE(run_id, group_key)`.
pub const MIN_EXPOSURE_TOLERANCE_SEC: f64 = 0.01;

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
    Ok(config)
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
/// 11: an earlier version of this comment claimed otherwise).
pub fn normalization_subtree(cfg: &StackingConfig) -> serde_json::Value {
    serde_json::json!({
        "normalization": cfg.normalization,
        "measurement": {
            "psfModel": cfg.measurement.psf_model,
            "maxStars": cfg.measurement.max_stars,
        },
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
        let m = preset(StackingPreset::MaximumQuality);
        assert_eq!(m.registration.distortion, DistortionChoice::Polynomial3);
        assert!(m.integration.write_rejection_maps);
        // M3 Task 5, brief test (h): MaximumQuality turns on drizzle 2x AND
        // local normalization (spec §9.2) — both hidden in M1/M2 only
        // because neither stage existed yet.
        assert!(m.normalization.local.enabled);
        assert!(m.drizzle.enabled);
        assert_eq!(m.drizzle.scale, 2);
        assert_eq!(preset(StackingPreset::Default), StackingConfig::default());
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
        assert_eq!(default_hash, "b74baaa1e4322e9f");
    }
}
