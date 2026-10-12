//! Frame aggregates over the accepted fits (math reference §1.1; spec 2026-10-07 §2.1 `frame`): the PSF-signal totals
//! `TFlux` / `TMeanFlux` and the residual-weighted frame FWHM and eccentricity. Moved verbatim from `psf_signal` (Plan
//! A Task 2). F-3 (Task 8): the frame FWHM and eccentricity are the residual-weighted means over the accepted
//! UNSATURATED fits (the statistic's population; F-10, Task 10b: nor extended); the signal totals still sum every
//! accepted fit.

use super::fit::StarFit;

/// Chauvenet's criterion, the rejection limit for the mean-flux vector.
pub const RCR_LIMIT: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalTotals {
    /// `Σ signal_i` over every accepted fit.
    pub tflux: f64,
    /// `Σ mean_i` after RCR and Winsorization of the mean fluxes.
    pub tmean_flux: f64,
    /// Mean fluxes RCR flagged (and Winsorization replaced).
    pub rejected: usize,
}

fn kahan_sum(it: impl Iterator<Item = f64>) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for x in it {
        let y = x - c;
        let t = s + y;
        c = (t - s) - y;
        s = t;
    }
    s
}

/// `TFlux` and `TMeanFlux` (math reference §1.1): the mean-flux vector is
/// cleaned by RCR (limit 0.5) and Winsorized so nothing leaves the sum.
pub fn signal_totals(fits: &[StarFit]) -> SignalTotals {
    if fits.is_empty() {
        return SignalTotals {
            tflux: 0.0,
            tmean_flux: 0.0,
            rejected: 0,
        };
    }
    debug_assert!(fits.iter().all(|f| f.area > 0.0));
    let means: Vec<f64> = fits.iter().map(StarFit::mean_flux).collect();
    let non_finite = means.iter().filter(|m| !m.is_finite()).count();
    if non_finite > 0 {
        tracing::warn!(
            count = non_finite,
            "non-finite mean fluxes dropped from the PSF-signal totals"
        );
    }
    let finite_means: Vec<f64> = means.iter().copied().filter(|m| m.is_finite()).collect();
    let r = super::robust::rcr(&finite_means, RCR_LIMIT);
    let w = super::robust::winsorize(&finite_means, &r.kept);
    SignalTotals {
        tflux: kahan_sum(fits.iter().map(|f| f.signal).filter(|s| s.is_finite())),
        tmean_flux: kahan_sum(w.iter().copied()),
        rejected: r.rejected,
    }
}

/// Frame FWHM and eccentricity: residual-weighted means over the fits
/// (`ω_i = res_min / res_i`, residuals floored at 1e-6). `None` without fits.
/// Any iterator of fits (Task 8: [`FrameAggregates::of`] hands it the unsaturated ones); it is walked twice.
pub fn frame_shape<'a, I>(fits: I) -> Option<(f64, f64)>
where
    I: IntoIterator<Item = &'a StarFit>,
    I::IntoIter: Clone,
{
    let fits = fits.into_iter();
    if fits.clone().next().is_none() {
        return None;
    }
    let res_min = fits
        .clone()
        .map(|f| f.residual.max(1e-6))
        .fold(f64::INFINITY, f64::min);
    let (mut sw, mut sf, mut se) = (0.0, 0.0, 0.0);
    for f in fits {
        let w = res_min / f.residual.max(1e-6);
        sw += w;
        sf += w * f.fwhm();
        se += w * f.eccentricity();
    }
    Some((sf / sw, se / sw))
}

