//! The per-frame β (spec 2026-10-07 §2.1 `beta`): today's discrete `Auto` search over [`AUTO_BETAS`], moved verbatim
//! from `psf_signal::fit_stars` (Plan A Task 2). Task 9 replaces it with the continuous free-β median. F-3 (Task 8):
//! on the measure path the sample is the brightest seeds whose saturation verdict fit succeeded unflagged.

use std::sync::Arc;

use super::fit::{fit_all, FitParams, Seed, SeedPass};

pub const AUTO_BETAS: [f64; 4] = [2.5, 4.0, 6.0, 10.0];
pub const AUTO_SAMPLE: usize = 64;

/// `PsfModel::Auto`'s β: fits the brightest [`AUTO_SAMPLE`] seeds with every β in [`AUTO_BETAS`] and keeps the β
/// with the smallest median residual over its accepted fits; 4.0 when no candidate accepts at least 8 fits. Seeds are
/// expected brightest-first (the detector's order). `mask` and `field_width` reach every sample fit as they reach the
/// frame's (Task 6a); each sample fit starts from its star's own width start, as the frame's do (Task 6b).
///
/// `passes` (F-3, Task 8; one verdict-pass record per seed, in seed order — the measure path's): the sample is the
/// [`AUTO_SAMPLE`] brightest seeds whose verdict fit succeeded unflagged (a seed with no verdict, a `MostlySaturated`
/// one, a failed verdict fit and a flagged star are out), each fitted from its record as the frame's fits are. `None`
/// (every other caller): the brightest [`AUTO_SAMPLE`] seeds, as always.
#[allow(clippy::too_many_arguments)]
pub(crate) fn auto_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    passes: Option<&[SeedPass]>,
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: f64,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> f64 {
    let (sample, sample_passes): (Vec<Seed>, Option<Vec<SeedPass>>) = match passes {
        Some(v) => {
            debug_assert_eq!(v.len(), seeds.len());
            let (s, sv): (Vec<Seed>, Vec<SeedPass>) = seeds
                .iter()
                .zip(v)
                .filter(|(_, v)| matches!(v.verdict, Some(Ok(sv)) if sv.fit && !sv.saturated))
                .take(AUTO_SAMPLE)
                .map(|(s, v)| (*s, *v))
                .unzip();
            (s, Some(sv))
        }
        None => (seeds[..seeds.len().min(AUTO_SAMPLE)].to_vec(), None),
    };
    let mut best: Option<(f64, f64)> = None; // (median residual, β)
    for &b in &AUTO_BETAS {
        let mut res: Vec<f64> = fit_all(
            data,
            w,
            h,
            &sample,
            b,
            p,
            mask,
            field_width,
            sample_passes.as_deref(),
            pool,
        )
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
