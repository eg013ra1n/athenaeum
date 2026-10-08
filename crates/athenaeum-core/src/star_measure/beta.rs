//! The per-frame β (spec 2026-10-07 §2.1 `beta`): today's discrete `Auto` search over [`AUTO_BETAS`], moved verbatim
//! from `psf_signal::fit_stars` (Plan A Task 2). Task 9 replaces it with the continuous free-β median.

use std::sync::Arc;

use super::fit::{fit_all, FitParams, Seed};

pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
pub const AUTO_SAMPLE: usize = 64;

/// `PsfModel::Auto`'s β: fits the brightest [`AUTO_SAMPLE`] seeds with every β in [`AUTO_BETAS`] and keeps the β
/// with the smallest median residual over its accepted fits; 4.0 when no candidate accepts at least 8 fits. `sigma0`
/// is the field σ `fit_stars` computed from the same seeds. Seeds are expected brightest-first (the detector's order).
pub(crate) fn auto_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    sigma0: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> f64 {
    let sample = &seeds[..seeds.len().min(AUTO_SAMPLE)];
    let mut best: Option<(f64, f64)> = None; // (median residual, β)
    for &b in &AUTO_BETAS {
        let mut res: Vec<f64> = fit_all(data, w, h, sample, sigma0, b, p, pool)
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