/// The frame aggregates one plane's accepted fits give (spec 2026-10-07 §2.1 `frame`), in the module's working units.
/// `fwhm_px`/`eccentricity` are the residual-weighted means over the accepted fits flagged neither
/// [`super::StarFlags::SATURATED`] (F-3, Task 8) nor [`super::StarFlags::EXTENDED`] (F-10, Task 10b ruling P3), `None`
/// exactly where [`frame_shape`] is `None` (no such fit); the stacking adapter maps that to `0.0`, as its `unwrap_or((0.0, 0.0))` always has (which passes
/// `select_frames`'s `max_fwhm_px`; the plan gate's and the UI's reading of `stars_unsaturated = 0` is Plan C's, M-12).
#[derive(Debug, Clone, PartialEq)]
pub struct FrameAggregates {
    pub fwhm_px: Option<f64>,
    pub eccentricity: Option<f64>,
    /// Every accepted fit, saturated ones included (M-4).
    pub stars_fitted: usize,
    /// The accepted fits not flagged saturated (F-3). The frame statistic's population also leaves the extended ones
    /// out (F-10, P3); this count keeps its meaning.
    pub stars_unsaturated: usize,
    /// [`SignalTotals::tflux`].
    pub tflux: f64,
    /// [`SignalTotals::tmean_flux`].
    pub tmean_flux: f64,
    /// [`SignalTotals::rejected`].
    pub mean_flux_rejected: usize,
    /// The median aperture SNR (F-6, Task 11, ruling R11-5) over the fits the frame statistic counts
    /// ([`super::StarFlags::in_statistic`]: neither saturated nor extended) whose SNR is finite; `None` when there is
    /// none (a star without a ring reads NaN and is left out, and counted in `PlaneMeasurement::snr_missing`). Counting
    /// the flagged stars too would lift it by 4 to 24 % on the lab and real frames measured.
    pub snr_median: Option<f64>,
    /// The median half-flux radius (px) over the same population with a finite HFR.
    pub hfr_median: Option<f64>,
}

