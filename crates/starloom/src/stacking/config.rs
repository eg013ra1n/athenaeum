//! Stacking configuration: the measurement and drizzle sections, the pure items a caller's whole stacking
//! configuration tree is built from (the caller owns the tree itself and its hashing).

use serde::{Deserialize, Serialize};

use crate::integration::stats::ScaleEstimator;
use crate::stacking::measure::MeasureOptions;
use crate::stacking::prefilter::SeedPrefilter;
use crate::stacking::psf_signal::PsfModel;
use crate::stacking::structure::SeedDetector;
use crate::stacking::weights::{FormulaWeights, WeightMode};

/// The measurement settings of a stacking run, chosen once per run: how frames are weighted, which star profile is
/// fitted and how stars are found (spec §9.2 `measurement:`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MeasurementConfig {
    /// How a frame's measurement becomes its stacking weight.
    pub weight_mode: WeightMode,
    /// The star profile fitted to each detected star.
    #[cfg_attr(feature = "ts", ts(as = "crate::stacking::psf_signal::PsfModelTs"))]
    pub psf_model: PsfModel,
    /// Detection cap: the most stars kept per frame (spec §9.2 `maxStars`; stars, not pixels).
    pub max_stars: usize,
    /// The coefficients of the user formula weight; read only by `WeightMode::Formula`.
    pub formula: FormulaWeights,
    /// FITS keyword `WeightMode::Keyword` reads its value from.
    pub keyword: String,
    /// Star-detection threshold for the quality measurement, in sigma above
    /// the local background (spec §9.2). The measurement detector's two
    /// levels sit at `background + k*noise` and `background + (k/2)*noise`,
    /// so the seed population follows THIS frame's sky instead of a fixed
    /// bright-pixel budget (M4a Task 2, ruling R-M4a-1). Peak only: the
    /// matched filter and the structure map do not read it.
    pub detection_sigma: f64,
    /// What the seed detection runs on (math reference §5.1): the plane as
    /// it is, or its 3×3 median. Everything downstream of detection always
    /// measures the untouched plane. Read by the peak detector only.
    pub seed_prefilter: SeedPrefilter,
    /// Legacy mirror of [`Self::seed_source`] (peak vs structure, R-M4c-11),
    /// kept so older readers of the stored document still find the field. It
    /// steers nothing: the measurement stage reads `seed_source` only.
    /// A stored config written before M4c Task 0 has no such field and
    /// decodes to the shipped default.
    pub seed_detector: SeedDetector,
    /// The seed source (spec §2.2): the star-measurement module's matched filter (default), the peak threshold
    /// (reads `detectionSigma` and `seedPrefilter`), or the structure map. A document without it is decoded by
    /// `legacy_seed_source`; one that carries it uses it as is.
    pub seed_source: SeedSource,
    /// The matched filter's threshold in units of the filtered plane's noise (ruling R-OSP-2). Clamped to
    /// `MIN_SEED_THRESHOLD`..=`MAX_SEED_THRESHOLD` by `resolve_config`.
    pub seed_threshold: f64,
    /// Fast measurement (FM-2, FM-3): the first K seeds in detection order (filter flux descending) get the PSF
    /// fits, the rest keep their detection record and are not fitted; `None` = all (the shipped behaviour). No
    /// imputation: the frame aggregates, `c` and the F-6 medians are over the fitted set. Clamped up to
    /// `max(MIN_FIT_STARS, registration.maxStars)` and canonicalized to `None` at or above
    /// `measurement.maxStars` by `resolve_config`. Absent from the document at `None`, so every All-mode hash is
    /// the one before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub fit_stars: Option<u32>,
}

/// Where the Measure stage takes its star seeds from, the starting points the profile fits refine (spec §2.2, ruling
/// R-OSP-2; R-M4c-11 for the other two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum SeedSource {
    /// The module's two-pass matched filter ([`crate::star_measure::seeds::detect`]) at `seedThreshold`.
    #[default]
    MatchedFilter,
    /// The peak threshold: `detectionSigma` and `seedPrefilter`.
    Peak,
    /// The structure map.
    Structure,
}

impl SeedSource {
    /// The serde name, for logs: `"matchedFilter"` / `"peak"` / `"structure"`.
    pub fn as_str(self) -> &'static str {
        match self {
            SeedSource::MatchedFilter => "matchedFilter",
            SeedSource::Peak => "peak",
            SeedSource::Structure => "structure",
        }
    }
}

/// The one decode for every stored document that has no `seedSource` (set, global, user preset — spec §2.2):
/// Structure → Structure; otherwise `seedPrefilter = median3` or a `detectionSigma` other than the old default
/// (20) → Peak; otherwise → MatchedFilter. A stored value is never reinterpreted in other units.
fn legacy_seed_source(
    detector: Option<SeedDetector>,
    prefilter: Option<SeedPrefilter>,
    sigma: Option<f64>,
) -> SeedSource {
    if detector == Some(SeedDetector::Structure) {
        return SeedSource::Structure;
    }
    let old_sigma = crate::stacking::measure::DEFAULT_DETECTION_SIGMA as f64;
    if prefilter == Some(SeedPrefilter::Median3) || sigma.is_some_and(|s| s != old_sigma) {
        return SeedSource::Peak;
    }
    SeedSource::MatchedFilter
}

