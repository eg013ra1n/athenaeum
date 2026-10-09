//! The per-frame β (spec 2026-10-07 §2.1 `beta`, §3 F-4; Plan A Task 9, ruling R-OSP-6): continuous, the median of
//! the bounded free-β Moffat fits (rustafits `fit_moffat_2d_bounded`: ln β in [`BETA_BOUNDS`]) of the frame's
//! brightest unsaturated stars, clamped to the bounds; every star is then fitted at it. The Gaussian is the large-β
//! limit (a Gaussian frame reads at or near the cap), never a separate model, so β is a finite number everywhere.
//!
//! **The sample's fit is pixel-integrated** (the owner's ruling E5, 2026-10-09): a point Moffat fitted to a star whose
//! pixels integrate its light reads β high, more so the fewer pixels its FWHM spans (m2.5: 3.5 at FWHM 1.2 px, 2.8 at
//! 2.6 px), so each sample star is fitted with the model and its Jacobian averaged over `k × k` midpoint sub-samples
//! per pixel, `k = clamp(⌈10 / F_start⌉, 3, 9)` ([`sample_subsample`]). The point model stays everywhere else: the
//! saturation verdict fit and every star's fit at β.
//!
//! On the measure path the sample is the [`AUTO_SAMPLE`] brightest seeds whose verdict fit succeeded unflagged
//! (C-b, PA-23; [`sample_seeds`]), and each is re-fitted on the verdict fit's own problem — its region, minus the
//! pixels its level excluded, from its starts, with its iteration budget ([`verdict_beta`],
//! `photometry::integrated_beta`). A caller that ran no verdict — `fit_stars` (tests and probes; a run's LN reference
//! fit is handed the group β), Plan C's green sites (ruling F45) — fits its own sample of the brightest seeds the same
//! way (Q7, [`frame_beta`]). Fewer than [`BETA_MIN_FITS`] accepted fits: [`BETA_FALLBACK`],
//! [`BetaSource::Fallback`], with a `warn!`.
//!
//! Through Task 8 this module held the discrete `Auto` search over [`AUTO_BETAS`] (moved from `psf_signal` in Task
//! 2); the ladder stays as the probe's betas arm (row 3's β sensitivity), which no production path reads.

use std::sync::Arc;

use tracing::warn;

use super::fit::{self, FitParams, Seed, SeedPass};
use super::FitStarsOptions;

/// The discrete β ladder the `Auto` search used through Task 8; now the probe's betas arm only.
pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
/// The frame β's sample: at most this many seeds, brightest first.
pub const AUTO_SAMPLE: usize = 64;
/// The bounded free-β fit's bounds and the frame β's clamp (spec §3 F-4): 1.5 below, 100 above — at β 100 a
/// Gaussian star reads −0.32 % in FWHM.
pub const BETA_BOUNDS: (f64, f64) = (1.5, 100.0);
/// The bounded verdict fit's start β (the free arm's start through Task 8, so only the parametrisation, the bounds
/// and the θ column changed), and the sample fit's.
pub const BETA_START: f64 = 3.0;
/// Fewer accepted fits in the sample than this: [`BETA_FALLBACK`].
pub const BETA_MIN_FITS: usize = 8;
/// The β of a frame whose sample is too small ([`BetaSource::Fallback`]).
pub const BETA_FALLBACK: f64 = 4.0;
/// The sample fit's sub-samples per pixel side (E5): `⌈SAMPLE_SUBSAMPLE_PX / F_start⌉`, clamped to
/// [`SAMPLE_SUBSAMPLE_RANGE`] — about 10 sub-samples across the star's FWHM.
pub const SAMPLE_SUBSAMPLE_PX: f64 = 10.0;
/// The clamp of [`sample_subsample`]: at least 3 per side (a well-sampled star still sees its pixels' curvature), at
/// most 9 (81 model evaluations per pixel).
pub const SAMPLE_SUBSAMPLE_RANGE: (usize, usize) = (3, 9);

