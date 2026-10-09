//! The per-frame β (spec 2026-10-07 §2.1 `beta`, §3 F-4; Plan A Task 9, ruling R-OSP-6): continuous, the median of
//! the bounded free-β Moffat fits (rustafits `fit_moffat_2d_bounded`: ln β in [`BETA_BOUNDS`]) of the frame's
//! brightest unsaturated stars, clamped to the bounds; every star is then fitted at it. The Gaussian is the large-β
//! limit (a Gaussian frame reads at or near the cap), never a separate model, so β is a finite number everywhere.
//!
//! On the measure path those fits are the seeds' saturation verdict fits (C-b, PA-23): the sample is the
//! [`AUTO_SAMPLE`] brightest seeds whose verdict fit succeeded unflagged, and their β is read, not fitted a second
//! time ([`verdict_beta`]). A caller that ran no verdict — LN's reference fit through `fit_stars`, Plan C's green
//! sites (ruling F45) — gets its own sample from the same starts (Q7, [`frame_beta`]). Fewer than
//! [`BETA_MIN_FITS`] accepted fits: [`BETA_FALLBACK`], [`BetaSource::Fallback`], with a `warn!`.
//!
//! Through Task 8 this module held the discrete `Auto` search over [`AUTO_BETAS`] (moved from `psf_signal` in Task
//! 2); the ladder stays as the probe's betas arm (row 3's β sensitivity), which no production path reads.

use std::sync::Arc;

use tracing::warn;

use super::fit::{self, FitParams, Seed, SeedPass};
use super::photometry::SaturationPlane;
use super::FitStarsOptions;

/// The discrete β ladder the `Auto` search used through Task 8; now the probe's betas arm only.
pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
/// The frame β's sample: at most this many seeds, brightest first.
pub const AUTO_SAMPLE: usize = 64;
/// The bounded free-β fit's bounds and the frame β's clamp (spec §3 F-4): 1.5 below, 100 above — at β 100 a
/// Gaussian star reads −0.32 % in FWHM.
pub const BETA_BOUNDS: (f64, f64) = (1.5, 100.0);
/// The bounded verdict fit's start β (the free arm's start through Task 8, so only the parametrisation, the bounds
/// and the θ column changed).
pub const BETA_START: f64 = 3.0;
/// Fewer accepted fits in the sample than this: [`BETA_FALLBACK`].
pub const BETA_MIN_FITS: usize = 8;
/// The β of a frame whose sample is too small ([`BetaSource::Fallback`]).
pub const BETA_FALLBACK: f64 = 4.0;

/// Where a plane's β came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BetaSource {
    /// `PsfModel::Auto`: the median of `sample` accepted bounded free-β fits ([`BETA_MIN_FITS`] ≤ `sample` ≤
    /// [`AUTO_SAMPLE`]).
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

/// The frame β for a caller that ran no verdict (Q7, ruling F45): the bounded free-β fit — no saturation test, no
/// exclusion, from the verdict fit's own starts ([`SaturationPlane::UNTESTED`]) — of the [`AUTO_SAMPLE`] brightest of
/// `sample` (expected brightest-first), on `opts.mask` and from `opts.field_width` (else the seeds' own,
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
    sample_beta(plane, w, h, sample, &opts.fit, mask, field.fwhm_px, pool)
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
) -> (f64, BetaSource) {
    let sample = &seeds[..seeds.len().min(AUTO_SAMPLE)];
    let passes = fit::seed_passes(
        data,
        w,
        h,
        sample,
        mask,
        field_width,
        &SaturationPlane::UNTESTED,
        p,
        pool,
    );
    median_beta(passes.iter().filter_map(|v| match v.verdict {
        Some(Ok(sv)) if sv.fit => Some(sv.beta),
        _ => None,
    }))
}

/// The measure path's frame β (C-b): the verdict β of the [`AUTO_SAMPLE`] brightest seeds whose verdict fit
/// succeeded unflagged (own and cross-plane flags united first on a multi-plane image), median, clamped; fewer than
/// [`BETA_MIN_FITS`] → the fallback. `passes` in seed order (brightest first).
pub(super) fn verdict_beta(passes: &[SeedPass]) -> (f64, BetaSource) {
    median_beta(
        passes
            .iter()
            .filter_map(|v| match v.verdict {
                Some(Ok(sv)) if sv.fit && !sv.saturated => Some(sv.beta),
                _ => None,
            })
            .take(AUTO_SAMPLE),
    )
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