/// The wire shape of [`MeasurementConfig`]: every field optional so the decode can tell "absent" from "default"
/// (the legacy decode needs it).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct MeasurementConfigWire {
    weight_mode: Option<WeightMode>,
    psf_model: Option<PsfModel>,
    max_stars: Option<usize>,
    formula: Option<FormulaWeights>,
    keyword: Option<String>,
    detection_sigma: Option<f64>,
    seed_prefilter: Option<SeedPrefilter>,
    seed_detector: Option<SeedDetector>,
    seed_source: Option<SeedSource>,
    seed_threshold: Option<f64>,
    fit_stars: Option<u32>,
}

impl<'de> Deserialize<'de> for MeasurementConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = MeasurementConfigWire::deserialize(d)?;
        let base = MeasurementConfig::default();
        let seed_source = w.seed_source.unwrap_or_else(|| {
            legacy_seed_source(w.seed_detector, w.seed_prefilter, w.detection_sigma)
        });
        Ok(MeasurementConfig {
            weight_mode: w.weight_mode.unwrap_or(base.weight_mode),
            psf_model: w.psf_model.unwrap_or(base.psf_model),
            max_stars: w.max_stars.unwrap_or(base.max_stars),
            formula: w.formula.unwrap_or(base.formula),
            keyword: w.keyword.unwrap_or(base.keyword),
            detection_sigma: w.detection_sigma.unwrap_or(base.detection_sigma),
            seed_prefilter: w.seed_prefilter.unwrap_or(base.seed_prefilter),
            // Kept on the wire for round-trips; it mirrors the source (Structure <=> Structure, else Peak).
            seed_detector: if seed_source == SeedSource::Structure {
                SeedDetector::Structure
            } else {
                SeedDetector::Peak
            },
            seed_source,
            seed_threshold: w.seed_threshold.unwrap_or(base.seed_threshold),
            fit_stars: w.fit_stars,
        })
    }
}

/// The calibrated [`MeasurementConfig::detection_sigma`] default (used by
/// `Default` and by the legacy decode when a stored document has no such
/// field — not to `0.0`).
fn default_detection_sigma() -> f64 {
    crate::stacking::measure::DEFAULT_DETECTION_SIGMA as f64
}

impl Default for MeasurementConfig {
    fn default() -> Self {
        MeasurementConfig {
            weight_mode: WeightMode::PsfSignalWeight,
            psf_model: PsfModel::Auto,
            max_stars: 9_000,
            formula: FormulaWeights::default(),
            keyword: "SSWEIGHT".to_string(),
            detection_sigma: default_detection_sigma(),
            seed_prefilter: crate::stacking::measure::DEFAULT_SEED_PREFILTER,
            seed_detector: SeedDetector::Peak,
            seed_source: SeedSource::MatchedFilter,
            seed_threshold: crate::star_measure::seeds::DEFAULT_THRESHOLD,
            fit_stars: None,
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
            seed_source: self.seed_source,
            seed_threshold: self.seed_threshold,
            // Not a config field: a run picks the detector, not its dials
            // (M4c Task 0, ruling R-M4c-11).
            structure: MeasureOptions::default().structure,
            // The run fits every seed (route A); only the probes opt into the two populations.
            population: None,
            fit_stars: self.fit_stars.map(|k| k as usize),
        }
    }
}