/// The sample fit's `k` for a star whose width start is `f_start` px (E5): `clamp(⌈10 / f_start⌉, 3, 9)`; a
/// non-finite or non-positive width reads the clamp's end it tends to.
pub fn sample_subsample(f_start: f64) -> usize {
    let (lo, hi) = SAMPLE_SUBSAMPLE_RANGE;
    let k = (SAMPLE_SUBSAMPLE_PX / f_start).ceil();
    if k.is_nan() {
        return lo;
    }
    k.clamp(lo as f64, hi as f64) as usize
}

/// Where a plane's β came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BetaSource {
    /// `PsfModel::Auto`: the median of `sample` accepted pixel-integrated bounded free-β fits ([`BETA_MIN_FITS`] ≤
    /// `sample` ≤ [`AUTO_SAMPLE`]).
    Fitted { sample: usize },
    /// `PsfModel::Auto` with fewer than [`BETA_MIN_FITS`] accepted fits: β = [`BETA_FALLBACK`].
    Fallback,
    /// The caller's β (`PsfModel::Moffat4`, `PsfModel::Fixed`, `fit_stars_at`): no sample.
    Fixed,
}

impl BetaSource {
    /// The probe's `betaSource`: `fitted` | `fallback` | `fixed`.
    pub fn name(self) -> &'static str {
        match self {
            BetaSource::Fitted { .. } => "fitted",
            BetaSource::Fallback => "fallback",
            BetaSource::Fixed => "fixed",
        }
    }

    /// The accepted fits the β is the median of; `None` unless [`BetaSource::Fitted`].
    pub fn sample(self) -> Option<usize> {
        match self {
            BetaSource::Fitted { sample } => Some(sample),
            BetaSource::Fallback | BetaSource::Fixed => None,
        }
    }
}

/// A fitted frame β at a bound of [`BETA_BOUNDS`]: every star fitted at it carries `StarFlags::BETA_AT_CAP`. A
/// bounded fit that ends at a bound returns the bound exactly, and the median of such fits is the bound exactly.
pub fn at_bound(beta: f64) -> bool {
    beta <= BETA_BOUNDS.0 || beta >= BETA_BOUNDS.1
}

/// A plane's β, where it came from, and the sample's accepted fits as `(seed index, integrated β)` in sample order
/// (each sample star's [`super::FitDiagnostics::sample_beta`]); `fits` is empty for a caller's β.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SampleBeta {
    pub(super) beta: f64,
    pub(super) source: BetaSource,
    pub(super) fits: Vec<(usize, f64)>,
}

impl SampleBeta {
    /// The caller's β ([`BetaSource::Fixed`]): no sample.
    pub(super) fn fixed(beta: f64) -> Self {
        SampleBeta {
            beta,
            source: BetaSource::Fixed,
            fits: Vec::new(),
        }
    }
}

