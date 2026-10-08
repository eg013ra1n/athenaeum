//! The per-star fit (math reference §1.4; spec 2026-10-07 §2.1 `fit`): elliptical Moffat fits on the seeds, the
//! adaptive sampling region, the acceptance rules, the hybrid PSF/aperture flux inside each fit's FWTM ellipse and the
//! ±1 px duplicate drop. Moved verbatim from `psf_signal` (Plan A Task 2); the `Auto` β search lives in
//! [`super::beta`]. Coordinates are 0-based pixel centres.
//!
//! F-0 (Plan A Task 4): every seed gets exactly one outcome — an accepted fit or a counted
//! [`RefusalReason`] — collected in seed order from the same parallel chain, so the accepted fits, their order and
//! every number are what they were before the outcomes were recorded.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use astroimage::analysis::fitting::{fit_moffat_2d_fixed_beta, Moffat2DResult, PixelSample};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::diagnostics::{FitDiagnostics, Outcome, RefusalCounts, RefusalReason};
use crate::integration::stats::median_in_place;

/// `2√(2 ln 2)`: a Gaussian's FWHM per σ (the start width [`FitDiagnostics::width_start`] reports).
const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PsfModel {
    /// Fit the brightest `AUTO_SAMPLE` seeds with every β in `AUTO_BETAS`
    /// and keep the β with the smallest median residual.
    #[default]
    Auto,
    Moffat4,
    /// One β for a whole run/group, resolved once (Tier C ruling C-1/C-1a:
    /// [`group_beta`] over the group's measured members' own `Auto` picks).
    /// INTERNAL ONLY — never on the config wire: `#[serde(skip)]` makes a
    /// stored document that somehow carried it fail to decode as this
    /// variant (an unrecognized name, exactly as if it did not exist) and
    /// makes an attempt to *serialize* it a loud `Error::custom` rather than
    /// a silent wrong config. `get_stacking_config`/every preset only ever
    /// round-trip `Auto`/`Moffat4`; `#[derive(Eq)]` is dropped from this
    /// enum because `f64` has no total order.
    #[serde(skip)]
    Fixed(f64),
}

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
    /// The fitted centre must lie inside the sampling region shrunk by this
    /// fraction per side: `|x0 − cx| ≤ (1 − 2·inner_margin)·r` (math
    /// reference §1.4). A fit that walked out towards the region's rim is
    /// measuring something the region only half covers.
    pub inner_margin: f64,
    /// Growth step of the adaptive sampling region, in pixels.
    pub region_growth_step_px: usize,
    /// The region stops growing once its median stops falling by at least
    /// this fraction per step — a region that still gets darker as it grows
    /// has not yet left the star.
    pub region_growth_min_drop: f64,
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
            inner_margin: 0.15,
            region_growth_step_px: 1,
            region_growth_min_drop: 0.01,
        }
    }
}

/// Per-accepted-fit flags (spec 2026-10-07 §3 F-0), persisted as the `.athf` record's flags byte
/// (`stacking::fits_artifact`). Set from Task 8 (`SATURATED`) and Task 9 (`BETA_AT_CAP`); nothing sets them yet, so
/// every fit reads none. A bit this build does not name is kept as it is (a newer writer's flag survives a round
/// trip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StarFlags(pub u8);

impl StarFlags {
    /// The star's core is clipped (F-3).
    pub const SATURATED: StarFlags = StarFlags(1);
    /// The fitted β sits at its bound (F-4).
    pub const BETA_AT_CAP: StarFlags = StarFlags(2);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: StarFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The named flags set, in bit order, as the probe's star lines write them (`"saturated"`, `"betaAtCap"`).
    pub fn names(self) -> Vec<&'static str> {
        [
            (StarFlags::SATURATED, "saturated"),
            (StarFlags::BETA_AT_CAP, "betaAtCap"),
        ]
        .into_iter()
        .filter(|(f, _)| self.contains(*f))
        .map(|(_, n)| n)
        .collect()
    }
}

impl BitOr for StarFlags {
    type Output = StarFlags;
    fn bitor(self, rhs: StarFlags) -> StarFlags {
        StarFlags(self.0 | rhs.0)
    }
}