/// The drizzle settings of a stacking run (spec §9.2 `drizzle:`): whether it runs, the output scale, the drop size,
/// the kernel and which maps feed it. Off by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct DrizzleConfig {
    /// Whether the run also produces a drizzled master (default off).
    pub enabled: bool,
    /// Output pixels per input pixel along each axis: 2 gives an image twice as wide and as tall (default 2).
    pub scale: u32,
    /// The drop's side as a fraction of an input pixel, the drizzle `pixfrac`: 1 deposits the whole pixel, smaller
    /// values a smaller drop (default 0.9; dimensionless).
    pub drop_shrink: f64,
    /// The shape each drop deposits its flux with.
    pub kernel: DrizzleKernel,
    /// Leave out the pixels the rejection stage flagged in each frame's rejection bitmap (default on).
    pub use_rejection: bool,
    /// Weight each frame's deposit by its stacking weight (default on).
    pub use_weights: bool,
    /// Whether the drizzle follows local output normalization (default on).
    ///
    /// B8 (M3 final fix wave, M5 ruling): the drizzle follows the plain
    /// master's OUTPUT normalization, so this toggle only has an effect when
    /// local normalization drives output normalization
    /// (`normalization.local.enabled == true`). With it off, the drizzle
    /// falls back to each included frame's own global output-normalization
    /// pair, exactly as [`super::integrate`]'s own engine does for the plain
    /// master - including when LN was computed for rejection alone
    /// (`normalization.rejection == "local"`, the default): those grids never
    /// drive the drizzle's output (pcores review r1, I1).
    pub use_local_normalization: bool,
    /// Also keep the drizzle's weight map, the accumulated deposit weight per output pixel divided by its maximum, so
    /// it lies in [0, 1] (default off).
    pub write_weight_map: bool,
    /// Bayer drizzle: whether each colour is deposited from its own samples of the calibrated mosaic instead of the
    /// debayered planes (default off).
    ///
    /// M4d Task 1 (ruling R-M4d-2): deposit each colour's OWN samples from
    /// the calibrated CFA mosaic instead of the debayered planes'
    /// interpolated ones. Ignored for a mono group (silently — there is no
    /// mosaic to deposit), and it makes stage 1 keep one extra artifact per
    /// OSC frame.
    ///
    /// `#[serde(default)]` rides the struct-level `default`, so every stored
    /// config written before M4d decodes as `false` and no
    /// `STACKING_CONFIG_VERSION` bump is needed. It deliberately does NOT
    /// enter `calibration_subtree`: the debayered artifact is
    /// byte-identical either way, so flipping this must not invalidate a
    /// set's whole calibrated cache — the missing `calibrated_mosaic`
    /// artifact is what makes the run regenerate the pair (`stacking::run`).
    pub bayer: bool,
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
            bayer: false,
        }
    }
}

/// The shape of the drop a drizzle deposits: how an input pixel's flux is spread over the output pixels it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum DrizzleKernel {
    /// A uniform square drop, clipped exactly against each output pixel (the default).
    #[default]
    Square,
    /// A uniform disc inscribed in the drop's square.
    Circle,
    /// A Gaussian falling off from the drop's centre, truncated at the drop's edge.
    Gaussian,
}

/// [`crate::stacking::integrate::LargeScaleRejection::protected_layers`]
/// bounds (M4c Task 3): 1 (the cascade's first median, window 3) to 6
/// (window 65, effective support ≈ 127 px) — the range the Integrate
/// panel's field offers.
pub const MIN_PROTECTED_LAYERS: u8 = 1;
/// The largest `protected_layers` the Integrate panel offers (window 65 pixels, support about 127 pixels).
pub const MAX_PROTECTED_LAYERS: u8 = 6;
/// [`crate::stacking::integrate::LargeScaleRejection::growth`]'s maximum
/// (0 — no dilation — is legitimate).
pub const MAX_GROWTH: u8 = 4;

/// The drizzle deposit kernel's own generation (Tier C item C2, spec
/// `2026-09-20-stacking-compute-tierC-design.md` §3, ruling C-3).
///
/// * `1` — M3..M4d: the exact Sutherland-Hodgman clip of every source
///   pixel's mapped drop against every output pixel it touches.
/// * `2` — Tier C: the same clip, tabulated over a `PHASES x PHASES` grid
///   of sub-pixel phases (per frame for a linear map, per 256-px source
///   tile under distortion). Level-preserving by construction; the drop's
///   mass is split among its neighbours as if the drop sat up to
///   `1 / (2 · PHASES)` = 1/128 of an output pixel from where it really
///   is. `2` names the phase table AS SHIPPED, i.e. at
///   `drizzle::phase_table::PHASES = 64` (ruling C-21). The 32-phase
///   variant of the same kernel never left this branch, so it gets no
///   generation of its own — a generation marks a change in what a
///   RELEASED run produces.
///
/// **Why it rides `config_hash` and no `stage_hash`.** The drizzle
/// stage has no cache: it re-runs from the calibrated frames on every run,
/// so there is no stored artifact for a version bump to invalidate and no
/// per-stage subtree that folds in `drizzle` at all (the four are
/// calibration/measurement/registration/normalization). What the constant
/// DOES have to move is the run-level fingerprint — two runs of the same
/// `StackingConfig` across this change produce different drizzled pixels,
/// and the fingerprint is what a provenance row, a results card and a
/// re-run comparison read to tell one run's settings from another's.
///
/// **One thing does go stale, and it is not an artifact.** The plan gate's
/// memory of which frames a run EXCLUDED for local normalization
/// (`plan::build_plan`'s `ln_runtime_excluded_by_frame`) is gated on
/// `last_run.config_hash == hash`, deliberately — an exclusion is only
/// trusted as "will fail again for the same reason" when it came from a run
/// of this exact config. Moving the fingerprint therefore empties that set
/// once, so on the FIRST plan after a bump an LN-excluded frame reverts to
/// ordinary "no fresh `ln` artifact" staleness and is re-owed one Normalize
/// pass. That costs those frames one pass on one plan and then settles;
/// every stored `stacking_artifacts` row is untouched.
pub const DRIZZLE_KERNEL_VERSION: u32 = 2;
