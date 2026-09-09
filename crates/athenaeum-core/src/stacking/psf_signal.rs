//! The PSF-signal family of frame-quality estimators (math reference §1;
//! spec §4.1): elliptical Moffat fits on detected stars, the hybrid
//! PSF/aperture flux inside each fit's FWTM ellipse, robust totals, the
//! large-scale background residual, MRS noise, and the PSF Signal Weight /
//! PSF SNR formulas. Coordinates are 0-based pixel centres.

use std::collections::HashMap;

use astroimage::analysis::fitting::{fit_moffat_2d_fixed_beta, Moffat2DResult, PixelSample};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::integration::stats::median_in_place;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PsfModel {
    /// Fit the brightest `AUTO_SAMPLE` seeds with every β in `AUTO_BETAS`
    /// and keep the β with the smallest median residual.
    #[default]
    Auto,
    Moffat4,
}

pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
pub const AUTO_SAMPLE: usize = 64;

/// A detection to fit: centroid, background-subtracted peak and flux.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seed {
    pub x: f64,
    pub y: f64,
    pub peak: f64,
    pub flux: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitParams {
    /// A fit is accepted only if its centre stays within this distance of the seed.
    pub centroid_tolerance_px: f64,
    /// Aperture growth `k`: the ellipse semi-axes are `k·FWTM/2`.
    pub growth: f64,
    pub max_iter: usize,
    pub conv_tol: f64,
    pub max_rejects: usize,
    /// A fit whose RMS residual reaches this fraction of its amplitude
    /// explains nothing and is dropped.
    pub max_fit_residual: f64,
}

