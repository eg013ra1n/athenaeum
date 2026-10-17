//! One star-measurement path (spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` §2.1):
//! the stacking Measure stage's per-star fit, per-frame β, frame aggregates, noise and sky statistic as ONE module the
//! stacking adapter (`stacking::measure`), local normalization's refit and — from Plan C — the Analysis tab call.
//! `render`-gated like `analysis`; nothing here depends on `stacking` (the stacking-only seed sources, the peak and
//! structure detectors and the median prefilter, stay adapter-side and hand their seeds in as [`SeedSpec::Given`]).
//!
//! Plan A Task 2 moved the code here verbatim (no arithmetic changed, no operation reordered); the later tasks fix it
//! defect by defect against the synthetic truth lab.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::integration::stats::ScaleEstimator;

pub mod beta;
pub mod budget;
pub mod diagnostics;
pub mod domain;
pub mod fit;
pub mod frame;
pub mod photometry;
pub mod population;
pub mod robust;
pub mod seeds;
pub mod substeps;

pub use beta::{frame_beta, BetaSource, AUTO_BETAS, AUTO_SAMPLE, BETA_BOUNDS};
pub use budget::BudgetReport;
pub use diagnostics::{FitDiagnostics, Outcome, RefusalCounts, RefusalReason};
pub use domain::{
    background_residual, noise_model, noise_mrs, sky_level, to_working_units, Domain, NoiseModel,
    NoiseSource, SkyLevel, ADU_SCALE,
};
pub use fit::{fwtm_from_alpha, FitOutcome, FitParams, PsfModel, Seed, StarFit, StarFlags};
pub use frame::{frame_shape, signal_totals, FrameAggregates, SignalTotals, RCR_LIMIT};
pub use photometry::{
    hfr, model_flux_in_fwtm, star_snr, ApertureNoise, SaturationPlane, SaturationVerdict, SnrTerms,
};
pub use population::{Population, PopulationReport};
pub use seeds::{DetectionReport, FieldWidth, MatchedFilter, DEFAULT_THRESHOLD};
pub use substeps::{Step, SubSteps};

/// The spatial correlation of a plane's pixel noise (Task 10b A1c, ruling C-10): a fact of the pixels' provenance, so
/// the caller states it. Sensor pixels (raw or calibrated, Plan C's CFA super-pixel plane) carry white noise: every
/// lag-≠0 covariance is 0 and the detection noise's gain is 1 by derivation, nothing read from the plane. A debayered,
/// resampled, warped or integrated plane mixed its neighbours: the gain is read from the plane's own noise
/// correlation. `Unknown` is read too: the failure of g := 1 on a mixed plane is a false-seed flood, the failure of
/// reading on a white plane only A1's lower nebula recall, so the safe side is the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelNoise {
    /// Sensor pixels, never spatially mixed: the gain is 1, not read.
    White,
    /// Pixels mixed by debayering, resampling or integration: the gain is read.
    Mixed,
    /// The caller does not know: the gain is read.
    #[default]
    Unknown,
}

impl PixelNoise {
    /// The value the probe and its flags use: `white` | `mixed` | `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            PixelNoise::White => "white",
            PixelNoise::Mixed => "mixed",
            PixelNoise::Unknown => "unknown",
        }
    }

    /// Parse [`Self::as_str`]'s spelling.
    pub fn parse(s: &str) -> Option<PixelNoise> {
        match s {
            "white" => Some(PixelNoise::White),
            "mixed" => Some(PixelNoise::Mixed),
            "unknown" => Some(PixelNoise::Unknown),
            _ => None,
        }
    }

    /// The statement with the debayered-colour fact folded in: a debayered plane is `Mixed`; `White` together with
    /// `colour` is a contradiction (one `warn!`, the gain is read).
    pub fn with_colour(self, colour: bool) -> PixelNoise {
        match (self, colour) {
            (PixelNoise::White, true) => {
                tracing::warn!(
                    "measure: pixel noise stated white on a debayered colour plane; the noise gain is read"
                );
                PixelNoise::Mixed
            }
            (PixelNoise::Unknown, true) => PixelNoise::Mixed,
            (v, _) => v,
        }
    }
}

/// How a plane's aperture SNR read the correlation of its pixel noise (Task 11, ruling R11-1): the probe's
/// `snrNoise`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnrNoise {
    /// The caller stated the pixels white: `kappa` is 1 by derivation, nothing was read.
    White,
    /// The plane's noise correlation was read (the matched filter's own read, or the same read once for given seeds)
    /// and `kappa(A)` sums it over each aperture.
    Read,
    /// A mixed or unknown plane whose correlation could not be read (or which has no seed): `kappa` is 1, the SNR is
    /// the white-pixel value and over-reads on a really correlated plane.
    Unread,
}

impl SnrNoise {
    /// The value the probe writes: `white` | `read` | `unread`.
    pub fn as_str(self) -> &'static str {
        match self {
            SnrNoise::White => "white",
            SnrNoise::Read => "read",
            SnrNoise::Unread => "unread",
        }
    }
}

/// Version of the measurement path's own behaviour — bumped whenever
/// `star_measure::fit`'s `fit_one`, `accept` or the aperture (or anything else in this module) change what a
/// fit accepts or measures; folded as `"measureVersion"` into every artifact hash that stores
/// fit-derived numbers (`measurement_subtree`, `registration_subtree`, `normalization_subtree`).
///
/// Without it a cached artifact cannot tell that the code which produced it
/// has changed: the config it was keyed on is untouched, but the numbers
/// would come out different today. M4a Task 2 is exactly that case — the
/// adaptive sampling region and the inner-region rule moved which stars are
/// accepted, for the quality measurement AND for local normalization's
/// PSF-flux scale, which fits through the same `fit_stars` with
/// `FitParams::default()` (ruling R-M4a-15).
///
/// 1 = the fixed `5σ` stamp with the `0.85·r` centre rule (M1-M3).
/// 2 = the adaptive sampling region + `inner_margin` (M4a Task 2).
/// 3 = no change to what a fit accepts — bumped so every cached `metrics`
/// artifact is treated as stale until it comes with a sibling `fits`
/// artifact (Tier C Task 1, spec §2.2.7): the two are written together from
/// this version on, and a document from before it never carries `fits` at
/// all. (Versions 1-3 were `PSF_FIT_VERSION`, formerly in `stacking::psf_signal`.)
/// 4 = Plan A, the one star-measurement path (this module, F-0…F-7; ruling
/// R-OSP-1, spec §4.4).
///
/// Policy: ONE bump per merged plan that changes what a fit accepts or
/// measures, never one per task; Plan C = 6. `AnalysisConfig::config_hash`
/// is Plan C's.
pub const MEASURE_VERSION: u32 = 4;

/// What the module is handed besides the plane (grows in Tasks 4–12). `Default` = `Domain::Calibrated` without a
/// gain, `PsfModel::Auto`, 24 576 stars, no seeds, `FitParams::default()`, no precomputed noise, `Bwmv` (ruling F14:
/// every test literal uses `..Default::default()` so a later field never breaks an earlier test).
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureOptions {
    pub domain: Domain,
    pub psf_model: PsfModel,
    /// The detection cap of the module's own seed source (Task 6a). [`SeedSpec::Given`] seeds are fitted as handed
    /// in: the caller's detector has already applied its cap.
    pub max_stars: usize,
    pub seeds: SeedSpec,
    pub fit: FitParams,
    /// A noise model the caller already computed (the adapter's Median3 arm hands the MRS σ to its detector
    /// BEFORE detection); `None` = the module computes it. Nothing is computed twice (I-1).
    pub noise: Option<NoiseModel>,
    /// The scale estimator the adapter keeps in lockstep with normalization (PA-21).
    pub scale_estimator: ScaleEstimator,
    /// Keep one [`FitDiagnostics`] per seed in [`PlaneMeasurement::diagnostics`] (F-0). The lab's probe sets it; the
    /// run never does — the per-seed vector costs memory on 24 576 seeds. The refusal counts are kept either way.
    pub diagnostics: bool,
    /// The plane's pixel mask (Task 6a): `mask[i] == false` makes pixel `i` absent, like a non-finite one, for the
    /// matched filter, the field width and the fit (its region and aperture). One plane, one mask; `None` = every
    /// pixel present. The plane's one background mesh (the noise model's `N*` model and the matched filter's background,
    /// Task 10b ruling Q4) leaves the absent pixels out; the noise model's residual sample, MRS and the sky statistic
    /// read the whole plane.
    pub mask: Option<Vec<bool>>,
    /// A field width the caller's detector already measured (Task 6b, Task 6a review M3): the stacking adapter's
    /// matched-filter arm hands the detector's own [`FieldWidth`] with its seeds, so the fit starts from the width
    /// [`SeedSpec::MatchedFilter`] would have given it. `None` = the module's matched filter's own width, or
    /// [`seeds::field_width_of`] the given seeds. Given, it is the width the fit reads and
    /// [`PlaneMeasurement::field_width`] reports; the matched filter's own second pass still runs at its own.
    pub field_width: Option<FieldWidth>,
    /// The plane is one of a debayered colour image's (F-3, Task 8, Q3): the saturation test's plateau needs all 8 of
    /// pixel0's neighbours, else 3. The stacking adapter sets it for a debayered OSC group's planes, the probe for a
    /// 3-plane input; an undebayered CFA plane (Plan C's green sites) keeps the mono rule.
    pub colour: bool,
    /// The spatial correlation of the plane's pixel noise, stated by the caller (Task 10b A1c, ruling C-10);
    /// `White` fixes the detection noise's gain at 1, `Mixed` and `Unknown` (the default) read it. `colour` implies
    /// `Mixed`.
    pub pixel_noise: PixelNoise,
    /// The share of the plane's pixels that are CFA sites of its own colour (Task 11, ruling R11-1): 0.25 / 0.5 / 0.25
    /// for a debayered RGGB plane's R / G / B. A flux-preserving interpolation spreads each site's shot noise over
    /// `1 / share` pixels, so an aperture's shot variance is `1 / share` times the white value. `None` (every plane
    /// whose pixels are sites) = 1.
    pub cfa_site_share: Option<f64>,
    /// Which seeds are fitted (Task 12): every one (`All`, the stacking Measure's path and the default) or, for probes and the lab, a [`Mode::Population`] / [`Mode::Budget`] subset
    /// (the Analysis mode's budget: [`budget`]).
    pub mode: Mode,
}

/// What [`measure_plane`] fits (Plan A Task 12, spec D-3): `All` fits every seed (the stacking Measure's path,
/// byte-identical to before the budget existed); `Budget { budget }` fits the candidates [`budget::select_candidates`]
/// chose and keeps at most `budget` of the accepted fits ([`budget::trim`]), spread over a 4 × 4 grid of the plane. A
/// budget no smaller than the seed count measures exactly what `All` does. `Population { statistic, list }` (Plan A
/// Task 13b; probe / lab only, the stacking Measure runs `All`): the pass fits population (a) - `statistic` isolated, brightness-stratified
/// stars the frame statistic, the ring width and `c` read - and list (b), the first `list` seeds the star-list consumers
/// read ([`population`]); every other detected seed keeps its detection record. It needs the module's own matched
/// filter (the selection reads the filter SNR and the knot flags); given seeds fit every seed with a `warn!`.
///
/// Route A (owner decision 2026-10-17, after the R13b-11 STOP): the stacking Measure runs `All`, not `Population` - the
/// frame weight (PSFSW, `c`, the ring width) and the frame shape (FWHM, eccentricity, the F-6 medians) read every fit
/// exactly as at 5836a970 (so `MEASURE_VERSION` stays 4). `Population` is kept, unused by stacking, for the cycle that
/// replaces the weight and the shape with truth-validated ones that do not need every faint star fitted (and for the
/// probes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    All,
    Budget {
        budget: usize,
    },
    Population {
        statistic: usize,
        list: usize,
    },
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            domain: Domain::Calibrated {
                gain_e_per_unit: None,
            },
            psf_model: PsfModel::Auto,
            max_stars: 24576,
            seeds: SeedSpec::None,
            fit: FitParams::default(),
            noise: None,
            scale_estimator: ScaleEstimator::Bwmv,
            diagnostics: false,
            mask: None,
            field_width: None,
            colour: false,
            pixel_noise: PixelNoise::Unknown,
            cfa_site_share: None,
            mode: Mode::All,
        }
    }
}

/// Wall time per phase the module owns (I-1); the adapter adds `read_ms` and its own `detect_ms` and keeps
/// `ChannelMeasurement.timings` exactly as today. A phase the module did not run (a precomputed noise model, seeds
/// handed in) reads 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseTimings {
    pub background_ms: u64,
    pub noise_ms: u64,
    /// The module's own matched filter ([`SeedSpec::MatchedFilter`], Task 6a).
    pub detect_ms: u64,
    pub fit_ms: u64,
}

/// Where the seeds come from. Task 2: the adapter detects (Peak/Structure) and passes them in; Task 6a: the module's
/// own matched filter ([`seeds::detect`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SeedSpec {
    /// No seeds: nothing is fitted.
    #[default]
    None,
    /// Seeds from the caller's detector, expected brightest-first (the detector's order).
    Given(Vec<Seed>),
    /// The module's two-pass matched filter at the field width, `threshold` in units of the detection noise σ_f
    /// ([`DEFAULT_THRESHOLD`], ruling R-OSP-2; propagated from Task 10b, ruling A1).
    MatchedFilter { threshold: f64 },
}

/// What [`fit_stars_at`] fits with besides the plane, the seeds and β (Task 6a; Tasks 9-11 add fields, so a caller
/// writes `..Default::default()`, ruling F14). `'a` is the borrowed [`Self::footprint`]'s (F-5 round 4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FitStarsOptions<'a> {
    pub fit: FitParams,
    /// `mask[i] == false` makes pixel `i` absent, like a non-finite one, in the fit's region and aperture (Plan C's
    /// CFA green sites); `None` = every pixel present.
    pub mask: Option<Vec<bool>>,
    /// The field width (FWHM, px) the fit is handed — its width-start box has radius `⌈2·field_width⌉` (Task 6b,
    /// PA-12) — as `FitOutcome.field_width` reports it with `from_stars = 0`; `None` = [`seeds::field_width_of`] of
    /// the given seeds.
    pub field_width: Option<f64>,
    /// Keep one [`FitDiagnostics`] per seed in [`FitOutcome::diagnostics`] (Task 6b: the refit path reports them as
    /// `measure_plane` does); empty otherwise.
    pub diagnostics: bool,
    /// Apply the plane-level defocused verdict (owner ruling 2026-10-08): `true` only on the measure path
    /// ([`measure_plane`]); every other caller (LN's refits) keeps the per-star `Donut` refusal alone.
    pub defocus_verdict: bool,
    /// Apply the plane-level trailed verdict (Task 10b, F-10; rulings A6, Q1, Q5: [`fit::TRAIL_REFUSAL_SHARE`] of the
    /// judged seeds refused `CentreMoved` / `RegionTooSmall` / `CentreAtRim`), as [`Self::defocus_verdict`]: `true` only on the measure
    /// path ([`measure_plane`], which always applies it); every other caller (LN's refits) keeps the per-seed refusals
    /// alone. `FitOutcome::trail_share` is reported either way.
    pub trail_verdict: bool,
    /// Where the fits' aperture correction comes from (F-5, Task 10): measured on the plane being fitted (the
    /// default, so LN's two sides each get their own), or the caller's.
    pub flux_correction: FluxCorrection,
    /// The fitted plane's full scale in its own units (F-5 round 4): the aperture correction's clip screen caps the
    /// plane max at it, `range = min(plane max, full_scale) − sky median` (F-3's amplitude clause); `None` = uncapped,
    /// `range = plane max − sky median`. LN passes its native planes' `1.0`; the measure path keeps its own
    /// [`SaturationPlane`] and never reads this.
    pub full_scale: Option<f64>,
    /// The aperture correction's footprint (F-5 round 4): a star qualifies only when every pixel centre within `12F` of
    /// it has `footprint[i] == true` — LN passes its `allowed` region (the eroded coverage or overlap), outside which
    /// the plane holds zero fill and stars it never seeded. Read by the correction and (Task 10b) the ring sky B_i, a
    /// pixel outside it being absent from the ring, never by a fit (that is `mask`'s); borrowed, never copied; `None` =
    /// the whole plane.
    pub footprint: Option<&'a [bool]>,
    /// The ring width `F` (px) every accepted fit's ring sky B_i is read at (`[10F, 12F]` outside every seed's `3F`
    /// disc) and the aperture correction's growth radius `5F` scales with (Task 10b, ruling B3's arm: LN's pair-common
    /// geometry hands both sides `max(F_R, F_T)`); `None` = the plane's own frame FWHM. Never read by a fit.
    pub ring_fwhm: Option<f64>,
    /// The plane's gain in e- per ADU of the working-unit plane (F-6, Task 11, [`Domain::gain_e_per_working_adu`]): the
    /// SNR's shot term; `None` omits it. Never read by a fit.
    pub gain_e_per_working_adu: Option<f64>,
    /// The plane's pixel-noise correlation and shot spreading the SNR's aperture noise reads (F-6, Task 11); the
    /// default is white (`kappa` 1, no spreading), which is what LN's and every refit's SNR is.
    pub aperture_noise: ApertureNoise,
}

impl FitStarsOptions<'_> {
    /// The aperture correction's inputs these options carry (the measure path's saturation plane is never one).
    fn correction(&self) -> fit::Correction<'_> {
        fit::Correction {
            flux: self.flux_correction,
            saturation: None,
            full_scale: self.full_scale,
            footprint: self.footprint,
            ring_fwhm: self.ring_fwhm,
            gain: self.gain_e_per_working_adu,
            noise: self.aperture_noise,
            neighbours: None,
            population: None,
            timers: None,
        }
    }
}

/// The aperture correction `c` a fit call multiplies every accepted fit's model flux by (F-5, Task 10, ruling
/// R-OSP-7): `StarFit::signal = model_flux_in_fwtm × c`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FluxCorrection {
    /// Measure `c` on the plane the call fits ([`photometry::aperture_correction`]); `c = 1` (and
    /// `aperture_correction: None`) when fewer than 5 isolated unsaturated stars qualify.
    #[default]
    FromPlane,
    /// The caller's `c`, applied as is and reported as `FitOutcome::aperture_correction`. One that is not a positive
    /// number is refused with a `warn!` and the plane's own is measured.
    Given(f64),
}

