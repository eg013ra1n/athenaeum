//! One star-measurement path (spec `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md` §2.1):
//! the stacking Measure stage's per-star fit, per-frame β, frame aggregates, noise and sky statistic as ONE module the
//! stacking adapter (`stacking::measure`), local normalization's refit and — from Plan C — the Analysis tab call.
//! `render`-gated like `analysis`; nothing here depends on `stacking` (the stacking-only seed sources, the peak and
//! structure detectors and the median prefilter, stay adapter-side and hand their seeds in as [`SeedSpec::Given`]).
//!
//! Plan A Task 2 moved the code here verbatim (no arithmetic changed, no operation reordered); the later tasks fix it
//! defect by defect against the synthetic truth lab.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::integration::stats::ScaleEstimator;

pub mod beta;
pub mod diagnostics;
pub mod domain;
pub mod fit;
pub mod frame;
pub mod photometry;
pub mod robust;
pub mod seeds;

pub use beta::{AUTO_BETAS, AUTO_SAMPLE};
pub use diagnostics::{FitDiagnostics, Outcome, RefusalCounts, RefusalReason};
pub use domain::{
    background_residual, noise_model, noise_mrs, sky_level, to_working_units, Domain, NoiseModel,
    NoiseSource, SkyLevel, ADU_SCALE,
};
pub use fit::{fwtm_from_alpha, FitOutcome, FitParams, PsfModel, Seed, StarFit, StarFlags};
pub use frame::{frame_shape, signal_totals, FrameAggregates, SignalTotals, RCR_LIMIT};
pub use photometry::{SaturationPlane, SaturationVerdict};
pub use seeds::{FieldWidth, MatchedFilter, DEFAULT_THRESHOLD};

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
/// measures, never one per task; Plan C = 5. `AnalysisConfig::config_hash`
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
    /// pixel present. The noise model and the sky statistic read the whole plane.
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
        }
    }
}

/// Wall time per phase the module owns (I-1); the adapter adds `read_ms` and its own `detect_ms` and keeps
/// `ChannelMeasurement.timings` exactly as today. A phase the module did not run (a precomputed noise model, seeds
/// handed in) reads 0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
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
    /// The module's two-pass matched filter at the field width, `threshold` in units of the filtered plane's measured
    /// noise ([`DEFAULT_THRESHOLD`], ruling R-OSP-2).
    MatchedFilter { threshold: f64 },
}

/// What [`fit_stars_at`] fits with besides the plane, the seeds and β (Task 6a; Tasks 9-11 add fields, so a caller
/// writes `..Default::default()`, ruling F14).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FitStarsOptions {
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
}

/// One plane's measurement (PA-1: the stacking adapter keeps its own `stacking::measure::PlaneMeasurement`). Energies
/// (`stars`, `frame`, `noise`) are in the module's working units; `sky` is in the plane's native units.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneMeasurement {
    /// The accepted fits, after the ±1 px duplicate drop.
    pub stars: Vec<StarFit>,
    /// The β every star was fitted at.
    pub beta: f64,
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
    /// The seeds whose saturation verdict fit failed for a reason other than n ≤ p (F-3, Q4): no flag, no refusal,
    /// out of the β sample.
    pub verdict_fit_failed: usize,
    /// The seeds flagged by the cross-plane rule ([`cross_plane_flags`]: another plane's own flag at their position);
    /// 0 on a single plane.
    pub cross_flags: usize,
    pub timings: PhaseTimings,
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
    let working = domain::to_working_units(plane, &opts.domain);
    let prep = prepare_working(plane, &working, w, h, opts, pool);
    finish_working(&working, w, h, opts, prep, pool)
}

