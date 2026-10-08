//! Frame aggregates over the accepted fits (math reference §1.1; spec 2026-10-07 §2.1 `frame`): the PSF-signal totals
//! `TFlux` / `TMeanFlux` and the residual-weighted frame FWHM and eccentricity. Moved verbatim from `psf_signal` (Plan
//! A Task 2).

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
pub fn frame_shape(fits: &[StarFit]) -> Option<(f64, f64)> {
    if fits.is_empty() {
        return None;
    }
    let res_min = fits
        .iter()
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
/// `fwhm_px`/`eccentricity` are `None` exactly where [`frame_shape`] is `None` (no accepted fit); the stacking adapter
/// maps that to `0.0`, as its `unwrap_or((0.0, 0.0))` always has (ruling F14 fixes the type now so Task 8's
/// unsaturated statistic changes no test).
#[derive(Debug, Clone, PartialEq)]
pub struct FrameAggregates {
    pub fwhm_px: Option<f64>,
    pub eccentricity: Option<f64>,
    /// Every accepted fit.
    pub stars_fitted: usize,
    /// [`SignalTotals::tflux`].
    pub tflux: f64,
    /// [`SignalTotals::tmean_flux`].
    pub tmean_flux: f64,
    /// [`SignalTotals::rejected`].
    pub mean_flux_rejected: usize,
}

impl FrameAggregates {
    /// [`signal_totals`] and [`frame_shape`] over `fits`, the two calls the stacking Measure stage has always made.
    pub fn of(fits: &[StarFit]) -> FrameAggregates {
        let totals = signal_totals(fits);
        let shape = frame_shape(fits);
        FrameAggregates {
            fwhm_px: shape.map(|(f, _)| f),
            eccentricity: shape.map(|(_, e)| e),
            stars_fitted: fits.len(),
            tflux: totals.tflux,
            tmean_flux: totals.tmean_flux,
            mean_flux_rejected: totals.rejected,
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
}
