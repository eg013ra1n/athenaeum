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
}

impl Default for FitParams {
    fn default() -> Self {
        FitParams {
            centroid_tolerance_px: 1.5,
            growth: 1.0,
            max_iter: 50,
            conv_tol: 1e-5,
            max_rejects: 5,
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

/// Fit one seed with a fixed β; `None` when the stamp leaves the image,
/// the fit fails, or the acceptance rules (math reference §1.4) reject it.
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
    let fit = fit_moffat_2d_fixed_beta(
        &px,
        b0,
        a0,
        seed.x,
        seed.y,
        sigma0,
        sigma0,
        0.0,
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
    if !m.converged || !finite || m.a <= 0.0 || m.alpha_x <= 0.0 || m.alpha_y <= 0.0 {
        return None;
    }
    if (m.x0 - seed.x).abs() > p.centroid_tolerance_px
        || (m.y0 - seed.y).abs() > p.centroid_tolerance_px
    {
        return None;
    }
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
}