/// One plane's measurement (PA-1: the stacking adapter keeps its own `stacking::measure::PlaneMeasurement`). Energies
/// (`stars`, `frame`, `noise`) are in the module's working units; `sky` is in the plane's native units.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    /// The accepted fits, after the ±1 px duplicate drop.
    pub stars: Vec<StarFit>,
    /// The β every star was fitted at.
    pub beta: f64,
    /// Where `beta` came from (F-4, Task 9): `Fitted { sample }` (the median of the unflagged bright seeds' bounded
    /// free-β verdict fits), `Fallback` (fewer than 8, β 4.0) or `Fixed` (`Moffat4`, `Fixed(b)`).
    pub beta_source: BetaSource,
    pub frame: FrameAggregates,
    pub noise: NoiseModel,
    pub sky: SkyLevel,
    /// Seeds handed to the fitter.
    pub seeds: usize,
    /// The seeds the module's own matched filter found ([`SeedSpec::MatchedFilter`]); empty for given seeds, which
    /// the caller already holds (Task 7: the stacking adapter returns them to the lab's probe).
    pub detected: Vec<Seed>,
    /// The field width the fit started from (Task 6a; the width-start boxes read it from Task 6b): the caller's
    /// [`MeasureOptions::field_width`] when given, else the matched filter's own ([`SeedSpec::MatchedFilter`]), or
    /// [`seeds::field_width_of`] the given seeds (`from_stars = 0` with the first pass's 3 px when no star read).
    pub field_width: FieldWidth,
    /// The seeds not accepted, by reason (F-0): `refused.accounted(stars.len()) == seeds` (ruling F13).
    pub refused: RefusalCounts,
    /// One record per seed, in seed order, when `MeasureOptions::diagnostics` is set; empty otherwise.
    pub diagnostics: Vec<FitDiagnostics>,
    /// The plane is defocused (owner ruling 2026-10-08): `stars` is empty, the accepted fits were refused `Defocused`.
    pub defocused: bool,
    /// The readable seeds refused `Donut` (the verdict's `n_donut`).
    pub defocus_donuts: usize,
    /// The plane is trailed (Task 10b, F-10; rulings A6, Q1, Q5): of at least [`fit::TRAIL_VERDICT_MIN_SEEDS`] judged
    /// seeds, more than [`fit::TRAIL_REFUSAL_SHARE`] were refused `CentreMoved`, `RegionTooSmall` or `CentreAtRim`, so `stars` is
    /// empty, the accepted fits were refused `Trailed` and `aperture_correction` is `None`. Never set on a defocused
    /// plane.
    pub trailed: bool,
    /// The share of the judged seeds refused `CentreMoved`, `RegionTooSmall` or `CentreAtRim` ([`fit::trail_share`]), the verdict's
    /// input; reported on every plane (0 with no seed).
    pub trail_share: f64,
    /// The seeds whose saturation verdict fit failed for a reason other than n ≤ p (F-3, Q4): no flag, no refusal,
    /// out of the β sample.
    pub verdict_fit_failed: usize,
    /// The seeds flagged by the cross-plane rule ([`cross_plane_flags`]: another plane's own flag at their position);
    /// 0 on a single plane.
    pub cross_flags: usize,
    /// The plane's aperture correction `c` (F-5, Task 10): every star's `signal` is its model flux in its FWTM times
    /// it. `None` = no correction (`c = 1`): no fit, or fewer than 5 isolated unsaturated stars qualified.
    pub aperture_correction: Option<f64>,
    /// The qualifying stars `aperture_correction` is the median over (at most 64; under 5 it is `None`).
    pub aperture_stars: usize,
    /// The standard error of `aperture_correction` as the median of its ratios ([`FitOutcome::aperture_se`]).
    pub aperture_se: Option<f64>,
    /// The accepted fits without a ring sky B_i (Task 10b, F-10: fewer than 8 present pixels in their `[10F, 12F]`
    /// ring outside every seed's `3F` disc, or no frame FWHM on the plane).
    pub ring_sky_missing: usize,
    /// The plane's near-clip floor (Task 10c, R10c-10) in range units, `(floor - bg) / range`; `None` = the plane showed
    /// no clipped evidence and has no near-clip class.
    pub clip_floor: Option<f64>,
    /// How many pieces of evidence set the floor ([`SaturationPlane::clip_evidence`]).
    pub clip_evidence: usize,
    /// What the module's matched filter measured besides its seeds (Task 10b, F-10): the detection noise σ_f (propagated
    /// and measured), the noise gain, the structure seeds dropped and the extended flags; `None` for given seeds.
    pub detection: Option<DetectionReport>,
    /// The gain the stars' SNR used, e- per working ADU (F-6, Task 11, ruling R11-6); `None` = the shot term was omitted
    /// (the probe's `snrGain` null).
    pub snr_gain: Option<f64>,
    /// The accepted fits whose SNR is NaN: no ring sky B_i (counted, never silent), no present aperture pixel, or a
    /// non-positive aperture variance.
    pub snr_missing: usize,
    /// How the aperture noise's correlation was obtained ([`SnrNoise`]).
    pub snr_noise: SnrNoise,
    /// `s`, the shot-noise spreading the SNR used (1 / the plane's CFA site share, 1 without one).
    pub shot_spread: f64,
    /// What the budget did (Task 12, [`budget::BudgetReport`]): `Some` exactly under [`Mode::Budget`].
    pub budget: Option<BudgetReport>,
    /// What the two populations did (Plan A Task 13b, [`PopulationReport`]): `Some` exactly when the pass fitted a
    /// [`Population`].
    pub population: Option<PopulationReport>,
    pub timings: PhaseTimings,
    /// The plane's sub-step wall and thread-CPU timers (Plan A Task 13b, 13-P); reported in the debug lines only, never
    /// compared, serialized or read by a number. A plane whose first pass was read back from a stored `.athm` carries
    /// the second pass's alone.
    pub sub_steps: SubSteps,
}

/// Measure one plane: the noise model (unless `opts.noise` carries one), the seeds (the matched filter, or the given
/// ones and their field width), the sky statistic on the native plane, then the fits on the working-unit copy — each
/// seed's saturation verdict first (F-3, Task 8: seeds → field width → verdicts → β sample → fits at β, the plane's
/// inputs from [`saturation_plane`]) — and the frame aggregates over the accepted fits. The fit runs on `pool` when
/// given one, as do the background model, MRS and the matched filter. [`prepare_plane`] then [`finish_plane`] on one
/// working copy; an image of more than one plane goes through both halves with [`cross_plane_flags`] between them.
pub fn measure_plane(
    plane: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    let sub = SubSteps::default();
    let working = {
        let _s = sub.span(Step::WorkCopy);
        domain::to_working_units(plane, &opts.domain)
    };
    match opts.mode {
        Mode::All | Mode::Population { .. } => {
            let prep = prepare_working(plane, &working, w, h, opts, pool, sub);
            finish_working(&working, w, h, opts, prep, pool)
        }
        Mode::Budget { budget } => measure_budget(plane, &working, w, h, opts, budget, pool, sub),
    }
}

/// One plane measured through its verdict pass (F-3; Task 8 fix wave, the cross-plane rule): the noise model, the
/// seeds and their field width, the sky statistic and every seed's verdict-pass record — what [`finish_plane`] needs
/// besides the plane. Holds no plane-sized buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanePrep {
    noise: NoiseModel,
    sky: SkyLevel,
    /// The seeds handed to the fitter: the given ones, or the module's matched filter's.
    seeds: Vec<Seed>,
    /// `seeds` came from the module's matched filter (they are [`PlaneMeasurement::detected`]).
    detected: bool,
    /// The width reported as [`PlaneMeasurement::field_width`].
    field_width: FieldWidth,
    /// The matched filter's report ([`PlaneMeasurement::detection`]); its `extended` flags ride with `seeds`.
    detection: Option<DetectionReport>,
    /// The fit's own width ([`fit::verdict_pass`]), the one the fits at β start from.
    fit_field: FieldWidth,
    /// One per seed, in seed order.
    passes: Vec<fit::SeedPass>,
    /// The plane's saturation inputs the verdicts read; the aperture correction's clip screen reads its range (F-5,
    /// owner ruling R6).
    saturation: SaturationPlane,
    cross_flags: usize,
    /// The plane's noise correlation `rho` the aperture SNR's `kappa(A)` sums (Task 11, ruling R11-1); `None` on a white
    /// plane or where the read failed (`snr_noise` says which).
    snr_rho: Option<[[f64; 5]; 5]>,
    snr_noise: SnrNoise,
    timings: PhaseTimings,
    /// The verdict pass's share of [`PhaseTimings::fit_ms`] (the population selection included).
    verdict_time: Duration,
    /// The two populations (Plan A Task 13b): `Some` exactly when the plane was prepared under [`Mode::Population`]
    /// with a detection to select from. Then `passes` holds one record per FITTED seed (`population.fitted[k]`'s is
    /// `passes[k]`), not per detected seed; `None` = one record per seed. NOT stored: only the probes run
    /// [`Mode::Population`] and the stacking Measure (the one writer of a first-pass artifact) runs [`Mode::All`], so a
    /// stored prep always has `None` and the first-pass layout stays 5836a970's (`PASS1_ARTIFACT_VERSION` 1).
    #[serde(skip)]
    population: Option<Population>,
    /// The plane's sub-step timers (Plan A Task 13b, 13-P): not stored, so a `.athm` read back holds zeros.
    #[serde(skip)]
    sub: SubSteps,
}

/// The seeds the verdict pass ran on, in the order of its records: every seed, or the population's `fitted`.
fn pass_seeds<'a>(
    seeds: &'a [Seed],
    population: Option<&'a Population>,
) -> Box<dyn Iterator<Item = &'a Seed> + 'a> {
    match population {
        Some(p) => Box::new(p.fitted.iter().map(move |&i| &seeds[i])),
        None => Box::new(seeds.iter()),
    }
}

/// The seeds a fit call runs on and their knot flags: all of them (borrowed, no copy) or the population's `fitted`.
fn fit_inputs<'a>(
    seeds: &'a [Seed],
    detection: Option<&'a DetectionReport>,
    population: Option<&Population>,
) -> (Cow<'a, [Seed]>, Cow<'a, [bool]>) {
    let extended = detection.map_or(&[][..], |d| d.extended.as_slice());
    match population {
        None => (Cow::Borrowed(seeds), Cow::Borrowed(extended)),
        Some(p) => (
            Cow::Owned(p.fitted.iter().map(|&i| seeds[i]).collect()),
            Cow::Owned(
                p.fitted
                    .iter()
                    .map(|&i| extended.get(i).copied().unwrap_or(false))
                    .collect(),
            ),
        ),
    }
}

impl PlanePrep {
    fn pass_seeds(&self) -> Box<dyn Iterator<Item = &Seed> + '_> {
        pass_seeds(&self.seeds, self.population.as_ref())
    }

    /// The seeds handed to the fitter (the given ones, or the matched filter's): what the stacking adapter's head
    /// hands the module again as `SeedSpec::Given` when a stored first pass is read back (Plan A Task 11c).
    pub(crate) fn seeds(&self) -> &[Seed] {
        &self.seeds
    }

    /// The verdict pass's wall (a share of [`PhaseTimings::fit_ms`]); read-only, reported in the debug lines only
    /// (Plan A Task 13, 13-P).
    pub(crate) fn verdict_time(&self) -> Duration {
        self.verdict_time
    }

    /// Whether this record can be stored in a `.athm` and read back whole: a [`Mode::Population`] record keeps one
    /// verdict record per FITTED seed and its `population` is not stored, so a stored copy would read back with
    /// `passes` shorter than `seeds` (Plan A Task 13b review, M1).
    pub(crate) fn is_storable(&self) -> bool {
        self.population.is_none()
    }

    /// `(fits, iterations)`: the seeds whose bounded verdict fit succeeded and the sum of their LM iterations
    /// (Plan A Task 13, 13-P; read-only, derived from the stored per-seed records).
    pub(crate) fn verdict_iterations(&self) -> (usize, u64) {
        self.passes
            .iter()
            .filter_map(|p| match &p.verdict {
                Some(Ok(v)) if v.fit => Some(u64::from(v.iterations)),
                _ => None,
            })
            .fold((0, 0), |(n, it), i| (n + 1, it + i))
    }

    /// A hand-built record for the encoding pin (Plan A Task 11c, `the_plane_prep_encoding_is_pinned`): exact binary
    /// fractions only, NaN and infinities where the real records hold them. `starless` = a plane without a seed (no
    /// detection report, the untested saturation plane).
    #[cfg(test)]
    pub(crate) fn for_pin(starless: bool) -> PlanePrep {
        let mut rho = [[0.0f64; 5]; 5];
        for (i, row) in rho.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = if i == 2 && j == 2 {
                    1.0
                } else {
                    (i * 5 + j) as f64 / 128.0
                };
            }
        }
        let seeds = if starless {
            Vec::new()
        } else {
            vec![
                Seed {
                    x: 10.5,
                    y: 20.25,
                    peak: 0.5,
                    flux: 4.0,
                },
                Seed {
                    x: 33.75,
                    y: 8.0,
                    peak: 0.25,
                    flux: 1.5,
                },
            ]
        };
        PlanePrep {
            noise: NoiseModel {
                sigma: 12.5,
                source: NoiseSource::BackgroundResidual,
                m_star: 0.25,
                n_star: 3.125,
            },
            sky: SkyLevel {
                median: 0.0625,
                mad: 0.001953125,
                median_mean_dev: 0.0029296875,
                location: 0.0625,
                scale: 0.00390625,
                noise_scale_low: 1.0,
                noise_scale_high: 1.25,
            },
            seeds,
            detected: !starless,
            field_width: FieldWidth {
                fwhm_px: 3.5,
                from_stars: if starless { 0 } else { 2 },
            },
            detection: (!starless).then(|| DetectionReport {
                sigma_f: 0.5,
                sigma_f_measured: f64::NAN,
                noise_gain: 1.0,
                structure_dropped: 1,
                extended: vec![false, true],
                filter_snr: vec![11.5, f64::INFINITY],
                envelope: seeds::WidthEnvelope {
                    spread_px: 0.25,
                    fwhm_px: 4.0,
                },
                noise_correlation: Some(seeds::NoiseCorrelation::for_pin()),
            }),
            fit_field: FieldWidth {
                fwhm_px: 3.75,
                from_stars: if starless { 0 } else { 2 },
            },
            passes: if starless {
                Vec::new()
            } else {
                vec![
                    fit::SeedPass::for_pin(0),
                    fit::SeedPass::for_pin(1),
                    fit::SeedPass::for_pin(2),
                ]
            },
            saturation: if starless {
                SaturationPlane::UNTESTED
            } else {
                SaturationPlane {
                    bg: 0.0625,
                    threshold: 0.125,
                    norm: 65535.0,
                    range: 0.875,
                    colour: true,
                    plane_max: 1.0,
                    max_count: 3,
                    clip_floor: Some(0.9375),
                    clip_evidence: 2,
                }
            },
            cross_flags: 1,
            snr_rho: (!starless).then_some(rho),
            snr_noise: if starless {
                SnrNoise::Unread
            } else {
                SnrNoise::Read
            },
            timings: PhaseTimings {
                background_ms: 1,
                noise_ms: 2,
                detect_ms: 3,
                fit_ms: 4,
            },
            verdict_time: Duration::new(2, 500),
            // A pin only: an unencoded population (`#[serde(skip)]`), so the encoding pin's bytes stay 5836a970's; a stored
            // record never has one (`is_storable`).
            population: (!starless).then(|| Population {
                fitted: vec![0, 1],
                statistic: vec![1],
                stratum: vec![37],
                list: 1,
            }),
            sub: SubSteps::default(),
        }
    }
}

