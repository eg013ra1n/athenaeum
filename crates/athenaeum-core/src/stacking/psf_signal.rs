//! The PSF-signal family of frame-quality estimators (math reference §1;
//! spec §4.1): the PSF Signal Weight / PSF SNR formulas, the group β (the
//! cache-key constant is `star_measure::MEASURE_VERSION`). The elliptical
//! Moffat fits, the star flux (from Plan A Task 10 the model's FWTM flux times the plane's aperture correction), the
//! robust totals, the large-scale background residual and MRS
//! noise moved to [`crate::star_measure`] (one star-measurement path, Plan A
//! Task 2) and are re-exported here, so every `psf_signal::…` caller is
//! unchanged; `fit_stars`/`fit_stars_with_beta` are thin wrappers over the
//! module's own. Coordinates are 0-based pixel centres.

use std::sync::Arc;

pub use crate::star_measure::domain::{
    BACKGROUND_MODEL_CELL_PX, MRS_LAYER1_GAIN, MRS_LAYERS, N_STAR_FROM_MAD,
};
pub use crate::star_measure::{
    background_residual, frame_shape, fwtm_from_alpha, model_flux_in_fwtm, noise_mrs, signal_totals,
    FitOutcome, FitParams, FluxCorrection, PsfModel, Seed, SignalTotals, StarFit, AUTO_BETAS,
    AUTO_SAMPLE, RCR_LIMIT,
};

/// [`crate::star_measure::fit::fit_stars`] (Plan A Task 2: the fit moved;
/// this wrapper keeps every LN and probe call site unchanged).
pub fn fit_stars(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    crate::star_measure::fit::fit_stars(data, w, h, seeds, model, p, pool)
}

/// [`crate::star_measure::fit::fit_stars_with_beta`] (Plan A Task 2: the fit
/// moved; this wrapper keeps every LN and probe call site unchanged).
pub fn fit_stars_with_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    crate::star_measure::fit::fit_stars_with_beta(data, w, h, seeds, beta, p, pool)
}

/// One β for a whole group (Tier C ruling C-1/C-1a): the LOWER median of the
/// group's measured members' own `Auto`-resolved β, so the result is always
/// one of the members' own βs — since Plan A Task 9 (F-4) each a continuous
/// per-frame β in [`crate::star_measure::BETA_BOUNDS`], so the group β is the
/// β some member was actually measured at, never an average no frame read.
/// The code is unchanged from the discrete ladder (`AUTO_BETAS`), where the
/// same rule kept it a candidate. `betas` is expected non-empty
/// (a group that reached this call measured at least one frame); an empty
/// slice returns `4.0` (the same fallback `Auto` uses when fewer than 8 of
/// its sample's fits are accepted) rather than panicking — this is a reporting
/// path, not a precondition the pipeline enforces upstream.
///
/// Recorded only — nothing consumes the resolved β yet (Tier C Task 2 will
/// hand it to the LN reference build and to every mapped fit's flux
/// correction as `PsfModel::Fixed`).
pub fn group_beta(betas: &[f64]) -> f64 {
    if betas.is_empty() {
        return 4.0;
    }
    let mut sorted = betas.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[(sorted.len() - 1) / 2]
}

pub const PSFSW_NUM: f64 = 5.326e-6;
pub const PSFSW_DEN: f64 = 9.0e6;
pub const PSFSNR_NUM: f64 = 1.316e-7;
pub const PSFSNR_DEN: f64 = 4.987e6;

/// `PSFSW = (5.326e-6 · TFlux · TMeanFlux) / (9.0e6 · σ_N · M*)`; 0 when
/// the denominator is not positive.
pub fn psf_signal_weight(tflux: f64, tmean_flux: f64, sigma_n: f64, m_star: f64) -> f64 {
    if !(sigma_n > 0.0) || !(m_star > 0.0) {
        return 0.0;
    }
    let w = PSFSW_NUM * tflux * tmean_flux / (PSFSW_DEN * sigma_n * m_star);
    if w.is_finite() && w > 0.0 {
        w
    } else {
        0.0
    }
}

/// `PSFSNR = (1.316e-7 · TFlux²) / (4.987e6 · σ_N²)`; 0 when σ_N ≤ 0.
pub fn psf_snr(tflux: f64, sigma_n: f64) -> f64 {
    if !(sigma_n > 0.0) {
        return 0.0;
    }
    let s = PSFSNR_NUM * tflux * tflux / (PSFSNR_DEN * sigma_n * sigma_n);
    if s.is_finite() {
        s
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renamed in Plan A Task 9 from `…_and_always_an_auto_candidate`: β is continuous from F-4, so the clause is
    /// "always one of the members' own βs" (ruling F14, M-6).
    #[test]
    fn group_beta_is_the_lower_median_and_always_one_of_the_members_betas() {
        // Odd count: the exact middle.
        assert_eq!(group_beta(&[2.5, 4.0, 6.0]), 4.0);
        // Even count: the LOWER of the two middle values, not their mean —
        // a plain average (5.0) is a β no member was measured at.
        assert_eq!(group_beta(&[4.0, 6.0]), 4.0);
        // One element: itself.
        assert_eq!(group_beta(&[10.0]), 10.0);
        // All equal.
        assert_eq!(group_beta(&[6.0, 6.0, 6.0, 6.0]), 6.0);
        // Order independence.
        assert_eq!(group_beta(&[10.0, 2.5, 6.0, 4.0]), 4.0);
        // The result is always literally one of the members' own βs,
        // whatever the input mix (the whole point of "lower median" over
        // "mean") — the discrete ladder's and continuous ones alike (F-4).
        assert!(AUTO_BETAS.contains(&group_beta(&AUTO_BETAS)));
        let continuous = [5.5, 3.17, 100.0, 2.9, 4.82, 1.5];
        assert_eq!(group_beta(&continuous), 3.17);
        assert!(continuous.contains(&group_beta(&continuous)));
        // Empty: the documented fallback, not a panic.
        assert_eq!(group_beta(&[]), 4.0);
    }

    #[test]
    fn estimator_formulas_by_hand() {
        let w = psf_signal_weight(1000.0, 50.0, 0.01, 0.007);
        assert!((w - 5.326e-6 * 1000.0 * 50.0 / (9.0e6 * 0.01 * 0.007)).abs() < 1e-12);
        assert_eq!(psf_signal_weight(1000.0, 50.0, 0.0, 0.007), 0.0);
        assert_eq!(psf_signal_weight(1000.0, 50.0, 0.01, 0.0), 0.0);
        let s = psf_snr(1000.0, 0.01);
        assert!((s - 1.316e-7 * 1e6 / (4.987e6 * 1e-4)).abs() < 1e-9);
        assert_eq!(psf_snr(1000.0, 0.0), 0.0);
        // scale invariance: everything ×65535 gives the same weights
        let k = 65535.0;
        assert!(
            (psf_signal_weight(1000.0 * k, 50.0 * k, 0.01 * k, 0.007 * k) - w).abs() < 1e-9 * w
        );
        assert!((psf_snr(1000.0 * k, 0.01 * k) - s).abs() < 1e-9 * s);
    }
}