impl Default for FitParams {
    fn default() -> Self {
        FitParams {
            centroid_tolerance_px: 1.5,
            growth: 1.0,
            max_iter: 100,
            conv_tol: 1e-5,
            max_rejects: 5,
            max_fit_residual: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarFit {
    pub x: f64,
    pub y: f64,
    pub background: f64,
    pub amplitude: f64,
    pub fwhm_x: f64,
    pub fwhm_y: f64,
    pub fwtm_x: f64,
    pub fwtm_y: f64,
    pub theta: f64,
    pub beta: f64,
    /// Normalized fit residual, `sqrt(cost/n)/amplitude`.
    pub residual: f64,
    /// Background-subtracted flux inside the FWTM ellipse.
    pub signal: f64,
    /// Analytic area of that ellipse, `π·(k/2)²·fwtm_x·fwtm_y`.
    pub area: f64,
}

impl StarFit {
    pub fn mean_flux(&self) -> f64 {
        self.signal / self.area
    }
    pub fn fwhm(&self) -> f64 {
        (self.fwhm_x * self.fwhm_y).sqrt()
    }
    pub fn eccentricity(&self) -> f64 {
        let (a, b) = if self.fwhm_x >= self.fwhm_y {
            (self.fwhm_x, self.fwhm_y)
        } else {
            (self.fwhm_y, self.fwhm_x)
        };
        if a <= 0.0 {
            0.0
        } else {
            (1.0 - (b / a).powi(2)).max(0.0).sqrt()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FitOutcome {
    pub fits: Vec<StarFit>,
    pub beta: f64,
    /// Seeds handed in.
    pub seeds: usize,
}

/// Full width at tenth maximum of a Moffat profile, `2α√(10^{1/β} − 1)`.
pub fn fwtm_from_alpha(alpha: f64, beta: f64) -> f64 {
    2.0 * alpha * (10f64.powf(1.0 / beta) - 1.0).sqrt()
}

/// Field-level initial σ from the seeds' flux/peak ratio (a Gaussian's
/// `flux/peak = 2πσ²`): median over the 100 brightest, clamped to [0.7, 10].
fn initial_sigma(seeds: &[Seed]) -> f64 {
    let mut s: Vec<f64> = seeds
        .iter()
        .take(100)
        .filter(|s| s.peak > 0.0 && s.flux > 0.0)
        .map(|s| (s.flux / (2.0 * std::f64::consts::PI * s.peak)).sqrt())
        .collect();
    if s.is_empty() {
        return 2.0;
    }
    s.sort_by(|a, b| a.total_cmp(b));
    s[s.len() / 2].clamp(0.7, 10.0)
}

fn stamp_radius(sigma: f64) -> usize {
    ((5.0 * sigma).ceil() as usize).clamp(6, 48)
}

/// Orientation and axis ratio of a stamp from its background-subtracted
/// second moments: `(sigma_major, sigma_minor, theta)` with θ the angle of
/// the major axis, `½·atan2(2μxy, μxx − μyy)`. `None` when the stamp has no
/// positive signal or degenerate moments.
fn moment_ellipse(px: &[PixelSample], b0: f64) -> Option<(f64, f64, f64)> {
    let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
    for p in px {
        let w = (p.value - b0).max(0.0);
        sw += w;
        sx += w * p.x;
        sy += w * p.y;
    }
    if sw <= 0.0 {
        return None;
    }
    let (cx, cy) = (sx / sw, sy / sw);
    let (mut mxx, mut myy, mut mxy) = (0.0f64, 0.0f64, 0.0f64);
    for p in px {
        let w = (p.value - b0).max(0.0);
        let (dx, dy) = (p.x - cx, p.y - cy);
        mxx += w * dx * dx;
        myy += w * dy * dy;
        mxy += w * dx * dy;
    }
    let (mxx, myy, mxy) = (mxx / sw, myy / sw, mxy / sw);
    let tr = mxx + myy;
    let disc = ((mxx - myy).powi(2) + 4.0 * mxy * mxy).sqrt();
    let (l1, l2) = (0.5 * (tr + disc), 0.5 * (tr - disc));
    if !(l2 > 0.0) || !l1.is_finite() {
        return None;
    }
    let theta = 0.5 * (2.0 * mxy).atan2(mxx - myy);
    Some((l1.sqrt(), l2.sqrt(), theta))
}

/// Fit one seed with a fixed β, seeded from the field σ (size) and the
/// stamp's second moments (orientation and axis ratio); `None` when the
/// stamp leaves the image, the fit fails, or the acceptance rules (math
/// reference §1.4) reject it.
fn fit_one(
    data: &[f32],
    w: usize,
    h: usize,
    seed: &Seed,
    sigma0: f64,
    beta: f64,
    p: &FitParams,
) -> Option<StarFit> {
    let r = stamp_radius(sigma0) as i64;
    let (cx, cy) = (seed.x.round() as i64, seed.y.round() as i64);
    if cx - r < 0 || cy - r < 0 || cx + r >= w as i64 || cy + r >= h as i64 {
        return None;
    }
    let cap = ((2 * r + 1) * (2 * r + 1)) as usize;
    let mut px = Vec::with_capacity(cap);
    let mut vals = Vec::with_capacity(cap);
    let mut peak = f64::NEG_INFINITY;
    for y in cy - r..=cy + r {
        for x in cx - r..=cx + r {
            let v = data[y as usize * w + x as usize];
            if !v.is_finite() {
                continue;
            }
            peak = peak.max(v as f64);
            vals.push(v);
            px.push(PixelSample {
                x: x as f64,
                y: y as f64,
                value: v as f64,
            });
        }
    }
    if px.len() < 10 {
        return None;
    }
    let b0 = median_in_place(&mut vals) as f64;
    let a0 = (peak - b0).max(1e-9);
    // Size from the field σ (flux/peak), orientation and axis ratio from the
    // stamp's moments: a circular θ = 0 seed leaves an elongated star in an
    // axis-aligned local minimum, and Moffat wings inflate the moments' size.
    let (sx0, sy0, th0) = match moment_ellipse(&px, b0) {
        Some((major, minor, theta)) => {
            let q = (major / minor).clamp(1.0, 4.0).sqrt();
            (sigma0 * q, sigma0 / q, theta)
        }
        None => (sigma0, sigma0, 0.0),
    };
    let fit = fit_moffat_2d_fixed_beta(
        &px,
        b0,
        a0,
        seed.x,
        seed.y,
        sx0,
        sy0,
        th0,
        beta,
        p.max_iter,
        p.conv_tol,
        p.max_rejects,
    )?;
    let mut f = accept(&fit, seed, cx as f64, cy as f64, r as f64, p)?;
    aperture(data, w, h, &mut f, p.growth);
    Some(f)
}

fn accept(
    m: &Moffat2DResult,
    seed: &Seed,
    cx: f64,
    cy: f64,
    r: f64,
    p: &FitParams,
) -> Option<StarFit> {
    let finite = [
        m.b,
        m.a,
        m.x0,
        m.y0,
        m.alpha_x,
        m.alpha_y,
        m.theta,
        m.fit_residual,
    ]
    .iter()
    .all(|v| v.is_finite());
    // The fitter's convergence flag is not an acceptance criterion: its LM
    // stops with the flag off after `max_rejects` unimproving steps, which is
    // where a model-mismatch minimum (a Gaussian star under any fixed-β
    // Moffat) always ends, and the submodule's own star measurement never
    // reads it. The gates below decide; the residual cap (`FitParams::
    // max_fit_residual`) only rejects a fit whose RMS residual reaches that
    // fraction of the amplitude, i.e. explains nothing.
    let settled = m.fit_residual.is_finite() && m.fit_residual < p.max_fit_residual;
    if !settled || !finite || m.a <= 0.0 || m.alpha_x <= 0.0 || m.alpha_y <= 0.0 {
        return None;
    }
    if (m.x0 - seed.x).abs() > p.centroid_tolerance_px
        || (m.y0 - seed.y).abs() > p.centroid_tolerance_px
    {
        return None;
    }
    // Unreachable in practice: the centroid gate above already bounds
    // |x0 − cx| ≤ 2 px while inner ≥ 5.1; kept for the reference's stated
    // rule (math reference §1.4).
    let inner = 0.85 * r;
    if (m.x0 - cx).abs() > inner || (m.y0 - cy).abs() > inner {
        return None;
    }
    let (fwtm_x, fwtm_y) = (
        fwtm_from_alpha(m.alpha_x, m.beta),
        fwtm_from_alpha(m.alpha_y, m.beta),
    );
    if 0.5 * p.growth * fwtm_x.max(fwtm_y) > r {
        return None;
    }
    Some(StarFit {
        x: m.x0,
        y: m.y0,
        background: m.b,
        amplitude: m.a,
        fwhm_x: m.fwhm_x(),
        fwhm_y: m.fwhm_y(),
        fwtm_x,
        fwtm_y,
        theta: m.theta,
        beta: m.beta,
        residual: m.fit_residual,
        signal: 0.0,
        area: 0.0,
    })
}

/// Sum `pixel − background` over the pixels whose centres lie inside the
/// ellipse with semi-axes `k·fwtm/2`, rotated by `theta` about the fitted
/// centre (`u = dx·cosθ + dy·sinθ`, `v = −dx·sinθ + dy·cosθ`); `area` is
/// the ellipse's analytic area.
pub(crate) fn aperture(data: &[f32], w: usize, h: usize, f: &mut StarFit, k: f64) {
    let (a, b) = (0.5 * k * f.fwtm_x, 0.5 * k * f.fwtm_y);
    let (st, ct) = f.theta.sin_cos();
    let hx = ((a * ct).powi(2) + (b * st).powi(2)).sqrt();
    let hy = ((a * st).powi(2) + (b * ct).powi(2)).sqrt();
    let x0 = (f.x - hx).floor().max(0.0) as usize;
    let x1 = ((f.x + hx).ceil().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = (f.y - hy).floor().max(0.0) as usize;
    let y1 = ((f.y + hy).ceil().max(0.0) as usize).min(h.saturating_sub(1));
    let mut sum = 0.0f64;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f64 - f.x, y as f64 - f.y);
            let u = dx * ct + dy * st;
            let v = -dx * st + dy * ct;
            if (u / a).powi(2) + (v / b).powi(2) <= 1.0 {
                let pv = data[y * w + x];
                if pv.is_finite() {
                    sum += pv as f64 - f.background;
                }
            }
        }
    }
    f.signal = sum;
    f.area = std::f64::consts::PI * a * b;
}

/// Drop every fit that has a brighter accepted fit within ±1 px.
fn dedupe(mut fits: Vec<StarFit>) -> Vec<StarFit> {
    fits.sort_by(|a, b| b.amplitude.total_cmp(&a.amplitude));
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    let mut out = Vec::with_capacity(fits.len());
    for f in fits {
        let (gx, gy) = (f.x.floor() as i64, f.y.floor() as i64);
        let mut clash = false;
        'scan: for dy in -1..=1 {
            for dx in -1..=1 {
                if let Some(list) = grid.get(&(gx + dx, gy + dy)) {
                    for &(x, y) in list {
                        if (x - f.x).abs() <= 1.0 && (y - f.y).abs() <= 1.0 {
                            clash = true;
                            break 'scan;
                        }
                    }
                }
            }
        }
        if clash {
            continue;
        }
        grid.entry((gx, gy)).or_default().push((f.x, f.y));
        out.push(f);
    }
    out
}

fn fit_all(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    sigma0: f64,
    beta: f64,
    p: &FitParams,
) -> Vec<StarFit> {
    seeds
        .par_iter()
        .filter_map(|s| fit_one(data, w, h, s, sigma0, beta, p))
        .collect()
}

/// Fit every seed with the chosen model. `Auto` fits the brightest
/// `AUTO_SAMPLE` seeds with each β in `AUTO_BETAS`, keeps the β with the
/// smallest median residual over its accepted fits (β = 4 when fewer than
/// 8 fits are accepted for every candidate), then fits all seeds with it.
/// Seeds are expected brightest-first (the detector's order).
pub fn fit_stars(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
) -> FitOutcome {
    let sigma0 = initial_sigma(seeds);
    let beta = match model {
        PsfModel::Moffat4 => 4.0,
        PsfModel::Auto => {
            let sample = &seeds[..seeds.len().min(AUTO_SAMPLE)];
            let mut best: Option<(f64, f64)> = None; // (median residual, β)
            for &b in &AUTO_BETAS {
                let mut res: Vec<f64> = fit_all(data, w, h, sample, sigma0, b, p)
                    .iter()
                    .map(|f| f.residual)
                    .collect();
                if res.len() < 8 {
                    continue;
                }
                res.sort_by(|x, y| x.total_cmp(y));
                let med = res[res.len() / 2];
                if best.map_or(true, |(m, _)| med < m) {
                    best = Some((med, b));
                }
            }
            best.map_or(4.0, |(_, b)| b)
        }
    };
    let fits = dedupe(fit_all(data, w, h, seeds, sigma0, beta, p));
    FitOutcome {
        fits,
        beta,
        seeds: seeds.len(),
    }
}

pub const PSFSW_NUM: f64 = 5.326e-6;
pub const PSFSW_DEN: f64 = 9.0e6;
pub const PSFSNR_NUM: f64 = 1.316e-7;
pub const PSFSNR_DEN: f64 = 4.987e6;
/// `N* = 2.48308·MAD(R)`: the MAD of a half-normal sample to its σ.
// The reference's constant; the analytic half-normal value is 2.5064 — the
// 1 % difference is deliberate parity, not an error.
pub const N_STAR_FROM_MAD: f64 = 2.48308;
/// Mesh cell of the large-scale background model (model scale ≈ 256 px).
pub const BACKGROUND_MODEL_CELL_PX: usize = 128;
pub const MRS_LAYERS: usize = 4;
/// Gaussian noise-propagation factor of the first B3-spline à-trous layer
/// (math reference §2.1): the estimator measures layer-1 coefficients, which
/// carry this fraction of the pixel noise σ.
pub const MRS_LAYER0_GAIN: f32 = 0.8907;
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
    let r = crate::stacking::robust::rcr(&finite_means, RCR_LIMIT);
    let w = crate::stacking::robust::winsorize(&finite_means, &r.kept);
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

/// `M*`, `N*` of the large-scale background residual (math reference
/// §1.3): the model `L` is the mesh background (cell 128 px);
/// `R = {L − v : v ≠ 0, v < L}` over the stratified sample;
/// `M* = median(R)`, `N* = 2.48308·MAD(R)`. `None` below 100 residuals.
pub fn background_residual(data: &[f32], w: usize, h: usize) -> Option<(f64, f64)> {
    let bg = astroimage::analysis::background::estimate_background_mesh(
        data,
        w,
        h,
        BACKGROUND_MODEL_CELL_PX,
    );
    let model = bg.background_map?;
    let mut r: Vec<f32> = Vec::with_capacity(data.len() / 32);
    crate::integration::stats::for_each_stratified(w, h, |i| {
        let (v, l) = (data[i], model[i]);
        if v.is_finite() && l.is_finite() && v != 0.0 && v < l {
            r.push(l - v);
        }
    });
    if r.len() < 100 {
        return None;
    }
    let m = median_in_place(&mut r);
    let mad = crate::integration::stats::mad_about(&r, m);
    Some((m as f64, N_STAR_FROM_MAD * mad as f64))
}

/// MRS noise (`estimate_noise_mrs`, 4 layers). The estimator carries an
/// absolute floor tuned for 16-bit ADU data, so callers feed it ADU-scaled
/// values (`measure::ADU_SCALE`); `None` when the result sits on that
/// floor (a constant or near-constant plane). The raw estimate is divided by
/// `MRS_LAYER0_GAIN` to recover σ from the first layer's coefficients (§2.2).
pub fn noise_mrs(data: &[f32], w: usize, h: usize) -> Option<f32> {
    let raw = astroimage::analysis::background::estimate_noise_mrs(data, w, h, MRS_LAYERS);
    if raw.is_finite() && raw > 0.002 {
        Some(raw / MRS_LAYER0_GAIN)
    } else {
        None
    }
}

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
    use crate::test_support::{gaussian_field, moffat_field, MoffatStar};

    fn round(x: f64, y: f64, amp: f64) -> MoffatStar {
        MoffatStar {
            x,
            y,
            amp,
            alpha_x: 5.0,
            alpha_y: 5.0,
            theta: 0.0,
        }
    }
    fn seeds_from(stars: &[MoffatStar], beta: f64) -> Vec<Seed> {
        stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: std::f64::consts::PI * s.amp * s.alpha_x * s.alpha_y / (beta - 1.0),
            })
            .collect()
    }
    fn nearest<'a>(fits: &'a [StarFit], x: f64, y: f64) -> &'a StarFit {
        fits.iter()
            .min_by(|a, b| {
                ((a.x - x).abs() + (a.y - y).abs()).total_cmp(&((b.x - x).abs() + (b.y - y).abs()))
            })
            .unwrap()
    }

    #[test]
    fn fixed_beta_fit_recovers_centre_width_background_and_aperture_signal() {
        let stars = [
            round(50.3, 60.7, 0.5),
            round(140.2, 130.9, 0.3),
            round(30.1, 150.4, 0.1),
        ];
        let data = moffat_field(200, 200, &stars, 4.0, 0.05);
        let out = fit_stars(
            &data,
            200,
            200,
            &seeds_from(&stars, 4.0),
            PsfModel::Moffat4,
            &FitParams::default(),
        );
        assert_eq!(out.beta, 4.0);
        assert_eq!(out.seeds, 3);
        assert_eq!(out.fits.len(), 3);
        let fwhm = 2.0 * 5.0 * (2f64.powf(0.25) - 1.0).sqrt(); // 4.3498
        let fwtm = 2.0 * 5.0 * (10f64.powf(0.25) - 1.0).sqrt(); // 8.8220
        assert!((fwtm_from_alpha(5.0, 4.0) - fwtm).abs() < 1e-12);
        for s in &stars {
            let f = nearest(&out.fits, s.x, s.y);
            assert!(
                (f.x - s.x).abs() < 0.05 && (f.y - s.y).abs() < 0.05,
                "centre {:?}",
                (f.x, f.y)
            );
            assert!((f.fwhm_x - fwhm).abs() < 0.03 * fwhm && (f.fwhm_y - fwhm).abs() < 0.03 * fwhm);
            assert!((f.fwtm_x - fwtm).abs() < 0.03 * fwtm);
            assert!(
                (f.background - 0.05).abs() < 0.002,
                "background {}",
                f.background
            );
            let total = std::f64::consts::PI * s.amp * 25.0 / 3.0;
            let expected = total * (1.0 - 10f64.powf(0.25 - 1.0)); // 0.8222·total
            assert!(
                (f.signal - expected).abs() < 0.06 * expected,
                "signal {} vs {expected}",
                f.signal
            );
            assert!((f.area - std::f64::consts::PI * 0.25 * f.fwtm_x * f.fwtm_y).abs() < 1e-9);
            assert!((f.mean_flux() - f.signal / f.area).abs() < 1e-12);
            assert!(f.residual < 0.02);
            assert!(f.eccentricity() < 0.15);
        }
    }

    #[test]
    fn auto_model_prefers_the_generating_beta() {
        let stars: Vec<MoffatStar> = (0..70)
            .map(|i| {
                round(
                    20.0 + (i % 10) as f64 * 40.3,
                    20.0 + (i / 10) as f64 * 40.7,
                    0.2 + 0.05 * (i % 5) as f64,
                )
            })
            .collect();
        let data = moffat_field(420, 320, &stars, 4.0, 0.05);
        let out = fit_stars(
            &data,
            420,
            320,
            &seeds_from(&stars, 4.0),
            PsfModel::Auto,
            &FitParams::default(),
        );
        assert_eq!(out.beta, 4.0);
        assert!(out.fits.len() >= 60, "{}", out.fits.len());

        let gstars: Vec<(f64, f64, f64)> = stars.iter().map(|s| (s.x, s.y, s.amp)).collect();
        let g = gaussian_field(420, 320, &gstars, 2.0, 0.05);
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 2.0 * std::f64::consts::PI * 4.0 * s.amp,
            })
            .collect();
        let out = fit_stars(&g, 420, 320, &seeds, PsfModel::Auto, &FitParams::default());
        assert_eq!(
            out.beta, 10.0,
            "a Gaussian field is closest to the largest β"
        );
        assert!(
            out.fits.len() >= 60,
            "Gaussian stars under a Moffat model must still be accepted: {}",
            out.fits.len()
        );
    }

    #[test]
    fn aperture_follows_the_fitted_ellipse_orientation() {
        let s = MoffatStar {
            x: 100.4,
            y: 90.6,
            amp: 0.4,
            alpha_x: 6.0,
            alpha_y: 3.0,
            theta: 30f64.to_radians(),
        };
        let data = moffat_field(200, 200, &[s], 4.0, 0.05);
        let total = std::f64::consts::PI * s.amp * 18.0 / 3.0;
        let seeds = [Seed {
            x: s.x,
            y: s.y,
            peak: s.amp,
            flux: total,
        }];
        let out = fit_stars(
            &data,
            200,
            200,
            &seeds,
            PsfModel::Moffat4,
            &FitParams::default(),
        );
        assert_eq!(out.fits.len(), 1);
        let f = &out.fits[0];
        let expected = total * (1.0 - 10f64.powf(0.25 - 1.0));
        assert!(
            (f.signal - expected).abs() < 0.06 * expected,
            "signal {} vs {expected}: the aperture rotation must follow the fitter's θ convention",
            f.signal
        );
        assert!((f.fwhm().powi(2) - f.fwhm_x * f.fwhm_y).abs() < 1e-9);
        assert!(
            f.eccentricity() > 0.8 && f.eccentricity() < 0.9,
            "{}",
            f.eccentricity()
        );
    }

    #[test]
    fn fits_are_rejected_when_they_wander_duplicate_or_touch_the_border() {
        let stars = [round(60.0, 60.0, 0.5), round(8.0, 100.0, 0.5)];
        let data = moffat_field(160, 160, &stars, 4.0, 0.05);
        let good = Seed {
            x: 60.0,
            y: 60.0,
            peak: 0.5,
            flux: 13.1,
        };
        let off = Seed { x: 63.5, ..good }; // 3.5 px off: the fit walks back > 1.5 px
        let dup = Seed {
            x: 60.4,
            y: 60.3,
            ..good
        };
        let border = Seed {
            x: 8.0,
            y: 100.0,
            ..good
        }; // stamp radius 11 leaves the image
        let out = fit_stars(
            &data,
            160,
            160,
            &[good, off, dup, border],
            PsfModel::Moffat4,
            &FitParams::default(),
        );
        assert_eq!(out.fits.len(), 1, "{:?}", out.fits);
        assert!((out.fits[0].x - 60.0).abs() < 0.05);
        assert!(
            fit_stars(&data, 160, 160, &[], PsfModel::Auto, &FitParams::default())
                .fits
                .is_empty()
        );
    }

    #[test]
    fn psf_model_serde_names() {
        assert_eq!(serde_json::to_string(&PsfModel::Auto).unwrap(), "\"auto\"");
        assert_eq!(
            serde_json::to_string(&PsfModel::Moffat4).unwrap(),
            "\"moffat4\""
        );
        assert_eq!(PsfModel::default(), PsfModel::Auto);
    }

    use crate::test_support::add_noise;

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
    fn background_residual_of_pure_noise_is_half_normal() {
        let (w, h) = (512, 512);
        let mut data = vec![0.1f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] += 0.005 * x as f32 / w as f32; // a 5 % gradient the model must follow
            }
        }
        add_noise(&mut data, 0.01, 21);
        let (m, n) = background_residual(&data, w, h).unwrap();
        assert!((m - 0.006745).abs() < 0.15 * 0.006745, "M* {m}");
        assert!((n - 0.01).abs() < 0.10 * 0.01, "N* {n}");
        assert!(background_residual(&[0.5; 64], 8, 8).is_none());
    }

    #[test]
    fn mrs_noise_tracks_the_true_sigma_and_survives_stars() {
        let (w, h) = (512, 512);
        let mut plain = vec![1000.0f32; w * h]; // ADU-scaled units, 20 ADU of noise
        add_noise(&mut plain, 20.0, 31);
        let n = noise_mrs(&plain, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.05 * 20.0, "pure noise: {n}");
        let stars: Vec<(f64, f64, f64)> = (0..200)
            .map(|i| {
                (
                    16.0 + (i % 20) as f64 * 24.0,
                    16.0 + (i / 20) as f64 * 48.0,
                    500.0 + 100.0 * (i % 7) as f64,
                )
            })
            .collect();
        let mut field = gaussian_field(w, h, &stars, 2.0, 1000.0);
        add_noise(&mut field, 20.0, 32);
        let n = noise_mrs(&field, w, h).unwrap();
        assert!((n - 20.0).abs() < 0.08 * 20.0, "star field: {n}");
        assert!(noise_mrs(&[5.0; 256], 16, 16).is_none());
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