/// [`measure_plane`]'s first half (Task 8 fix wave): everything up to and including the verdict pass, on its own
/// working-unit copy (dropped on return). `opts` must be the options [`finish_plane`] is then given.
pub fn prepare_plane(
    plane: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlanePrep {
    if matches!(opts.mode, Mode::Budget { .. }) {
        // The budget lives in `measure_plane` alone (R12-2): the stacking halves keep every seed.
        tracing::warn!(
            width = w,
            height = h,
            "measure: the budget applies to measure_plane only; every seed is measured"
        );
    }
    let sub = SubSteps::default();
    let working = {
        let _s = sub.span(Step::WorkCopy);
        domain::to_working_units(plane, &opts.domain)
    };
    prepare_working(plane, &working, w, h, opts, pool, sub)
}

/// [`measure_plane`]'s second half: β and the fits at β from `prep`'s records (cross-plane flags included), the frame
/// aggregates, on a fresh working-unit copy of the same `plane` (the same bytes the first half read).
pub fn finish_plane(
    plane: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    prep: PlanePrep,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    let working = {
        let _s = prep.sub.span(Step::WorkCopy);
        domain::to_working_units(plane, &opts.domain)
    };
    finish_working(&working, w, h, opts, prep, pool)
}

/// The plane's own β and where it came from: exactly the β [`finish_plane`] would fit `prep`'s seeds at under
/// `opts.psf_model` (the one resolution, `fit::resolve_beta`): under `Auto` F-4's E5 estimator on `prep`'s verdict
/// pass, unchanged; under `Moffat4` / `Fixed(b)` the caller's β ([`BetaSource::Fixed`]). On a fresh working-unit copy
/// of the same `plane` `prep` was prepared from; on an image of more than one plane, after [`cross_plane_flags`].
///
/// Plan A Task 11b (owner decision R11b-12): the stacking Measure stage's first pass. Every member of a group is
/// prepared and reads its own β here; the group β (`stacking::psf_signal::group_beta` of the members' own βs) is
/// resolved once; [`finish_plane`] then fits every member at `PsfModel::Fixed(group β)`. `prep` is borrowed, never
/// changed, so the second pass fits the very records this β was read from.
pub fn plane_beta(
    plane: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    prep: &PlanePrep,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (f64, BetaSource) {
    let working = {
        let _s = prep.sub.span(Step::WorkCopy);
        domain::to_working_units(plane, &opts.domain)
    };
    // The seeds the verdict pass ran on (all of them, or population `fitted`) and their knot flags.
    let (fit_seeds, extended) = fit_inputs(
        &prep.seeds,
        prep.detection.as_ref(),
        prep.population.as_ref(),
    );
    // As `finish_working`'s fit call: inside the pool's `install`, so the sample's own pool param is `None`.
    let call = || {
        fit::resolve_beta(
            &working,
            w,
            h,
            &fit_seeds,
            opts.psf_model,
            &opts.fit,
            opts.mask.as_deref(),
            prep.fit_field,
            Some(&prep.passes),
            &extended,
            None,
        )
    };
    let sample = match pool {
        Some(p) => p.install(call),
        None => call(),
    };
    (sample.beta, sample.source)
}

/// The match radius of the cross-plane rule (px, between seed positions, Euclidean): R3's same-star match.
pub const CROSS_PLANE_MATCH_PX: f64 = 1.0;

/// The cross-plane rule (owner ruling 2026-10-09, ruling R-OSP-5): on an image of more than one plane a star flagged on
/// any plane is flagged on every plane at that position — the debayer mixes the channels, so a star saturated in one
/// is saturated in all (R3: every one of the 52 OSC false negatives was a G-saturated star with no plateau at its R or
/// B seed pixel). Given every plane's [`PlanePrep`] (after every plane's verdict pass, before any plane's β sample,
/// frame statistic or fits at β): a seed whose verdict exists (`Some(Ok(_))`, its verdict fit failed or not) and is not
/// flagged is flagged when a seed of ANOTHER plane within [`CROSS_PLANE_MATCH_PX`] carries its own flag
/// ([`SaturationVerdict::cross`]). Only own flags spread — a `MostlySaturated` refusal counts as one — so a second call
/// changes nothing. One plane (or none): nothing changes.
pub fn cross_plane_flags(planes: &mut [PlanePrep]) {
    if planes.len() < 2 {
        return;
    }
    let cell = |s: &Seed| (s.x.floor() as i64, s.y.floor() as i64);
    // Every own flag's position by its 1-px cell: a seed within 1 px of it lies in the 3×3 cells about its own.
    let mut own: HashMap<(i64, i64), Vec<(usize, f64, f64)>> = HashMap::new();
    for (plane, prep) in planes.iter().enumerate() {
        for (s, pass) in prep.pass_seeds().zip(&prep.passes) {
            // A `MostlySaturated` refusal is the seed's own flag too (owner rule: saturated in any channel is
            // saturated in all): the verdict fit had nothing left to fit, which only a saturated core does. A
            // cross-plane flag never spreads again (`!v.cross`), so a second call changes nothing.
            if matches!(pass.verdict, Some(Ok(v)) if v.saturated && !v.cross)
                || matches!(pass.verdict, Some(Err(RefusalReason::MostlySaturated)))
            {
                own.entry(cell(s)).or_default().push((plane, s.x, s.y));
            }
        }
    }
    for (q, prep) in planes.iter_mut().enumerate() {
        let mut n = 0;
        // Borrow `seeds` and `population` beside `passes` (the records are the only thing changed).
        let PlanePrep {
            seeds,
            population,
            passes,
            ..
        } = prep;
        for (s, pass) in pass_seeds(seeds, population.as_ref()).zip(passes.iter_mut()) {
            let Some(Ok(v)) = pass.verdict.as_mut() else {
                continue;
            };
            if v.saturated {
                continue;
            }
            let (cx, cy) = cell(s);
            let flagged_elsewhere = (cx - 1..=cx + 1).any(|gx| {
                (cy - 1..=cy + 1).any(|gy| {
                    own.get(&(gx, gy)).is_some_and(|at| {
                        at.iter().any(|&(p, x, y)| {
                            p != q && (x - s.x).hypot(y - s.y) <= CROSS_PLANE_MATCH_PX
                        })
                    })
                })
            });
            if flagged_elsewhere {
                v.saturated = true;
                v.cross = true;
                n += 1;
            }
        }
        prep.cross_flags += n;
    }
}

/// What [`prepare_head`] measures before any seed is fitted: the noise model, the sky, the saturation plane, the seeds
/// (detected, or given), the field width, the detection report and the aperture SNR's noise read.
struct Head {
    noise: NoiseModel,
    sky: SkyLevel,
    seeds: Vec<Seed>,
    detected: bool,
    field_width: FieldWidth,
    detection: Option<DetectionReport>,
    saturation: SaturationPlane,
    snr_rho: Option<[[f64; 5]; 5]>,
    snr_noise: SnrNoise,
    timings: PhaseTimings,
    sub: SubSteps,
}

/// [`prepare_plane`] on a working copy the caller holds: the head, then the verdict pass over every seed.
fn prepare_working(
    plane: &[f32],
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    sub: SubSteps,
) -> PlanePrep {
    let head = prepare_head(plane, working, w, h, opts, pool, sub);
    // Plan A Task 13b: the two populations are chosen from the detection alone, BEFORE the verdict pass, which then runs
    // over the fitted seeds only (R12-2's order).
    let select_t = Instant::now();
    let population = match opts.mode {
        Mode::Population { statistic, list } => {
            select_population(working, w, h, opts, &head, statistic, list, true)
        }
        _ => None,
    };
    let select_time = select_t.elapsed();
    let (pass_seeds, _) = fit_inputs(&head.seeds, None, population.as_ref());
    let passes = prepare_passes(
        working,
        w,
        h,
        opts,
        pool,
        &pass_seeds,
        head.field_width,
        head.saturation,
    );
    drop(pass_seeds);
    PlanePrep {
        noise: head.noise,
        sky: head.sky,
        seeds: head.seeds,
        detected: head.detected,
        field_width: head.field_width,
        detection: head.detection,
        fit_field: passes.fit_field,
        passes: passes.passes,
        saturation: passes.saturation,
        cross_flags: 0,
        snr_rho: head.snr_rho,
        snr_noise: head.snr_noise,
        timings: head.timings,
        verdict_time: select_time + passes.verdict_time,
        population,
        sub: head.sub,
    }
}

/// The populations' selection ([`population::select`]) over the head's detection; `None` for seeds without a detection
/// report (given seeds carry no filter SNR and no knot flags to select by: every seed is fitted, said once per plane
/// when `fitting`, the selection being the fit's).
fn select_population(
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    head: &Head,
    statistic: usize,
    list: usize,
    fitting: bool,
) -> Option<Population> {
    let Some(detection) = head.detection.as_ref() else {
        if fitting {
            tracing::warn!(
                width = w,
                height = h,
                count = head.seeds.len(),
                "measure: the populations select from the matched filter's detection; every seed is measured"
            );
        }
        return None;
    };
    let pre = population::pre_saturated(
        working,
        opts.mask.as_deref(),
        w,
        h,
        &head.seeds,
        &head.saturation,
    );
    let pop = population::select(
        &head.seeds,
        &detection.filter_snr,
        &detection.extended,
        &pre,
        w,
        h,
        head.field_width.fwhm_px,
        statistic,
        list,
    );
    tracing::debug!(
        seeds = head.seeds.len(),
        statistic = pop.statistic.len(),
        list = pop.list,
        fitted = pop.fitted.len(),
        pre_saturated = pre.iter().filter(|&&p| p).count(),
        plateau_cores = population::plateau_core_count(
            working,
            opts.mask.as_deref(),
            w,
            h,
            &head.seeds,
            &head.saturation
        ),
        "populations selected"
    );
    Some(pop)
}

/// [`prepare_working`]'s first half: noise, mesh, sky, saturation plane, detection and the noise correlation.
fn prepare_head(
    plane: &[f32],
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    sub: SubSteps,
) -> Head {
    let mask = opts.mask.as_deref();
    let pixel_noise = opts.pixel_noise.with_colour(opts.colour);
    let detecting = matches!(opts.seeds, SeedSpec::MatchedFilter { .. });
    // Given seeds on a plane not stated white: the aperture SNR's noise read needs the mesh's noise map (R11-1).
    let given_read = pixel_noise != PixelNoise::White
        && matches!(opts.seeds, SeedSpec::Given(ref g) if !g.is_empty());
    let want_noise = detecting || given_read;
    // The plane's ONE background mesh (Task 10b, ruling Q4): `N*`'s model and the matched filter's background (with
    // its noise map, the detection noise's), built once over the plane's present pixels and handed to both. A plane
    // whose noise model is given and whose seeds are given needs none.
    let t = Instant::now();
    let mut mesh = (opts.noise.is_none() || want_noise).then(|| {
        let build = || {
            let _s = sub.span(Step::Mesh);
            domain::background_mesh(working, w, h, mask, want_noise)
        };
        match pool {
            Some(p) => p.install(build),
            None => build(),
        }
    });
    let mesh_ms = t.elapsed().as_millis() as u64;
    let (noise, mut timings) = match opts.noise {
        Some(given) => (given, PhaseTimings::default()),
        None => {
            let (model, mut phases) =
                domain::noise_model_timed(working, w, h, mesh.as_ref(), pool, Some(&sub));
            phases.background_ms += mesh_ms;
            (model, phases)
        }
    };
    // The saturation test reads the sky median (F-3), so the sky statistic precedes the fits (Q7); the plane's
    // saturation inputs precede detection too, whose knot tests read the plateau-clip clause's range (Task 10b).
    let sky = {
        let _s = sub.span(Step::SkyLevel);
        domain::sky_level(plane, w, h, &opts.domain, opts.scale_estimator)
    };
    let saturation = {
        let submitted = Instant::now();
        let build = || {
            sub.add_wall(Step::SatWait, submitted.elapsed());
            let _s = sub.span(Step::SatPlane);
            saturation_plane(working, mask, &noise, &sky, &opts.domain, opts.colour)
        };
        match pool {
            Some(p) => p.install(build),
            None => build(),
        }
    };
    // A width the caller's detector measured (Task 6b, Task 6a review M3) stands in for the module's own, which is
    // then not computed for given seeds; one that is not a positive number is refused loudly and the module's own is
    // used.
    let given_width = match opts.field_width {
        Some(given) if given.fwhm_px.is_finite() && given.fwhm_px > 0.0 => Some(given),
        Some(bad) => {
            tracing::warn!(
                fwhm_px = bad.fwhm_px,
                from_stars = bad.from_stars,
                "measure: the given field width is not a positive number; the module's own is used"
            );
            None
        }
        None => None,
    };
    let t = Instant::now();
    let (detected, field_width, detection) = match &opts.seeds {
        SeedSpec::None => (
            None,
            given_width.unwrap_or_else(|| seeds::field_width_of(working, w, h, &[], mask)),
            None,
        ),
        SeedSpec::Given(given) => (
            None,
            given_width.unwrap_or_else(|| seeds::field_width_of(working, w, h, given, mask)),
            None,
        ),
        SeedSpec::MatchedFilter { threshold } => {
            // The mesh moves into the detection, which frees its background map once the plane is subtracted.
            let (found, own, report) = seeds::detect_with(
                working,
                w,
                h,
                &noise,
                *threshold,
                opts.max_stars,
                mask,
                pool,
                pixel_noise,
                mesh.take(),
                Some(&saturation),
                Some(&sub),
            );
            timings.detect_ms =
                t.elapsed().as_millis() as u64 + if opts.noise.is_some() { mesh_ms } else { 0 };
            (Some(found), given_width.unwrap_or(own), Some(report))
        }
    };
    let (seeds, detected) = match (detected, &opts.seeds) {
        (Some(found), _) => (found, true),
        (None, SeedSpec::Given(given)) => (given.clone(), false),
        (None, _) => (Vec::new(), false),
    };
    // Under `NoiseSource::None` the saturation test's threshold is the sky median: one `warn!` per plane when there are
    // seeds to judge (Q1).
    if noise.source == NoiseSource::None && !seeds.is_empty() {
        tracing::warn!(
            count = seeds.len(),
            "measure: no noise estimate; the saturation test's threshold is the sky median"
        );
    }
    // The aperture SNR's noise correlation (Task 11, ruling R11-1): none on a plane stated white (derived, nothing read);
    // else the matched filter's own read, or - for seeds handed in - the same read once.
    let (snr_rho, snr_noise) = if pixel_noise == PixelNoise::White {
        (None, SnrNoise::White)
    } else {
        let read = match &detection {
            Some(report) => report.noise_correlation,
            None if !seeds.is_empty() => {
                let mesh =
                    mesh.unwrap_or_else(|| domain::background_mesh(working, w, h, mask, true));
                seeds::read_noise_correlation(working, w, h, mask, &mesh)
            }
            None => None,
        };
        if read.is_none() && !seeds.is_empty() {
            // The matched filter path has already warned (`detect_on`); say it once.
            if detection.is_none() {
                tracing::warn!(
                    count = seeds.len(),
                    width = w,
                    height = h,
                    "snr: the noise correlation could not be read; the aperture noise is taken white"
                );
            } else {
                tracing::debug!(
                    count = seeds.len(),
                    "snr: the matched filter's noise read failed; aperture noise white"
                );
            }
        }
        match read {
            Some(c) => (Some(c.rho()), SnrNoise::Read),
            None => (None, SnrNoise::Unread),
        }
    };
    Head {
        noise,
        sky,
        seeds,
        detected,
        field_width,
        detection,
        saturation,
        snr_rho,
        snr_noise,
        timings,
        sub,
    }
}

/// What [`prepare_passes`] adds: the fit's field width, one verdict-pass record per seed handed in, the saturation plane
/// with its near-clip floor read from those records, and the pass's wall time.
struct Passes {
    fit_field: FieldWidth,
    passes: Vec<fit::SeedPass>,
    saturation: SaturationPlane,
    verdict_time: Duration,
}

/// [`prepare_working`]'s second half: the verdict pass over `seeds` (every seed, or the budget's candidates), the clip
/// floor from their verdicts and the near-clip class.
#[allow(clippy::too_many_arguments)]
fn prepare_passes(
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seeds: &[Seed],
    field_width: FieldWidth,
    saturation: SaturationPlane,
) -> Passes {
    let mask = opts.mask.as_deref();
    let t = Instant::now();
    let pass_call = || {
        fit::verdict_pass(
            working,
            w,
            h,
            seeds,
            &opts.fit,
            mask,
            Some(field_width.fwhm_px),
            &saturation,
            None,
        )
    };
    // Already inside this `install`: the pass's own pool param gets `None` (perf tier 1 Task 2).
    let (fit_field, mut passes) = match pool {
        Some(p) => p.install(pass_call),
        None => pass_call(),
    };
    // The plane's near-clip floor (Task 10c, R10c-10) comes from the plane's own verdicts, so clause 3 is re-read here,
    // before the cross-plane rule; level and fit are untouched.
    let mut saturation = saturation;
    let floor = photometry::derive_clip_floor(
        &saturation,
        passes.iter().filter_map(|p| match &p.verdict {
            Some(Ok(v)) => Some(v),
            _ => None,
        }),
    );
    saturation.clip_floor = floor.map(|(f, _)| f);
    saturation.clip_evidence = floor.map_or(0, |(_, n)| n);
    for pass in passes.iter_mut() {
        if let Some(Ok(v)) = pass.verdict.as_mut() {
            photometry::mark_near_clip(&saturation, v);
        }
    }
    Passes {
        fit_field,
        passes,
        saturation,
        verdict_time: t.elapsed(),
    }
}

/// [`finish_plane`] on a working copy the caller holds.
fn finish_working(
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    prep: PlanePrep,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    let PlanePrep {
        noise,
        sky,
        seeds,
        detected,
        field_width,
        detection,
        fit_field,
        passes,
        saturation,
        cross_flags,
        snr_rho,
        snr_noise,
        mut timings,
        verdict_time,
        population,
        sub,
    } = prep;
    // Plan A Task 13b: under a population the fit call runs on `fitted` (its records are `passes`) and reads every
    // detected seed as a neighbour, as the budget does (R12-5).
    let (fit_seeds, extended) = fit_inputs(&seeds, detection.as_ref(), population.as_ref());
    let (in_statistic, _) = population
        .as_ref()
        .map_or((Vec::new(), Vec::new()), Population::fit_flags);
    let of_fit: &[usize] = population.as_ref().map_or(&[], |p| p.fitted.as_slice());
    // F-6 (Task 11): the SNR's gain, and the shot noise's spreading by a debayered plane's CFA site share (R11-1).
    let snr_gain = opts.domain.gain_e_per_working_adu();
    let shot_spread = opts
        .cfa_site_share
        .filter(|s| s.is_finite() && *s > 0.0 && *s <= 1.0)
        .map_or(1.0, |s| 1.0 / s);
    let t = Instant::now();
    let fit_call = || {
        fit::fit_after_passes(
            working,
            w,
            h,
            &fit_seeds,
            opts.psf_model,
            &opts.fit,
            opts.mask.as_deref(),
            fit_field,
            Some(&passes),
            fit::Correction {
                saturation: Some(&saturation),
                gain: snr_gain,
                noise: ApertureNoise {
                    rho: snr_rho,
                    shot_spread,
                },
                neighbours: population.as_ref().map(|_| fit::Neighbours {
                    seeds: &seeds,
                    of_fit,
                }),
                population: population.as_ref().map(|_| population::PopulationView {
                    statistic: &in_statistic,
                }),
                timers: Some(&sub),
                ..Default::default()
            },
            None,
            opts.diagnostics,
            true,
            true,
            &extended,
        )
    };
    let mut outcome = match pool {
        // Already inside this `install` — `fit_stars`'s own pool param gets
        // `None` so it does not nest a second, pointless `install` inside
        // this one (perf tier 1 Task 2).
        Some(p) => p.install(fit_call),
        None => fit_call(),
    };
    timings.fit_ms = (verdict_time + t.elapsed()).as_millis() as u64;
    let mut refused = std::mem::take(&mut outcome.refused);
    refused.not_selected = seeds.len() - fit_seeds.len();
    debug_assert_eq!(
        refused.accounted(outcome.fits.len()),
        seeds.len(),
        "every seed has exactly one outcome (ruling F13)"
    );
    // One record per detected seed, in seed order: the fit call's records re-indexed to the detected seeds, the ones
    // the populations left out `NotSelected` (as the budget does).
    let mut diagnostics = std::mem::take(&mut outcome.diagnostics);
    if population.is_some() && opts.diagnostics {
        let mut by_seed: Vec<Option<FitDiagnostics>> = vec![None; seeds.len()];
        for mut d in diagnostics {
            let seed = of_fit[d.seed];
            d.seed = seed;
            by_seed[seed] = Some(d);
        }
        diagnostics = by_seed
            .into_iter()
            .enumerate()
            .map(|(seed, d)| d.unwrap_or_else(|| not_selected_record(seed)))
            .collect();
    }
    // The fits' signals already carry the plane's aperture correction (F-5: measured after the duplicate drop, inside
    // the fit call), so the totals read the corrected flux. The frame statistic reads population (a) alone.
    let frame = match &population {
        None => FrameAggregates::of_timed(&outcome.fits, Some(&sub)),
        Some(_) => {
            let in_a: Vec<bool> = outcome.fit_seeds.iter().map(|&k| in_statistic[k]).collect();
            FrameAggregates::of_population(&outcome.fits, &in_a, Some(&sub))
        }
    };
    let population_report = population.as_ref().map(|p| {
        let (mut accepted, mut merged) = (0usize, 0usize);
        for (f, &k) in outcome.fits.iter().zip(&outcome.fit_seeds) {
            if in_statistic[k] {
                accepted += 1;
                merged += usize::from(
                    f.fwhm() > MERGED_PAIR_FWHM_PER_FIELD * field_width.fwhm_px
                        && f.eccentricity() > MERGED_PAIR_ECCENTRICITY,
                );
            }
        }
        PopulationReport {
            statistic: p.statistic.len(),
            list: p.list,
            fitted: p.fitted.len(),
            statistic_accepted: accepted,
            merged_share: if accepted == 0 {
                f64::NAN
            } else {
                merged as f64 / accepted as f64
            },
            aperture_se: outcome.aperture_se,
        }
    });
    let snr_missing = outcome.fits.iter().filter(|f| f.snr.is_nan()).count();
    if snr_missing > 0 {
        tracing::debug!(
            count = snr_missing,
            stars_fitted = outcome.fits.len(),
            "snr: stars without a ring sky have no SNR"
        );
    }
    PlaneMeasurement {
        stars: outcome.fits,
        beta: outcome.beta,
        beta_source: outcome.beta_source,
        frame,
        noise,
        sky,
        seeds: seeds.len(),
        detected: if detected { seeds } else { Vec::new() },
        field_width,
        refused,
        diagnostics,
        defocused: outcome.defocused,
        defocus_donuts: outcome.defocus_donuts,
        trailed: outcome.trailed,
        trail_share: outcome.trail_share,
        verdict_fit_failed: outcome.verdict_fit_failed,
        cross_flags,
        aperture_correction: outcome.aperture_correction,
        aperture_stars: outcome.aperture_stars,
        aperture_se: outcome.aperture_se,
        ring_sky_missing: outcome.ring_sky_missing,
        clip_floor: saturation
            .clip_floor
            .map(|f| (f - saturation.bg) / saturation.range),
        clip_evidence: saturation.clip_evidence,
        detection,
        snr_gain,
        snr_missing,
        snr_noise,
        shot_spread,
        budget: None,
        population: population_report,
        timings,
        sub_steps: sub,
    }
}

/// A fit wider than this many field widths with an eccentricity above [`MERGED_PAIR_ECCENTRICITY`] is a merged pair
/// the seed isolation could not see (the design read's rule; reported as [`PopulationReport::merged_share`]).
const MERGED_PAIR_FWHM_PER_FIELD: f64 = 1.3;
const MERGED_PAIR_ECCENTRICITY: f64 = 0.8;

/// [`measure_plane`] under [`Mode::Budget`] (Task 12, R12-2…R12-6): the head over every seed, the budget's candidates,
/// the verdict pass and the fits on the candidates alone, then the trim. The ring sky and `c` still read every detected
/// seed (R12-5). `Moffat4` and `Fixed(b)` run no β sample; `Auto` samples the candidates' verdicts (R12-1).
fn measure_budget(
    plane: &[f32],
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    budget: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
    steps: SubSteps,
) -> PlaneMeasurement {
    let head = prepare_head(plane, working, w, h, opts, pool, steps);
    let Head {
        noise,
        sky,
        seeds,
        detected,
        field_width,
        detection,
        saturation,
        snr_rho,
        snr_noise,
        mut timings,
        sub: steps,
    } = head;
    let select_t = Instant::now();
    // The shared stratified rank (Task 13b): by the detection's filter SNR, the knot-flagged and pre-fit saturated seeds
    // last; the seeds of a plane without a detection report rank by detector order.
    let pre = population::pre_saturated(working, opts.mask.as_deref(), w, h, &seeds, &saturation);
    let (brightness, skip): (Vec<f64>, Vec<bool>) = match detection.as_ref() {
        Some(d) => (
            d.filter_snr.clone(),
            (0..seeds.len())
                .map(|i| pre[i] || d.extended.get(i).copied().unwrap_or(false))
                .collect(),
        ),
        None => (budget::rank_brightness(seeds.len()), pre),
    };
    let (candidates, _counts) = budget::select_candidates(&seeds, &brightness, &skip, w, h, budget);
    let select_time = select_t.elapsed();
    let seed_cells: Vec<u8> = seeds
        .iter()
        .map(|s| budget::cell_of(s.x, s.y, w, h))
        .collect();
    let sub: Vec<Seed> = candidates.iter().map(|&i| seeds[i]).collect();
    let extended: Vec<bool> = detection.as_ref().map_or(Vec::new(), |d| {
        candidates
            .iter()
            .map(|&i| d.extended.get(i).copied().unwrap_or(false))
            .collect()
    });
    let prepared = prepare_passes(working, w, h, opts, pool, &sub, field_width, saturation);
    let Passes {
        fit_field,
        passes,
        saturation,
        verdict_time,
    } = prepared;
    let snr_gain = opts.domain.gain_e_per_working_adu();
    let shot_spread = opts
        .cfa_site_share
        .filter(|s| s.is_finite() && *s > 0.0 && *s <= 1.0)
        .map_or(1.0, |s| 1.0 / s);
    let t = Instant::now();
    let fit_call = || {
        fit::fit_after_passes(
            working,
            w,
            h,
            &sub,
            opts.psf_model,
            &opts.fit,
            opts.mask.as_deref(),
            fit_field,
            Some(&passes),
            fit::Correction {
                saturation: Some(&saturation),
                gain: snr_gain,
                noise: ApertureNoise {
                    rho: snr_rho,
                    shot_spread,
                },
                neighbours: Some(fit::Neighbours {
                    seeds: &seeds,
                    of_fit: &candidates,
                }),
                timers: Some(&steps),
                ..Default::default()
            },
            None,
            opts.diagnostics,
            true,
            true,
            &extended,
        )
    };
    let mut outcome = match pool {
        Some(p) => p.install(fit_call),
        None => fit_call(),
    };
    // The candidate selection's wall is part of `fit_ms` (M-5), so the timings sum covers it.
    timings.fit_ms = (select_time + verdict_time + t.elapsed()).as_millis() as u64;
    // R12-4: the trim over the accepted fits, each in its SEED's cell.
    let accepted: Vec<usize> = outcome.fit_seeds.iter().map(|&k| candidates[k]).collect();
    let fit_cells: Vec<u8> = accepted.iter().map(|&i| seed_cells[i]).collect();
    let removed = budget::trim(&outcome.fits, &fit_cells, budget);
    let mut is_removed = vec![false; outcome.fits.len()];
    for &k in &removed {
        is_removed[k] = true;
    }
    // New star index of every kept fit (the fit call's order).
    let mut star_of: Vec<Option<usize>> = Vec::with_capacity(is_removed.len());
    let mut kept_seeds: Vec<usize> = Vec::new();
    let mut stars: Vec<StarFit> = Vec::with_capacity(is_removed.len() - removed.len());
    for (k, f) in std::mem::take(&mut outcome.fits).into_iter().enumerate() {
        if is_removed[k] {
            star_of.push(None);
        } else {
            star_of.push(Some(stars.len()));
            kept_seeds.push(accepted[k]);
            stars.push(f);
        }
    }
    let plane_verdict = outcome.defocused || outcome.trailed;
    let report = budget::report(
        &seed_cells,
        &candidates,
        &accepted,
        &kept_seeds,
        plane_verdict,
    );
    let mut refused = std::mem::take(&mut outcome.refused);
    refused.not_selected = seeds.len() - candidates.len();
    refused.trimmed = removed.len();
    debug_assert_eq!(
        refused.accounted(stars.len()),
        seeds.len(),
        "every seed has exactly one outcome (ruling F13)"
    );
    // One record per detected seed, in seed order (R12-2): the candidates' records re-indexed to the detected seeds,
    // `Accepted { star }` renumbered after the trim, the trimmed fits' records kept as `Trimmed`.
    let diagnostics = if opts.diagnostics {
        let mut by_seed: Vec<Option<FitDiagnostics>> = vec![None; seeds.len()];
        for mut d in std::mem::take(&mut outcome.diagnostics) {
            let seed = candidates[d.seed];
            d.seed = seed;
            if let Outcome::Accepted { star } = d.outcome {
                d.outcome = match star_of[star] {
                    Some(star) => Outcome::Accepted { star },
                    None => Outcome::Trimmed,
                };
            }
            by_seed[seed] = Some(d);
        }
        by_seed
            .into_iter()
            .enumerate()
            .map(|(seed, d)| d.unwrap_or_else(|| not_selected_record(seed)))
            .collect()
    } else {
        Vec::new()
    };
    let frame = FrameAggregates::of_timed(&stars, Some(&steps));
    let snr_missing = stars.iter().filter(|f| f.snr.is_nan()).count();
    let ring_sky_missing = stars.iter().filter(|f| f.sky_ring_n == 0).count();
    if snr_missing > 0 {
        tracing::debug!(
            count = snr_missing,
            stars_fitted = stars.len(),
            "snr: stars without a ring sky have no SNR"
        );
    }
    tracing::debug!(
        budget,
        candidates = report.candidates,
        kept = report.kept,
        not_selected = report.not_selected,
        trimmed = report.trimmed,
        cells_with_stars = report.cells_with_stars,
        cells_covered = report.cells_covered,
        "budget applied"
    );
    PlaneMeasurement {
        stars,
        beta: outcome.beta,
        beta_source: outcome.beta_source,
        frame,
        noise,
        sky,
        seeds: seeds.len(),
        detected: if detected { seeds } else { Vec::new() },
        field_width,
        refused,
        diagnostics,
        defocused: outcome.defocused,
        defocus_donuts: outcome.defocus_donuts,
        trailed: outcome.trailed,
        trail_share: outcome.trail_share,
        verdict_fit_failed: outcome.verdict_fit_failed,
        cross_flags: 0,
        aperture_correction: outcome.aperture_correction,
        aperture_stars: outcome.aperture_stars,
        aperture_se: outcome.aperture_se,
        ring_sky_missing,
        clip_floor: saturation
            .clip_floor
            .map(|f| (f - saturation.bg) / saturation.range),
        clip_evidence: saturation.clip_evidence,
        detection,
        snr_gain,
        snr_missing,
        snr_noise,
        shot_spread,
        budget: Some(report),
        population: None,
        timings,
        sub_steps: steps,
    }
}

/// A seed the budget never fitted: the "not reached" values of [`FitDiagnostics`].
fn not_selected_record(seed: usize) -> FitDiagnostics {
    FitDiagnostics {
        seed,
        outcome: Outcome::NotSelected,
        region_radius: 0,
        background_start: f64::NAN,
        width_start: f64::NAN,
        growth_steps: 0,
        pixels_used: 0,
        iterations: None,
        converged: false,
        verdict: None,
        sample_beta: None,
    }
}

/// The plane-level inputs of the saturation test (spec §2.1, PA-23), in working units: `bg` = the sky median
/// (`sky.median`, the native plane's, scaled by `ADU_SCALE` for a calibrated plane), σ = `noise.sigma`, `norm` =
/// `ADU_SCALE` for `Domain::Calibrated` and `full_scale` for `Domain::RawAdu` (Q2), the plane max over the working
/// copy's finite, unmasked pixels. Under [`NoiseSource::None`] σ is 0 and the threshold is the sky median (the caller
/// says so once the seeds are known, Q1). Built before detection (Task 10b: the knot tests' near-clip exemption reads
/// its range), from the same inputs.
fn saturation_plane(
    working: &[f32],
    mask: Option<&[bool]>,
    noise: &NoiseModel,
    sky: &SkyLevel,
    domain: &Domain,
    colour: bool,
) -> SaturationPlane {
    let (scale, norm) = match domain {
        Domain::Calibrated { .. } => (ADU_SCALE as f64, ADU_SCALE as f64),
        Domain::RawAdu { full_scale, .. } => (1.0, *full_scale),
    };
    // The maximum of the finite, unmasked values and how many pixels equal it (an exact clip has two or more): both
    // order-free, so read in one parallel reduce.
    let (plane_max, max_count) = working
        .par_iter()
        .enumerate()
        .filter(|&(i, v)| v.is_finite() && mask.map_or(true, |m| m[i]))
        .map(|(_, &v)| (v as f64, 1usize))
        .reduce(
            || (f64::NEG_INFINITY, 0),
            |a, b| match a.0.partial_cmp(&b.0) {
                Some(std::cmp::Ordering::Greater) => a,
                Some(std::cmp::Ordering::Less) => b,
                _ => (a.0, a.1 + b.1),
            },
        );
    SaturationPlane::new(sky.median * scale, noise.sigma, plane_max, norm, colour)
        .with_max_count(max_count)
}

/// The refit entry point LN uses (PA-6): every seed fitted at `beta`. The plane is in the module's working units
/// (ADU-scaled), exactly as `fit_stars_with_beta` takes it. `opts.mask` makes pixels absent in the region and the
/// aperture; `opts.field_width` (else [`seeds::field_width_of`] the seeds) sizes every star's width-start box (Task
/// 6b, PA-12) and is reported as `FitOutcome.field_width`; `opts.diagnostics` keeps one record per seed in
/// `FitOutcome.diagnostics`; `opts.flux_correction` says where the fits' aperture correction comes from (F-5: the
/// plane's own by default), `opts.full_scale` and `opts.footprint` what it reads (F-5 round 4).
pub fn fit_stars_at(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    opts: &FitStarsOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    fit::fit_stars_with_beta_diagnosed(
        plane,
        w,
        h,
        seeds,
        beta,
        &opts.fit,
        opts.mask.as_deref(),
        opts.field_width,
        pool,
        opts.diagnostics,
        opts.defocus_verdict,
        opts.trail_verdict,
        opts.correction(),
    )
}

/// [`fit_stars_at`] with β from `model` as [`fit::fit_stars`] resolves it (`Auto`: the brightest seeds' sample, Q7;
/// `Moffat4`/`Fixed`: as given) — LN's reference side, so its footprint and full scale reach the aperture correction
/// (F-5 round 4). With default options it is [`fit::fit_stars`].
pub fn fit_stars_with_model(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    opts: &FitStarsOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    fit::fit_stars_diagnosed(
        plane,
        w,
        h,
        seeds,
        model,
        &opts.fit,
        opts.mask.as_deref(),
        opts.field_width,
        pool,
        opts.diagnostics,
        opts.defocus_verdict,
        opts.trail_verdict,
        opts.correction(),
    )
}

/// Every fit of `outcome` — a [`fit_stars_at`] / [`fit_stars_with_model`] result on `plane` with `seeds` and `opts` —
/// re-reads its ring sky B_i, σ_i at the ring width `ring_fwhm`, and the plane's aperture correction is measured again
/// from them, as the call would have measured them with `opts.ring_fwhm = Some(ring_fwhm)` (Task 10b, ruling B3: LN's
/// pair-common geometry knows `max(F_R, F_T)` only once both sides are fitted, and fits nothing twice). Every fit's
/// `signal` becomes its model flux in its FWTM times the new `c`; `aperture_correction`, `aperture_stars` and
/// `ring_sky_missing` are the new ones; no centre, width, β or flag moves.
pub fn correct_at_ring(
    plane: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    outcome: &mut FitOutcome,
    ring_fwhm: f64,
    opts: &FitStarsOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) {
    let correction = fit::Correction {
        ring_fwhm: Some(ring_fwhm),
        ..opts.correction()
    };
    fit::correct_at_ring(
        plane,
        opts.mask.as_deref(),
        w,
        h,
        seeds,
        outcome,
        &correction,
        pool,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_field;
    use crate::test_support::{moffat_field, MoffatStar};

    /// Plan A Task 11c, ruling F28 (a pin, outside RED): the first-pass artifact's body is postcard, which is not
    /// self-describing, so this fails on ANY layout change of a serialised type (a `PlanePrep` field, `RefusalReason`'s
    /// variant order, a derive's field order) until `PASS1_ARTIFACT_VERSION` moves and the literals are re-recorded.
    /// The values are hand-built exact binary fractions: no platform's libm enters.
    #[test]
    fn the_plane_prep_encoding_is_pinned() {
        let bytes =
            postcard::to_stdvec(&[PlanePrep::for_pin(false), PlanePrep::for_pin(true)]).unwrap();
        assert_eq!(
            (bytes.len(), xxhash_rust::xxh3::xxh3_64(&bytes)),
            (1011, 7_237_352_163_525_747_691u64),
            "the layout of a serialised first-pass type changed: bump PASS1_ARTIFACT_VERSION"
        );
    }

    /// F-5 (Task 10): 30 round Moffat stars of `alpha`, amplitude 0.3, on a 70-px grid of a 480 × 400 plane.
    pub(super) fn grid30(alpha: f64) -> Vec<MoffatStar> {
        grid30_spaced(alpha, 70.0, 60.0)
    }

    /// [`grid30`]'s 6 × 5 stars `pitch` px apart, the first at (`origin`, `origin`).
    pub(super) fn grid30_spaced(alpha: f64, pitch: f64, origin: f64) -> Vec<MoffatStar> {
        (0..30)
            .map(|i| MoffatStar {
                x: origin + (i % 6) as f64 * pitch,
                y: origin + (i / 6) as f64 * pitch,
                amp: 0.3,
                alpha_x: alpha,
                alpha_y: alpha,
                theta: 0.0,
            })
            .collect()
    }

    /// One seed per star, at its true centre.
    pub(super) fn seeds_of(stars: &[MoffatStar]) -> Vec<Seed> {
        stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect()
    }

    /// The median of `it` (the mean of the two middle values for an even count).
    pub(super) fn median(it: impl Iterator<Item = f64>) -> f64 {
        let mut v: Vec<f64> = it.collect();
        v.sort_by(f64::total_cmp);
        let n = v.len();
        assert!(n > 0, "the median of nothing");
        if n % 2 == 1 {
            v[n / 2]
        } else {
            0.5 * (v[n / 2 - 1] + v[n / 2])
        }
    }

    #[test]
    fn ln_refit_takes_a_given_correction() {
        let stars = grid30(5.0);
        // F-5 round 4 (the fixture defect of STOP 1 on the no-verdict path): the clip screen's range is the plane max
        // less the sky median, which the 30 equal stars' wings lift, so every one of them fits above it and none
        // qualifies. One range-setting star (1.5x the grid amplitude, rendered, NOT seeded) sets the plane max; it sits
        // 140 px below the grid on a taller plane, outside every grid star's 12F disc and isolation radius.
        let mut field = stars.clone();
        field.push(MoffatStar {
            x: 240.0,
            y: 480.0,
            amp: 0.45,
            ..stars[0]
        });
        let data: Vec<f32> = moffat_field(480, 560, &field, 4.0, 0.05)
            .iter()
            .map(|v| v * ADU_SCALE)
            .collect();
        let own = fit_stars_at(
            &data,
            480,
            560,
            &seeds_of(&stars),
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        let given = fit_stars_at(
            &data,
            480,
            560,
            &seeds_of(&stars),
            4.0,
            &FitStarsOptions {
                flux_correction: FluxCorrection::Given(1.0),
                ..Default::default()
            },
            None,
        );
        assert!(
            own.aperture_correction.unwrap() > 1.05,
            "{:?}",
            own.aperture_correction
        );
        assert!(
            (given.fits[0].signal * own.aperture_correction.unwrap() / own.fits[0].signal - 1.0)
                .abs()
                < 1e-9
        );
    }

    /// F-5 round 3 (owner ruling R6 / C6): on a path without a saturation verdict (`fit_stars_at`) a clipped star — a
    /// flat-topped core at the plane max, its fitted amplitude above the range its own plane gives (`plane max − sky
    /// median`: no full scale given, round 4) — does not enter the aperture correction: the qualifying count and `c`
    /// equal those of the same call without its seed (the same plane; the field width given, so every other fit is the
    /// same).
    #[test]
    fn a_clipped_star_is_screened_out_of_the_aperture_correction_without_a_verdict() {
        let (w, h) = (480usize, 560usize);
        let stars = grid30(5.0);
        // Far below the grid (≥ 143 px from every grid star): its light, seed disc and isolation touch none of them.
        let clipped = MoffatStar {
            x: 240.0,
            y: 480.0,
            amp: 1.5,
            ..stars[0]
        };
        let mut field = stars.clone();
        field.push(clipped);
        let data: Vec<f32> = moffat_field(w, h, &field, 4.0, 0.05)
            .iter()
            .map(|v| v.min(1.0) * ADU_SCALE)
            .collect();
        let opts = FitStarsOptions {
            field_width: Some(4.35),
            ..Default::default()
        };
        let mut with_seeds = seeds_of(&stars);
        with_seeds.push(Seed {
            x: clipped.x,
            y: clipped.y,
            peak: 1.0,
            flux: 1.0,
        });
        let with = fit_stars_at(&data, w, h, &with_seeds, 4.0, &opts, None);
        let without = fit_stars_at(&data, w, h, &seeds_of(&stars), 4.0, &opts, None);
        // The premise: the clipped star is fitted, and its amplitude tops the range (plane max 1.0, sky ≥ 0.05).
        let fit = with
            .fits
            .iter()
            .find(|f| (f.x - clipped.x).hypot(f.y - clipped.y) < 1.0)
            .expect("the clipped star is fitted");
        assert!(
            fit.amplitude > 0.95 * ADU_SCALE as f64,
            "amplitude {}",
            fit.amplitude
        );
        assert_eq!(without.aperture_stars, 30);
        assert_eq!(with.aperture_stars, without.aperture_stars);
        let (cw, co) = (
            with.aperture_correction.unwrap(),
            without.aperture_correction.unwrap(),
        );
        assert!((cw / co - 1.0).abs() < 1e-9, "c {cw} vs {co}");
    }

    /// A prep whose seeds and verdict-pass records are the given ones (the noise model and sky statistic come from a
    /// small real plane; the cross-plane rule reads neither).
    fn prep_with(seeds: &[(f64, f64, fit::SeedVerdict)]) -> PlanePrep {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let mut prep = prepare_plane(&data, w, h, &MeasureOptions::default(), None);
        prep.seeds = seeds
            .iter()
            .map(|&(x, y, _)| Seed {
                x,
                y,
                peak: 1.0,
                flux: 1.0,
            })
            .collect();
        prep.passes = seeds.iter().map(|s| fit::SeedPass::for_test(s.2)).collect();
        prep
    }

    fn verdict(saturated: bool) -> fit::SeedVerdict {
        Some(Ok(SaturationVerdict {
            plateau: false,
            level: 1.0,
            excluded: 0,
            amplitude: 1.0,
            beta: 2.5,
            fwhm: 3.0,
            saturated,
            by_amplitude: saturated,
            by_plateau_clip: false,
            by_near_clip: false,
            pixel0: f64::NAN,
            top3x3: f64::NEG_INFINITY,
            fit: true,
            cross: false,
            iterations: 12,
            centred: true,
        }))
    }

    fn flags(prep: &PlanePrep) -> Vec<(bool, bool)> {
        prep.passes
            .iter()
            .map(|p| match p.verdict {
                Some(Ok(v)) => (v.saturated, v.cross),
                _ => (false, false),
            })
            .collect()
    }

    #[test]
    fn cross_plane_match_edges() {
        // Plane 0 holds the own flag at (100, 100). Plane 1: exactly 1.0 px away (matches), 1.01 px (does not), two
        // candidates within 1 px of one flag (one flag each, no double count), the 3x3-cell edge across a cell border.
        let mut planes = [
            prep_with(&[(100.0, 100.0, verdict(true)), (200.5, 200.0, verdict(true))]),
            prep_with(&[
                (101.0, 100.0, verdict(false)),
                (100.0, 101.01, verdict(false)),
                (200.0, 200.0, verdict(false)),
                (200.9, 200.0, verdict(false)),
                (300.0, 300.0, verdict(false)),
            ]),
        ];
        cross_plane_flags(&mut planes);
        assert_eq!(
            flags(&planes[1]),
            vec![
                (true, true),
                (false, false),
                (true, true),
                (true, true),
                (false, false)
            ]
        );
        assert_eq!(planes[1].cross_flags, 3);
        // The own-flag plane is unchanged: no flag of plane 1 existed to spread.
        assert_eq!(flags(&planes[0]), vec![(true, false), (true, false)]);
        assert_eq!(planes[0].cross_flags, 0);
        // A flag on two planes at one position: the third plane takes it once; both flagged planes stay own.
        let mut three = [
            prep_with(&[(50.0, 50.0, verdict(true))]),
            prep_with(&[(50.4, 50.0, verdict(true))]),
            prep_with(&[(50.2, 50.0, verdict(false))]),
        ];
        cross_plane_flags(&mut three);
        assert_eq!(flags(&three[0]), vec![(true, false)]);
        assert_eq!(flags(&three[1]), vec![(true, false)]);
        assert_eq!(flags(&three[2]), vec![(true, true)]);
        assert_eq!(three[2].cross_flags, 1);
    }

    #[test]
    fn cross_plane_flags_is_idempotent_and_only_own_flags_spread() {
        // Plane 0 own flag at A; plane 1 has a seed at A (takes the cross flag) and another 0.9 px further on, which
        // only the (cross) flag at A could reach: it must stay unflagged, on the first call and on the second.
        let mut planes = [
            prep_with(&[(10.0, 10.0, verdict(true))]),
            prep_with(&[(10.5, 10.0, verdict(false)), (11.4, 10.0, verdict(false))]),
        ];
        cross_plane_flags(&mut planes);
        let once: Vec<_> = planes.iter().map(flags).collect();
        let counts: Vec<_> = planes.iter().map(|p| p.cross_flags).collect();
        assert_eq!(once[1], vec![(true, true), (false, false)]);
        cross_plane_flags(&mut planes);
        assert_eq!(planes.iter().map(flags).collect::<Vec<_>>(), once);
        // The counts accumulate what each call set: the second sets nothing and spreads nothing.
        assert_eq!(counts, vec![0, 1]);
        assert_eq!(
            planes.iter().map(|p| p.cross_flags).collect::<Vec<_>>(),
            counts
        );
    }

    #[test]
    fn a_mostly_saturated_refusal_is_an_own_flag_for_the_other_planes() {
        let refused: fit::SeedVerdict = Some(Err(RefusalReason::MostlySaturated));
        let mut planes = [
            prep_with(&[
                (30.0, 30.0, refused),
                (60.0, 60.0, Some(Err(RefusalReason::Donut))),
            ]),
            prep_with(&[(30.3, 30.0, verdict(false)), (60.0, 60.0, verdict(false))]),
        ];
        cross_plane_flags(&mut planes);
        // The refused seed itself stays refused (no verdict to flag); the star at its position is flagged on the
        // other plane. A Donut refusal is no flag.
        assert!(matches!(
            planes[0].passes[0].verdict,
            Some(Err(RefusalReason::MostlySaturated))
        ));
        assert_eq!(flags(&planes[1]), vec![(true, true), (false, false)]);
        assert_eq!(planes[1].cross_flags, 1);
    }

    #[cfg(feature = "solver")]
    #[test]
    fn module_measure_equals_the_adapter_bit_for_bit() {
        use crate::stacking::measure::{
            measure_plane_detailed, measure_plane_with_seeds, MeasureOptions as AdapterOptions,
            SeedOverride,
        };
        let (data, w, h) = fixture_field(2026_09_19, 1.0, 0.002); // 150 Gaussian stars, σ 1.8 px, bg 0.08, noise 0.002
        let adapter = measure_plane_with_seeds(
            &data,
            w,
            h,
            &AdapterOptions::default(),
            None,
            SeedOverride::Fast,
        );
        let (pm, seeds) = measure_plane_detailed(
            &data,
            w,
            h,
            &AdapterOptions::default(),
            None,
            SeedOverride::Fast,
        );
        let opts = MeasureOptions {
            seeds: SeedSpec::Given(seeds),
            ..Default::default()
        };
        let m = measure_plane(&data, w, h, &opts, None);
        assert_eq!(m.stars, pm.fits);
        assert_eq!(m.beta, adapter.channel.beta);
        assert_eq!(m.frame.fwhm_px.unwrap(), adapter.channel.fwhm_px);
        assert_eq!(m.frame.tflux / ADU_SCALE as f64, adapter.channel.tflux);
        assert_eq!(m.noise.sigma / ADU_SCALE as f64, adapter.channel.noise);
        assert_eq!(m.sky.median, adapter.channel.median);
        assert_eq!(m.sky.scale, adapter.channel.scale);
        assert_eq!(m.sky.noise_scale_high, adapter.channel.noise_scale_high);
    }

    /// A pin (ruling F28: no plane was defocused before the rule): the Gaussian-star `fixture_field` is not defocused
    /// and keeps its stars.
    #[test]
    fn fixture_field_is_not_defocused() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let seeds = test_seeds_of(&data, w, h);
        let n = seeds.len();
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.seeds, n);
        assert!(!m.defocused, "{:?}", m.refused);
        assert!(m.stars.len() >= 100, "{}", m.stars.len());
        assert!(!m.refused.by_reason.contains_key(&RefusalReason::Defocused));
    }

    /// Ruling A4 (D2: on the trailed L 0010 all 64 sample seeds were trail segments the fit refuses): the β sample takes
    /// only seeds whose verdict fit stays centred. RED today: the 25 bright seeds, each 4.5 px off its β-1.8 star (outside
    /// its half-maximum contour, semi-axis 2.5 px; re-pinned from 1.8 px at ruling A4-2, when the fixed 1.5-px tolerance went), join the sample and pull the frame β to ≈ 1.8. Premise check first: if HEAD already
    /// reads β within 0.3 of 4, this is a pin — STOP with the numbers.
    #[test]
    fn beta_sample_takes_only_seeds_whose_verdict_fit_stays_centred() {
        use crate::test_support::{add_noise, integrated_field, IntegratedProfile};
        let (w, h) = (480usize, 400usize);
        let at = |i: usize| (40.0 + (i % 8) as f64 * 57.0, 40.0 + (i / 8) as f64 * 80.0);
        let bright: Vec<(f64, f64, f64)> = (0..25)
            .map(|i| {
                let (x, y) = at(i);
                (x, y, 0.4)
            })
            .collect();
        let faint: Vec<(f64, f64, f64)> = (25..40)
            .map(|i| {
                let (x, y) = at(i);
                (x, y, 0.2)
            })
            .collect();
        let a = integrated_field(
            w,
            h,
            &bright,
            IntegratedProfile::Moffat {
                fwhm: 5.0,
                beta: 1.8,
            },
            5,
            0.05,
        );
        let b = integrated_field(
            w,
            h,
            &faint,
            IntegratedProfile::Moffat {
                fwhm: 5.0,
                beta: 4.0,
            },
            5,
            0.0,
        );
        let mut data: Vec<f32> = a.iter().zip(&b).map(|(p, q)| p + q).collect();
        add_noise(&mut data, 0.002, 21);
        let seeds: Vec<Seed> = bright
            .iter()
            .map(|&(x, y, p)| Seed {
                x: x + 4.5,
                y,
                peak: p,
                flux: 2.0,
            })
            .chain(faint.iter().map(|&(x, y, p)| Seed {
                x,
                y,
                peak: p,
                flux: 1.0,
            }))
            .collect();
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds),
                diagnostics: true,
                ..Default::default()
            },
            None,
        );
        assert!(
            (m.beta - 4.0).abs() < 0.3,
            "frame β {} from the 15 centred stars",
            m.beta
        );
        assert!(
            m.diagnostics[..25].iter().all(|d| d.sample_beta.is_none()),
            "no displaced seed in the sample"
        );
        assert!(
            m.diagnostics[25..]
                .iter()
                .filter(|d| d.sample_beta.is_some())
                .count()
                >= 8
        );
    }

    /// Ruling P3 (D2's full proposal): an `extended` seed stays a seed and is fitted, but leaves `c`'s qualifiers and the
    /// frame FWHM/ecc statistic as it leaves the β sample — the saturation flag's treatment. A Gaussian blob of FWHM 3F
    /// (F = 4.35 px, `grid30(5.0)` at β 4) and peak 0.05 (25σ; zero-sum ratio ≈ 1.48, the extended replica above) sits
    /// ≈ 50 px from its four neighbours (outside `c`'s 6F isolation). RED today: the blob's accepted fit is a 31st
    /// qualifier and pulls the frame FWHM. Premise check first: if HEAD refuses the blob's fit (no 31st qualifier, frame
    /// FWHM already within 1 %), this is a pin — STOP with the numbers.
    #[test]
    fn an_extended_seed_leaves_cs_qualifiers_and_the_frame_statistic() {
        use crate::test_support::{add_noise, gaussian_field};
        let stars = grid30(5.0);
        let plane = |blob: bool| {
            let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
            if blob {
                let f = 2.0 * 5.0 * (2f64.powf(0.25) - 1.0).sqrt();
                let g = gaussian_field(
                    480,
                    400,
                    &[(95.3, 94.8, 0.05)],
                    3.0 * f / 2.354_820_045,
                    0.0,
                );
                for (d, b) in data.iter_mut().zip(g) {
                    *d += b;
                }
            }
            add_noise(&mut data, 0.002, 35);
            let opts = MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                psf_model: PsfModel::Fixed(4.0),
                ..Default::default()
            };
            measure_plane(&data, 480, 400, &opts, None)
        };
        let (base, m) = (plane(false), plane(true));
        assert_eq!(
            base.aperture_stars, 30,
            "premise: the 30 stars qualify without the blob"
        );
        assert_eq!(m.aperture_stars, 30, "the extended blob is not a qualifier");
        let (fb, fm) = (base.frame.fwhm_px.unwrap(), m.frame.fwhm_px.unwrap());
        assert!(
            (fm / fb - 1.0).abs() < 0.01,
            "frame FWHM {fm} vs {fb} without the blob"
        );
        let blob = m
            .stars
            .iter()
            .find(|s| (s.x - 95.3).abs() < 2.0 && (s.y - 94.8).abs() < 2.0)
            .expect("the blob is seeded and fitted");
        assert!(blob.flags.contains(StarFlags::EXTENDED), "{:?}", blob.flags);
    }

    /// Ruling Q4 (Task 10b step 1b, a pin): the plane's ONE background mesh changes no bit. Its background map is the
    /// same bytes with or without the noise map, on the whole plane and on a masked one (the mesh the matched filter
    /// built before); `N*` read against it is the noise model's own; and `measure_plane`'s seeds — the matched filter
    /// on the shared mesh — are exactly `seeds::detect`'s, which builds its own.
    #[test]
    fn one_background_mesh_per_plane_changes_no_bit() {
        use astroimage::analysis::background::estimate_background_mesh;
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let scaled: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let mask: Vec<bool> = (0..w * h).map(|i| (i % w) % 7 != 3).collect();
        let masked: Vec<f32> = scaled
            .iter()
            .zip(&mask)
            .map(|(&v, &k)| if k { v } else { f32::NAN })
            .collect();
        for (input, m) in [(&scaled, None), (&masked, Some(mask.as_slice()))] {
            let old =
                estimate_background_mesh(input, w, h, domain::BACKGROUND_MODEL_CELL_PX, false)
                    .background_map
                    .unwrap();
            let new = domain::background_mesh(&scaled, w, h, m, true);
            let map = new.background.as_deref().unwrap();
            assert!(old.iter().zip(map).all(|(a, b)| a.to_bits() == b.to_bits()));
            assert_eq!(new.noise.as_ref().map(Vec::len), Some(w * h));
        }
        let mesh = domain::background_mesh(&scaled, w, h, None, true);
        let (own, _) = domain::noise_model(&scaled, w, h, None);
        let (shared, _) = domain::noise_model_with(&scaled, w, h, Some(&mesh), None);
        assert_eq!(own, shared);
        let (seeds, field, _) = seeds::detect(
            &scaled,
            w,
            h,
            &own,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.noise, own);
        assert_eq!(m.detected, seeds);
        assert_eq!(m.field_width, field);
    }

    /// Ruling Q4 / D-1 (recorded, not reverted): on a MASKED plane `N*` reads the masked mesh — an absent pixel (a
    /// colour plane's other-colour CFA site, Plan C) is not sky, so the model `L` the residual `L − v` is taken
    /// against must not average it in. This pins that defined value: `measure_plane`'s `N*` with a mask is
    /// `noise_model_with` against the mesh built over the present pixels, bit for bit, and (`M*` and `N*` both
    /// being read from the same sample) the unmasked mesh's value is a different number on this fixture. The
    /// no-mask case stays the bytes of the pre-Q4 code (the test above).
    #[test]
    fn on_a_masked_plane_n_star_reads_the_masked_mesh() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let mask: Vec<bool> = (0..w * h).map(|i| (i % w) % 7 != 3).collect();
        let scaled: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let masked_mesh = domain::background_mesh(&scaled, w, h, Some(&mask), true);
        let (want, _) = domain::noise_model_with(&scaled, w, h, Some(&masked_mesh), None);
        let (unmasked, _) = domain::noise_model(&scaled, w, h, None);
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                mask: Some(mask),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.noise, want);
        assert_ne!(
            m.noise.n_star.to_bits(),
            unmasked.n_star.to_bits(),
            "the masked mesh moved N* on this fixture"
        );
    }

    #[test]
    fn a_precomputed_noise_model_is_used_and_not_recomputed() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let given = NoiseModel {
            sigma: 123.0,
            source: NoiseSource::Mrs,
            m_star: 1.0,
            n_star: 2.0,
        };
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                noise: Some(given),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.noise, given);
        assert_eq!(m.timings.noise_ms, 0);
        let fresh = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert_ne!(fresh.noise.sigma, 123.0);
    }

    #[test]
    fn sky_level_clips_in_the_planes_own_domain() {
        // the same field as ADU: the RawAdu statistics equal the Calibrated ones × 65535 (clip [1, clip−1] ↔ [CLIP_LO, CLIP_HI])
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let adu: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let a = sky_level(
            &data,
            w,
            h,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
            ScaleEstimator::Bwmv,
        );
        let b = sky_level(
            &adu,
            w,
            h,
            &Domain::RawAdu {
                clip: 65535.0,
                full_scale: 65535.0,
                gain_e_per_adu: None,
            },
            ScaleEstimator::Bwmv,
        );
        assert!((b.median / ADU_SCALE as f64 - a.median).abs() < 1e-6 * a.median);
        assert!((b.mad / ADU_SCALE as f64 - a.mad).abs() < 1e-6 * a.mad);
        assert!(b.median > 1000.0, "the ADU sample is not empty");
    }

    /// A pin, not RED (ruling F28): after the move the wrapper and the entry point share one body. The real pins of
    /// this task are the probe parity gate and `measure.rs`'s literals.
    #[test]
    fn fit_stars_at_equals_fit_stars_with_beta() {
        use crate::test_support::{moffat_field, MoffatStar};
        let stars: Vec<MoffatStar> = (0..20)
            .map(|i| MoffatStar {
                x: 20.0 + (i % 5) as f64 * 40.3,
                y: 20.0 + (i / 5) as f64 * 40.7,
                amp: 0.2,
                alpha_x: 5.0,
                alpha_y: 5.0,
                theta: 0.0,
            })
            .collect();
        let data = moffat_field(220, 180, &stars, 4.0, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        // Plan A Task 6a (ruling F14): `fit_stars_at` takes `&FitStarsOptions` in place of `&FitParams`.
        let a = fit_stars_at(
            &data,
            220,
            180,
            &seeds,
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        let b = fit::fit_stars_with_beta(&data, 220, 180, &seeds, 4.0, &FitParams::default(), None);
        assert_eq!(a, b);
        assert_eq!(a.fits.len(), 20);
    }

    /// RawAdu's noise-scale factors are taken on `[2, clip − 2]` ADU, the unit domain's
    /// `[NOISE_CLIP_LO, NOISE_CLIP_HI]` in the plane's own counts: the same field as ADU gives the calibrated
    /// factors, location and scale × 65535 (the unit-domain range on an ADU sample would empty it and read 0).
    #[test]
    fn raw_adu_noise_scale_factors_location_and_scale_are_the_calibrated_ones_in_adu() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let adu: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
        let a = sky_level(
            &data,
            w,
            h,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
            ScaleEstimator::Bwmv,
        );
        let b = sky_level(
            &adu,
            w,
            h,
            &Domain::RawAdu {
                clip: 65535.0,
                full_scale: 65535.0,
                gain_e_per_adu: None,
            },
            ScaleEstimator::Bwmv,
        );
        let s = ADU_SCALE as f64;
        for (what, got, want) in [
            ("noise_scale_low", b.noise_scale_low, a.noise_scale_low),
            ("noise_scale_high", b.noise_scale_high, a.noise_scale_high),
            ("location", b.location, a.location),
            ("scale", b.scale, a.scale),
            ("median_mean_dev", b.median_mean_dev, a.median_mean_dev),
        ] {
            assert!(
                want > 0.0,
                "{what}: the calibrated value is measured ({want})"
            );
            assert!(
                (got / s - want).abs() < 1e-5 * want,
                "{what}: RawAdu {got} / 65535 vs Calibrated {want}"
            );
        }
    }

    /// The working units: a calibrated plane is the ADU-scaled copy the stacking Measure stage always made, a raw
    /// plane is used as it is.
    #[test]
    fn working_units_scale_a_calibrated_plane_and_borrow_a_raw_one() {
        let plane = [0.5f32, 1.0, 0.25];
        let cal = to_working_units(
            &plane,
            &Domain::Calibrated {
                gain_e_per_unit: None,
            },
        );
        assert_eq!(&*cal, &[0.5 * ADU_SCALE, ADU_SCALE, 0.25 * ADU_SCALE]);
        let raw = to_working_units(
            &plane,
            &Domain::RawAdu {
                clip: 65535.0,
                full_scale: 65535.0,
                gain_e_per_adu: None,
            },
        );
        assert!(matches!(raw, std::borrow::Cow::Borrowed(_)));
    }

    /// The seeds of `fixture_field(7, …)` (ruling F38): its 150 grid positions, rebuilt from the same `SplitMix64`
    /// stream (seed 7, three draws per star: x, y, amplitude), with `peak` = the plane's maximum in the 3×3 box about
    /// each and `flux = 2π·σ²·peak` (σ 1.8, the fixture's), brightest first. The plane must be that fixture: every
    /// rebuilt position is checked to hold a star.
    fn test_seeds_of(plane: &[f32], w: usize, h: usize) -> Vec<Seed> {
        assert_eq!(
            (w, h),
            (768, 512),
            "test_seeds_of reads fixture_field(7, …) only"
        );
        let mut rng = crate::geometry::ransac::SplitMix64(7);
        let mut seeds = Vec::with_capacity(150);
        for j in 0..10 {
            for i in 0..15 {
                let x = 40.0 + i as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                let y = 40.0 + j as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
                let _amplitude = rng.next_f64();
                let (cx, cy) = (x.round() as usize, y.round() as usize);
                let mut peak = f64::NEG_INFINITY;
                for yy in cy - 1..=cy + 1 {
                    for xx in cx - 1..=cx + 1 {
                        peak = peak.max(plane[yy * w + xx] as f64);
                    }
                }
                assert!(peak > 0.08 + 0.05, "no star at ({x}, {y}): peak {peak}");
                seeds.push(Seed {
                    x,
                    y,
                    peak,
                    flux: 2.0 * std::f64::consts::PI * 1.8 * 1.8 * peak,
                });
            }
        }
        seeds.sort_by(|a, b| b.peak.total_cmp(&a.peak));
        seeds
    }

    /// F-0 (ruling F13): every seed has exactly one outcome, the per-seed records are only a report, and asking for
    /// them moves no number.
    #[test]
    fn every_seed_has_exactly_one_outcome_and_the_numbers_do_not_move() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let seeds = test_seeds_of(&data, w, h); // the 150 star centres as seeds (peak/flux from the fixture's amplitudes)
        let before = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds.clone()),
                ..Default::default()
            },
            None,
        );
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                diagnostics: true,
                seeds: SeedSpec::Given(seeds.clone()),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.diagnostics.len(), seeds.len());
        assert_eq!(m.refused.accounted(m.stars.len()), seeds.len());
        assert_eq!(
            m.refused.not_selected + m.refused.trimmed,
            0,
            "Mode::All has no budget outcomes"
        );
        let accepted = m
            .diagnostics
            .iter()
            .filter(|d| matches!(d.outcome, Outcome::Accepted { .. }))
            .count();
        assert_eq!(accepted, m.stars.len());
        assert_eq!(m.stars, before.stars, "diagnostics change no number");
    }

    #[test]
    fn edge_seed_is_refused_outside_image_with_its_reason() {
        let data = vec![0.05f32; 64 * 64];
        let opts = MeasureOptions {
            diagnostics: true,
            seeds: SeedSpec::Given(vec![Seed {
                x: 2.0,
                y: 30.0,
                peak: 0.5,
                flux: 5.0,
            }]),
            ..Default::default()
        };
        let m = measure_plane(&data, 64, 64, &opts, None);
        assert_eq!(
            m.diagnostics[0].outcome,
            Outcome::Refused(RefusalReason::OutsideImage)
        );
        assert_eq!(m.refused.by_reason[&RefusalReason::OutsideImage], 1);
        assert_eq!(m.refused.total(), 1);
        assert!(m.stars.is_empty());
    }

    #[test]
    fn starless_plane_has_no_outcomes_and_no_panic() {
        let data = vec![0.05f32; 128 * 128];
        let m = measure_plane(
            &data,
            128,
            128,
            &MeasureOptions {
                diagnostics: true,
                seeds: SeedSpec::Given(vec![]),
                ..Default::default()
            },
            None,
        );
        assert!(m.stars.is_empty() && m.diagnostics.is_empty() && m.refused.total() == 0);
        assert_eq!(m.frame.fwhm_px, None);
    }

    #[test]
    fn duplicate_and_rim_refusals_are_counted_by_reason() {
        use crate::test_support::{moffat_field, MoffatStar};
        // one star, two seeds 0.5 px apart → one Accepted, one Duplicate
        let star = MoffatStar {
            x: 40.3,
            y: 40.7,
            amp: 0.5,
            alpha_x: 3.0,
            alpha_y: 3.0,
            theta: 0.0,
        };
        let data = moffat_field(80, 80, &[star], 4.0, 0.05);
        let seeds = vec![
            Seed {
                x: 40.3,
                y: 40.7,
                peak: 0.5,
                flux: 3.0,
            },
            Seed {
                x: 40.8,
                y: 40.7,
                peak: 0.49,
                flux: 2.9,
            },
        ];
        let m = measure_plane(
            &data,
            80,
            80,
            &MeasureOptions {
                diagnostics: true,
                seeds: SeedSpec::Given(seeds),
                psf_model: PsfModel::Moffat4,
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.stars.len(), 1);
        assert_eq!(m.refused.by_reason.get(&RefusalReason::Duplicate), Some(&1));
        // a seed whose region cannot hold half the FWTM → RegionTooSmall, not a silent drop. Plan A Task 6b (ruling
        // F14): the field-level nominal stamp this fixture relied on is gone, so it takes PA-18's border geometry — a
        // FWHM-12 star (α 13.8, half-FWTM 12.2 px) 10 px from the left border of a field whose width is 3 px. Four
        // FWHM-3 stars (α 3.45) give that width: the broad star's own half-maximum box leaves the image, so
        // `field_width_of` reads the four. Its width-start box (radius ⌈2·F_field⌉ ≤ 7) fits, the border clips its
        // region at r = 10, and 12.2 > 10.
        let stars = [
            MoffatStar {
                x: 10.0,
                y: 100.0,
                amp: 0.3,
                alpha_x: 13.8,
                alpha_y: 13.8,
                theta: 0.0,
            },
            MoffatStar {
                x: 70.0,
                y: 40.0,
                amp: 0.3,
                alpha_x: 3.45,
                alpha_y: 3.45,
                theta: 0.0,
            },
            MoffatStar {
                x: 150.0,
                y: 40.0,
                amp: 0.3,
                alpha_x: 3.45,
                alpha_y: 3.45,
                theta: 0.0,
            },
            MoffatStar {
                x: 70.0,
                y: 160.0,
                amp: 0.3,
                alpha_x: 3.45,
                alpha_y: 3.45,
                theta: 0.0,
            },
            MoffatStar {
                x: 150.0,
                y: 160.0,
                amp: 0.3,
                alpha_x: 3.45,
                alpha_y: 3.45,
                theta: 0.0,
            },
        ];
        let data = moffat_field(200, 200, &stars, 4.0, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        let m = measure_plane(
            &data,
            200,
            200,
            &MeasureOptions {
                diagnostics: true,
                seeds: SeedSpec::Given(seeds),
                psf_model: PsfModel::Moffat4,
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.field_width.from_stars, 4, "{:?}", m.field_width);
        assert!(
            (m.field_width.fwhm_px / 3.0 - 1.0).abs() < 0.1,
            "{:?}",
            m.field_width
        );
        assert_eq!(
            m.diagnostics[0].outcome,
            Outcome::Refused(RefusalReason::RegionTooSmall)
        );
        assert_eq!(m.diagnostics[0].region_radius, 10);
        assert_eq!(m.stars.len(), 4, "the four narrow stars fit");
        assert_eq!(
            m.refused.by_reason.get(&RefusalReason::RegionTooSmall),
            Some(&1)
        );
    }

    /// Plan A Task 6b (Task 6a review M3, controller ruling): the adapter's matched-filter arm hands the detector's own
    /// field width to the fit with its seeds, so the probe's measure path (`SeedOverride::MatchedFilter`, the seeds
    /// handed to the module as `Given`) and the module's own `SeedSpec::MatchedFilter` start every star from the same
    /// width and fit the same stars; `field_width_of` serves only seeds another detector found.
    ///
    /// The plane makes the two widths differ: 120 Moffat-4 stars, 60 narrow (FWHM 3, amplitude 0.4) and 60 broad
    /// (FWHM 7, amplitude 0.3), and a cap of 40 seeds. Task 10b A2 gave `field_width_of` the matched filter's own rule
    /// (I-4: the mode of every readable read), so the widths now differ by the POOL, not the rule (ruling F14, the
    /// premise rebuilt; through A1 it was the 100 brightest by peak against the first pass's 100 by flux): the matched
    /// filter reads all 120 first-pass candidates (the narrow reads are the denser mode), while the 40 seeds the cap
    /// keeps are the broad ones (a broad star's filtered flux reads ≈ 1.7× its amplitude), whose own mode is broad —
    /// box radii ⌈2·F⌉ far apart.
    #[cfg(feature = "solver")]
    #[test]
    fn adapter_matched_filter_seeds_fit_exactly_as_the_module_matched_filter() {
        use crate::stacking::measure::{
            measure_plane_diagnosed, MeasureOptions as AdapterOptions, SeedOverride,
        };
        use crate::test_support::{add_noise, moffat_field, MoffatStar};
        let (w, h) = (640usize, 480usize);
        let stars: Vec<MoffatStar> = (0..120)
            .map(|i| {
                let narrow = (i + i / 12) % 2 == 0;
                let alpha = if narrow { 3.449 } else { 8.048 };
                MoffatStar {
                    x: 30.0 + (i % 12) as f64 * 50.0,
                    y: 30.0 + (i / 12) as f64 * 45.0,
                    amp: if narrow { 0.4 } else { 0.3 },
                    alpha_x: alpha,
                    alpha_y: alpha,
                    theta: 0.0,
                }
            })
            .collect();
        let mut data = moffat_field(w, h, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 11);
        let (pm, seeds, _) = measure_plane_diagnosed(
            &data,
            w,
            h,
            &AdapterOptions {
                max_stars: 40,
                ..Default::default()
            },
            None,
            SeedOverride::MatchedFilter {
                threshold: DEFAULT_THRESHOLD,
            },
            false,
            false,
            PixelNoise::Unknown,
            None,
            None,
        );
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                max_stars: 40,
                ..Default::default()
            },
            None,
        );
        assert_eq!(seeds.len(), m.seeds);
        // The premise: `field_width_of` the same (capped) seeds reads a different width.
        let given = seeds::field_width_of(&working(&data), w, h, &seeds, None);
        assert!(
            (given.fwhm_px - m.field_width.fwhm_px).abs() > 1.0,
            "given {given:?} vs the matched filter's {:?}",
            m.field_width
        );
        assert_eq!(pm.field_width, m.field_width, "the detector's own width");
        assert_eq!(pm.channel.beta, m.beta);
        // Debug strings (Task 10b, ruling F14 edit): a fit without a ring sky carries NaN, which `==` never matches.
        assert_eq!(
            format!("{:?}", pm.fits),
            format!("{:?}", m.stars),
            "the same fits"
        );
    }

    /// No seeds, nothing fitted: the frame aggregates are `None`, the β is the `Auto` fallback 4.0, and the noise and
    /// sky are still measured.
    #[test]
    fn no_seeds_fit_nothing_and_still_measure_the_noise_and_the_sky() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let m = measure_plane(&data, w, h, &MeasureOptions::default(), None);
        assert!(m.stars.is_empty());
        assert_eq!(m.seeds, 0);
        assert_eq!(m.beta, 4.0);
        assert_eq!((m.frame.fwhm_px, m.frame.eccentricity), (None, None));
        assert_eq!((m.frame.stars_fitted, m.frame.tflux), (0, 0.0));
        assert_eq!(m.noise.source, NoiseSource::Mrs);
        assert!((m.noise.sigma / ADU_SCALE as f64 - 0.002).abs() < 0.1 * 0.002);
        assert!(
            (m.sky.median - 0.08).abs() < 0.001,
            "median {}",
            m.sky.median
        );
    }

    // --- Plan A Task 6a (F-1, the seeds): the matched filter, the mask, the field width ---

    fn working(data: &[f32]) -> Vec<f32> {
        data.iter().map(|v| v * ADU_SCALE).collect()
    }

    #[test]
    fn nan_border_seeds_nothing_on_the_border_and_fits_every_interior_star() {
        use crate::test_support::nan_border_field;
        let data = nan_border_field(240, 240, 6, true); // 40 Gaussian stars at FWHM 3.5, a 6-px NaN border
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, 240, 240, None);
        let (seeds, _, _) = seeds::detect(
            &scaled,
            240,
            240,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
            PixelNoise::White,
        );
        assert_eq!(seeds.len(), 40);
        assert!(
            seeds.iter().all(|s| s.x >= 14.0 && s.y >= 14.0),
            "no seed within 8 px of the border"
        );
        let m = measure_plane(
            &data,
            240,
            240,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.stars.len(), 40);
    }

    #[test]
    fn refused_noise_estimate_seeds_nothing_and_warns() {
        let data = vec![0.05f32; 256 * 256]; // a constant plane: MRS at its floor, no residuals
        let m = measure_plane(
            &data,
            256,
            256,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.noise.source, NoiseSource::None);
        assert_eq!(m.seeds, 0);
        assert!(m.stars.is_empty());
    }

    #[test]
    fn given_seeds_get_a_field_width_from_the_halfmax_profile() {
        use crate::test_support::{moffat_field, MoffatStar};
        // PA-17: Moffat-4 at FWHM 5 (α = 5/(2√(2^{1/4} − 1)) = 5.748) reads +1.8 % noiseless; the bar is the estimator's own 10 %
        let stars: Vec<MoffatStar> = (0..30)
            .map(|i| MoffatStar {
                x: 40.0 + (i % 6) as f64 * 60.0,
                y: 40.0 + (i / 6) as f64 * 60.0,
                amp: 0.3,
                alpha_x: 5.748,
                alpha_y: 5.748,
                theta: 0.0,
            })
            .collect();
        let mut data = moffat_field(400, 340, &stars, 4.0, 0.05);
        crate::test_support::add_noise(&mut data, 0.006, 1); // peak SNR 50
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        let fw = seeds::field_width_of(&working(&data), 400, 340, &seeds, None);
        assert_eq!(fw.from_stars, 30);
        assert!((fw.fwhm_px / 5.0 - 1.0).abs() < 0.10, "{fw:?}");
        let out = fit_stars_at(
            &working(&data),
            400,
            340,
            &seeds,
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        assert!(
            (out.field_width.fwhm_px - fw.fwhm_px).abs() < 1e-9,
            "fit_stars_at computed the same width"
        );
    }

    #[test]
    fn pixel_mask_excludes_masked_pixels_from_detection_and_fit() {
        let (data, w, h) = fixture_field(7, 1.0, 0.002);
        let mask: Vec<bool> = (0..w * h).map(|i| (i % w + i / w) % 2 == 0).collect(); // a checkerboard
        let scaled = working(&data);
        let (noise, _) = domain::noise_model(&scaled, w, h, None);
        let (seeds, _, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            Some(&mask),
            None,
            PixelNoise::White,
        );
        assert!(seeds.len() >= 140, "{}", seeds.len());
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                diagnostics: true,
                mask: Some(mask.clone()),
                seeds: SeedSpec::Given(seeds.clone()),
                ..Default::default()
            },
            None,
        );
        assert!(m.stars.len() >= 140);
        // the fit's pixel count is the unmasked count of its region: (2r+1)² / 2 ± 1
        for d in m
            .diagnostics
            .iter()
            .filter(|d| matches!(d.outcome, Outcome::Accepted { .. }))
            .take(5)
        {
            let r = d.region_radius as usize;
            let n = (2 * r + 1) * (2 * r + 1);
            assert!((d.pixels_used as i64 - (n / 2) as i64).abs() <= 1, "{d:?}");
        }
    }

    /// Task 10b A2b truth test 2 (rulings A2-2, A2-3; written in full before the code, RED at 1bc29ef8): the stars of a
    /// field whose widths spread about the field width are point sources. [`crate::test_support::width_spread_field_beta`]
    /// — 600 Moffat stars, FWHM N(3.0, 0.24) px, peak SNR 20-2000, the field of truth test 1 without its events — at
    /// β 1.5, 2.0 and 2.5, the product's range. Bar at EACH β: stars flagged extended <= the Q7 allowance at p = 0.001
    /// (the one-sided 97.7 % binomial quantile of Binomial(600, max(0.001, 1/600)): 3). A bound at the field width (A3,
    /// through A2) flags the bright half that reads broader than the mode (32 of 600 at β 1.5, 5 at β 2.0).
    #[test]
    fn stars_whose_widths_spread_around_the_field_width_are_not_flagged() {
        use crate::test_support::{add_noise, width_spread_field_beta};
        let (w, h) = (2048usize, 2048usize);
        for beta in [1.5, 2.0, 2.5] {
            let (mut data, stars, sigma) = width_spread_field_beta(beta);
            add_noise(&mut data, sigma as f32, 102);
            let opts = MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                ..Default::default()
            };
            let m = measure_plane(&data, w, h, &opts, None);
            let det = m.detection.as_ref().expect("the matched filter ran");
            let is_star = |s: &Seed| stars.iter().any(|t| (t.x - s.x).hypot(t.y - s.y) < 2.0);
            let found = m.detected.iter().filter(|s| is_star(s)).count();
            assert!(
                found >= 590,
                "beta {beta}: premise: the stars are seeded ({found} of 600)"
            );
            let flagged = m
                .detected
                .iter()
                .zip(&det.extended)
                .filter(|&(s, &e)| e && is_star(s))
                .count();
            assert!(
                flagged <= 3,
                "beta {beta}: {flagged} of {found} stars flagged extended (Q7 allowance 3); field width {:?}, envelope {:?}",
                m.field_width,
                det.envelope
            );
        }
    }

    /// Task 10b A2b truth test 3 (ruling A2-2), a GUARD (F28: it cannot fail at 1bc29ef8, which reads injected β 2.0 /
    /// 3.0 as 2.02 / 3.01): a small field's β is read within 10 %. Two 2048² fields of 3 000 Moffat stars, β 2.0 and β
    /// 3.0, FWHM N(2.0, 0.22) px (F ≈ 2.0), peak SNR log-uniform in [20, 3000]. Bars: |β − truth| ≤ 0.10·truth, and at
    /// most 10 % of the β sample at the 1.5 clamp.
    #[test]
    fn small_field_beta_is_read_within_ten_percent() {
        use crate::star_measure::beta::BETA_BOUNDS;
        use crate::test_support::{add_noise, spread_moffat_field};
        let (w, h, sigma) = (2048usize, 2048usize, 0.0003f64);
        for (truth, seed) in [(2.0f64, 201u64), (3.0, 211)] {
            let (mut data, _) = spread_moffat_field(
                (w, h),
                3000,
                (2.0, 0.22),
                truth,
                sigma,
                (20.0, 3000.0),
                (16.0, 30.0),
                0.05,
                seed,
            );
            add_noise(&mut data, sigma as f32, seed + 1);
            let opts = MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: DEFAULT_THRESHOLD,
                },
                diagnostics: true,
                ..Default::default()
            };
            let m = measure_plane(&data, w, h, &opts, None);
            let sample: Vec<f64> = m.diagnostics.iter().filter_map(|d| d.sample_beta).collect();
            let at_clamp = sample
                .iter()
                .filter(|&&b| b <= BETA_BOUNDS.0 + 1e-6)
                .count();
            assert!(
                sample.len() >= 8,
                "β {truth}: a fitted sample ({} stars, {:?})",
                sample.len(),
                m.beta_source
            );
            assert!(
                (m.beta - truth).abs() <= 0.10 * truth,
                "β {truth}: read {} (field width {:?})",
                m.beta,
                m.field_width
            );
            assert!(
                at_clamp as f64 <= 0.10 * sample.len() as f64,
                "β {truth}: {at_clamp} of {} sample stars at the 1.5 clamp",
                sample.len()
            );
        }
    }

    /// Rulings B2, B4: every accepted fit carries B_i (R6's ring) and σ_i. Flat sky 0.05, noise 0.002, β 4, FWHM 4.35:
    /// the ring 43.5–52.2 px holds ≈ 2615 pixels; B_i within 4 SE of the sky, σ_i within 6 % (4 SE) of the noise.
    #[test]
    fn every_accepted_fit_carries_its_ring_sky_and_noise() {
        use crate::test_support::add_noise;
        let stars = grid30(5.0);
        let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 31);
        let m = measure_plane(
            &data,
            480,
            400,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds_of(&stars)),
                psf_model: PsfModel::Fixed(4.0),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.stars.len(), 30);
        let (sky, noise) = (0.05 * ADU_SCALE as f64, 0.002 * ADU_SCALE as f64);
        let f = m.frame.fwhm_px.unwrap();
        let full = std::f64::consts::PI * ((12.0 * f).powi(2) - (10.0 * f).powi(2));
        for s in &m.stars {
            let n = s.sky_ring_n as f64;
            assert!(n > 0.97 * full && n < 1.03 * full, "{n} of {full}");
            assert!(
                (s.sky_ring - sky).abs() < 4.0 * noise / n.sqrt(),
                "B_i {} vs {sky}",
                s.sky_ring
            );
            assert!(
                (s.sky_ring_sigma / noise - 1.0).abs() < 0.06,
                "σ_i {} vs {noise}",
                s.sky_ring_sigma
            );
            assert_eq!(s.sky_offset(), s.background - s.sky_ring);
        }
        assert_eq!(m.ring_sky_missing, 0);
    }

    /// Ruling B2: `c` reads each qualifier's stored B_i — recomputed here from the stored fields and the plane alone, it
    /// is the plane's `c`. The replica's growth rule: every pixel whose CENTRE lies within 5F of the fit's centre, the
    /// value in f32 working units (`v * ADU_SCALE`, the module's conversion) widened to f64, minus B_i, summed in
    /// row-major order. The tolerance 1e-6 covers a different summation order; if the product's rule differs in the
    /// disc or the units, the replica copies the product's rule — the tolerance is never widened.
    #[test]
    fn aperture_correction_is_the_growth_over_the_stored_ring_sky() {
        use crate::test_support::add_noise;
        let stars = grid30(5.0);
        let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 32);
        let m = measure_plane(
            &data,
            480,
            400,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds_of(&stars)),
                psf_model: PsfModel::Fixed(4.0),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.aperture_stars, 30);
        let (f, w) = (m.frame.fwhm_px.unwrap(), 480usize);
        let r2 = (5.0 * f).powi(2);
        let c = median(m.stars.iter().map(|s| {
            let growth: f64 = (0..400 * w)
                .filter(|&i| ((i % w) as f64 - s.x).powi(2) + ((i / w) as f64 - s.y).powi(2) <= r2)
                .map(|i| (data[i] * ADU_SCALE) as f64 - s.sky_ring)
                .sum();
            growth / model_flux_in_fwtm(s)
        }));
        assert!(
            (c / m.aperture_correction.unwrap() - 1.0).abs() < 1e-6,
            "{c} vs {:?}",
            m.aperture_correction
        );
    }

    /// B_i is every accepted star's (its present ring pixels); `c`'s qualifier rule stays `c`'s: a star whose 12F disc
    /// leaves the plane has a B_i over its partial ring and is not a qualifier.
    #[test]
    fn an_edge_star_has_a_ring_sky_but_does_not_qualify_for_c() {
        use crate::test_support::add_noise;
        let mut stars = grid30(5.0);
        stars.push(MoffatStar {
            x: 20.0,
            y: 25.0,
            ..stars[0]
        });
        let mut data = moffat_field(480, 400, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 33);
        let m = measure_plane(
            &data,
            480,
            400,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds_of(&stars)),
                psf_model: PsfModel::Fixed(4.0),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.stars.len(), 31);
        let edge = m
            .stars
            .iter()
            .find(|s| (s.x - 20.0).abs() < 1.0 && (s.y - 25.0).abs() < 1.0)
            .unwrap();
        let f = m.frame.fwhm_px.unwrap();
        let full = std::f64::consts::PI * ((12.0 * f).powi(2) - (10.0 * f).powi(2));
        let n = edge.sky_ring_n as f64;
        assert!(n >= 8.0 && n < 0.75 * full, "{n} of {full}");
        assert!(
            (edge.sky_ring - 0.05 * ADU_SCALE as f64).abs()
                < 4.0 * 0.002 * ADU_SCALE as f64 / n.sqrt()
        );
        assert_eq!(m.aperture_stars, 30, "the edge star is not a qualifier");
    }

    /// Ruling B3's arm: a given ring width sets every ring (and `c`'s growth radius); at F 4 the ring 40–48 px holds
    /// ≈ 2212 pixels, while the plane's own F 2.61 gives ≈ 940.
    #[test]
    fn a_given_ring_width_sets_every_ring() {
        use crate::test_support::add_noise;
        let stars = grid30(3.0);
        let mut unit = moffat_field(480, 400, &stars, 4.0, 0.05);
        add_noise(&mut unit, 0.002, 34);
        let data: Vec<f32> = unit.iter().map(|v| v * ADU_SCALE).collect();
        let own = fit_stars_at(
            &data,
            480,
            400,
            &seeds_of(&stars),
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        let common = fit_stars_at(
            &data,
            480,
            400,
            &seeds_of(&stars),
            4.0,
            &FitStarsOptions {
                ring_fwhm: Some(4.0),
                ..Default::default()
            },
            None,
        );
        let ring = std::f64::consts::PI * (48.0f64.powi(2) - 40.0f64.powi(2));
        assert!(common
            .fits
            .iter()
            .all(|f| (f.sky_ring_n as f64 / ring - 1.0).abs() < 0.03));
        assert!(own.fits.iter().all(|f| (f.sky_ring_n as f64) < 0.5 * ring));
        assert!(own.aperture_correction.is_some() && common.aperture_correction.is_some());
    }

    /// Task 10b, ruling B3's arm without a second fit: [`correct_at_ring`] after a fit at the plane's own ring width is,
    /// bit for bit, the fit call handed that ring width — every fit (its ring sky and its signal), `c`, its qualifiers
    /// and the missing rings.
    #[test]
    fn correcting_at_a_ring_width_after_the_fit_is_the_fit_at_that_width() {
        use crate::test_support::add_noise;
        let stars = grid30(3.0);
        let mut unit = moffat_field(480, 400, &stars, 4.0, 0.05);
        add_noise(&mut unit, 0.002, 35);
        let data: Vec<f32> = unit.iter().map(|v| v * ADU_SCALE).collect();
        let seeds = seeds_of(&stars);
        let opts = FitStarsOptions::default();
        let mut own = fit_stars_at(&data, 480, 400, &seeds, 4.0, &opts, None);
        let at = fit_stars_at(
            &data,
            480,
            400,
            &seeds,
            4.0,
            &FitStarsOptions {
                ring_fwhm: Some(4.0),
                ..Default::default()
            },
            None,
        );
        assert_ne!(own.aperture_correction, at.aperture_correction);
        correct_at_ring(&data, 480, 400, &seeds, &mut own, 4.0, &opts, None);
        assert_eq!(format!("{:?}", own.fits), format!("{:?}", at.fits));
        assert_eq!(
            (
                own.aperture_correction,
                own.aperture_stars,
                own.ring_sky_missing
            ),
            (
                at.aperture_correction,
                at.aperture_stars,
                at.ring_sky_missing
            )
        );
    }

    /// F-10 (Plan A Task 10b, phase A4; rulings A6, Q1, Q5): the frame-level trailed verdict from the refusal
    /// structure. A trail longer than its fit region cannot be contained, so its seeds end `centreMoved` /
    /// `regionTooSmall` / `centreAtRim`; over [`fit::TRAIL_VERDICT_MIN_SEEDS`] judged seeds a share of those three above
    /// [`fit::TRAIL_REFUSAL_SHARE`] marks the plane trailed. No per-star streak refusal exists.
    mod trailed {
        use super::*;
        use crate::test_support::add_noise;

        /// A horizontal trail `len` px long centred at (x, y): Gaussian points of FWHM 2.6 every 0.25 px (peak ≈ 11·amp).
        fn trail(points: &mut Vec<(f64, f64, f64)>, x: f64, y: f64, len: f64, amp: f64) {
            let n = (len / 0.25) as usize + 1;
            points.extend((0..n).map(|k| (x - len / 2.0 + k as f64 * 0.25, y, amp)));
        }

        /// 20-px trails (amplitude 0.018 per point) at `trails` and round β-4 Moffat stars of FWHM 2.6 and peak 0.2 at
        /// `round`, on sky 0.05 with noise 0.002; one seed (peak 0.2) at every position, the round ones first.
        fn fixture(
            w: usize,
            h: usize,
            round: &[(f64, f64)],
            trails: &[(f64, f64)],
            seed: u64,
        ) -> (Vec<f32>, Vec<Seed>) {
            use crate::test_support::gaussian_field;
            let mut points = Vec::new();
            for &(x, y) in trails {
                trail(&mut points, x, y, 20.0, 0.018);
            }
            let mut data = gaussian_field(w, h, &points, 2.6 / 2.354_820_045, 0.05);
            let a = 2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
            let stars: Vec<MoffatStar> = round
                .iter()
                .map(|&(x, y)| MoffatStar {
                    x,
                    y,
                    amp: 0.2,
                    alpha_x: a,
                    alpha_y: a,
                    theta: 0.0,
                })
                .collect();
            for (d, m) in data.iter_mut().zip(moffat_field(w, h, &stars, 4.0, 0.0)) {
                *d += m;
            }
            add_noise(&mut data, 0.002, seed);
            let seeds = round
                .iter()
                .chain(trails)
                .map(|&(x, y)| Seed {
                    x,
                    y,
                    peak: 0.2,
                    flux: 1.0,
                })
                .collect();
            (data, seeds)
        }

        /// [`fixture`] on a 40-px grid from (30, 30) with a 30-px margin on every side: 13 columns on a 540-px plane (the
        /// previous version of these tests), as many as fit on a narrower one. Of the `round + trails` grid slots in order,
        /// every `(round + trails) / round`-th holds a round star (`round` of them), the rest trails.
        fn trail_plane_fixture(
            w: usize,
            h: usize,
            round: usize,
            trails: usize,
            seed: u64,
        ) -> (Vec<f32>, Vec<Seed>) {
            let n = round + trails;
            let cols = 13.min((w - 60) / 40 + 1);
            assert!(
                30 + 40 * ((n - 1) / cols) <= h - 30,
                "{n} slots of {cols} columns do not fit {w} x {h}"
            );
            let at = |i: usize| {
                (
                    30.0 + 40.0 * (i % cols) as f64,
                    30.0 + 40.0 * (i / cols) as f64,
                )
            };
            let stride = n / round;
            let is_round = |i: usize| i % stride == 0 && i / stride < round;
            let round_at: Vec<(f64, f64)> = (0..n).filter(|&i| is_round(i)).map(at).collect();
            let trails_at: Vec<(f64, f64)> = (0..n).filter(|&i| !is_round(i)).map(at).collect();
            assert_eq!((round_at.len(), trails_at.len()), (round, trails));
            fixture(w, h, &round_at, &trails_at, seed)
        }

        /// 120 trails and 5 round stars: 125 judged seeds, the trails' fits end centreMoved / regionTooSmall / centreAtRim above the
        /// derived share → the plane is trailed, keeps no star, `c` is None. No `Streak` reason exists.
        #[test]
        fn a_plane_of_trails_is_trailed_by_its_refusal_share_and_keeps_no_star() {
            let (data, seeds) = trail_plane_fixture(540, 420, 5, 120, 42);
            let m = measure_plane(
                &data,
                540,
                420,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds),
                    ..Default::default()
                },
                None,
            );
            assert!(m.trailed);
            assert!(
                m.trail_share >= fit::TRAIL_REFUSAL_SHARE,
                "{}",
                m.trail_share
            );
            assert!(m.stars.is_empty());
            assert_eq!(
                m.refused
                    .by_reason
                    .get(&RefusalReason::Trailed)
                    .copied()
                    .unwrap_or(0)
                    > 0,
                true
            );
            assert_eq!(m.aperture_correction, None);
        }

        /// Under 100 judged seeds there is no verdict, however many fits end centreMoved / regionTooSmall / centreAtRim.
        #[test]
        fn fewer_than_the_minimum_judged_seeds_is_never_trailed() {
            let (data, seeds) = trail_plane_fixture(420, 310, 5, 15, 41);
            let m = measure_plane(
                &data,
                420,
                310,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds),
                    ..Default::default()
                },
                None,
            );
            assert!(!m.trailed);
        }

        /// Elongated stars (eccentricity 0.7, the lab's most elongated `shape` family, and a G 0003-like 0.65 field) are
        /// stars: no verdict, none refused for their shape, all 120 kept.
        #[test]
        fn elongated_stars_are_measured_as_is_with_no_verdict() {
            let (ax, ay) = (
                2.6 / 0.714 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt()),
                2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt()),
            );
            let stars: Vec<MoffatStar> = (0..120)
                .map(|i| MoffatStar {
                    x: 30.0 + 40.0 * (i % 12) as f64,
                    y: 30.0 + 40.0 * (i / 12) as f64,
                    amp: 0.2,
                    alpha_x: ax,
                    alpha_y: ay,
                    theta: 0.5,
                })
                .collect();
            let mut data = moffat_field(500, 420, &stars, 4.0, 0.05);
            add_noise(&mut data, 0.002, 43);
            let m = measure_plane(
                &data,
                500,
                420,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds_of(&stars)),
                    ..Default::default()
                },
                None,
            );
            assert!(!m.trailed);
            assert_eq!(m.stars.len(), 120);
        }

        /// The verdict is the measure path's alone (as the defocused one): an LN-style refit (`fit_stars_at`, the
        /// default `trail_verdict: false`) of the trailed plane keeps its round stars' fits and reports the share; the
        /// same call asking for the verdict refuses every one of them `Trailed`, and every seed keeps one outcome.
        #[test]
        fn only_a_call_that_asks_for_the_verdict_refuses_a_trailed_planes_fits() {
            let (data, seeds) = trail_plane_fixture(540, 420, 5, 120, 42);
            let scaled: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
            let off = fit_stars_at(
                &scaled,
                540,
                420,
                &seeds,
                4.0,
                &FitStarsOptions::default(),
                None,
            );
            assert!(!off.trailed);
            assert!(
                off.trail_share > fit::TRAIL_REFUSAL_SHARE,
                "{}",
                off.trail_share
            );
            assert!(!off.fits.is_empty());
            assert!(!off.refused.by_reason.contains_key(&RefusalReason::Trailed));
            let opts = FitStarsOptions {
                trail_verdict: true,
                ..Default::default()
            };
            let on = fit_stars_at(&scaled, 540, 420, &seeds, 4.0, &opts, None);
            assert!(on.trailed);
            assert!(on.fits.is_empty());
            assert_eq!(on.aperture_correction, None);
            assert_eq!(on.trail_share, off.trail_share);
            assert_eq!(
                on.refused.by_reason.get(&RefusalReason::Trailed).copied(),
                Some(off.fits.len())
            );
            assert_eq!(on.refused.accounted(0), seeds.len());
        }
    }

    /// F-3 (Plan A Task 8): the saturation test on the module's own seed region (C-f; R2's fixtures, not R1's ± 6F
    /// box), through `measure_plane`.
    mod saturation {
        use super::*;
        use crate::star_measure::photometry::SaturationVerdict;
        use crate::test_support::{add_noise, moffat_field, MoffatStar};

        fn clipped(data: &mut [f32], at: f32) {
            for v in data.iter_mut() {
                if *v > at {
                    *v = at;
                }
            }
        }
        /// `data` measured at the given seeds through the module's own regions, diagnostics on, Moffat-4 (no β sample).
        fn measured(data: &[f32], w: usize, h: usize, seeds: Vec<Seed>) -> PlaneMeasurement {
            measure_plane(
                data,
                w,
                h,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds),
                    psf_model: PsfModel::Moffat4,
                    diagnostics: true,
                    ..Default::default()
                },
                None,
            )
        }
        fn verdict(m: &PlaneMeasurement, seed: usize) -> SaturationVerdict {
            m.diagnostics
                .iter()
                .find(|d| d.seed == seed)
                .and_then(|d| d.verdict)
                .expect("a seed with a region has a verdict")
        }
        fn seed_at(x: f64, y: f64) -> Seed {
            Seed {
                x,
                y,
                peak: 0.9,
                flux: 1.0,
            }
        }
        /// R2's single-star fixtures: FWHM 2.6 at β 4 (α 2.99), off the pixel grid.
        fn small_core_star(x: f64, y: f64, amp: f64) -> MoffatStar {
            MoffatStar {
                x,
                y,
                amp,
                alpha_x: 2.99,
                alpha_y: 2.99,
                theta: 0.0,
            }
        }
        fn twelve_clipped() -> (Vec<f32>, Vec<Seed>) {
            let stars: Vec<MoffatStar> = (0..12)
                .map(|i| MoffatStar {
                    x: 30.0 + (i % 4) as f64 * 50.0,
                    y: 30.0 + (i / 4) as f64 * 50.0,
                    amp: 3.0,
                    alpha_x: 3.0,
                    alpha_y: 3.0,
                    theta: 0.0,
                })
                .collect();
            let mut data = moffat_field(220, 170, &stars, 4.0, 0.05);
            clipped(&mut data, 0.95);
            add_noise(&mut data, 0.002, 5);
            (data, stars.iter().map(|s| seed_at(s.x, s.y)).collect())
        }

        #[test]
        fn a_clipped_star_without_a_full_3x3_plateau_is_fitted_uncensored_and_still_flagged() {
            // amplitude 3× the clip 0.95: about eight pixels reach the clip, but one neighbour of the centre pixel sits ≈ 0.11
            // below it (> 0.1·range), so the plateau test fails, the level is norm and nothing is excluded; the uncensored
            // fit overshoots (R2: A/range 1.30, FWHM +48 %) — spec §2.1's known limit
            let mut hot = moffat_field(60, 60, &[small_core_star(30.3, 30.7, 2.85)], 4.0, 0.05);
            clipped(&mut hot, 0.95);
            add_noise(&mut hot, 0.002, 1);
            let v = verdict(&measured(&hot, 60, 60, vec![seed_at(30.3, 30.7)]), 0);
            assert!(!v.plateau && v.excluded == 0 && v.saturated, "{v:?}");
        }

        #[test]
        fn a_planes_brightest_unclipped_star_is_flagged_because_its_own_peak_sets_the_range() {
            // by design (spec §2.1): the lone star at 0.6× the clip sets the plane max, so the range is its own brightest
            // pixel − bg, and the fitted peak between pixels tops it (R2: A/range 1.08)
            let mut cool = moffat_field(60, 60, &[small_core_star(30.3, 30.7, 0.57)], 4.0, 0.05);
            add_noise(&mut cool, 0.002, 1);
            let v = verdict(&measured(&cool, 60, 60, vec![seed_at(30.3, 30.7)]), 0);
            assert!(!v.plateau && v.saturated, "{v:?}");
        }

        #[test]
        fn a_lone_broad_unclipped_star_passes_the_plateau_test_and_its_censored_fit_keeps_its_width(
        ) {
            // FWHM 9.01 (α 24, β 20), peak 0.3 on 0.05: its 3x3 lies within 0.1·range of the centre, so its top is censored
            // (R2: FWHM within 0.2 %). The amplitude flag is not pinned: the lone star sets the range itself (R2: A/range 0.994)
            let mut broad = moffat_field(
                120,
                120,
                &[MoffatStar {
                    x: 60.0,
                    y: 60.0,
                    amp: 0.3,
                    alpha_x: 24.0,
                    alpha_y: 24.0,
                    theta: 0.0,
                }],
                20.0,
                0.05,
            );
            add_noise(&mut broad, 0.002, 2);
            let v = verdict(&measured(&broad, 120, 120, vec![seed_at(60.0, 60.0)]), 0);
            let truth = 2.0 * 24.0 * (2f64.powf(1.0 / 20.0) - 1.0).sqrt();
            assert!(v.plateau && v.excluded >= 9, "{v:?}");
            // known limit (README R-OSP-6): its top sits within 0.1·range of the clip it sets itself, so the
            // plateau-clip clause flags the lone broad star
            assert!(v.by_plateau_clip, "{v:?}");
            assert!((v.fwhm / truth - 1.0).abs() < 0.03, "{} vs {truth}", v.fwhm);
        }

        #[test]
        fn plateau_clip_a_seed_inside_a_large_clipped_area_is_flagged_by_the_plateau_clip_clause() {
            // owner plateau-clip ruling (2026-10-09): a nebula-core-like area, a 12 px disc at the clip on a hazy 0.45 sky with a
            // faint broad swell. The seed at its centre passes the plateau test with pixel0 at the clip, and the verdict
            // fit sees only the rim, no peak above it (amplitude under the range): the amplitude clause stays silent and
            // the plateau-clip clause flags the seed
            let (w, h) = (120usize, 120usize);
            let mut data = vec![0.45f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let r2 = (x as f64 - 60.0).powi(2) + (y as f64 - 60.0).powi(2);
                    data[y * w + x] +=
                        (0.1 * (-(r2 / 400.0f64)).exp() + if r2 < 36.0 { 0.5 } else { 0.0 }) as f32;
                }
            }
            clipped(&mut data, 0.95);
            add_noise(&mut data, 0.002, 4);
            let m = measured(&data, w, h, vec![seed_at(60.0, 60.0)]);
            let v = verdict(&m, 0);
            assert!(v.plateau && v.saturated && v.by_plateau_clip, "{v:?}");
            assert!(
                !v.by_amplitude,
                "the amplitude clause must not carry this case: {v:?}"
            );
        }

        #[test]
        fn plateau_clip_a_broad_unclipped_star_below_the_clip_band_of_a_brighter_stars_range_is_not_flagged(
        ) {
            // the plane's range is set by a bright narrow star (peak ≈ 0.85); the broad star's flat top (≈ 0.7) passes the
            // plateau test but sits below 0.9·range, so the clause does not fire (pixel0 − bg ≈ 0.81 of the range)
            let mut field = moffat_field(
                160,
                120,
                &[
                    MoffatStar {
                        x: 50.0,
                        y: 60.0,
                        amp: 0.65,
                        alpha_x: 24.0,
                        alpha_y: 24.0,
                        theta: 0.0,
                    },
                    small_core_star(130.0, 60.0, 0.8),
                ],
                20.0,
                0.05,
            );
            add_noise(&mut field, 0.002, 6);
            let m = measured(&field, 160, 120, vec![seed_at(50.0, 60.0)]);
            let v = verdict(&m, 0);
            assert!(v.plateau && !v.by_plateau_clip, "{v:?}");
        }

        #[test]
        fn a_full_clipped_plateau_is_censored_flagged_and_keeps_its_width() {
            // every 3x3 of the twelve sits at the clip: the plateau test passes, the level excludes the plateau, and the
            // censored fit reads the width (R2: 12 of 12 censored and flagged, FWHM median −0.6 %)
            let (data, seeds) = twelve_clipped();
            let m = measured(&data, 220, 170, seeds);
            let truth = 2.0 * 3.0 * (2f64.powf(0.25) - 1.0).sqrt();
            let mut err: Vec<f64> = (0..12)
                .map(|i| {
                    let v = verdict(&m, i);
                    assert!(v.plateau && v.excluded >= 9 && v.saturated, "{i}: {v:?}");
                    (v.fwhm / truth - 1.0).abs()
                })
                .collect();
            err.sort_by(f64::total_cmp);
            assert!(err[6] < 0.03, "median |ΔF|/F {}", err[6]);
        }

        #[test]
        fn the_capped_range_flags_a_small_core_star_beside_a_plateau_lifted_above_full_scale() {
            // a flat-divided field: vignetting 1 + 0.3·d²/28900 about (40, 40) lifts the full plateau at (190, 120) to
            // ≈ 1.24 (legal on a calibrated plane), which sets the plane max. The range stays 1 − bg, so the small-core star
            // keeps A/range ≈ 1.25 and is flagged; an uncapped range (1.24 − bg) would put it at ≈ 1.0
            let stars = [
                small_core_star(40.3, 40.7, 2.85),
                MoffatStar {
                    x: 190.0,
                    y: 120.0,
                    amp: 3.0,
                    alpha_x: 3.0,
                    alpha_y: 3.0,
                    theta: 0.0,
                },
            ];
            let (w, h) = (240usize, 160usize);
            let mut data = moffat_field(w, h, &stars, 4.0, 0.05);
            clipped(&mut data, 0.95);
            for y in 0..h {
                for x in 0..w {
                    let d2 = (x as f64 - 40.0).powi(2) + (y as f64 - 40.0).powi(2);
                    data[y * w + x] *= (1.0 + 0.3 * d2 / 28900.0) as f32;
                }
            }
            add_noise(&mut data, 0.002, 3);
            let m = measured(
                &data,
                w,
                h,
                vec![seed_at(40.3, 40.7), seed_at(190.0, 120.0)],
            );
            let (small, lifted) = (verdict(&m, 0), verdict(&m, 1));
            assert!(!small.plateau && small.saturated, "{small:?}");
            assert!(
                lifted.plateau && lifted.excluded >= 9 && lifted.saturated,
                "{lifted:?}"
            );
        }

        #[test]
        fn all_saturated_sample_leaves_the_frame_statistic_empty_not_wrong() {
            let (data, seeds) = twelve_clipped();
            let m = measure_plane(
                &data,
                220,
                170,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds),
                    psf_model: PsfModel::Moffat4,
                    ..Default::default()
                },
                None,
            );
            assert_eq!(m.frame.stars_fitted, 12);
            assert_eq!(m.frame.stars_unsaturated, 0);
            assert_eq!(m.frame.fwhm_px, None);
            assert!(m.stars.iter().all(|f| f.flags.saturated()));
        }

        #[test]
        fn beta_sample_skips_saturated_stars() {
            // 70 Moffat-4 stars, the 40 brightest clipped: Auto must still resolve β 4 from the unsaturated 30 (the
            // baseline flipped to 10); R2: 40 of 40 flagged, 0 of 30. F-4 (Task 9, ruling F14): β is continuous, so
            // within 0.2 of 4 (was `== 4.0`, the discrete ladder's pick)
            let stars: Vec<MoffatStar> = (0..70)
                .map(|i| MoffatStar {
                    x: 20.0 + (i % 10) as f64 * 40.3,
                    y: 20.0 + (i / 10) as f64 * 40.7,
                    amp: if i < 40 { 2.0 } else { 0.25 },
                    alpha_x: 5.0,
                    alpha_y: 5.0,
                    theta: 0.0,
                })
                .collect();
            let mut data = moffat_field(420, 320, &stars, 4.0, 0.05);
            clipped(&mut data, 0.95);
            add_noise(&mut data, 0.002, 6);
            let seeds: Vec<Seed> = stars
                .iter()
                .map(|s| Seed {
                    x: s.x,
                    y: s.y,
                    peak: s.amp.min(0.9),
                    flux: s.amp,
                })
                .collect();
            let m = measure_plane(
                &data,
                420,
                320,
                &MeasureOptions {
                    seeds: SeedSpec::Given(seeds),
                    ..Default::default()
                },
                None,
            );
            assert!(
                (m.beta - 4.0).abs() < 0.2,
                "the sample is the unsaturated 30, not the clipped 40: {}",
                m.beta
            );
            assert_eq!(m.beta_source, BetaSource::Fitted { sample: 30 });
            assert_eq!(m.frame.stars_unsaturated, 30);
        }

        #[test]
        fn an_unflagged_plateau_stars_fit_at_beta_is_the_no_verdict_fit_bit_for_bit_and_a_flagged_ones_is_censored(
        ) {
            // Q6 as revised (controller ruling from R3, 2026-10-09): a broad star (FWHM 9 at β 4, peak 0.75 on 0.05)
            // passes the plateau test but stays under the range the clipped star sets (0.95 − 0.05), so it is unflagged
            // and its fit at β is the fit with no verdict; the clipped star is flagged and its fit at β leaves out its
            // pixels ≥ its level, so it reads the width the uncensored fit misses
            let (w, h) = (240usize, 160usize);
            let broad = MoffatStar {
                x: 70.0,
                y: 80.0,
                amp: 0.75,
                alpha_x: 10.35,
                alpha_y: 10.35,
                theta: 0.0,
            };
            let plateau = MoffatStar {
                x: 190.0,
                y: 80.0,
                amp: 3.0,
                alpha_x: 3.0,
                alpha_y: 3.0,
                theta: 0.0,
            };
            let mut data = moffat_field(w, h, &[broad, plateau], 4.0, 0.05);
            clipped(&mut data, 0.95);
            add_noise(&mut data, 0.002, 7);
            let seeds = vec![seed_at(70.0, 80.0), seed_at(190.0, 80.0)];
            let m = measured(&data, w, h, seeds.clone());
            let (vb, vc) = (verdict(&m, 0), verdict(&m, 1));
            assert!(vb.plateau && vb.excluded > 0 && !vb.saturated, "{vb:?}");
            assert!(vc.plateau && vc.excluded > 0 && vc.saturated, "{vc:?}");
            let working: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
            // Task 10b (ruling F14 edit): each call reads its ring skies at its own frame FWHM, and the flagged star
            // leaves the measure path's statistic but not the no-verdict call's; at the measure path's ring width
            // the no-verdict call's ring sky is the same read, so the whole record still compares bit for bit.
            let free = fit_stars_at(
                &working,
                w,
                h,
                &seeds,
                m.beta,
                &FitStarsOptions {
                    field_width: Some(m.field_width.fwhm_px),
                    ring_fwhm: m.frame.fwhm_px,
                    ..Default::default()
                },
                None,
            );
            let at = |fits: &[StarFit], x: f64| -> StarFit {
                *fits
                    .iter()
                    .find(|f| (f.x - x).abs() < 2.0)
                    .expect("the star is fitted")
            };
            let (with, mut without) = (at(&m.stars, 70.0), at(&free.fits, 70.0));
            // Task 11 (ruling F14 edit): the SNR reads the plane's noise correlation, which the measure path reads
            // (this plane's noise is stated `Unknown`) and a bare fit call does not (kappa 1); it is the one field of the
            // record that is not the fit's. Every other field still compares bit for bit.
            assert_ne!(with.snr, without.snr, "the measure path read a correlation");
            without.snr = with.snr;
            assert_eq!(
                format!("{with:?}"),
                format!("{without:?}"),
                "unflagged: no exclusion"
            );
            let (censored, whole) = (at(&m.stars, 190.0), at(&free.fits, 190.0));
            assert!(censored.flags.saturated() && !whole.flags.saturated());
            let truth = 2.0 * 3.0 * (2f64.powf(0.25) - 1.0).sqrt();
            assert!(
                (censored.fwhm() / truth - 1.0).abs() < (whole.fwhm() / truth - 1.0).abs(),
                "censored {} vs uncensored {} (truth {truth})",
                censored.fwhm(),
                whole.fwhm()
            );
        }
    }

    // ---- Plan A Task 12: the Analysis mode and the budget ----

    fn uniform(n: usize, w: usize, h: usize, seed: u64) -> Vec<MoffatStar> {
        let mut rng = crate::geometry::ransac::SplitMix64(seed);
        (0..n)
            .map(|_| MoffatStar {
                x: 20.0 + rng.next_f64() * (w as f64 - 40.0),
                y: 20.0 + rng.next_f64() * (h as f64 - 40.0),
                amp: 0.05 + 0.3 * rng.next_f64(),
                alpha_x: 3.0,
                alpha_y: 3.0,
                theta: 0.0,
            })
            .collect()
    }

    /// The 1024² field with pseudo-noise, as the probe's own fixture adds it (ruling F29): the MRS noise estimate needs
    /// some, and a noiseless plane is a refused noise model that seeds nothing under `SeedSpec::MatchedFilter` (Review
    /// Focus 4).
    fn noisy_field(stars: &[MoffatStar], noise_seed: u64) -> Vec<f32> {
        let mut data = moffat_field(1024, 1024, stars, 4.0, 0.05);
        crate::test_support::add_noise(&mut data, 0.002, noise_seed);
        data
    }

    fn budget_opts(budget: usize) -> MeasureOptions {
        MeasureOptions {
            mode: Mode::Budget { budget },
            seeds: SeedSpec::MatchedFilter {
                threshold: seeds::DEFAULT_THRESHOLD,
            },
            ..Default::default()
        }
    }

    #[test]
    fn budget_covers_every_cell_that_holds_a_star() {
        let stars = uniform(800, 1024, 1024, 11);
        let data = noisy_field(&stars, 11);
        let m = measure_plane(&data, 1024, 1024, &budget_opts(500), None);
        let b = m.budget.unwrap();
        assert!(
            m.stars.len() <= 500 && m.stars.len() >= 480,
            "{}",
            m.stars.len()
        );
        assert_eq!(b.cells_with_stars, 16);
        assert_eq!(b.cells_covered, 16);
        assert_eq!(
            m.stars.len() + m.refused.total() + m.refused.not_selected + m.refused.trimmed,
            m.seeds
        );
    }

    #[test]
    fn budget_keeps_the_brightest_per_cell_when_one_cell_dominates() {
        let mut stars = uniform(600, 256, 256, 3); // all inside the top-left cell of a 1024² plane
        stars.extend((0..15).map(|i| MoffatStar {
            x: 300.0 + (i % 5) as f64 * 150.0,
            y: 300.0 + (i / 5) as f64 * 200.0,
            amp: 0.2,
            alpha_x: 3.0,
            alpha_y: 3.0,
            theta: 0.0,
        }));
        let data = noisy_field(&stars, 3);
        let m = measure_plane(&data, 1024, 1024, &budget_opts(200), None);
        let b = m.budget.unwrap();
        assert_eq!(
            b.cells_covered, b.cells_with_stars,
            "every cell with a star keeps one"
        );
        let outside = m
            .stars
            .iter()
            .filter(|f| f.x >= 256.0 || f.y >= 256.0)
            .count();
        assert!(outside >= 14, "the sparse cells' stars are kept: {outside}");
        assert!(m.stars.len() <= 200);
    }

    #[test]
    fn budget_mode_accounts_for_every_seed() {
        let stars = uniform(900, 1024, 1024, 5);
        let data = noisy_field(&stars, 5);
        let m = measure_plane(
            &data,
            1024,
            1024,
            &MeasureOptions {
                diagnostics: true,
                ..budget_opts(500)
            },
            None,
        );
        let b = m.budget.unwrap();
        assert!(
            b.not_selected > 0,
            "900 seeds, 1.25 × 500 / 16 per cell: many are never fitted"
        );
        assert_eq!(
            m.diagnostics
                .iter()
                .filter(|d| d.outcome == Outcome::NotSelected)
                .count(),
            b.not_selected
        );
        assert_eq!(
            m.diagnostics
                .iter()
                .filter(|d| d.outcome == Outcome::Trimmed)
                .count(),
            b.trimmed
        );
        assert_eq!(
            m.stars.len() + m.refused.total() + b.not_selected + b.trimmed,
            m.seeds
        );
        assert_eq!(m.refused.not_selected, b.not_selected);
        assert!(
            m.diagnostics.iter().enumerate().all(|(i, d)| d.seed == i),
            "one record per detected seed, in seed order"
        );
    }

    /// M-2 (Task 12 review): after a trim that removes stars from the middle of the order, every renumbered
    /// `Accepted { star }` points at the fit of its own seed (the star sits on the seed's position), and the
    /// record's `star` indices are a permutation-free 0..kept.
    #[test]
    fn a_renumbered_accepted_star_is_the_fit_of_its_own_seed() {
        let stars = uniform(900, 1024, 1024, 5);
        let data = noisy_field(&stars, 5);
        let m = measure_plane(
            &data,
            1024,
            1024,
            &MeasureOptions {
                diagnostics: true,
                ..budget_opts(200)
            },
            None,
        );
        let b = m.budget.unwrap();
        assert!(
            b.trimmed > 0 && !m.stars.is_empty(),
            "the trim removes stars"
        );
        let mut seen = vec![false; m.stars.len()];
        let mut first_gap = false;
        for d in &m.diagnostics {
            match d.outcome {
                Outcome::Accepted { star } => {
                    assert!(!seen[star], "star {star} claimed twice");
                    seen[star] = true;
                    let s = &m.detected[d.seed];
                    let off = (m.stars[star].x - s.x).hypot(m.stars[star].y - s.y);
                    assert!(off < 2.0, "star {star} is {off} px from seed {}", d.seed);
                }
                Outcome::Trimmed => first_gap = true,
                _ => {}
            }
        }
        assert!(seen.iter().all(|&x| x), "every kept star has its record");
        assert!(first_gap, "a Trimmed record sits between the accepted ones");
    }

    /// R12-2 / R12-3: a budget no smaller than the seed count selects every seed and trims nothing. The measurement is then
    /// `Mode::All`'s bit for bit (Debug text, so NaN fields compare; the wall-clock timings set equal), on given and
    /// matched-filter seeds, under both PSF models.
    #[test]
    fn a_budget_at_or_above_the_seed_count_is_mode_all_bit_for_bit() {
        let (fixture, fw, fh) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let given = test_seeds_of(&fixture, fw, fh);
        let noisy = noisy_field(&uniform(300, 1024, 1024, 9), 9);
        for (plane, w, h, spec) in [
            (&fixture, fw, fh, SeedSpec::Given(given)),
            (
                &noisy,
                1024,
                1024,
                SeedSpec::MatchedFilter {
                    threshold: seeds::DEFAULT_THRESHOLD,
                },
            ),
        ] {
            for psf_model in [PsfModel::Auto, PsfModel::Moffat4] {
                let all = MeasureOptions {
                    seeds: spec.clone(),
                    psf_model,
                    diagnostics: true,
                    ..Default::default()
                };
                let a = measure_plane(plane, w, h, &all, None);
                let mut b = measure_plane(
                    plane,
                    w,
                    h,
                    &MeasureOptions {
                        mode: Mode::Budget { budget: a.seeds },
                        ..all.clone()
                    },
                    None,
                );
                let r = b.budget.take().expect("a budget report");
                assert_eq!(
                    (r.not_selected, r.trimmed, r.candidates, r.kept),
                    (0, 0, a.seeds, a.stars.len())
                );
                b.timings = a.timings;
                assert_eq!(format!("{b:?}"), format!("{a:?}"));
            }
        }
    }

    /// Determinism (no order-dependent container in the selection, the trim or the re-indexing): two runs, one result.
    #[test]
    fn the_budget_measurement_is_deterministic() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let opts = MeasureOptions {
            diagnostics: true,
            ..budget_opts(500)
        };
        let a = measure_plane(&data, 1024, 1024, &opts, None);
        let mut b = measure_plane(&data, 1024, 1024, &opts, None);
        b.timings = a.timings;
        assert_eq!(format!("{b:?}"), format!("{a:?}"));
    }

    /// F13 with the diagnostics off (the Analysis tab's setting): the counts are kept in `refused` and the report, and
    /// asking for the per-seed records moves no number.
    #[test]
    fn the_budget_identity_holds_without_diagnostics_and_they_move_no_number() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let off = measure_plane(&data, 1024, 1024, &budget_opts(500), None);
        let on = measure_plane(
            &data,
            1024,
            1024,
            &MeasureOptions {
                diagnostics: true,
                ..budget_opts(500)
            },
            None,
        );
        let b = off.budget.unwrap();
        assert!(off.diagnostics.is_empty());
        assert_eq!(off.refused.accounted(off.stars.len()), off.seeds);
        assert_eq!(
            (off.refused.not_selected, off.refused.trimmed),
            (b.not_selected, b.trimmed)
        );
        assert_eq!(
            (b.kept, b.candidates),
            (off.stars.len(), off.seeds - b.not_selected)
        );
        assert_eq!(
            format!("{:?}", off.stars),
            format!("{:?}", on.stars),
            "diagnostics change no number"
        );
    }

    /// R12-1 (owner): under the Analysis default, Moffat 4 is zero extra work. Every fit is at 4, the source is the
    /// caller's, and no record carries a sample β. `Auto` runs the verdict sample on the candidates only.
    #[test]
    fn budget_with_moffat4_fits_at_four_and_runs_no_beta_sample() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let opts = |psf_model| MeasureOptions {
            psf_model,
            diagnostics: true,
            ..budget_opts(500)
        };
        assert_eq!(budget::ANALYSIS_PSF_MODEL, PsfModel::Moffat4);
        let m = measure_plane(&data, 1024, 1024, &opts(budget::ANALYSIS_PSF_MODEL), None);
        assert_eq!((m.beta, m.beta_source), (4.0, BetaSource::Fixed));
        assert!(
            m.diagnostics.iter().all(|d| d.sample_beta.is_none()),
            "R12-1: no beta sample under Moffat 4"
        );
        assert!(m
            .stars
            .iter()
            .all(|f| f.beta == 4.0 && !f.flags.beta_at_cap()));
        let a = measure_plane(&data, 1024, 1024, &opts(PsfModel::Auto), None);
        assert!(
            matches!(a.beta_source, BetaSource::Fitted { .. }),
            "{:?}",
            a.beta_source
        );
        let sampled: Vec<&FitDiagnostics> = a
            .diagnostics
            .iter()
            .filter(|d| d.sample_beta.is_some())
            .collect();
        assert!(!sampled.is_empty());
        assert!(
            sampled.iter().all(|d| d.outcome != Outcome::NotSelected),
            "Auto's sample is the candidates'"
        );
    }

    /// R12-5: the budget fits a subset. The ring sky still leaves out every seed's 3F disc, and `c`'s isolation still
    /// reads every seed, exactly as when all of them are fitted. In working units at a given ring width and field width,
    /// so the two calls fit A identically.
    #[test]
    fn the_ring_sky_and_the_isolation_read_every_seed_not_only_the_fitted_ones() {
        let (f_ring, field) = (2.6, Some(2.61));
        let star = |x: f64, y: f64, amp: f64| MoffatStar {
            x,
            y,
            amp,
            alpha_x: 3.0,
            alpha_y: 3.0,
            theta: 0.0,
        };
        let run = |stars: &[MoffatStar], fitted: usize, neighbours: bool| {
            let mut data = moffat_field(200, 200, stars, 4.0, 0.05);
            crate::test_support::add_noise(&mut data, 0.002, 7);
            let data: Vec<f32> = data.iter().map(|v| v * ADU_SCALE).collect();
            let seeds: Vec<Seed> = stars
                .iter()
                .map(|s| Seed {
                    x: s.x,
                    y: s.y,
                    peak: s.amp * ADU_SCALE as f64,
                    flux: s.amp,
                })
                .collect();
            let of_fit: Vec<usize> = (0..fitted).collect();
            let n = neighbours.then(|| fit::Neighbours {
                seeds: &seeds,
                of_fit: &of_fit,
            });
            fit::fit_stars_with_beta_diagnosed(
                &data,
                200,
                200,
                &seeds[..fitted],
                4.0,
                &FitParams::default(),
                None,
                field,
                None,
                false,
                false,
                false,
                fit::Correction {
                    ring_fwhm: Some(f_ring),
                    neighbours: n,
                    ..Default::default()
                },
            )
        };
        // B 11·F from A: inside A's [10F, 12F] ring.
        let ring = [
            star(100.0, 100.0, 0.3),
            star(100.0 + 11.0 * f_ring, 100.0, 0.3),
        ];
        let (both, alone, with_n) = (
            run(&ring, 2, false),
            run(&ring, 1, false),
            run(&ring, 1, true),
        );
        let a = both
            .fits
            .iter()
            .find(|f| (f.x - 100.0).abs() < 1.0)
            .unwrap();
        assert_eq!(
            with_n.fits[0].sky_ring.to_bits(),
            a.sky_ring.to_bits(),
            "B's disc is left out of A's ring"
        );
        assert!(
            alone.fits[0].sky_ring > a.sky_ring,
            "without it, B lifts A's ring sky"
        );
        // C (brighter, so the clip screen keeps A) 12 px from A: within 6 · field width, so A is not isolated.
        let iso = [star(100.0, 100.0, 0.3), star(100.0, 112.0, 0.6)];
        assert_eq!(run(&iso, 1, false).aperture_stars, 1, "alone, A qualifies");
        assert_eq!(
            run(&iso, 1, true).aperture_stars,
            0,
            "with its neighbour, A is not isolated"
        );
    }

    /// A pin (ruling F28): the default is the stacking path, so every existing literal stays on it.
    #[test]
    fn the_default_mode_is_all() {
        assert_eq!(MeasureOptions::default().mode, Mode::All);
        let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(test_seeds_of(&data, w, h)),
                ..Default::default()
            },
            None,
        );
        assert!(m.budget.is_none());
        assert_eq!(m.refused.not_selected + m.refused.trimmed, 0);
    }

    // ---- Plan A Task 13b: the two star populations ----

    fn population_opts(statistic: usize, list: usize) -> MeasureOptions {
        MeasureOptions {
            mode: Mode::Population { statistic, list },
            seeds: SeedSpec::MatchedFilter {
                threshold: seeds::DEFAULT_THRESHOLD,
            },
            ..Default::default()
        }
    }

    /// The pass fits (a) U (b) and nothing else: every other seed keeps its detection record as `NotSelected`, the F-0
    /// identity holds with `not_selected`, list (b) is the first M seeds, and the report counts what was selected.
    #[test]
    fn a_population_fits_a_and_b_and_leaves_the_rest_not_selected() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let opts = MeasureOptions {
            diagnostics: true,
            ..population_opts(200, 150)
        };
        let m = measure_plane(&data, 1024, 1024, &opts, None);
        let r = m.population.clone().expect("a population report");
        assert!(r.statistic > 100 && r.statistic <= 200, "{r:?}");
        assert_eq!(r.list, 150);
        assert!(r.fitted >= 150 && r.fitted <= 150 + r.statistic, "{r:?}");
        assert_eq!(m.refused.not_selected, m.seeds - r.fitted);
        assert_eq!(
            m.stars.len() + m.refused.total() + m.refused.not_selected + m.refused.trimmed,
            m.seeds,
            "ruling F13 with the populations"
        );
        assert_eq!(m.diagnostics.len(), m.seeds, "one record per detected seed");
        assert_eq!(
            m.diagnostics
                .iter()
                .filter(|d| d.outcome == Outcome::NotSelected)
                .count(),
            m.refused.not_selected
        );
        assert!(
            m.diagnostics[..150]
                .iter()
                .all(|d| d.outcome != Outcome::NotSelected),
            "list (b) is the first M seeds"
        );
        assert!(m.budget.is_none());
        assert!(r.statistic_accepted <= r.statistic);
        assert!(m.frame.fwhm_px.is_some());
        // `c` is the median of at most the 64 brightest qualifying fits of (a).
        // R13b-6: no per-stratum cap - a field this rich fills the 64 from (a)'s brightest qualifiers.
        assert_eq!(
            m.aperture_stars,
            photometry::CORRECTION_STARS,
            "{}",
            m.aperture_stars
        );
        assert_eq!(r.aperture_se.is_some(), m.aperture_correction.is_some());
        // Deterministic.
        let again = measure_plane(&data, 1024, 1024, &opts, None);
        assert_eq!(format!("{:?}", again.stars), format!("{:?}", m.stars));
    }

    /// With no population (a) the frame statistic has no star to read, and the star list alone is fitted; with (a) the
    /// statistic reads (a)'s accepted fits only.
    #[test]
    fn the_frame_statistic_reads_population_a_alone() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let list_only = measure_plane(&data, 1024, 1024, &population_opts(0, 120), None);
        let r = list_only.population.clone().unwrap();
        assert_eq!((r.statistic, r.fitted), (0, 120));
        assert_eq!(list_only.frame.fwhm_px, None, "no (a), no frame FWHM");
        assert_eq!(list_only.frame.snr_median, None);
        assert!(!list_only.stars.is_empty(), "the star list is still fitted");
        assert_eq!(list_only.frame.stars_fitted, list_only.stars.len());
    }

    /// The plane's own β and its sample are bit-identical to `Mode::All`'s when the β sample sits inside list (b)
    /// (R13b-3), and a list at or above the seed count fits the very stars `Mode::All` fits (centres, widths, β).
    #[test]
    fn the_own_beta_and_the_fits_of_list_b_are_mode_alls() {
        let data = noisy_field(&uniform(900, 1024, 1024, 5), 5);
        let all = MeasureOptions {
            mode: Mode::All,
            ..population_opts(0, 0)
        };
        let prep_all = prepare_plane(&data, 1024, 1024, &all, None);
        let beta_all = plane_beta(&data, 1024, 1024, &all, &prep_all, None);
        for list in [400, 100_000] {
            let opts = population_opts(300, list);
            let prep = prepare_plane(&data, 1024, 1024, &opts, None);
            let beta = plane_beta(&data, 1024, 1024, &opts, &prep, None);
            assert_eq!(beta.0.to_bits(), beta_all.0.to_bits(), "list {list}");
            assert_eq!(beta.1, beta_all.1, "list {list}");
        }
        let m_all = finish_plane(&data, 1024, 1024, &all, prep_all, None);
        let opts = population_opts(300, 100_000);
        let prep = prepare_plane(&data, 1024, 1024, &opts, None);
        let m = finish_plane(&data, 1024, 1024, &opts, prep, None);
        assert_eq!(m.refused.not_selected, 0);
        assert_eq!(m.stars.len(), m_all.stars.len());
        for (a, b) in m.stars.iter().zip(&m_all.stars) {
            assert_eq!(
                (
                    a.x.to_bits(),
                    a.y.to_bits(),
                    a.fwhm_x.to_bits(),
                    a.beta.to_bits()
                ),
                (
                    b.x.to_bits(),
                    b.y.to_bits(),
                    b.fwhm_x.to_bits(),
                    b.beta.to_bits()
                )
            );
        }
    }

    /// Given seeds carry no filter SNR to select by: every seed is fitted and no report is made.
    #[test]
    fn a_population_over_given_seeds_fits_every_seed() {
        let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let given = test_seeds_of(&data, w, h);
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(given),
                mode: Mode::Population {
                    statistic: 1000,
                    list: 10,
                },
                ..Default::default()
            },
            None,
        );
        assert!(m.population.is_none());
        assert_eq!(m.refused.not_selected, 0);
        assert_eq!(m.refused.accounted(m.stars.len()), m.seeds);
    }

    /// Three planes through the stacking halves under a population: the cross-plane rule reads each plane's records by
    /// their FITTED seeds. The selection is not part of the stored first-pass bytes (route A keeps 5836a970's layout):
    /// a record read back holds none.
    #[test]
    fn the_halves_run_a_population_across_planes() {
        let planes: Vec<Vec<f32>> = (0..2)
            .map(|k| noisy_field(&uniform(500, 1024, 1024, 21), 31 + k))
            .collect();
        let opts = MeasureOptions {
            colour: true,
            ..population_opts(150, 200)
        };
        let mut preps: Vec<PlanePrep> = planes
            .iter()
            .map(|p| prepare_plane(p, 1024, 1024, &opts, None))
            .collect();
        for prep in &preps {
            let pop = prep.population.as_ref().expect("a selection");
            assert_eq!(
                prep.passes.len(),
                pop.fitted.len(),
                "a record per fitted seed"
            );
        }
        cross_plane_flags(&mut preps);
        let bytes = postcard::to_stdvec(&preps[0]).unwrap();
        let back: PlanePrep = postcard::from_bytes(&bytes).unwrap();
        assert!(back.population.is_none(), "the selection is not stored");
        for (p, prep) in planes.iter().zip(preps) {
            let m = finish_plane(p, 1024, 1024, &opts, prep, None);
            let r = m.population.unwrap();
            assert_eq!(m.refused.not_selected, m.seeds - r.fitted);
        }
    }
}