/// The frame β for a caller that ran no verdict (Q7, ruling F45): the pixel-integrated bounded free-β fit (E5) — no
/// saturation test, no exclusion, from the verdict fit's own starts — of the [`AUTO_SAMPLE`] brightest of `sample`
/// (expected brightest-first), on `opts.mask` and from `opts.field_width` (else the seeds' own,
/// `seeds::field_width_of`); the median of the accepted fits' β, clamped to [`BETA_BOUNDS`]; fewer than
/// [`BETA_MIN_FITS`] → `(BETA_FALLBACK, BetaSource::Fallback)` with a `warn!`.
pub fn frame_beta(
    plane: &[f32],
    w: usize,
    h: usize,
    sample: &[Seed],
    opts: &FitStarsOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (f64, BetaSource) {
    let mask = opts.mask.as_deref();
    let field = fit::resolve_field_width(plane, w, h, sample, mask, opts.field_width);
    let s = sample_beta(plane, w, h, sample, &opts.fit, mask, field.fwhm_px, pool);
    (s.beta, s.source)
}

/// [`frame_beta`] on a field width the caller already resolved (`fit_stars`'s, from every seed).
#[allow(clippy::too_many_arguments)]
pub(super) fn sample_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: f64,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> SampleBeta {
    let sample = &seeds[..seeds.len().min(AUTO_SAMPLE)];
    let betas = fit::own_sample_fits(data, w, h, sample, mask, field_width, p, pool);
    let indices: Vec<usize> = (0..sample.len()).collect();
    sample_median(&indices, &betas)
}

/// The measure path's frame β (C-b as revised by E5): the [`sample_seeds`] of `passes` (seed order, brightest first;
/// cross-plane flags already united), each re-fitted with the pixel-integrated model on its verdict fit's own problem
/// (`fit::sample_fits`); the median of the accepted fits' β, clamped; fewer than [`BETA_MIN_FITS`] → the fallback.
#[allow(clippy::too_many_arguments)]
pub(super) fn verdict_beta(
    data: &[f32],
    w: usize,
    seeds: &[Seed],
    p: &FitParams,
    mask: Option<&[bool]>,
    passes: &[SeedPass],
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> SampleBeta {
    let sample = sample_seeds(passes);
    let betas = fit::sample_fits(data, w, seeds, mask, p, passes, &sample, pool);
    sample_median(&sample, &betas)
}

/// The measure path's β sample (C-b): the indices of the [`AUTO_SAMPLE`] first seeds of `passes` (seed order,
/// brightest first) whose verdict fit succeeded unflagged — an own or a cross-plane flag, a failed verdict fit, a
/// `MostlySaturated` refusal or no verdict at all keeps a seed out.
pub(super) fn sample_seeds(passes: &[SeedPass]) -> Vec<usize> {
    passes
        .iter()
        .enumerate()
        .filter_map(|(i, v)| match v.verdict {
            Some(Ok(sv)) if sv.fit && !sv.saturated => Some(i),
            _ => None,
        })
        .take(AUTO_SAMPLE)
        .collect()
}

/// The frame β from the sample's fits: `betas[j]` is seed `sample[j]`'s integrated β, `None` = not accepted; the
/// accepted ones' median ([`median_beta`]) and the fits themselves.
fn sample_median(sample: &[usize], betas: &[Option<f64>]) -> SampleBeta {
    debug_assert_eq!(sample.len(), betas.len());
    let fits: Vec<(usize, f64)> = sample
        .iter()
        .zip(betas)
        .filter_map(|(&i, b)| b.filter(|b| b.is_finite()).map(|b| (i, b)))
        .collect();
    let (beta, source) = median_beta(fits.iter().map(|&(_, b)| b));
    SampleBeta { beta, source, fits }
}

/// The median of the sample's β (the mean of the two middle ones for an even count), clamped to [`BETA_BOUNDS`].
fn median_beta(betas: impl Iterator<Item = f64>) -> (f64, BetaSource) {
    let mut v: Vec<f64> = betas.filter(|b| b.is_finite()).collect();
    if v.len() < BETA_MIN_FITS {
        warn!(
            count = v.len(),
            beta = BETA_FALLBACK,
            "beta: fewer than 8 accepted free-beta fits in the sample; the fallback beta is used"
        );
        return (BETA_FALLBACK, BetaSource::Fallback);
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    };
    (
        median.clamp(BETA_BOUNDS.0, BETA_BOUNDS.1),
        BetaSource::Fitted { sample: n },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::{measure_plane, seeds, MeasureOptions, SeedSpec};
    use crate::test_support::{integrated_field, IntegratedProfile};

    /// A noiseless field of 70 stars of one `profile` (10 × 7, off-grid centres, amplitudes 0.2–0.4 on a 0.05 sky, the
    /// first `bright` at 0.9), every pixel integrated over 15 × 15 sub-samples, spaced so no star's rendered window
    /// reaches another's; the plane, its size and the seeds at the true centres, brightest first.
    fn integrated_stars(
        profile: IntegratedProfile,
        bright: usize,
    ) -> (Vec<f32>, usize, usize, Vec<Seed>) {
        let f = match profile {
            IntegratedProfile::Moffat { fwhm, .. } | IntegratedProfile::Gaussian { fwhm } => fwhm,
        };
        let reach = (6.0 * f).max(6.0) + 2.0;
        let spacing = (2.0 * reach).ceil() + 2.0;
        let margin = reach.ceil() + 2.0;
        let (w, h) = (
            (2.0 * margin + 9.0 * spacing + 2.0) as usize,
            (2.0 * margin + 6.0 * spacing + 2.0) as usize,
        );
        let mut stars: Vec<(f64, f64, f64)> = (0..70)
            .map(|i| {
                let (fx, fy) = ((i as f64 * 0.618_034) % 1.0, (i as f64 * 0.414_214) % 1.0);
                (
                    margin + (i % 10) as f64 * spacing + fx,
                    margin + (i / 10) as f64 * spacing + fy,
                    if i < bright {
                        0.9
                    } else {
                        0.2 + 0.05 * (i % 5) as f64
                    },
                )
            })
            .collect();
        stars.sort_by(|a, b| b.2.total_cmp(&a.2));
        let data = integrated_field(w, h, &stars, profile, 15, 0.05);
        let seeds = stars
            .iter()
            .map(|&(x, y, amp)| Seed {
                x,
                y,
                peak: amp,
                flux: amp,
            })
            .collect();
        (data, w, h, seeds)
    }

    /// E5 (the owner's ruling 2026-10-09): on pixel-integrated stars the frame β is the truth β within 1 % at every
    /// sampling from FWHM 1.2 px (undersampled; the point model read m2.5 at 3.5 there) to 8 px — the sample fit
    /// integrates its model over `k = clamp(⌈10/F_start⌉, 3, 9)` sub-samples per pixel side.
    fn frame_beta_reads_the_truth_beta_on_integrated_stars(beta: f64) {
        let mut worst = 0.0_f64;
        for fwhm in [1.2, 1.6, 2.0, 2.6, 3.5, 5.0, 8.0] {
            let (data, w, h, seeds) = integrated_stars(IntegratedProfile::Moffat { fwhm, beta }, 0);
            let (got, src) = frame_beta(&data, w, h, &seeds, &FitStarsOptions::default(), None);
            let err = got / beta - 1.0;
            println!(
                "integrated m{beta} F {fwhm}: frame beta {got:.4} ({:+.3} %), {src:?}",
                100.0 * err
            );
            assert_eq!(src, BetaSource::Fitted { sample: 64 }, "m{beta} F {fwhm}");
            assert!(
                err.abs() < 0.01,
                "m{beta} F {fwhm}: frame beta {got} vs {beta}"
            );
            worst = worst.max(err.abs());
        }
        println!("integrated m{beta}: worst {:.3} %", 100.0 * worst);
    }

    #[test]
    fn frame_beta_is_the_truth_on_integrated_moffat_2_5_stars() {
        frame_beta_reads_the_truth_beta_on_integrated_stars(2.5);
    }

    #[test]
    fn frame_beta_is_the_truth_on_integrated_moffat_4_stars() {
        frame_beta_reads_the_truth_beta_on_integrated_stars(4.0);
    }

    #[test]
    fn frame_beta_is_the_truth_on_integrated_moffat_10_stars() {
        frame_beta_reads_the_truth_beta_on_integrated_stars(10.0);
    }

    /// The Gaussian is the large-β limit on integrated pixels too: an integrated Gaussian frame reads β ≥ 60.
    #[test]
    fn an_integrated_gaussian_frame_reads_near_the_cap() {
        for fwhm in [1.6, 4.0] {
            let (data, w, h, seeds) = integrated_stars(IntegratedProfile::Gaussian { fwhm }, 0);
            let (got, src) = frame_beta(&data, w, h, &seeds, &FitStarsOptions::default(), None);
            println!("integrated gaussian F {fwhm}: frame beta {got:.4}, {src:?}");
            assert!(got >= 60.0, "F {fwhm}: {got}");
            assert!(matches!(src, BetaSource::Fitted { .. }));
        }
    }

    /// The measure path (verdicts first): the frame β is the median of the sample's integrated fits, and each sample
    /// star's record carries its own (`FitDiagnostics::sample_beta`) — only theirs, every one an unflagged successful
    /// verdict among the first 64 such.
    #[test]
    fn the_measure_path_reads_beta_from_the_samples_integrated_fits() {
        // Two stars well above the rest set the plane's range, so the 68 others stay unflagged.
        let (mut data, w, h, seeds) = integrated_stars(
            IntegratedProfile::Moffat {
                fwhm: 2.0,
                beta: 2.5,
            },
            2,
        );
        crate::test_support::add_noise(&mut data, 0.0005, 11);
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds.clone()),
                diagnostics: true,
                ..Default::default()
            },
            None,
        );
        println!(
            "measure path, integrated m2.5 F 2.0: beta {:.4} {:?}",
            m.beta, m.beta_source
        );
        assert!((m.beta / 2.5 - 1.0).abs() < 0.02, "{}", m.beta);
        let eligible: Vec<usize> = m
            .diagnostics
            .iter()
            .filter(|d| matches!(d.verdict, Some(v) if v.fit && !v.saturated))
            .map(|d| d.seed)
            .take(AUTO_SAMPLE)
            .collect();
        let with: Vec<(usize, f64)> = m
            .diagnostics
            .iter()
            .filter_map(|d| d.sample_beta.map(|b| (d.seed, b)))
            .collect();
        assert!(with.iter().all(|(s, _)| eligible.contains(s)));
        assert_eq!(m.beta_source, BetaSource::Fitted { sample: with.len() });
        assert!(with.len() >= 60, "{}", with.len());
        assert_eq!(median_beta(with.iter().map(|&(_, b)| b)).0, m.beta);
    }

    /// Review M-3: the sample is the first [`AUTO_SAMPLE`] seeds (seed order) whose verdict fit succeeded unflagged; a
    /// cross-plane flag, an own flag, a failed verdict fit, a `MostlySaturated` refusal and a seed with no verdict are
    /// left out, and the frame β is the median of the chosen seeds' fits.
    #[test]
    fn the_sample_is_the_first_64_unflagged_successful_verdicts() {
        use crate::star_measure::photometry::SaturationVerdict;
        use crate::star_measure::RefusalReason;
        let v = |beta: f64, saturated: bool, cross: bool, fit: bool| SaturationVerdict {
            plateau: false,
            level: 1.0,
            excluded: 0,
            amplitude: 1.0,
            beta,
            fwhm: 3.0,
            saturated,
            fit,
            iterations: 5,
            cross,
        };
        let mut passes = vec![
            SeedPass::for_test(Some(Ok(v(9.0, true, true, true)))), // cross-flagged
            SeedPass::for_test(Some(Ok(v(f64::NAN, false, false, false)))), // verdict fit failed
            SeedPass::for_test(Some(Err(RefusalReason::MostlySaturated))),
            SeedPass::for_test(None), // refused before its region
            SeedPass::for_test(Some(Ok(v(8.0, true, false, true)))), // own flag
        ];
        passes.extend(
            (0..70).map(|j| {
                SeedPass::for_test(Some(Ok(v(2.0 + 0.01 * j as f64, false, false, true))))
            }),
        );
        let sample = sample_seeds(&passes);
        assert_eq!(sample, (5..69).collect::<Vec<_>>());
        let verdict_beta_of = |i: usize| match passes[i].verdict {
            Some(Ok(sv)) => Some(sv.beta),
            _ => None,
        };
        let betas: Vec<Option<f64>> = sample.iter().map(|&i| verdict_beta_of(i)).collect();
        let s = sample_median(&sample, &betas);
        assert_eq!(s.source, BetaSource::Fitted { sample: 64 });
        assert!((s.beta - 2.315).abs() < 1e-12, "{}", s.beta);
        assert_eq!(
            s.fits,
            sample
                .iter()
                .map(|&i| (i, verdict_beta_of(i).unwrap()))
                .collect::<Vec<_>>()
        );
        // a failed sample fit leaves its seed out of the median and out of `fits`
        let mut betas = betas;
        betas[0] = None;
        let s = sample_median(&sample, &betas);
        assert_eq!(s.source, BetaSource::Fitted { sample: 63 });
        assert_eq!(s.fits.first().map(|f| f.0), Some(6));
    }

    #[test]
    fn the_sample_fit_samples_about_ten_per_fwhm_between_three_and_nine() {
        let k: Vec<usize> = [0.0, 1.0, 1.2, 2.0, 2.6, 3.5, 5.0, 10.0, 40.0, f64::NAN]
            .iter()
            .map(|&f| sample_subsample(f))
            .collect();
        assert_eq!(k, [9, 9, 9, 5, 4, 3, 3, 3, 3, 3]);
    }

    #[test]
    fn frame_beta_is_the_median_of_the_sample_and_gaussian_reads_near_the_cap() {
        use crate::test_support::{gaussian_field, moffat_field, MoffatStar};
        let stars: Vec<MoffatStar> = (0..70)
            .map(|i| MoffatStar {
                x: 20.0 + (i % 10) as f64 * 40.3,
                y: 20.0 + (i / 10) as f64 * 40.7,
                amp: 0.2 + 0.05 * (i % 5) as f64,
                alpha_x: 5.0,
                alpha_y: 5.0,
                theta: 0.0,
            })
            .collect();
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: s.amp * 26.0,
            })
            .collect();
        let data = moffat_field(420, 320, &stars, 4.0, 0.05);
        let (beta, src) = frame_beta(&data, 420, 320, &seeds, &FitStarsOptions::default(), None);
        assert!((beta - 4.0).abs() < 0.2, "{beta}");
        assert_eq!(src, BetaSource::Fitted { sample: 64 });
        let g = gaussian_field(
            420,
            320,
            &stars.iter().map(|s| (s.x, s.y, s.amp)).collect::<Vec<_>>(),
            2.0,
            0.05,
        );
        let (beta, _) = frame_beta(&g, 420, 320, &seeds, &FitStarsOptions::default(), None);
        assert!(beta >= 60.0, "{beta}");
        let m = measure_plane(
            &g,
            420,
            320,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds.clone()),
                ..Default::default()
            },
            None,
        );
        assert_eq!(
            m.stars.iter().all(|f| f.flags.beta_at_cap()),
            m.beta >= BETA_BOUNDS.1 - 1e-9
        );
    }

    #[test]
    fn beta_sample_below_eight_falls_back_to_four_and_says_so() {
        use crate::test_support::{moffat_field, MoffatStar};
        let stars: Vec<MoffatStar> = (0..5)
            .map(|i| MoffatStar {
                x: 30.0 + i as f64 * 40.0,
                y: 30.0,
                amp: 0.3,
                alpha_x: 5.0,
                alpha_y: 5.0,
                theta: 0.0,
            })
            .collect();
        let data = moffat_field(240, 60, &stars, 2.5, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        let (beta, src) = frame_beta(&data, 240, 60, &seeds, &FitStarsOptions::default(), None);
        assert_eq!((beta, src), (4.0, BetaSource::Fallback));
        let m = measure_plane(
            &data,
            240,
            60,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds),
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.beta_source, BetaSource::Fallback);
        assert_eq!(m.stars.len(), 5, "the frame still measures");
    }

    #[test]
    fn every_star_is_fitted_at_the_frame_beta() {
        let (data, w, h) = crate::test_support::fixture_field(7, 1.0, 0.002);
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::MatchedFilter {
                    threshold: seeds::DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        assert!(m.stars.len() >= 140);
        assert!(m.stars.iter().all(|f| f.beta == m.beta));
        assert!(matches!(m.beta_source, BetaSource::Fitted { .. }));
    }

    /// The median is the middle accepted β (the mean of the two middle ones for an even count), clamped to the
    /// bounds; a sample of exactly [`BETA_MIN_FITS`] is fitted, one fewer falls back.
    #[test]
    fn the_median_its_minimum_and_the_bound_flag() {
        let b = |v: &[f64]| median_beta(v.iter().copied());
        assert_eq!(
            b(&[3.0, 1.6, 9.0, 2.0, 5.0, 4.0, 7.0, 8.0, 6.0]),
            (5.0, BetaSource::Fitted { sample: 9 })
        );
        assert_eq!(
            b(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]),
            (5.5, BetaSource::Fitted { sample: 8 })
        );
        assert_eq!(
            b(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
            (BETA_FALLBACK, BetaSource::Fallback)
        );
        assert_eq!(b(&[100.0; 8]).0, 100.0);
        assert!(at_bound(100.0) && at_bound(1.5) && !at_bound(99.99) && !at_bound(1.51));
        assert_eq!(BetaSource::Fitted { sample: 12 }.sample(), Some(12));
        assert_eq!(
            [
                BetaSource::Fitted { sample: 1 },
                BetaSource::Fallback,
                BetaSource::Fixed
            ]
            .map(BetaSource::name),
            ["fitted", "fallback", "fixed"]
        );
    }
}