/// One plane measured through its verdict pass (F-3; Task 8 fix wave, the cross-plane rule): the noise model, the
/// seeds and their field width, the sky statistic and every seed's verdict-pass record — what [`finish_plane`] needs
/// besides the plane. Holds no plane-sized buffer.
#[derive(Debug, Clone)]
pub struct PlanePrep {
    noise: NoiseModel,
    sky: SkyLevel,
    /// The seeds handed to the fitter: the given ones, or the module's matched filter's.
    seeds: Vec<Seed>,
    /// `seeds` came from the module's matched filter (they are [`PlaneMeasurement::detected`]).
    detected: bool,
    /// The width reported as [`PlaneMeasurement::field_width`].
    field_width: FieldWidth,
    /// The fit's own width ([`fit::verdict_pass`]), the one the fits at β start from.
    fit_field: FieldWidth,
    /// One per seed, in seed order.
    passes: Vec<fit::SeedPass>,
    cross_flags: usize,
    timings: PhaseTimings,
    /// The verdict pass's share of [`PhaseTimings::fit_ms`].
    verdict_time: Duration,
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
    let working = domain::to_working_units(plane, &opts.domain);
    prepare_working(plane, &working, w, h, opts, pool)
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
    let working = domain::to_working_units(plane, &opts.domain);
    finish_working(&working, w, h, opts, prep, pool)
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
        for (s, pass) in prep.seeds.iter().zip(&prep.passes) {
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
        for (s, pass) in prep.seeds.iter().zip(prep.passes.iter_mut()) {
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

/// [`prepare_plane`] on a working copy the caller holds.
fn prepare_working(
    plane: &[f32],
    working: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlanePrep {
    let (noise, mut timings) = match opts.noise {
        Some(given) => (given, PhaseTimings::default()),
        None => domain::noise_model(working, w, h, pool),
    };
    let mask = opts.mask.as_deref();
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
    let (detected, field_width) = match &opts.seeds {
        SeedSpec::None => (
            None,
            given_width.unwrap_or_else(|| seeds::field_width_of(working, w, h, &[], mask)),
        ),
        SeedSpec::Given(given) => (
            None,
            given_width.unwrap_or_else(|| seeds::field_width_of(working, w, h, given, mask)),
        ),
        SeedSpec::MatchedFilter { threshold } => {
            let (found, own) = seeds::detect(
                working,
                w,
                h,
                &noise,
                *threshold,
                opts.max_stars,
                mask,
                pool,
            );
            timings.detect_ms = t.elapsed().as_millis() as u64;
            (Some(found), given_width.unwrap_or(own))
        }
    };
    let (seeds, detected) = match (detected, &opts.seeds) {
        (Some(found), _) => (found, true),
        (None, SeedSpec::Given(given)) => (given.clone(), false),
        (None, _) => (Vec::new(), false),
    };
    // The saturation test reads the sky median (F-3), so the sky statistic precedes the fits (Q7).
    let sky = domain::sky_level(plane, w, h, &opts.domain, opts.scale_estimator);
    let saturation = saturation_plane(
        working,
        mask,
        &noise,
        &sky,
        &opts.domain,
        opts.colour,
        seeds.len(),
    );
    let t = Instant::now();
    let pass_call = || {
        fit::verdict_pass(
            working,
            w,
            h,
            &seeds,
            &opts.fit,
            mask,
            Some(field_width.fwhm_px),
            &saturation,
            None,
        )
    };
    // Already inside this `install`: the pass's own pool param gets `None` (perf tier 1 Task 2).
    let (fit_field, passes) = match pool {
        Some(p) => p.install(pass_call),
        None => pass_call(),
    };
    PlanePrep {
        noise,
        sky,
        seeds,
        detected,
        field_width,
        fit_field,
        passes,
        cross_flags: 0,
        timings,
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
        fit_field,
        passes,
        cross_flags,
        mut timings,
        verdict_time,
    } = prep;
    let t = Instant::now();
    let fit_call = || {
        fit::fit_after_passes(
            working,
            w,
            h,
            &seeds,
            opts.psf_model,
            &opts.fit,
            opts.mask.as_deref(),
            fit_field,
            Some(&passes),
            None,
            opts.diagnostics,
            true,
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
    debug_assert_eq!(
        outcome.refused.accounted(outcome.fits.len()),
        seeds.len(),
        "every seed has exactly one outcome (ruling F13)"
    );
    let diagnostics = std::mem::take(&mut outcome.diagnostics);
    let frame = FrameAggregates::of(&outcome.fits);
    PlaneMeasurement {
        stars: outcome.fits,
        beta: outcome.beta,
        frame,
        noise,
        sky,
        seeds: seeds.len(),
        detected: if detected { seeds } else { Vec::new() },
        field_width,
        refused: outcome.refused,
        diagnostics,
        defocused: outcome.defocused,
        defocus_donuts: outcome.defocus_donuts,
        verdict_fit_failed: outcome.verdict_fit_failed,
        cross_flags,
        timings,
    }
}

/// The plane-level inputs of the saturation test (spec §2.1, PA-23), in working units: `bg` = the sky median
/// (`sky.median`, the native plane's, scaled by `ADU_SCALE` for a calibrated plane), σ = `noise.sigma`, `norm` =
/// `ADU_SCALE` for `Domain::Calibrated` and `full_scale` for `Domain::RawAdu` (Q2), the plane max over the working
/// copy's finite, unmasked pixels. Under [`NoiseSource::None`] σ is 0 and the threshold is the sky median: one
/// `warn!` per plane when there are seeds to judge (Q1).
fn saturation_plane(
    working: &[f32],
    mask: Option<&[bool]>,
    noise: &NoiseModel,
    sky: &SkyLevel,
    domain: &Domain,
    colour: bool,
    n_seeds: usize,
) -> SaturationPlane {
    let (scale, norm) = match domain {
        Domain::Calibrated { .. } => (ADU_SCALE as f64, ADU_SCALE as f64),
        Domain::RawAdu { full_scale, .. } => (1.0, *full_scale),
    };
    if noise.source == NoiseSource::None && n_seeds > 0 {
        tracing::warn!(
            count = n_seeds,
            "measure: no noise estimate; the saturation test's threshold is the sky median"
        );
    }
    let plane_max = working
        .iter()
        .enumerate()
        .filter(|&(i, v)| v.is_finite() && mask.map_or(true, |m| m[i]))
        .fold(f64::NEG_INFINITY, |m, (_, &v)| m.max(v as f64));
    SaturationPlane::new(sky.median * scale, noise.sigma, plane_max, norm, colour)
}

/// The refit entry point LN uses (PA-6): every seed fitted at `beta`. The plane is in the module's working units
/// (ADU-scaled), exactly as `fit_stars_with_beta` takes it. `opts.mask` makes pixels absent in the region and the
/// aperture; `opts.field_width` (else [`seeds::field_width_of`] the seeds) sizes every star's width-start box (Task
/// 6b, PA-12) and is reported as `FitOutcome.field_width`; `opts.diagnostics` keeps one record per seed in
/// `FitOutcome.diagnostics`.
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_field;

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
            fit: true,
            cross: false,
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
    /// The plane makes the two estimators read different stars: 120 Moffat-4 stars, 60 narrow (FWHM 3, amplitude
    /// 0.4) and 60 broad (FWHM 7, amplitude 0.3). The 100 brightest by peak — what `field_width_of` the given seeds
    /// reads — are the 60 narrow and 40 broad ones (a narrow median); the matched filter's first pass ranks by its
    /// 3-px flux, where a broad star reads ≈ 1.7× its amplitude, so its 100 are the 60 broad and 40 narrow ones (a
    /// broad median) — box radii ⌈2·F⌉ far apart.
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
            &AdapterOptions::default(),
            None,
            SeedOverride::MatchedFilter {
                threshold: DEFAULT_THRESHOLD,
            },
            false,
            false,
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
        assert_eq!(seeds.len(), m.seeds);
        // The premise: `field_width_of` the same seeds reads a different width.
        let given = seeds::field_width_of(&working(&data), w, h, &seeds, None);
        assert!(
            (given.fwhm_px - m.field_width.fwhm_px).abs() > 1.0,
            "given {given:?} vs the matched filter's {:?}",
            m.field_width
        );
        assert_eq!(pm.field_width, m.field_width, "the detector's own width");
        assert_eq!(pm.channel.beta, m.beta);
        assert_eq!(pm.fits, m.stars, "the same fits");
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
        let (seeds, _) = seeds::detect(
            &scaled,
            240,
            240,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            None,
            None,
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
        let (seeds, _) = seeds::detect(
            &scaled,
            w,
            h,
            &noise,
            DEFAULT_THRESHOLD,
            24576,
            Some(&mask),
            None,
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
            // (R2: FWHM within 0.2 %). The flag is not pinned: the lone star sets the range itself (R2: A/range 0.994)
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
            assert!((v.fwhm / truth - 1.0).abs() < 0.03, "{} vs {truth}", v.fwhm);
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
            // 70 Moffat-4 stars, the 40 brightest clipped: the discrete Auto search must still resolve 4.0 (the baseline
            // flipped to 10); R2: 40 of 40 flagged, 0 of 30
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
            assert_eq!(
                m.beta, 4.0,
                "the sample is the unsaturated 30, not the clipped 40"
            );
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
            let free = fit_stars_at(
                &working,
                w,
                h,
                &seeds,
                m.beta,
                &FitStarsOptions {
                    field_width: Some(m.field_width.fwhm_px),
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
            let (with, without) = (at(&m.stars, 70.0), at(&free.fits, 70.0));
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
}