impl BitOrAssign for StarFlags {
    fn bitor_assign(&mut self, rhs: StarFlags) {
        self.0 |= rhs.0;
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
    /// Mean of the five brightest finite pixels in the fit's sampling
    /// region (spec 2026-10-04 L-12) — the peak the LN star selection
    /// judges SNR on. Not persisted: NaN on a fit decoded from the `fits`
    /// artifact.
    #[serde(skip, default = "not_measured")]
    pub peak5: f64,
    /// [`StarFlags`]: written and read as the `.athf` flags byte; a fit decoded from a file written before Plan A
    /// Task 4 (whose byte is 0) reads none.
    #[serde(default)]
    pub flags: StarFlags,
}

fn not_measured() -> f64 {
    f64::NAN
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
    /// The seeds not accepted, by reason (F-0): `refused.accounted(fits.len()) == seeds` (ruling F13).
    pub refused: RefusalCounts,
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

/// Pushes the finite pixels of row `y`, columns `xs`, onto `scratch`.
/// `xs` is a plain range (inclusive or not) so both a full-width row and a
/// side-strip's narrow one share this loop.
fn push_finite_row(
    data: &[f32],
    w: usize,
    y: i64,
    xs: impl Iterator<Item = i64>,
    scratch: &mut Vec<f32>,
) {
    let row = y as usize * w;
    for x in xs {
        let v = data[row + x as usize];
        if v.is_finite() {
            scratch.push(v);
        }
    }
}

/// Median of the `(2r+1)²` square centred on `(cx, cy)`, non-finite pixels
/// skipped. `None` when the square holds no finite pixel.
///
/// `scratch` is the running multiset the caller (`sampling_radius`) grows
/// as it grows `r`: when `prev_r` is `Some`, `scratch` already holds the
/// finite pixels of the smaller `(2*prev_r+1)²` square and only the RING
/// newly exposed between `prev_r` and `r` is appended — the two squares
/// share a centre and `sampling_radius` only ever grows `r`, so that ring
/// is the exact set difference of the two squares, each pixel entering the
/// multiset exactly once. `prev_r = None` rebuilds the square from
/// scratch (the growth loop's first radius). Either way the resulting
/// multiset is identical to a full rebuild at `r` — a median does not
/// depend on insertion order.
fn region_median(
    data: &[f32],
    w: usize,
    cx: i64,
    cy: i64,
    prev_r: Option<i64>,
    r: i64,
    scratch: &mut Vec<f32>,
) -> Option<f64> {
    match prev_r {
        None => {
            scratch.clear();
            for y in cy - r..=cy + r {
                push_finite_row(data, w, y, cx - r..=cx + r, scratch);
            }
        }
        Some(pr) => {
            debug_assert!(r > pr, "sampling_radius only ever grows r");
            // Top and bottom strips: full new width, the rows the smaller
            // square did not cover at all.
            for y in cy - r..cy - pr {
                push_finite_row(data, w, y, cx - r..=cx + r, scratch);
            }
            for y in (cy + pr + 1)..=(cy + r) {
                push_finite_row(data, w, y, cx - r..=cx + r, scratch);
            }
            // Left and right strips: the smaller square's own row range,
            // the new columns only.
            for y in cy - pr..=cy + pr {
                push_finite_row(data, w, y, cx - r..cx - pr, scratch);
                push_finite_row(data, w, y, (cx + pr + 1)..=(cx + r), scratch);
            }
        }
    }
    if scratch.is_empty() {
        return None;
    }
    Some(median_in_place(scratch) as f64)
}

/// The adaptive sampling region (math reference §1.4): start at half the
/// nominal stamp and grow while the region's MEDIAN keeps falling by at
/// least `min_drop` per step. A region whose median still drops as it grows
/// is still inside the star's light; once the median flattens, the region
/// has reached the local sky and there is nothing to gain from more pixels
/// — which is exactly the sky-relative behaviour a fixed `5σ` stamp lacks:
/// on a bright sky the star's share of the median is small, so the region
/// settles sooner, and on a dark one it keeps growing.
///
/// `r_max` is the caller's cap (twice the nominal stamp, and never past the
/// image border). Returns the radius the growth stopped at — the first
/// radius whose median no longer dropped by `min_drop` (one step past the
/// last dropping one), or `r_max` when every step kept dropping.
fn sampling_radius(
    data: &[f32],
    w: usize,
    cx: i64,
    cy: i64,
    r_start: i64,
    r_max: i64,
    p: &FitParams,
    scratch: &mut Vec<f32>,
) -> i64 {
    let step = p.region_growth_step_px.max(1) as i64;
    let mut r = r_start;
    let Some(mut prev) = region_median(data, w, cx, cy, None, r, scratch) else {
        return r;
    };
    while r + step <= r_max {
        let next = r + step;
        // `scratch` currently holds exactly the square at `r` (the last
        // radius region_median built or grew it to) — that IS `prev_r`.
        let Some(m) = region_median(data, w, cx, cy, Some(r), next, scratch) else {
            return r;
        };
        r = next;
        if !(m < (1.0 - p.region_growth_min_drop) * prev) {
            break;
        }
        prev = m;
    }
    r
}

/// Per-worker scratch [`fit_one`] reuses across seeds instead of
/// allocating fresh buffers every call: the region-median growth's running
/// multiset (`region`) and the stamp's pixel/value buffers (`px`/`vals`).
/// `fit_all` hands one instance to each rayon worker via `map_init` (a
/// thread-local, one instance per worker for the whole `fit_all` call, not
/// per seed) instead of `fit_one` allocating two fresh `Vec`s — three,
/// counting `region` — on every seed. Every buffer is `clear()`ed at the
/// point it starts being rebuilt for a new seed, so a worker's retained
/// capacity is invisible to the numbers: the content is always exactly
/// what a fresh, empty `Vec` would have accumulated.
#[derive(Default)]
struct FitScratch {
    region: Vec<f32>,
    px: Vec<PixelSample>,
    vals: Vec<f32>,
}

/// What [`fit_one`] records about one seed beside its result: [`FitDiagnostics`] without the seed index and the
/// final outcome, which only the duplicate drop settles.
#[derive(Debug, Clone, Copy)]
struct FitTrace {
    region_radius: u16,
    background_start: f64,
    width_start: f64,
    growth_steps: u8,
    converged: bool,
}

impl FitTrace {
    fn record(self, seed: usize, outcome: Outcome) -> FitDiagnostics {
        FitDiagnostics {
            seed,
            outcome,
            region_radius: self.region_radius,
            background_start: self.background_start,
            width_start: self.width_start,
            growth_steps: self.growth_steps,
            iterations: None,
            converged: self.converged,
        }
    }
}

/// Fit one seed with a fixed β, seeded from the field σ (size) and the
/// stamp's second moments (orientation and axis ratio); `Err` with its
/// reason when the stamp leaves the image, the region holds too few pixels,
/// the fit fails, or the acceptance rules (math reference §1.4) reject it.
/// The [`FitTrace`] records the region and the start whatever the result.
fn fit_one(
    data: &[f32],
    w: usize,
    h: usize,
    seed: &Seed,
    sigma0: f64,
    beta: f64,
    p: &FitParams,
    scratch: &mut FitScratch,
) -> (Result<StarFit, RefusalReason>, FitTrace) {
    // Admission: the seed must have room for the NOMINAL stamp. The
    // adaptive region below starts at half of it and may grow to twice it,
    // but a seed that cannot even host the nominal one sits on the border
    // and is measuring a truncated star.
    let nominal = stamp_radius(sigma0) as i64;
    // `stamp_radius` is at most 48, so every radius below fits a u16.
    let mut trace = FitTrace {
        region_radius: nominal as u16,
        background_start: f64::NAN,
        width_start: GAUSSIAN_FWHM_PER_SIGMA * sigma0,
        growth_steps: 0,
        converged: false,
    };
    let (cx, cy) = (seed.x.round() as i64, seed.y.round() as i64);
    if cx - nominal < 0 || cy - nominal < 0 || cx + nominal >= w as i64 || cy + nominal >= h as i64
    {
        return (Err(RefusalReason::OutsideImage), trace);
    }
    let to_border = cx.min(cy).min(w as i64 - 1 - cx).min(h as i64 - 1 - cy);
    let r_max = (2 * nominal).min(48).min(to_border);
    let r_start = (nominal / 2).max(3).min(r_max);
    let r = sampling_radius(data, w, cx, cy, r_start, r_max, p, &mut scratch.region);
    let step = p.region_growth_step_px.max(1) as i64;
    trace.region_radius = r as u16;
    trace.growth_steps = ((r - r_start) / step).clamp(0, u8::MAX as i64) as u8;
    let cap = ((2 * r + 1) * (2 * r + 1)) as usize;
    scratch.px.clear();
    scratch.px.reserve(cap);
    scratch.vals.clear();
    scratch.vals.reserve(cap);
    let mut peak = f64::NEG_INFINITY;
    // The five largest values seen, ascending (L-12's `peak5`).
    let mut top = [f64::NEG_INFINITY; 5];
    for y in cy - r..=cy + r {
        for x in cx - r..=cx + r {
            let v = data[y as usize * w + x as usize];
            if !v.is_finite() {
                continue;
            }
            let v64 = v as f64;
            peak = peak.max(v64);
            if v64 > top[0] {
                top[0] = v64;
                top.sort_by(f64::total_cmp);
            }
            scratch.vals.push(v);
            scratch.px.push(PixelSample {
                x: x as f64,
                y: y as f64,
                value: v as f64,
            });
        }
    }
    if scratch.px.len() < 10 {
        return (Err(RefusalReason::TooFewPixels), trace);
    }
    let b0 = median_in_place(&mut scratch.vals) as f64;
    trace.background_start = b0;
    let a0 = (peak - b0).max(1e-9);
    // Size from the field σ (flux/peak), orientation and axis ratio from the
    // stamp's moments: a circular θ = 0 seed leaves an elongated star in an
    // axis-aligned local minimum, and Moffat wings inflate the moments' size.
    let (sx0, sy0, th0) = match moment_ellipse(&scratch.px, b0) {
        Some((major, minor, theta)) => {
            let q = (major / minor).clamp(1.0, 4.0).sqrt();
            (sigma0 * q, sigma0 / q, theta)
        }
        None => (sigma0, sigma0, 0.0),
    };
    let Some(fit) = fit_moffat_2d_fixed_beta(
        &scratch.px,
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
    ) else {
        return (Err(RefusalReason::FitFailed), trace);
    };
    trace.converged = fit.converged;
    let mut f = match accept(&fit, seed, cx as f64, cy as f64, r as f64, p) {
        Ok(f) => f,
        Err(reason) => return (Err(reason), trace),
    };
    aperture(data, w, h, &mut f, p.growth);
    // At least 10 finite pixels were gathered, so all five slots are filled.
    f.peak5 = top.iter().sum::<f64>() / top.len() as f64;
    (Ok(f), trace)
}

/// The acceptance rules (math reference §1.4), each refusal with its reason. The first gate is one condition, as it
/// always was; its reason is `NonFinite` when a parameter or the residual is not finite, else `ResidualTooLarge`
/// when the residual reaches the cap, else `FitFailed` (`A ≤ 0` or an `α ≤ 0`).
fn accept(
    m: &Moffat2DResult,
    seed: &Seed,
    cx: f64,
    cy: f64,
    r: f64,
    p: &FitParams,
) -> Result<StarFit, RefusalReason> {
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
        return Err(if !finite {
            RefusalReason::NonFinite
        } else if !settled {
            RefusalReason::ResidualTooLarge
        } else {
            RefusalReason::FitFailed
        });
    }
    if (m.x0 - seed.x).abs() > p.centroid_tolerance_px
        || (m.y0 - seed.y).abs() > p.centroid_tolerance_px
    {
        return Err(RefusalReason::CentreMoved);
    }
    // The fitted centre must sit inside the sampling region shrunk by
    // `inner_margin` per side (math reference §1.4). With the default
    // centroid tolerance this only bites on a small region, which is
    // precisely where it should: a star whose region stayed narrow is one
    // whose light the region barely contains.
    let inner = (1.0 - 2.0 * p.inner_margin) * r;
    if (m.x0 - cx).abs() > inner || (m.y0 - cy).abs() > inner {
        return Err(RefusalReason::CentreAtRim);
    }
    let (fwtm_x, fwtm_y) = (
        fwtm_from_alpha(m.alpha_x, m.beta),
        fwtm_from_alpha(m.alpha_y, m.beta),
    );
    if 0.5 * p.growth * fwtm_x.max(fwtm_y) > r {
        return Err(RefusalReason::RegionTooSmall);
    }
    Ok(StarFit {
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
        peak5: f64::NAN,
        flags: StarFlags::default(),
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

/// Drop every fit that has a brighter accepted fit within ±1 px. Each fit
/// travels with its seed index, which the comparison never reads: the stable
/// sort orders the pairs exactly as it orders the bare fits, so the kept
/// fits and their order are the duplicate drop's as always.
fn dedupe(mut fits: Vec<(usize, StarFit)>) -> Vec<(usize, StarFit)> {
    fits.sort_by(|a, b| b.1.amplitude.total_cmp(&a.1.amplitude));
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    let mut out = Vec::with_capacity(fits.len());
    for (seed, f) in fits {
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
        out.push((seed, f));
    }
    out
}

/// `seeds.par_iter()`'s work runs on `pool` when given one (perf tier 1
/// Task 2) — without it, the fit lands on rayon's GLOBAL pool, sized by
/// `available_parallelism` and invisible to `image_pool`/the admission
/// budget. A caller that has already entered a `pool.install` (the module's
/// `measure_plane` wraps the whole `fit_stars` call) passes `None` here — nesting an
/// `install` inside another is legal but pointless, since the outer one
/// already runs every parallel iterator inside it on that same pool.
///
/// The accepted fits, in seed order (the `Auto` β search's sample reads
/// their residuals); [`fit_all_outcomes`] keeps every seed's outcome.
pub(super) fn fit_all(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    sigma0: f64,
    beta: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<StarFit> {
    fit_all_outcomes(data, w, h, seeds, sigma0, beta, p, pool)
        .into_iter()
        .filter_map(|(result, _)| result.ok())
        .collect()
}

/// Every seed's result and trace, in seed order (M-4): the parallel chain is
/// the one `fit_all` always ran, collected per seed instead of filtered in
/// flight — rayon's indexed `collect` keeps the order, so the accepted fits
/// read off it sequentially are exactly the list the filtered chain gave.
fn fit_all_outcomes(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    sigma0: f64,
    beta: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<(Result<StarFit, RefusalReason>, FitTrace)> {
    let body = || {
        seeds
            .par_iter()
            // `map_init` calls `FitScratch::default` once per worker (not
            // per seed) and hands that same instance to every seed the
            // worker processes — `fit_one` clears each buffer itself, so
            // this is only the allocation that disappears, never the data.
            .map_init(FitScratch::default, |scratch, s| {
                fit_one(data, w, h, s, sigma0, beta, p, scratch)
            })
            .collect()
    };
    match pool {
        Some(pl) => pl.install(body),
        None => body(),
    }
}

/// Fit every seed with the chosen model. `Auto` fits the brightest
/// `AUTO_SAMPLE` seeds with each β in `AUTO_BETAS`, keeps the β with the
/// smallest median residual over its accepted fits (β = 4 when fewer than
/// 8 fits are accepted for every candidate; [`super::beta::auto_beta`]),
/// then fits all seeds with it — via [`fit_stars_with_beta`], once β is
/// resolved to a number.
/// Seeds are expected brightest-first (the detector's order).
pub fn fit_stars(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    fit_stars_diagnosed(data, w, h, seeds, model, p, pool, false).0
}

/// [`fit_stars`], also returning one [`FitDiagnostics`] per seed, in seed
/// order, when `diagnostics` is set (empty otherwise: the run never asks).
pub(super) fn fit_stars_diagnosed(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> (FitOutcome, Vec<FitDiagnostics>) {
    let sigma0 = initial_sigma(seeds);
    let beta = match model {
        PsfModel::Moffat4 => 4.0,
        // Tier C Task 1: a caller-resolved β (the group β, or any other
        // fixed value) is fit exactly like `Moffat4`'s fixed 4.0 — the
        // `Auto` search below never runs for it.
        PsfModel::Fixed(b) => b,
        PsfModel::Auto => super::beta::auto_beta(data, w, h, seeds, sigma0, p, pool),
    };
    fit_stars_with_beta_diagnosed(data, w, h, seeds, beta, p, pool, diagnostics)
}

/// Fit every seed at a caller-chosen, already-concrete β — the tail
/// [`fit_stars`] itself runs once `Auto`'s search (or `Moffat4`'s fixed
/// 4.0) has resolved a model to a number: `σ0` from the seeds (the same
/// `initial_sigma` estimate `fit_stars` computes), then
/// `dedupe(fit_all(..))`. `beta` is echoed back unchanged on the returned
/// `FitOutcome`.
///
/// Exposed so a caller that must fit two independent planes at the SAME β
/// — e.g. local normalization's `relative_scale`, which resolves β from a
/// reference frame via `fit_stars` and then has to fit its target at
/// exactly that β rather than let `Auto` pick independently per plane
/// (the FWTM-enclosed flux fraction depends on β, so two different
/// per-plane β choices bias a flux ratio between them) — can reuse a
/// resolved β without re-running the `Auto` search a second time.
pub fn fit_stars_with_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> FitOutcome {
    fit_stars_with_beta_diagnosed(data, w, h, seeds, beta, p, pool, false).0
}

/// [`fit_stars_with_beta`], also returning one [`FitDiagnostics`] per seed,
/// in seed order, when `diagnostics` is set (empty otherwise).
///
/// The outcomes (M-4): every seed's result comes off [`fit_all_outcomes`]
/// in seed order; the accepted fits are read off it sequentially — the very
/// list the filtered chain collected before — and handed to [`dedupe`], so
/// the kept fits, their order and every sum over them are unchanged. A seed
/// whose fit the duplicate drop removed is refused `Duplicate`.
pub(super) fn fit_stars_with_beta_diagnosed(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> (FitOutcome, Vec<FitDiagnostics>) {
    let sigma0 = initial_sigma(seeds);
    let per_seed = fit_all_outcomes(data, w, h, seeds, sigma0, beta, p, pool);
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(per_seed.len());
    let mut traces: Vec<FitTrace> =
        Vec::with_capacity(if diagnostics { per_seed.len() } else { 0 });
    let mut accepted: Vec<(usize, StarFit)> = Vec::with_capacity(per_seed.len());
    for (i, (result, trace)) in per_seed.into_iter().enumerate() {
        match result {
            Ok(f) => {
                // Settled by the duplicate drop: a kept fit is overwritten
                // below with its star index, a dropped one stays Duplicate.
                outcomes.push(Outcome::Refused(RefusalReason::Duplicate));
                accepted.push((i, f));
            }
            Err(reason) => outcomes.push(Outcome::Refused(reason)),
        }
        if diagnostics {
            traces.push(trace);
        }
    }
    let kept = dedupe(accepted);
    for (star, &(seed, _)) in kept.iter().enumerate() {
        outcomes[seed] = Outcome::Accepted { star };
    }
    let mut refused = RefusalCounts::default();
    for o in &outcomes {
        if let Outcome::Refused(reason) = *o {
            refused.add(reason);
        }
    }
    let fits: Vec<StarFit> = kept.into_iter().map(|(_, f)| f).collect();
    debug_assert_eq!(
        refused.accounted(fits.len()),
        seeds.len(),
        "every seed has exactly one outcome (ruling F13)"
    );
    let records = traces
        .into_iter()
        .zip(outcomes)
        .enumerate()
        .map(|(seed, (trace, outcome))| trace.record(seed, outcome))
        .collect();
    (
        FitOutcome {
            fits,
            beta,
            seeds: seeds.len(),
            refused,
        },
        records,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

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
            None,
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
            None,
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
        let out = fit_stars(
            &g,
            420,
            320,
            &seeds,
            PsfModel::Auto,
            &FitParams::default(),
            None,
        );
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
    fn fit_stars_with_beta_matches_fit_stars_for_the_resolved_beta() {
        // The same Gaussian field `auto_model_prefers_the_generating_beta`
        // uses, which resolves to a NON-default β (10.0) under `Auto` — so
        // this pins `fit_stars_with_beta` reproducing an arbitrary resolved
        // β exactly, not just the β = 4.0 default.
        let stars: Vec<MoffatStar> = (0..70)
            .map(|i| {
                round(
                    20.0 + (i % 10) as f64 * 40.3,
                    20.0 + (i / 10) as f64 * 40.7,
                    0.2 + 0.05 * (i % 5) as f64,
                )
            })
            .collect();
        let data = gaussian_field(
            420,
            320,
            &stars.iter().map(|s| (s.x, s.y, s.amp)).collect::<Vec<_>>(),
            2.0,
            0.05,
        );
        let seeds: Vec<Seed> = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 2.0 * std::f64::consts::PI * 4.0 * s.amp,
            })
            .collect();

        let auto_out = fit_stars(
            &data,
            420,
            320,
            &seeds,
            PsfModel::Auto,
            &FitParams::default(),
            None,
        );
        let explicit_out = fit_stars_with_beta(
            &data,
            420,
            320,
            &seeds,
            auto_out.beta,
            &FitParams::default(),
            None,
        );

        assert_eq!(auto_out.beta, explicit_out.beta);
        assert_eq!(auto_out.fits.len(), explicit_out.fits.len());
        assert!(!auto_out.fits.is_empty());
        for (a, b) in auto_out.fits.iter().zip(&explicit_out.fits) {
            assert!((a.x - b.x).abs() < 1e-9, "x: {} vs {}", a.x, b.x);
            assert!((a.y - b.y).abs() < 1e-9, "y: {} vs {}", a.y, b.y);
            assert!(
                (a.signal - b.signal).abs() < 1e-9,
                "signal: {} vs {}",
                a.signal,
                b.signal
            );
        }
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
            None,
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
            None,
        );
        assert_eq!(out.fits.len(), 1, "{:?}", out.fits);
        assert!((out.fits[0].x - 60.0).abs() < 0.05);
        assert!(fit_stars(
            &data,
            160,
            160,
            &[],
            PsfModel::Auto,
            &FitParams::default(),
            None
        )
        .fits
        .is_empty());
    }

    /// The inner-region rule (math reference §1.4): a fit that settled
    /// `0.8·r` from the sampling region's centre is refused, because the
    /// region only half covers what it settled on. `inner_margin = 0.0` is
    /// the SAME fit with the rule switched off — it is the rule that
    /// rejects, not the fit failing.
    ///
    /// The fit here is a real one, run through the same fitter `fit_one`
    /// uses, and then handed to `accept` directly. Driving the whole of
    /// `fit_stars` with an off-centre seed cannot express this case: the LM
    /// never walks more than about a pixel from its start on a real Moffat
    /// field — past ~3 px off it simply fails instead of finding the star
    /// — so a seed placed at `0.8·r` produces no fit to judge at all.
    #[test]
    fn a_fit_settled_at_the_region_rim_is_rejected() {
        let stars = [round(100.0, 100.0, 0.5)];
        let data = moffat_field(200, 200, &stars, 4.0, 0.05);
        // Region: radius 10 centred 8 px right of the star, i.e. the fit
        // will settle at 0.8·r from the centre.
        let (cx, cy, r) = (108.0f64, 100.0f64, 10.0f64);
        let mut px = Vec::new();
        let mut vals = Vec::new();
        let mut peak = f64::NEG_INFINITY;
        for y in (cy as i64 - r as i64)..=(cy as i64 + r as i64) {
            for x in (cx as i64 - r as i64)..=(cx as i64 + r as i64) {
                let v = data[y as usize * 200 + x as usize];
                peak = peak.max(v as f64);
                vals.push(v);
                px.push(PixelSample {
                    x: x as f64,
                    y: y as f64,
                    value: v as f64,
                });
            }
        }
        let b0 = median_in_place(&mut vals) as f64;
        let p = FitParams {
            // The centroid gate must not be what rejects: this test is
            // about the region rule alone.
            centroid_tolerance_px: 40.0,
            ..FitParams::default()
        };
        // Start the fit AT the star so it converges there — the point is
        // where it settles relative to the region, not how it got there.
        let fit = fit_moffat_2d_fixed_beta(
            &px,
            b0,
            (peak - b0).max(1e-9),
            100.0,
            100.0,
            2.0,
            2.0,
            0.0,
            4.0,
            p.max_iter,
            p.conv_tol,
            p.max_rejects,
        )
        .expect("the fit converges on a clean Moffat star");
        assert!(
            (fit.x0 - 100.0).abs() < 0.2,
            "the fit settled on the star: {}",
            fit.x0
        );
        let seed = Seed {
            x: cx,
            y: cy,
            peak: 0.5,
            flux: std::f64::consts::PI * 0.5 * 25.0 / 3.0,
        };
        // Plan A Task 4 (F-0): the refusal carries its reason.
        assert_eq!(
            accept(&fit, &seed, cx, cy, r, &p).err(),
            Some(RefusalReason::CentreAtRim),
            "|x0 − cx| = 8 exceeds (1 − 2·0.15)·10 = 7"
        );
        let off = FitParams {
            inner_margin: 0.0,
            ..p
        };
        assert!(
            accept(&fit, &seed, cx, cy, r, &off).is_ok(),
            "with the rule off the same fit is accepted"
        );
    }

    // --- Task 5 pin: sampling_radius's incremental ring-append multiset ---

    /// An INDEPENDENT re-implementation of the pre-Task-5 rescan-every-step
    /// algorithm — full `(2r+1)²` square rebuild at every radius, never
    /// calling `region_median`/`push_finite_row` — so the pin tests below
    /// check the incremental code against a real oracle, not against a copy
    /// of itself.
    fn pin_rescan_region_median(data: &[f32], w: usize, cx: i64, cy: i64, r: i64) -> Option<f64> {
        let mut v: Vec<f32> = Vec::new();
        for y in cy - r..=cy + r {
            let row = y as usize * w;
            for x in cx - r..=cx + r {
                let p = data[row + x as usize];
                if p.is_finite() {
                    v.push(p);
                }
            }
        }
        if v.is_empty() {
            return None;
        }
        Some(median_in_place(&mut v) as f64)
    }

    /// The pre-Task-5 growth loop over [`pin_rescan_region_median`], byte-
    /// for-byte the same control flow `sampling_radius` has (same stop
    /// rule), returning the stopping radius AND every median it visited.
    fn pin_rescan_sampling_radius(
        data: &[f32],
        w: usize,
        cx: i64,
        cy: i64,
        r_start: i64,
        r_max: i64,
        p: &FitParams,
    ) -> (i64, Vec<f64>) {
        let step = p.region_growth_step_px.max(1) as i64;
        let mut seq = Vec::new();
        let mut r = r_start;
        let Some(mut prev) = pin_rescan_region_median(data, w, cx, cy, r) else {
            return (r, seq);
        };
        seq.push(prev);
        while r + step <= r_max {
            let next = r + step;
            let Some(m) = pin_rescan_region_median(data, w, cx, cy, next) else {
                return (r, seq);
            };
            r = next;
            seq.push(m);
            if !(m < (1.0 - p.region_growth_min_drop) * prev) {
                break;
            }
            prev = m;
        }
        (r, seq)
    }

    /// Mirrors `sampling_radius`'s own loop exactly, but through the real
    /// (incremental) `region_median`, collecting every intermediate median
    /// — `sampling_radius` itself only returns the final radius.
    fn incremental_sampling_radius_with_seq(
        data: &[f32],
        w: usize,
        cx: i64,
        cy: i64,
        r_start: i64,
        r_max: i64,
        p: &FitParams,
    ) -> (i64, Vec<f64>) {
        let step = p.region_growth_step_px.max(1) as i64;
        let mut scratch: Vec<f32> = Vec::new();
        let mut seq = Vec::new();
        let mut r = r_start;
        let Some(mut prev) = region_median(data, w, cx, cy, None, r, &mut scratch) else {
            return (r, seq);
        };
        seq.push(prev);
        while r + step <= r_max {
            let next = r + step;
            let Some(m) = region_median(data, w, cx, cy, Some(r), next, &mut scratch) else {
                return (r, seq);
            };
            r = next;
            seq.push(m);
            if !(m < (1.0 - p.region_growth_min_drop) * prev) {
                break;
            }
            prev = m;
        }
        (r, seq)
    }

    /// A fixture star well inside the field, growth walking several steps.
    #[test]
    fn sampling_radius_incremental_matches_rescan_on_a_normal_star() {
        let stars = [round(100.4, 100.6, 0.6)];
        let mut data = moffat_field(200, 200, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.01, 41);
        let p = FitParams::default();
        let (cx, cy) = (100i64, 100i64);
        let (r_start, r_max) = (5i64, 25i64);

        let (r_ref, seq_ref) = pin_rescan_sampling_radius(&data, 200, cx, cy, r_start, r_max, &p);
        let (r_seq, seq_new) =
            incremental_sampling_radius_with_seq(&data, 200, cx, cy, r_start, r_max, &p);
        let mut scratch: Vec<f32> = Vec::new();
        let r_prod = sampling_radius(&data, 200, cx, cy, r_start, r_max, &p, &mut scratch);

        assert_eq!(
            r_prod, r_seq,
            "production sampling_radius vs the sequence wrapper"
        );
        assert_eq!(r_ref, r_seq, "growth stopped at a different radius");
        assert_eq!(seq_ref.len(), seq_new.len(), "different number of steps");
        for (a, b) in seq_ref.iter().zip(&seq_new) {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "median sequence: {seq_ref:?} vs {seq_new:?}"
            );
        }

        // Recorded literals (Task 5, pinned before the incremental change
        // shipped, cross-checked against `pin_rescan_sampling_radius` above
        // on every run — this assertion additionally survives a future
        // change that broke both the oracle and the incremental code the
        // same way).
        assert_eq!(r_prod, 15);
        let expected: [f64; 11] = [
            0.11737800389528275,
            0.07933449000120163,
            0.06783409416675568,
            0.06370921432971954,
            0.05825279280543327,
            0.05651697516441345,
            0.05566517263650894,
            0.055028729140758514,
            0.054084811359643936,
            0.05354057624936104,
            0.053332407027482986,
        ];
        assert_eq!(seq_new.len(), expected.len());
        for (got, want) in seq_new.iter().zip(&expected) {
            assert_eq!(got.to_bits(), want.to_bits(), "{seq_new:?} vs {expected:?}");
        }
    }

    /// A fixture star near the image border, so `r_max` is tight and the
    /// growth loop exercises a shorter, boundary-constrained walk.
    #[test]
    fn sampling_radius_incremental_matches_rescan_near_the_border() {
        let stars = [round(15.3, 12.7, 0.5)];
        let mut data = moffat_field(120, 120, &stars, 3.0, 0.04);
        add_noise(&mut data, 0.008, 43);
        let p = FitParams::default();
        let (cx, cy) = (15i64, 13i64);
        let to_border = cx.min(cy).min(120 - 1 - cx).min(120 - 1 - cy);
        let (r_start, r_max) = (3i64, to_border);
        assert!(r_max >= r_start, "fixture must leave room to grow");

        let (r_ref, seq_ref) = pin_rescan_sampling_radius(&data, 120, cx, cy, r_start, r_max, &p);
        let (r_seq, seq_new) =
            incremental_sampling_radius_with_seq(&data, 120, cx, cy, r_start, r_max, &p);
        let mut scratch: Vec<f32> = Vec::new();
        let r_prod = sampling_radius(&data, 120, cx, cy, r_start, r_max, &p, &mut scratch);

        assert_eq!(
            r_prod, r_seq,
            "production sampling_radius vs the sequence wrapper"
        );
        assert_eq!(r_ref, r_seq, "growth stopped at a different radius");
        assert_eq!(seq_ref.len(), seq_new.len(), "different number of steps");
        for (a, b) in seq_ref.iter().zip(&seq_new) {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "median sequence: {seq_ref:?} vs {seq_new:?}"
            );
        }

        assert_eq!(r_prod, 13);
        let expected: [f64; 11] = [
            0.255771666765213,
            0.1849888265132904,
            0.12626828253269196,
            0.09833625704050064,
            0.07689442485570908,
            0.06416510790586472,
            0.05707979202270508,
            0.051649294793605804,
            0.048868581652641296,
            0.04728369787335396,
            0.04579973965883255,
        ];
        assert_eq!(seq_new.len(), expected.len());
        for (got, want) in seq_new.iter().zip(&expected) {
            assert_eq!(got.to_bits(), want.to_bits(), "{seq_new:?} vs {expected:?}");
        }
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

    /// Tier C Task 1: `Fixed` is INTERNAL — it must error rather than
    /// silently write something onto the config wire, and a config that
    /// somehow carried its spelling must not decode as it either.
    #[test]
    fn psf_model_fixed_never_reaches_the_wire() {
        assert!(
            serde_json::to_string(&PsfModel::Fixed(6.0)).is_err(),
            "a skipped variant must refuse to serialize, not silently drop the payload"
        );
        // Its own would-be spelling reads as an unknown variant, exactly as
        // if `Fixed` were not declared at all.
        assert!(serde_json::from_str::<PsfModel>("{\"fixed\":6.0}").is_err());
    }

    #[test]
    fn fit_stars_with_fixed_model_uses_the_given_beta_verbatim() {
        let seeds = vec![Seed {
            x: 10.0,
            y: 10.0,
            peak: 1.0,
            flux: 6.0,
        }];
        let data = vec![0.0f32; 20 * 20];
        let a = fit_stars(
            &data,
            20,
            20,
            &seeds,
            PsfModel::Fixed(7.5),
            &FitParams::default(),
            None,
        );
        let b = fit_stars_with_beta(&data, 20, 20, &seeds, 7.5, &FitParams::default(), None);
        assert_eq!(a.beta, 7.5);
        assert_eq!(a.beta, b.beta);
    }

    /// L-12: `peak5` is the mean of the five brightest finite pixels of the
    /// square `fit_one` sampled — independent of the fitted model.
    #[test]
    fn peak5_is_the_mean_of_the_five_brightest_pixels_in_the_fit_region() {
        // Nothing is planted: a rendered noise-free Gaussian star (FWHM 3 px)
        // on a flat 0.1 plane. Every square `fit_one` can sample has r >= 3
        // and the star's five brightest pixels lie within 1.5 px of its
        // centre, so the expected set is the same whatever r the region grew to.
        let (w, h) = (64usize, 64usize);
        let sigma = 3.0 / 2.354_820_045;
        let (sx, sy, amp) = (32.3, 31.7, 0.4);
        let data = gaussian_field(w, h, &[(sx, sy, amp)], sigma, 0.1);
        let seeds = vec![Seed {
            x: sx,
            y: sy,
            peak: amp,
            flux: 2.0 * std::f64::consts::PI * sigma * sigma * amp,
        }];
        let out = fit_stars(
            &data,
            w,
            h,
            &seeds,
            PsfModel::Moffat4,
            &FitParams::default(),
            None,
        );
        assert_eq!(out.fits.len(), 1, "the star must fit");
        let mut near: Vec<f64> = Vec::new();
        for y in 29..=35usize {
            for x in 29..=35usize {
                near.push(data[y * w + x] as f64);
            }
        }
        near.sort_by(|a, b| b.total_cmp(a));
        let expected = near[..5].iter().sum::<f64>() / 5.0;
        assert!(
            (out.fits[0].peak5 - expected).abs() < 1e-9,
            "peak5 {} vs {}",
            out.fits[0].peak5,
            expected
        );
    }
}