/// The median of the finite values of `it` (the mean of the two middle values for an even count); `None` when there
/// is none.
fn finite_median(it: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = it.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

impl FrameAggregates {
    /// [`signal_totals`] over every fit and [`frame_shape`] over the ones neither saturated (F-3) nor extended (F-10),
    /// the two calls the stacking Measure stage has always made.
    pub fn of(fits: &[StarFit]) -> FrameAggregates {
        let totals = signal_totals(fits);
        let unsaturated = || fits.iter().filter(|f| !f.flags.saturated());
        let shape = frame_shape(fits.iter().filter(|f| f.flags.in_statistic()));
        FrameAggregates {
            fwhm_px: shape.map(|(f, _)| f),
            eccentricity: shape.map(|(_, e)| e),
            stars_fitted: fits.len(),
            stars_unsaturated: unsaturated().count(),
            tflux: totals.tflux,
            tmean_flux: totals.tmean_flux,
            mean_flux_rejected: totals.rejected,
            snr_median: finite_median(
                fits.iter()
                    .filter(|f| f.flags.in_statistic())
                    .map(|f| f.snr),
            ),
            hfr_median: finite_median(
                fits.iter()
                    .filter(|f| f.flags.in_statistic())
                    .map(|f| f.hfr),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit_with(mean: f64) -> StarFit {
        StarFit {
            x: 0.0,
            y: 0.0,
            background: 0.0,
            amplitude: 1.0,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            fwtm_x: 6.0,
            fwtm_y: 6.0,
            theta: 0.0,
            beta: 4.0,
            residual: 0.01,
            signal: mean * 10.0,
            area: 10.0,
            peak5: f64::NAN,
            sky_ring: f64::NAN,
            sky_ring_sigma: f64::NAN,
            sky_ring_n: 0,
            snr: f64::NAN,
            hfr: f64::NAN,
            flags: crate::star_measure::StarFlags::default(),
        }
    }

    #[test]
    fn totals_winsorize_the_rcr_rejects_but_sum_every_signal() {
        let mut fits: Vec<StarFit> = (0..40)
            .map(|j| fit_with(0.9 + 0.2 * j as f64 / 39.0))
            .collect();
        fits.push(fit_with(50.0)); // a saturated fit
        fits.push(fit_with(0.01)); // a blended / failed fit
        let t = signal_totals(&fits);
        assert_eq!(t.rejected, 2);
        assert!((t.tflux - 900.1).abs() < 1e-6, "{}", t.tflux);
        assert!((t.tmean_flux - 42.0).abs() < 1e-6, "{}", t.tmean_flux);
        let empty = signal_totals(&[]);
        assert_eq!(
            (empty.tflux, empty.tmean_flux, empty.rejected),
            (0.0, 0.0, 0)
        );
    }

    #[test]
    fn non_finite_means_are_dropped_before_the_totals() {
        let a = fit_with(1.0);
        let b = fit_with(2.0);
        let mut bad = fit_with(1.0);
        bad.signal = f64::NAN;
        let t = signal_totals(&[a, b, bad]);
        assert!(t.tmean_flux.is_finite());
        assert!((t.tmean_flux - 3.0).abs() < 1e-9, "{}", t.tmean_flux);
        assert!((t.tflux - 30.0).abs() < 1e-9, "{}", t.tflux);
    }

    #[test]
    fn frame_shape_weights_by_residual() {
        let mut a = fit_with(1.0);
        a.fwhm_x = 4.0;
        a.fwhm_y = 4.0;
        a.residual = 0.01;
        let mut b = fit_with(1.0);
        b.fwhm_x = 8.0;
        b.fwhm_y = 2.0;
        b.residual = 0.03;
        // ω = 1 and 1/3: FWHM = (4 + 4/3)/(4/3) = 4; ecc = (0 + 0.96825/3)/(4/3) = 0.24206
        let (fwhm, ecc) = frame_shape(&[a, b]).unwrap();
        assert!((fwhm - 4.0).abs() < 1e-9, "{fwhm}");
        assert!((ecc - 0.24206).abs() < 1e-4, "{ecc}");
        assert!(frame_shape(&[]).is_none());
    }

    #[test]
    fn frame_medians_read_finite_unflagged_stars_only() {
        // F-6 (Task 11, ruling R11-5): the frame SNR and HFR are the medians over the fits the frame statistic counts
        // (neither saturated nor extended) whose value is finite. A NaN (no ring) is left out; flagged stars, which
        // read high, never lift them.
        let with = |snr: f64, hfr: f64, flags: crate::star_measure::StarFlags| {
            let mut f = fit_with(1.0);
            f.snr = snr;
            f.hfr = hfr;
            f.flags = flags;
            f
        };
        let none = crate::star_measure::StarFlags::default();
        let mut fits: Vec<StarFit> = [10.0, 20.0, 30.0, 40.0]
            .iter()
            .map(|&v| with(v, v / 10.0, none))
            .collect();
        fits.push(with(f64::NAN, f64::NAN, none));
        fits.push(with(900.0, 9.0, crate::star_measure::StarFlags::SATURATED));
        fits.push(with(
            1000.0,
            10.0,
            crate::star_measure::StarFlags::SATURATED,
        ));
        fits.push(with(800.0, 8.0, crate::star_measure::StarFlags::EXTENDED));
        let agg = FrameAggregates::of(&fits);
        assert_eq!(agg.snr_median, Some(25.0));
        assert_eq!(agg.hfr_median, Some(2.5));
        let flagged: Vec<StarFit> = fits[5..].to_vec();
        let agg = FrameAggregates::of(&flagged);
        assert_eq!((agg.snr_median, agg.hfr_median), (None, None));
        let agg = FrameAggregates::of(&[]);
        assert_eq!((agg.snr_median, agg.hfr_median), (None, None));
        // a lone NaN star leaves the frame without a value, not with NaN
        let agg = FrameAggregates::of(&[with(f64::NAN, f64::NAN, none)]);
        assert_eq!((agg.snr_median, agg.hfr_median), (None, None));
    }
}
