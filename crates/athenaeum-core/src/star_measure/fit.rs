//! The per-star fit (math reference §1.4; spec 2026-10-07 §2.1 `fit`): elliptical Moffat fits on the seeds, the
//! per-star sampling region, the acceptance rules, the hybrid PSF/aperture flux inside each fit's FWTM ellipse and the
//! ±1 px duplicate drop. Moved verbatim from `psf_signal` (Plan A Task 2); the `Auto` β search lives in
//! [`super::beta`]. Coordinates are 0-based pixel centres.
//!
//! F-0 (Plan A Task 4): every seed gets exactly one outcome — an accepted fit or a counted
//! [`RefusalReason`] — collected in seed order from the same parallel chain, so the accepted fits, their order and
//! every number are what they were before the outcomes were recorded.
//!
//! F-2 (Plan A Task 5): the fitter sees a stamp-centred, amplitude-normalised problem (every parameter O(1), so its
//! step-size convergence test means the same on every star), through rustafits' conditioned entry point, whose
//! Jacobian carries the θ column with its correct sign (ruling R-OSP-3); and every accepted fit reports canonical
//! axes — the major axis on `fwhm_x`, `theta` on it, in `[-π/2, π/2)`.
//!
//! F-1 seeds (Plan A Task 6a, no fit change — ruling F16): a pixel mask makes a pixel absent, exactly like a
//! non-finite one, in the region's growth, its pixels and the aperture; the field width is reported as
//! [`FitOutcome::field_width`].
//!
//! F-1 with F-7, the fit (Plan A Task 6b, ruling R-OSP-4): every star starts from its own width, `F_start`, read
//! from its second moments in a box of radius `⌈2·F_field⌉` (the box is also the admission test, PA-18); its region
//! grows from `max(3, ⌈1.5·F_start⌉)` up to `clamp(⌈6·F_start⌉, 6, 96)` px while the region's median stands above its
//! rim's by more than 1 % of the peak (PA-12); the fit's background starts from that rim's median. An annular star is
//! refused `Donut` before any fit, and a fit narrower than 0.8 px `NarrowerThanPixel`. The field σ0 from the seeds'
//! flux/peak and the field-level nominal stamp are retired.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use astroimage::analysis::fitting::{
    fit_moffat_2d_fixed_beta_conditioned, Moffat2DResult, PixelSample,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::diagnostics::{FitDiagnostics, Outcome, RefusalCounts, RefusalReason};
use super::seeds::{field_width_of, FieldWidth};
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
    /// Growth step of the sampling region, in pixels.
    pub region_growth_step_px: usize,
    /// The region grows while its median stands above the median of its own
    /// outer rim by more than this fraction of its peak above that rim —
    /// `(m_r − s_r) > 0.01·(max − s_r)` (PA-12, Plan A Task 6b): a region whose
    /// median still holds the star's light has not yet left the star.
    pub region_growth_contrast: f64,
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
            region_growth_contrast: 0.01,
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
    /// The MAJOR axis's FWHM: `fwhm_x >= fwhm_y` on every accepted fit (F-2, canonical axes).
    pub fwhm_x: f64,
    pub fwhm_y: f64,
    /// The FWTMs follow the FWHMs: `fwtm_x` is the major axis's.
    pub fwtm_x: f64,
    pub fwtm_y: f64,
    /// Angle of the major (`fwhm_x`) axis from the image x axis, radians, in `[-π/2, π/2)` (F-2). A fit decoded from
    /// a `fits` artifact written before Plan A keeps the fitter's own axis order; such a file is stale by
    /// `MEASURE_VERSION`.
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
    /// The field width the call used (Task 6a, ruling F14): `FieldWidth { fwhm_px: f, from_stars: 0 }` for a given
    /// `f`, else [`field_width_of`] the seeds. Every star's width-start box has radius `⌈2·fwhm_px⌉` (Task 6b, PA-12).
    pub field_width: FieldWidth,
    /// One record per seed, in seed order, when the caller asked for them (`FitStarsOptions::diagnostics`,
    /// `MeasureOptions::diagnostics`); empty otherwise (Task 6b, ruling F14).
    pub diagnostics: Vec<FitDiagnostics>,
}

/// Full width at tenth maximum of a Moffat profile, `2α√(10^{1/β} − 1)`.
pub fn fwtm_from_alpha(alpha: f64, beta: f64) -> f64 {
    2.0 * alpha * (10f64.powf(1.0 / beta) - 1.0).sqrt()
}

/// The width-start box's radius per field FWHM (PA-12): `F_start` is read in a box of radius `⌈2·F_field⌉` px, and
/// a seed whose box leaves the image is refused `OutsideImage` (PA-18).
const WIDTH_START_BOX_PER_FIELD: f64 = 2.0;
/// `F_start`'s clamp, px (PA-12).
const WIDTH_START_MIN_PX: f64 = 0.6;
const WIDTH_START_MAX_PX: f64 = 40.0;
/// The region's start radius, `max(3, ⌈1.5·F_start⌉)` px (PA-12).
const REGION_START_PER_WIDTH: f64 = 1.5;
const REGION_START_MIN_PX: i64 = 3;
/// The region's cap, `clamp(⌈6·F_start⌉, 6, 96)` px, and never past the image border (PA-12; spec §3 F-1: ≥ 6 FWHM,
/// cap 96 px).
const REGION_MAX_PER_WIDTH: f64 = 6.0;
const REGION_MAX_MIN_PX: i64 = 6;
const REGION_MAX_MAX_PX: i64 = 96;
/// The donut test (Task 6b): a star whose 3×3 core mean is below this fraction of its 1-px ring's mean is refused
/// `Donut`.
const DONUT_CORE_TO_RING: f64 = 0.8;
/// The donut ring's radius floor, px: the ring sits at `max(1.5, F_start/2.3548)` (the fold check's ruling — at the
/// unfloored radius a real star of FWHM ≤ 2.0 read 0.34–0.61 on the lab's renderer and would be refused).
const DONUT_RING_MIN_RADIUS_PX: f64 = 1.5;
/// An accepted fit whose narrower FWHM is below this many px is a hot pixel, not a star (Task 6b,
/// `NarrowerThanPixel`).
const NARROWEST_FWHM_PX: f64 = 0.8;

/// A stamp's background-subtracted second moments (weights `max(v − b0, 0)`): the axes' σ, the major axis's angle
/// `½·atan2(2μxy, μxx − μyy)`, and the centroid, in the stamp's own coordinates.
#[derive(Debug, Clone, Copy)]
struct Moments {
    sigma_major: f64,
    sigma_minor: f64,
    theta: f64,
    cx: f64,
    cy: f64,
}

/// Orientation, axis ratio and centroid of a stamp from its background-subtracted
/// second moments ([`Moments`]). `None` when the stamp has no positive signal or
/// degenerate moments.
fn moment_ellipse(px: &[PixelSample], b0: f64) -> Option<Moments> {
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
    Some(Moments {
        sigma_major: l1.sqrt(),
        sigma_minor: l2.sqrt(),
        theta,
        cx,
        cy,
    })
}

/// Whether pixel `i` is present: finite, and not `false` in the mask (Task 6a).
#[inline]
fn present(data: &[f32], mask: Option<&[bool]>, i: usize) -> bool {
    data[i].is_finite() && mask.map_or(true, |m| m[i])
}

/// Pushes the present (finite, unmasked) pixels of row `y`, columns `xs`, onto `scratch`.
/// `xs` is a plain range (inclusive or not) so both a full-width row and a
/// side-strip's narrow one share this loop.
fn push_finite_row(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    y: i64,
    xs: impl Iterator<Item = i64>,
    scratch: &mut Vec<f32>,
) {
    let row = y as usize * w;
    for x in xs {
        let i = row + x as usize;
        if present(data, mask, i) {
            scratch.push(data[i]);
        }
    }
}

/// Median of the `(2r+1)²` square centred on `(cx, cy)`, non-finite and masked
/// pixels skipped, and the largest pixel this call appended (`−∞` when it
/// appended none). `None` when the square holds no present pixel.
///
/// `scratch` is the running multiset the caller ([`grow_region`]) grows
/// as it grows `r`: when `prev_r` is `Some`, `scratch` already holds the
/// finite pixels of the smaller `(2*prev_r+1)²` square and only the RING
/// newly exposed between `prev_r` and `r` is appended — the two squares
/// share a centre and the region only ever grows `r`, so that ring
/// is the exact set difference of the two squares, each pixel entering the
/// multiset exactly once. `prev_r = None` rebuilds the square from
/// scratch (the growth loop's first radius). Either way the resulting
/// multiset is identical to a full rebuild at `r` — a median does not
/// depend on insertion order. The appended pixels' maximum is read before
/// the median reorders the multiset.
#[allow(clippy::too_many_arguments)]
fn region_median(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    cx: i64,
    cy: i64,
    prev_r: Option<i64>,
    r: i64,
    scratch: &mut Vec<f32>,
) -> Option<(f64, f64)> {
    let appended_from = match prev_r {
        None => {
            scratch.clear();
            for y in cy - r..=cy + r {
                push_finite_row(data, mask, w, y, cx - r..=cx + r, scratch);
            }
            0
        }
        Some(pr) => {
            debug_assert!(r > pr, "the region only ever grows r");
            let from = scratch.len();
            // Top and bottom strips: full new width, the rows the smaller
            // square did not cover at all.
            for y in cy - r..cy - pr {
                push_finite_row(data, mask, w, y, cx - r..=cx + r, scratch);
            }
            for y in (cy + pr + 1)..=(cy + r) {
                push_finite_row(data, mask, w, y, cx - r..=cx + r, scratch);
            }
            // Left and right strips: the smaller square's own row range,
            // the new columns only.
            for y in cy - pr..=cy + pr {
                push_finite_row(data, mask, w, y, cx - r..cx - pr, scratch);
                push_finite_row(data, mask, w, y, (cx + pr + 1)..=(cx + r), scratch);
            }
            from
        }
    };
    if scratch.is_empty() {
        return None;
    }
    let max = scratch[appended_from..]
        .iter()
        .fold(f64::NEG_INFINITY, |m, &v| m.max(v as f64));
    Some((median_in_place(scratch) as f64, max))
}

/// Median of the present pixels on the outer 1-px rim of the `(2r+1)²` square
/// centred on `(cx, cy)` — the pixels at Chebyshev distance exactly `r`
/// (`r = 0`: the centre pixel). `None` when the rim holds no present pixel.
/// `buf` is scratch, cleared first.
fn rim_median(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    cx: i64,
    cy: i64,
    r: i64,
    buf: &mut Vec<f32>,
) -> Option<f64> {
    buf.clear();
    if r == 0 {
        push_finite_row(data, mask, w, cy, cx..=cx, buf);
    } else {
        push_finite_row(data, mask, w, cy - r, cx - r..=cx + r, buf);
        push_finite_row(data, mask, w, cy + r, cx - r..=cx + r, buf);
        for y in cy - r + 1..cy + r {
            push_finite_row(data, mask, w, y, cx - r..=cx - r, buf);
            push_finite_row(data, mask, w, y, cx + r..=cx + r, buf);
        }
    }
    if buf.is_empty() {
        return None;
    }
    Some(median_in_place(buf) as f64)
}

/// The sampling region (PA-12, spec §3 F-1): start at `r_start` and grow, one
/// step at a time up to `r_max`, while the square's median `m_r` stands above
/// the median of its own outer 1-px rim `s_r` by more than
/// `FitParams::region_growth_contrast` of the square's peak above that rim —
/// `(m_r − s_r) > 0.01·(max − s_r)`. The growth is judged on (median − local
/// sky), not on the absolute median, so a bright or sloping sky does not stop it
/// early, and a region that still holds the star's light in its median keeps
/// growing.
///
/// Returns the final radius and its rim median `s_r` — the fit's background
/// start `b0` (F-7: the sky under the star read where the star is not). A rim
/// with no present pixel (every one non-finite or masked) leaves the square's own
/// median as the level, so the growth stops there. `(r_start, NaN)` when the
/// starting square holds no present pixel.
#[allow(clippy::too_many_arguments)]
fn grow_region(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    cx: i64,
    cy: i64,
    r_start: i64,
    r_max: i64,
    p: &FitParams,
    scratch: &mut FitScratch,
) -> (i64, f64) {
    let step = p.region_growth_step_px.max(1) as i64;
    let mut r = r_start;
    let Some((mut m, mut max)) = region_median(data, mask, w, cx, cy, None, r, &mut scratch.region)
    else {
        return (r, f64::NAN);
    };
    loop {
        let s = rim_median(data, mask, w, cx, cy, r, &mut scratch.rim).unwrap_or(m);
        if r + step > r_max || !(m - s > p.region_growth_contrast * (max - s)) {
            return (r, s);
        }
        let next = r + step;
        // `scratch.region` holds exactly the square at `r` — that IS `prev_r`.
        let Some((m_next, max_next)) =
            region_median(data, mask, w, cx, cy, Some(r), next, &mut scratch.region)
        else {
            return (r, s);
        };
        r = next;
        m = m_next;
        max = max.max(max_next);
    }
}

/// Per-worker scratch [`fit_one`] reuses across seeds instead of
/// allocating fresh buffers every call: the region's running multiset
/// (`region`), the rim medians' buffer (`rim`) and the stamp's pixels (`px`,
/// the width-start box's first, then the region's). `fit_all` hands one
/// instance to each rayon worker via `map_init` (a thread-local, one instance
/// per worker for the whole `fit_all` call, not per seed). Every buffer is
/// `clear()`ed at the point it starts being rebuilt for a new seed, so a
/// worker's retained capacity is invisible to the numbers: the content is
/// always exactly what a fresh, empty `Vec` would have accumulated.
#[derive(Default)]
struct FitScratch {
    region: Vec<f32>,
    rim: Vec<f32>,
    px: Vec<PixelSample>,
}

/// What [`fit_one`] records about one seed beside its result: [`FitDiagnostics`] without the seed index and the
/// final outcome, which only the duplicate drop settles.
#[derive(Debug, Clone, Copy)]
struct FitTrace {
    region_radius: u16,
    background_start: f64,
    width_start: f64,
    growth_steps: u8,
    pixels_used: u32,
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
            pixels_used: self.pixels_used,
            iterations: None,
            converged: self.converged,
        }
    }
}

/// The star's own width start (PA-12): its FWHM `F_start`, the axis ratio and
/// orientation of its moments, and their centroid in image coordinates.
#[derive(Debug, Clone, Copy)]
struct WidthStart {
    fwhm: f64,
    /// `σ_major/σ_minor ≥ 1`.
    axis_ratio: f64,
    theta: f64,
    centroid: (f64, f64),
}

/// The radius of a field width's width-start box, `⌈2·F_field⌉` px (PA-12),
/// at least 1.
fn width_start_box_radius(field_width: f64) -> i64 {
    ((WIDTH_START_BOX_PER_FIELD * field_width).ceil() as i64).max(1)
}

/// `F_start` (PA-12): the background-subtracted second moments of the present
/// pixels in the box of radius `rb` about `(cx, cy)` — the subtraction level is
/// the median of the box's own outer 1-px rim, the weights `max(v − level, 0)`
/// ([`moment_ellipse`]) — as the FWHM of the Gaussian of the same geometric-mean
/// σ, `2.3548·√(σ_major·σ_minor)`, clamped to [0.6, 40] px. The caller has
/// checked that the box lies inside the image. A box with no present rim pixel or
/// no positive moments starts from the field width itself (clamped the same
/// way), round, at θ 0, centred on the seed.
#[allow(clippy::too_many_arguments)]
fn width_start(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    seed: &Seed,
    cx: i64,
    cy: i64,
    rb: i64,
    field_width: f64,
    scratch: &mut FitScratch,
) -> WidthStart {
    let fallback = WidthStart {
        fwhm: field_width.clamp(WIDTH_START_MIN_PX, WIDTH_START_MAX_PX),
        axis_ratio: 1.0,
        theta: 0.0,
        centroid: (seed.x, seed.y),
    };
    let Some(level) = rim_median(data, mask, w, cx, cy, rb, &mut scratch.rim) else {
        return fallback;
    };
    scratch.px.clear();
    for y in cy - rb..=cy + rb {
        for x in cx - rb..=cx + rb {
            let i = y as usize * w + x as usize;
            if present(data, mask, i) {
                scratch.px.push(PixelSample {
                    x: (x - cx) as f64,
                    y: (y - cy) as f64,
                    value: data[i] as f64,
                });
            }
        }
    }
    match moment_ellipse(&scratch.px, level) {
        Some(m) => WidthStart {
            fwhm: (GAUSSIAN_FWHM_PER_SIGMA * (m.sigma_major * m.sigma_minor).sqrt())
                .clamp(WIDTH_START_MIN_PX, WIDTH_START_MAX_PX),
            axis_ratio: m.sigma_major / m.sigma_minor,
            theta: m.theta,
            centroid: (cx as f64 + m.cx, cy as f64 + m.cy),
        },
        None => fallback,
    }
}

/// The donut test (Task 6b): the mean of the 3×3 core about the pixel nearest
/// the moment centroid against the mean of the 1-px ring `R − ½ ≤ d < R + ½`
/// about the centroid itself, `R = max(1.5, F_start/2.3548)` px, both minus the
/// background start `b0`; a donut when `core < 0.8·ring`. Only present pixels
/// inside the image count. No verdict (`false`) when the core or the ring holds
/// none.
fn is_donut(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    centroid: (f64, f64),
    f_start: f64,
    b0: f64,
) -> bool {
    let (mx, my) = centroid;
    let (px, py) = (mx.round() as i64, my.round() as i64);
    let inside = |x: i64, y: i64| x >= 0 && y >= 0 && x < w as i64 && y < h as i64;
    let mean_minus_b0 = |pixels: &mut dyn Iterator<Item = (i64, i64)>| -> Option<f64> {
        let (mut sum, mut n) = (0.0f64, 0usize);
        for (x, y) in pixels {
            if !inside(x, y) {
                continue;
            }
            let i = y as usize * w + x as usize;
            if present(data, mask, i) {
                sum += data[i] as f64;
                n += 1;
            }
        }
        (n > 0).then(|| sum / n as f64 - b0)
    };
    let Some(core) = mean_minus_b0(
        &mut (-1i64..=1).flat_map(|dy| (-1i64..=1).map(move |dx| (px + dx, py + dy))),
    ) else {
        return false;
    };
    let radius = DONUT_RING_MIN_RADIUS_PX.max(f_start / GAUSSIAN_FWHM_PER_SIGMA);
    let reach = (radius + 0.5).ceil() as i64;
    let on_ring = |&(x, y): &(i64, i64)| {
        let d = (x as f64 - mx).hypot(y as f64 - my);
        d >= radius - 0.5 && d < radius + 0.5
    };
    let Some(ring) = mean_minus_b0(
        &mut (-reach..=reach)
            .flat_map(|dy| (-reach..=reach).map(move |dx| (px + dx, py + dy)))
            .filter(on_ring),
    ) else {
        return false;
    };
    core < DONUT_CORE_TO_RING * ring
}

/// Fit one seed with a fixed β (PA-12, spec §3 F-1 and F-7; Plan A Task 6b).
///
/// 1. **Admission:** the width-start box of radius `⌈2·F_field⌉` about the seed's pixel must lie inside the image,
///    else `OutsideImage` (PA-18: a star broader than the field width can still be admitted and meet the border as
///    `RegionTooSmall`).
/// 2. **The width start** `F_start` from the star's own second moments in that box ([`width_start`]).
/// 3. **The region** grows from `max(3, ⌈1.5·F_start⌉)` to at most `clamp(⌈6·F_start⌉, 6, 96)` px, never past the
///    border, while its median stands above its rim's ([`grow_region`]); the background start `b0` is the final
///    rim's median, and fewer than 10 present pixels are `TooFewPixels`.
/// 4. **The donut test** ([`is_donut`]) refuses an annular star `Donut` before any fit.
/// 5. **The fit** starts at `b0`, `a0 = max − b0`, σ `F_start/2.3548` split by the moments' axis ratio (`√ratio`
///    clamped to [1, 2]) at their angle; then the acceptance rules ([`accept`], `NarrowerThanPixel` among them).
///
/// The [`FitTrace`] records the region and the start whatever the result. A pixel `mask` leaves its `false` pixels
/// out exactly like non-finite ones (Task 6a).
#[allow(clippy::too_many_arguments)]
fn fit_one(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    field_width: f64,
    beta: f64,
    p: &FitParams,
    scratch: &mut FitScratch,
) -> (Result<StarFit, RefusalReason>, FitTrace) {
    let rb = width_start_box_radius(field_width);
    // Until the region grows, its radius reads the width-start box's; the start width and background read NaN.
    let mut trace = FitTrace {
        region_radius: rb.min(u16::MAX as i64) as u16,
        background_start: f64::NAN,
        width_start: f64::NAN,
        growth_steps: 0,
        pixels_used: 0,
        converged: false,
    };
    let (cx, cy) = (seed.x.round() as i64, seed.y.round() as i64);
    if cx - rb < 0 || cy - rb < 0 || cx + rb >= w as i64 || cy + rb >= h as i64 {
        return (Err(RefusalReason::OutsideImage), trace);
    }
    let start = width_start(data, mask, w, seed, cx, cy, rb, field_width, scratch);
    trace.width_start = start.fwhm;
    let to_border = cx.min(cy).min(w as i64 - 1 - cx).min(h as i64 - 1 - cy);
    let r_max = ((REGION_MAX_PER_WIDTH * start.fwhm).ceil() as i64)
        .clamp(REGION_MAX_MIN_PX, REGION_MAX_MAX_PX)
        .min(to_border);
    let r_start = ((REGION_START_PER_WIDTH * start.fwhm).ceil() as i64)
        .max(REGION_START_MIN_PX)
        .min(r_max);
    let (r, b0) = grow_region(data, mask, w, cx, cy, r_start, r_max, p, scratch);
    let step = p.region_growth_step_px.max(1) as i64;
    // `r ≤ 96`, so the radius fits a u16.
    trace.region_radius = r as u16;
    trace.growth_steps = ((r - r_start) / step).clamp(0, u8::MAX as i64) as u8;
    trace.background_start = b0;
    let cap = ((2 * r + 1) * (2 * r + 1)) as usize;
    scratch.px.clear();
    scratch.px.reserve(cap);
    let mut peak = f64::NEG_INFINITY;
    // The five largest values seen, ascending (L-12's `peak5`).
    let mut top = [f64::NEG_INFINITY; 5];
    for y in cy - r..=cy + r {
        for x in cx - r..=cx + r {
            let i = y as usize * w + x as usize;
            if !present(data, mask, i) {
                continue;
            }
            let v64 = data[i] as f64;
            peak = peak.max(v64);
            if v64 > top[0] {
                top[0] = v64;
                top.sort_by(f64::total_cmp);
            }
            // F-2: stamp-centred coordinates (exact integers), the value
            // normalised below once the start background and amplitude are
            // known.
            scratch.px.push(PixelSample {
                x: (x - cx) as f64,
                y: (y - cy) as f64,
                value: v64,
            });
        }
    }
    // The region's present pixels: at most (2r+1)² ≤ 193², so a u32 holds it.
    trace.pixels_used = scratch.px.len() as u32;
    if scratch.px.len() < 10 {
        return (Err(RefusalReason::TooFewPixels), trace);
    }
    if is_donut(data, mask, w, h, start.centroid, start.fwhm, b0) {
        return (Err(RefusalReason::Donut), trace);
    }
    let a0 = (peak - b0).max(1e-9);
    // F-2 (Plan A Task 5): the fitter sees a conditioned problem — the stamp
    // centred on (cx, cy) and the values as `(v − b0)/a0`, so it starts at
    // B = 0, A = 1 with every parameter O(1). Its step-size convergence test
    // (`‖Δp‖/‖p‖ < conv_tol`) then means the same thing on every star: with
    // absolute coordinates and native amplitudes `‖p‖` was dominated by the
    // position (thousands of pixels) or the sky level. The result is mapped
    // back below; `fit_residual` is already amplitude-normalised.
    for s in scratch.px.iter_mut() {
        s.value = (s.value - b0) / a0;
    }
    // Size from the star's own width start, orientation and axis ratio from its
    // moments (PA-12): a circular θ = 0 start leaves an elongated star in an
    // axis-aligned local minimum.
    let sigma = start.fwhm / GAUSSIAN_FWHM_PER_SIGMA;
    let q = start.axis_ratio.clamp(1.0, 4.0).sqrt();
    let (sx0, sy0, th0) = (sigma * q, sigma / q, start.theta);
    let (ox, oy) = (cx as f64, cy as f64);
    // The conditioned entry point carries the Jacobian's θ column with its
    // correct sign (ruling R-OSP-3): `fit_moffat_2d_fixed_beta`'s column is
    // the negative of ∂Q/∂θ, so its θ steps went uphill and a noiseless
    // elliptical star stalled 3.8° off. That entry point keeps its column for
    // the legacy analysis path (ruling F7).
    let Some(mut fit) = fit_moffat_2d_fixed_beta_conditioned(
        &scratch.px,
        0.0,
        1.0,
        seed.x - ox,
        seed.y - oy,
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
    // Back to image coordinates and the plane's units.
    fit.x0 += ox;
    fit.y0 += oy;
    fit.b = fit.b * a0 + b0;
    fit.a *= a0;
    trace.converged = fit.converged;
    let mut f = match accept(&fit, seed, cx as f64, cy as f64, r as f64, p) {
        Ok(f) => f,
        Err(reason) => return (Err(reason), trace),
    };
    aperture(data, mask, w, h, &mut f, p.growth);
    // At least 10 present pixels were gathered, so all five slots are filled.
    f.peak5 = top.iter().sum::<f64>() / top.len() as f64;
    (Ok(f), trace)
}

/// The acceptance rules (math reference §1.4), each refusal with its reason. The first gate is one condition, as it
/// always was; its reason is `NonFinite` when a parameter or the residual is not finite, else `ResidualTooLarge`
/// when the residual reaches the cap, else `FitFailed` (`A ≤ 0` or an `α ≤ 0`). The last (Task 6b) refuses a fit
/// whose narrower FWHM is below 0.8 px `NarrowerThanPixel`.
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
    // Task 6b: a fit narrower than a pixel on either axis is a hot pixel (or a
    // cosmic) that got a seed, not a star.
    if m.fwhm_x().min(m.fwhm_y()) < NARROWEST_FWHM_PX {
        return Err(RefusalReason::NarrowerThanPixel);
    }
    // F-2 (Plan A Task 5): canonical axes. The fitter's first axis is
    // whichever its start made it, so the same star could read (major, minor,
    // θ) or (minor, major, θ ± 90°). Every accepted fit reports the major axis
    // on `fwhm_x`/`fwtm_x` and `theta` on that axis, in [-π/2, π/2). Both
    // forms are the same ellipse, so the gates above (symmetric in the pair)
    // and the aperture are unaffected.
    let (fwhm_a, fwhm_b) = (m.fwhm_x(), m.fwhm_y());
    let (fwhm_x, fwhm_y, fwtm_x, fwtm_y, theta) = if fwhm_a >= fwhm_b {
        (fwhm_a, fwhm_b, fwtm_x, fwtm_y, m.theta)
    } else {
        // The fitter's θ is in [-π/2, π/2]; the perpendicular axis lies a
        // quarter turn away, on the side that stays in range.
        let perpendicular = if m.theta < 0.0 {
            m.theta + std::f64::consts::FRAC_PI_2
        } else {
            m.theta - std::f64::consts::FRAC_PI_2
        };
        (fwhm_b, fwhm_a, fwtm_y, fwtm_x, perpendicular)
    };
    Ok(StarFit {
        x: m.x0,
        y: m.y0,
        background: m.b,
        amplitude: m.a,
        fwhm_x,
        fwhm_y,
        fwtm_x,
        fwtm_y,
        theta: canonical_theta(theta),
        beta: m.beta,
        residual: m.fit_residual,
        signal: 0.0,
        area: 0.0,
        peak5: f64::NAN,
        flags: StarFlags::default(),
    })
}

/// `theta` wrapped into `[-π/2, π/2)` (F-2): an axis is a line, so θ and θ ± π name the same one. A value already in
/// range is returned as it is.
fn canonical_theta(theta: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, PI};
    if (-FRAC_PI_2..FRAC_PI_2).contains(&theta) {
        return theta;
    }
    let t = (theta + FRAC_PI_2).rem_euclid(PI) - FRAC_PI_2;
    // `rem_euclid` may round up to its divisor on a value just below a multiple of it.
    if t >= FRAC_PI_2 {
        t - PI
    } else {
        t
    }
}

/// Sum `pixel − background` over the present (finite, unmasked) pixels whose
/// centres lie inside the ellipse with semi-axes `k·fwtm/2`, rotated by
/// `theta` about the fitted centre (`u = dx·cosθ + dy·sinθ`,
/// `v = −dx·sinθ + dy·cosθ`); `area` is the ellipse's analytic area.
pub(crate) fn aperture(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    f: &mut StarFit,
    k: f64,
) {
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
                let i = y * w + x;
                if present(data, mask, i) {
                    sum += data[i] as f64 - f.background;
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
#[allow(clippy::too_many_arguments)]
pub(super) fn fit_all(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: f64,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<StarFit> {
    fit_all_outcomes(data, w, h, seeds, beta, p, mask, field_width, pool)
        .into_iter()
        .filter_map(|(result, _)| result.ok())
        .collect()
}

/// Every seed's result and trace, in seed order (M-4): the parallel chain is
/// the one `fit_all` always ran, collected per seed instead of filtered in
/// flight — rayon's indexed `collect` keeps the order, so the accepted fits
/// read off it sequentially are exactly the list the filtered chain gave.
#[allow(clippy::too_many_arguments)]
fn fit_all_outcomes(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: f64,
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
                fit_one(data, mask, w, h, s, field_width, beta, p, scratch)
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
    fit_stars_diagnosed(data, w, h, seeds, model, p, None, None, pool, false)
}

/// The field width a fit call starts from and reports (Task 6a; read by every star's width-start box from Task 6b):
/// the caller's `Some(f)` with `from_stars = 0`, else [`field_width_of`] the seeds. A given width that is not a
/// positive number is refused with a `warn!` and the seeds' own is used.
fn resolve_field_width(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
    given: Option<f64>,
) -> FieldWidth {
    match given {
        Some(fwhm_px) if fwhm_px.is_finite() && fwhm_px > 0.0 => FieldWidth {
            fwhm_px,
            from_stars: 0,
        },
        Some(bad) => {
            warn!(
                fwhm_px = bad,
                count = seeds.len(),
                "fit: the given field width is not a positive number; the seeds' own is used"
            );
            field_width_of(data, w, h, seeds, mask)
        }
        None => field_width_of(data, w, h, seeds, mask),
    }
}

/// [`fit_stars`], also keeping one [`FitDiagnostics`] per seed, in seed
/// order, in [`FitOutcome::diagnostics`] when `diagnostics` is set (empty
/// otherwise: the run never asks).
/// `mask` and `field_width` as [`super::FitStarsOptions`] carries them.
#[allow(clippy::too_many_arguments)]
pub(super) fn fit_stars_diagnosed(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: Option<f64>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> FitOutcome {
    let field = resolve_field_width(data, w, h, seeds, mask, field_width);
    let beta = match model {
        PsfModel::Moffat4 => 4.0,
        // Tier C Task 1: a caller-resolved β (the group β, or any other
        // fixed value) is fit exactly like `Moffat4`'s fixed 4.0 — the
        // `Auto` search below never runs for it.
        PsfModel::Fixed(b) => b,
        PsfModel::Auto => super::beta::auto_beta(data, w, h, seeds, p, mask, field.fwhm_px, pool),
    };
    fit_at_width(data, w, h, seeds, beta, p, mask, field, pool, diagnostics)
}

/// Fit every seed at a caller-chosen, already-concrete β — the tail
/// [`fit_stars`] itself runs once `Auto`'s search (or `Moffat4`'s fixed
/// 4.0) has resolved a model to a number: the field width from the seeds
/// (the same [`field_width_of`] `fit_stars` reads), every star from its own
/// width start (Task 6b), then `dedupe(fit_all(..))`. `beta` is echoed back
/// unchanged on the returned `FitOutcome`.
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
    fit_stars_with_beta_diagnosed(data, w, h, seeds, beta, p, None, None, pool, false)
}

/// [`fit_stars_with_beta`], also keeping one [`FitDiagnostics`] per seed, in
/// seed order, in [`FitOutcome::diagnostics`] when `diagnostics` is set
/// (empty otherwise).
///
/// The outcomes (M-4): every seed's result comes off [`fit_all_outcomes`]
/// in seed order; the accepted fits are read off it sequentially — the very
/// list the filtered chain collected before — and handed to [`dedupe`], so
/// the kept fits, their order and every sum over them are unchanged. A seed
/// whose fit the duplicate drop removed is refused `Duplicate`. `mask` and
/// `field_width` as [`super::FitStarsOptions`] carries them.
#[allow(clippy::too_many_arguments)]
pub(super) fn fit_stars_with_beta_diagnosed(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: Option<f64>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> FitOutcome {
    let field = resolve_field_width(data, w, h, seeds, mask, field_width);
    fit_at_width(data, w, h, seeds, beta, p, mask, field, pool, diagnostics)
}

/// The body both entry points share once β and the field width are resolved.
#[allow(clippy::too_many_arguments)]
fn fit_at_width(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    beta: f64,
    p: &FitParams,
    mask: Option<&[bool]>,
    field: FieldWidth,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
) -> FitOutcome {
    let per_seed = fit_all_outcomes(data, w, h, seeds, beta, p, mask, field.fwhm_px, pool);
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
    let diagnostics = traces
        .into_iter()
        .zip(outcomes)
        .enumerate()
        .map(|(seed, (trace, outcome))| trace.record(seed, outcome))
        .collect();
    FitOutcome {
        fits,
        beta,
        seeds: seeds.len(),
        refused,
        field_width: field,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::{fit_stars_at, FitStarsOptions};
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
        // Plan A Task 5: the conditioned entry point, which is the fitter `fit_one` calls now.
        let fit = fit_moffat_2d_fixed_beta_conditioned(
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

    // --- Task 5 pins of the incremental ring-append multiset, moved to `grow_region` (Plan A Task 6b, ruling F14) ---
    //
    // `sampling_radius` (the median-falls rule) is retired; `grow_region` (PA-12) grows the same multiset through the
    // same `region_median`. The oracle below rebuilds every square and every rim from scratch and applies PA-12's
    // rule independently; the square medians it visits are Task 5's recorded literals (the squares are the same).

    /// An INDEPENDENT rescan oracle: the full `(2r+1)²` square's median and maximum, never calling
    /// `region_median`/`push_finite_row`.
    fn pin_rescan_square(data: &[f32], w: usize, cx: i64, cy: i64, r: i64) -> Option<(f64, f64)> {
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
        let max = v.iter().fold(f64::NEG_INFINITY, |m, &p| m.max(p as f64));
        Some((median_in_place(&mut v) as f64, max))
    }

    /// The oracle's rim: every pixel of the square whose Chebyshev distance from the centre is exactly `r`.
    fn pin_rescan_rim(data: &[f32], w: usize, cx: i64, cy: i64, r: i64) -> Option<f64> {
        let mut v: Vec<f32> = Vec::new();
        for y in cy - r..=cy + r {
            for x in cx - r..=cx + r {
                if (x - cx).abs().max((y - cy).abs()) == r {
                    let p = data[y as usize * w + x as usize];
                    if p.is_finite() {
                        v.push(p);
                    }
                }
            }
        }
        if v.is_empty() {
            return None;
        }
        Some(median_in_place(&mut v) as f64)
    }

    /// PA-12's growth over the oracle: the stopping radius, its rim median, and every square median visited.
    fn pin_rescan_grow_region(
        data: &[f32],
        w: usize,
        cx: i64,
        cy: i64,
        r_start: i64,
        r_max: i64,
        p: &FitParams,
    ) -> (i64, f64, Vec<f64>) {
        let mut seq = Vec::new();
        let mut r = r_start;
        let mut max = f64::NEG_INFINITY;
        loop {
            let (m, square_max) = pin_rescan_square(data, w, cx, cy, r).expect("a present square");
            max = max.max(square_max);
            seq.push(m);
            let s = pin_rescan_rim(data, w, cx, cy, r).unwrap_or(m);
            if r + 1 > r_max || !(m - s > p.region_growth_contrast * (max - s)) {
                return (r, s, seq);
            }
            r += 1;
        }
    }

    /// Mirrors `grow_region`'s own loop exactly, but collecting every square median it visits through the real
    /// (incremental) `region_median` — `grow_region` itself returns only the radius and the rim median.
    fn incremental_grow_region_with_seq(
        data: &[f32],
        w: usize,
        cx: i64,
        cy: i64,
        r_start: i64,
        r_max: i64,
        p: &FitParams,
    ) -> (i64, f64, Vec<f64>) {
        let mut scratch: Vec<f32> = Vec::new();
        let mut rim: Vec<f32> = Vec::new();
        let mut seq = Vec::new();
        let mut r = r_start;
        let (mut m, mut max) =
            region_median(data, None, w, cx, cy, None, r, &mut scratch).expect("a present square");
        seq.push(m);
        loop {
            let s = rim_median(data, None, w, cx, cy, r, &mut rim).unwrap_or(m);
            if r + 1 > r_max || !(m - s > p.region_growth_contrast * (max - s)) {
                return (r, s, seq);
            }
            let (m_next, max_next) =
                region_median(data, None, w, cx, cy, Some(r), r + 1, &mut scratch).expect("grows");
            r += 1;
            m = m_next;
            max = max.max(max_next);
            seq.push(m);
        }
    }

    /// The three agree bit for bit, and the visited square medians are the first of Task 5's recorded literals.
    fn assert_growth_matches(
        data: &[f32],
        w: usize,
        (cx, cy): (i64, i64),
        (r_start, r_max): (i64, i64),
        task5_medians: &[f64],
    ) -> (i64, f64) {
        let p = FitParams::default();
        let (r_ref, s_ref, seq_ref) = pin_rescan_grow_region(data, w, cx, cy, r_start, r_max, &p);
        let (r_seq, s_seq, seq_new) =
            incremental_grow_region_with_seq(data, w, cx, cy, r_start, r_max, &p);
        let mut scratch = FitScratch::default();
        let (r_prod, s_prod) = grow_region(data, None, w, cx, cy, r_start, r_max, &p, &mut scratch);
        assert_eq!(
            (r_prod, s_prod.to_bits()),
            (r_seq, s_seq.to_bits()),
            "production vs the sequence wrapper"
        );
        assert_eq!(
            (r_ref, s_ref.to_bits()),
            (r_seq, s_seq.to_bits()),
            "growth stopped elsewhere"
        );
        assert_eq!(seq_ref.len(), seq_new.len(), "different number of steps");
        for (a, b) in seq_ref.iter().zip(&seq_new) {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "median sequence: {seq_ref:?} vs {seq_new:?}"
            );
        }
        let n = seq_new.len().min(task5_medians.len());
        assert!(
            n > 1,
            "the growth visits more than its first square: {seq_new:?}"
        );
        for (got, want) in seq_new[..n].iter().zip(&task5_medians[..n]) {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{seq_new:?} vs Task 5's {task5_medians:?}"
            );
        }
        (r_prod, s_prod)
    }

    // Recorded at Task 6b on this build: the stopping radius and its rim median, the fit's background start (the
    // normal star stops at r = 11 under PA-12's rule; the border star runs to its cap, to_border = 13).
    const R_NORMAL: i64 = 11;
    const S_NORMAL: f64 = 0.05049996078014374;
    const R_BORDER: i64 = 13;
    const S_BORDER: f64 = 0.04011831432580948;

    /// A fixture star well inside the field, growth walking several steps.
    #[test]
    fn grow_region_incremental_matches_rescan_on_a_normal_star() {
        let stars = [round(100.4, 100.6, 0.6)];
        let mut data = moffat_field(200, 200, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.01, 41);
        // Task 5's recorded square medians at r = 5…15 (pinned before the incremental change shipped).
        let task5: [f64; 11] = [
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
        let (r, s) = assert_growth_matches(&data, 200, (100, 100), (5, 25), &task5);
        // Recorded at Task 6b: the stopping radius and its rim median (the background start).
        assert_eq!(
            (r, s.to_bits()),
            (R_NORMAL, S_NORMAL.to_bits()),
            "r {r}, s {s:?}"
        );
    }

    /// A fixture star near the image border, so `r_max` is tight and the
    /// growth loop exercises a shorter, boundary-constrained walk.
    #[test]
    fn grow_region_incremental_matches_rescan_near_the_border() {
        let stars = [round(15.3, 12.7, 0.5)];
        let mut data = moffat_field(120, 120, &stars, 3.0, 0.04);
        add_noise(&mut data, 0.008, 43);
        let (cx, cy) = (15i64, 13i64);
        let to_border = cx.min(cy).min(120 - 1 - cx).min(120 - 1 - cy);
        assert!(to_border >= 3, "fixture must leave room to grow");
        // Task 5's recorded square medians at r = 3…13.
        let task5: [f64; 11] = [
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
        let (r, s) = assert_growth_matches(&data, 120, (cx, cy), (3, to_border), &task5);
        assert_eq!(
            (r, s.to_bits()),
            (R_BORDER, S_BORDER.to_bits()),
            "r {r}, s {s:?}"
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

    // --- Plan A Task 5 (F-2): canonical axes and a conditioned fit ---

    fn grid(n: usize, alpha_x: f64, alpha_y: f64, theta: f64) -> (Vec<MoffatStar>, Vec<Seed>) {
        let stars: Vec<MoffatStar> = (0..n)
            .map(|i| MoffatStar {
                x: 30.0 + (i % 8) as f64 * 40.0,
                y: 30.0 + (i / 8) as f64 * 40.0,
                amp: 0.3,
                alpha_x,
                alpha_y,
                theta,
            })
            .collect();
        let seeds = stars
            .iter()
            .map(|s| Seed {
                x: s.x,
                y: s.y,
                peak: s.amp,
                flux: 1.0,
            })
            .collect();
        (stars, seeds)
    }

    #[test]
    fn accepted_fits_have_canonical_axes() {
        // αx 3 / αy 6 at θ = 20°: the generator's u axis (θ) is the MINOR one; the canonical fit must report the major
        // axis on fwhm_x and θ_canonical = 20° + 90° (mod 180°)
        let (stars, seeds) = grid(40, 3.0, 6.0, 20f64.to_radians());
        let data = moffat_field(360, 240, &stars, 4.0, 0.05);
        // Plan A Task 6a (ruling F14): `fit_stars_at` takes `&FitStarsOptions` in place of `&FitParams`.
        let out = fit_stars_at(
            &data,
            360,
            240,
            &seeds,
            4.0,
            &FitStarsOptions::default(),
            None,
        );
        assert!(out.fits.len() >= 36, "{}", out.fits.len());
        let want = (20f64 + 90.0).to_radians();
        for f in &out.fits {
            assert!(f.fwhm_x >= f.fwhm_y, "{f:?}");
            let d = ((f.theta - want + std::f64::consts::FRAC_PI_2)
                .rem_euclid(std::f64::consts::PI)
                - std::f64::consts::FRAC_PI_2)
                .abs();
            assert!(d < 2f64.to_radians(), "theta {} vs {want}", f.theta);
            assert!((f.fwtm_x / f.fwhm_x - f.fwtm_y / f.fwhm_y).abs() < 1e-9);
            assert!(
                f.theta >= -std::f64::consts::FRAC_PI_2 && f.theta < std::f64::consts::FRAC_PI_2
            );
        }
    }

    #[test]
    fn fit_is_translation_and_scale_invariant() {
        let star = |x, y, amp| MoffatStar {
            x,
            y,
            amp,
            alpha_x: 3.0,
            alpha_y: 4.5,
            theta: 0.5,
        };
        // The bright copy is ×1024, not ×1000 (controller ruling, Task 5): the planes are noiseless f32, so their only
        // noise is f32 rounding, and a factor that is not a power of two rounds the bright copy differently from the
        // faint one — that alone moves a noiseless fit by ~1e-8 relative (the residual, which sits at that rounding
        // floor, by 20 %). A power of two scales every f32 value exactly. The seed's peak AND flux scale with it, so
        // σ0 = √(flux/(2π·peak)) is the same start on both.
        const K: f64 = 1024.0;
        let near = moffat_field(80, 80, &[star(30.3, 20.7, 0.3)], 4.0, 0.05);
        let far = moffat_field(3100, 2100, &[star(3000.3, 2000.7, 0.3)], 4.0, 0.05);
        let bright = moffat_field(80, 80, &[star(30.3, 20.7, 0.3 * K)], 4.0, 0.05 * K as f32);
        let f = |d: &[f32], w, h, x, y, p, flux| {
            fit_stars_at(
                d,
                w,
                h,
                &[Seed {
                    x,
                    y,
                    peak: p,
                    flux,
                }],
                4.0,
                // Plan A Task 6a (ruling F14): `&FitStarsOptions` in place of `&FitParams`.
                &FitStarsOptions::default(),
                None,
            )
            .fits[0]
        };
        let a = f(&near, 80, 80, 30.3, 20.7, 0.3, 1.0);
        let b = f(&far, 3100, 2100, 3000.3, 2000.7, 0.3, 1.0);
        let c = f(&bright, 80, 80, 30.3, 20.7, 0.3 * K, K);
        let rel = |p: f64, q: f64| ((p - q) / q).abs();
        assert!(
            rel(b.x - 2970.0, a.x) < 1e-9 && rel(b.y - 1980.0, a.y) < 1e-9,
            "translation: {a:?} {b:?}"
        );
        for (p, q) in [
            (a.fwhm_x, b.fwhm_x),
            (a.fwhm_y, b.fwhm_y),
            (a.theta, b.theta),
            (a.residual, b.residual),
        ] {
            assert!(rel(p, q) < 1e-9, "{p} vs {q}");
        }
        assert!(
            rel(c.amplitude, K * a.amplitude) < 1e-9 && rel(c.background, K * a.background) < 1e-9,
            "scale: amplitude {:e}, background {:e}",
            rel(c.amplitude, K * a.amplitude),
            rel(c.background, K * a.background)
        );
        assert!(
            rel(c.fwhm_x, a.fwhm_x) < 1e-9 && rel(c.residual, a.residual) < 1e-9,
            "scale: fwhm_x {:e}, residual {:e}",
            rel(c.fwhm_x, a.fwhm_x),
            rel(c.residual, a.residual)
        );
    }

    /// A pin, not RED (ruling F28): `fwhm()` and `eccentricity()` are symmetric in x/y by construction.
    #[test]
    fn eccentricity_and_fwhm_are_unchanged_by_the_swap() {
        let mut f = StarFit::test_at(1.0, 1.0, 1.0);
        f.fwhm_x = 2.0;
        f.fwhm_y = 3.0;
        let (fw, e) = (f.fwhm(), f.eccentricity());
        std::mem::swap(&mut f.fwhm_x, &mut f.fwhm_y);
        assert_eq!((f.fwhm(), f.eccentricity()), (fw, e));
    }

    /// A pin, not RED (ruling F28; review M4): `canonical_theta` and the axis swap in `accept`, both added and
    /// verified with Task 5, at their boundaries. Every θ they return lies in `[-π/2, π/2)`.
    #[test]
    fn canonical_theta_and_the_swap_hold_at_their_boundaries() {
        use std::f64::consts::{FRAC_PI_2, PI};
        let in_range = |t: f64| (-FRAC_PI_2..FRAC_PI_2).contains(&t);
        // The two ends: -π/2 stays, π/2 is the same axis and folds to -π/2.
        assert_eq!(canonical_theta(-FRAC_PI_2), -FRAC_PI_2);
        assert_eq!(canonical_theta(FRAC_PI_2), -FRAC_PI_2);
        // Just inside each end: returned exactly as given.
        let (lo, hi) = (-FRAC_PI_2 + 1e-12, FRAC_PI_2 - 1e-12);
        assert_eq!(canonical_theta(lo).to_bits(), lo.to_bits());
        assert_eq!(canonical_theta(hi).to_bits(), hi.to_bits());
        // Just outside each end: folded by π to just inside the other.
        let below = canonical_theta(-FRAC_PI_2 - 1e-9);
        assert!(
            in_range(below) && (below - (FRAC_PI_2 - 1e-9)).abs() < 1e-12,
            "{below}"
        );
        let above = canonical_theta(FRAC_PI_2 + 1e-9);
        assert!(
            in_range(above) && (above - (-FRAC_PI_2 + 1e-9)).abs() < 1e-12,
            "{above}"
        );
        // Whole turns either way: in range and the same axis (a multiple of π away).
        for k in -4..=4 {
            for t in [-FRAC_PI_2, -1.0, 0.0, 0.3, FRAC_PI_2 - 1e-15, FRAC_PI_2] {
                let c = canonical_theta(t + k as f64 * PI);
                let turns = (c - t) / PI;
                assert!(
                    in_range(c) && (turns - turns.round()).abs() < 1e-9,
                    "{t} + {k}π -> {c}"
                );
            }
        }

        // Through `accept`: a clean fit at the seed, β 4.
        let fit = |alpha_x: f64, alpha_y: f64, theta: f64| Moffat2DResult {
            b: 0.0,
            a: 1.0,
            x0: 10.0,
            y0: 10.0,
            alpha_x,
            alpha_y,
            theta,
            beta: 4.0,
            converged: true,
            fit_residual: 0.01,
        };
        let seed = Seed {
            x: 10.0,
            y: 10.0,
            peak: 1.0,
            flux: 10.0,
        };
        let p = FitParams::default();
        let acc = |m: Moffat2DResult| {
            accept(&m, &seed, 10.0, 10.0, 20.0, &p).expect("a clean fit is accepted")
        };
        // Minor axis first (fwhm_x < fwhm_y): swapped, θ a quarter turn away, on the side that stays in range.
        for theta in [-FRAC_PI_2, -0.3, 0.0, 0.3, FRAC_PI_2] {
            let f = acc(fit(2.0, 3.0, theta));
            assert!(f.fwhm_x > f.fwhm_y, "{f:?}");
            assert_eq!(
                (f.fwtm_x, f.fwtm_y),
                (fwtm_from_alpha(3.0, 4.0), fwtm_from_alpha(2.0, 4.0))
            );
            assert!(in_range(f.theta), "θ {theta} -> {}", f.theta);
            assert!(
                ((f.theta - theta).abs() - FRAC_PI_2).abs() < 1e-12,
                "θ {theta} -> {}",
                f.theta
            );
        }
        // Major axis already first: θ unchanged, but for the one end that folds.
        for theta in [-FRAC_PI_2, -0.3, 0.3] {
            let f = acc(fit(3.0, 2.0, theta));
            assert!(f.fwhm_x > f.fwhm_y);
            assert_eq!(f.theta.to_bits(), theta.to_bits());
        }
        assert_eq!(acc(fit(3.0, 2.0, FRAC_PI_2)).theta, -FRAC_PI_2);
        // A round star (fwhm_x == fwhm_y): no swap, θ only folded into range.
        let f = acc(fit(2.5, 2.5, 0.7));
        assert_eq!((f.fwhm_x, f.theta), (f.fwhm_y, 0.7));
        let f = acc(fit(2.5, 2.5, FRAC_PI_2));
        assert_eq!((f.fwhm_x, f.theta), (f.fwhm_y, -FRAC_PI_2));
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

    // --- Plan A Task 6b (F-1 with F-7): the per-star width start, the region, the rim background, the refusals ---

    use crate::star_measure::{measure_plane, seeds, MeasureOptions, SeedSpec};
    use crate::test_support::nan_border_field;

    fn one(x: f64, y: f64, amp: f64, alpha: f64) -> MoffatStar {
        MoffatStar {
            x,
            y,
            amp,
            alpha_x: alpha,
            alpha_y: alpha,
            theta: 0.0,
        }
    }
    fn seed(s: &MoffatStar) -> Seed {
        Seed {
            x: s.x,
            y: s.y,
            peak: s.amp,
            flux: 1.0,
        }
    }
    fn fit_with(
        data: &[f32],
        w: usize,
        h: usize,
        seeds: &[Seed],
        field: f64,
    ) -> (FitOutcome, Vec<FitDiagnostics>) {
        let opts = FitStarsOptions {
            field_width: Some(field),
            diagnostics: true,
            ..Default::default()
        };
        let out = fit_stars_at(data, w, h, seeds, 4.0, &opts, None);
        let d = out.diagnostics.clone();
        (out, d)
    }

    /// A deterministic 2000-point sunflower lattice over the annulus `r_in ≤ r ≤ r_out` about `(cx, cy)`: point `k`
    /// at the radius enclosing the area fraction `(k + ½)/2000` of the annulus, turned by the golden angle per point,
    /// so the points sample the annulus uniformly by area.
    fn ring_samples(cx: f64, cy: f64, r_in: f64, r_out: f64) -> Vec<(f64, f64)> {
        const N: usize = 2000;
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        (0..N)
            .map(|k| {
                let t = (k as f64 + 0.5) / N as f64;
                let r = (r_in * r_in + t * (r_out * r_out - r_in * r_in)).sqrt();
                let (s, c) = (k as f64 * golden).sin_cos();
                (cx + r * c, cy + r * s)
            })
            .collect()
    }

    #[test]
    fn broad_stars_fit_inside_a_region_that_scales_with_their_own_width() {
        // FWHM 14 at β 4: α = 14 / (2√(2^{1/4} − 1)) = 16.09; half-FWTM = 14.2 px — the D1 cause-1 regime
        let stars: Vec<MoffatStar> = (0..20)
            .map(|i| {
                one(
                    120.0 + (i % 5) as f64 * 180.0,
                    120.0 + (i / 5) as f64 * 180.0,
                    0.3,
                    16.09,
                )
            })
            .collect();
        let data = moffat_field(1024, 840, &stars, 4.0, 0.05);
        let seeds: Vec<Seed> = stars.iter().map(seed).collect();
        let (out, diag) = fit_with(&data, 1024, 840, &seeds, 14.0);
        assert_eq!(out.fits.len(), 20, "{:?}", out.refused);
        for (f, d) in out.fits.iter().zip(&diag) {
            assert!((f.fwhm() / 14.0 - 1.0).abs() < 0.01, "{}", f.fwhm());
            assert!(
                d.region_radius as f64 >= 0.5 * f.fwtm_x.max(f.fwtm_y),
                "{d:?}"
            );
            assert!(d.region_radius as f64 >= 1.5 * 14.0 && d.region_radius <= 96);
        }
    }

    #[test]
    fn donut_is_refused_with_its_reason() {
        // an annulus (outer 6, obstruction 2.4) blurred by a Gaussian of σ 1.1 (FWHM 2.6), rendered by numeric convolution
        let (w, h) = (80usize, 80usize);
        let mut data = vec![0.05f32; w * h];
        let ring = ring_samples(40.0, 40.0, 2.4, 6.0); // 2000 samples of the annulus, uniform
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0f64;
                for &(ux, uy) in &ring {
                    let d2 = (x as f64 - ux).powi(2) + (y as f64 - uy).powi(2);
                    acc += (-d2 / (2.0 * 1.1 * 1.1)).exp();
                }
                data[y * w + x] += (0.3 * acc / 2000.0) as f32;
            }
        }
        let (out, diag) = fit_with(
            &data,
            w,
            h,
            &[Seed {
                x: 40.0,
                y: 40.0,
                peak: 0.1,
                flux: 1.0,
            }],
            3.0,
        );
        assert!(
            out.fits.is_empty(),
            "a donut must never fit: {:?}",
            out.fits
        );
        assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::Donut));
    }

    /// A pin on the ring radius's 1.5-px floor (ruling F28: before this task no star is refused `Donut` at all, so it
    /// cannot fail by behaviour; it fails on a Donut rule without the floor): a sharp real star is never a donut.
    /// Computed on point-sampled Moffat-4 and Gaussian stars over 100 sub-pixel phases: core/ring ≥ 2.32 at FWHM 1.2,
    /// ≥ 1.61 at 1.6, ≥ 1.37 at 2.0 (the unfloored radius read 0.29–0.59 there).
    #[test]
    fn sharp_stars_are_never_refused_as_donuts() {
        for (k, fwhm) in [1.2f64, 1.6, 2.0].into_iter().enumerate() {
            let alpha = fwhm / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
            let stars: Vec<MoffatStar> = (0..25)
                .map(|i| {
                    one(
                        20.0 + (i % 5) as f64 * 30.1 - 0.2,
                        20.0 + (i / 5) as f64 * 30.1 - 0.2,
                        0.3,
                        alpha,
                    )
                })
                .collect();
            let mut data = moffat_field(160, 160, &stars, 4.0, 0.05);
            crate::test_support::add_noise(&mut data, 0.002, k as u64);
            let seeds: Vec<Seed> = stars.iter().map(seed).collect();
            let (_, diag) = fit_with(&data, 160, 160, &seeds, fwhm);
            assert_eq!(diag.len(), 25);
            assert!(
                diag.iter()
                    .all(|d| d.outcome != Outcome::Refused(RefusalReason::Donut)),
                "FWHM {fwhm}: {diag:?}"
            );
        }
    }

    #[test]
    fn a_given_hot_pixel_is_refused_narrower_than_pixel() {
        let (mut data, w, h) = crate::test_support::fixture_field(3, 0.0, 0.002);
        data[100 * w + 100] = 0.9;
        let (out, diag) = fit_with(
            &data,
            w,
            h,
            &[Seed {
                x: 100.0,
                y: 100.0,
                peak: 0.85,
                flux: 0.85,
            }],
            3.0,
        );
        assert!(out.fits.is_empty());
        assert_eq!(
            diag[0].outcome,
            Outcome::Refused(RefusalReason::NarrowerThanPixel)
        );
    }

    #[test]
    fn rim_background_start_is_the_sky_under_nebulosity() {
        // sky 0.05 + a quadratic ramp of amplitude 0.02 over the 80-px stamp; σ_px = 0.002; the F-7 condition at one star
        let (w, h) = (80usize, 80usize);
        let mut data = moffat_field(w, h, &[one(40.3, 39.7, 0.3, 3.0)], 4.0, 0.0);
        for y in 0..h {
            for x in 0..w {
                let r2 = ((x as f64 - 10.0).powi(2) + (y as f64 - 70.0).powi(2)) / 6400.0;
                data[y * w + x] += 0.05 + (0.02 * r2) as f32;
            }
        }
        crate::test_support::add_noise(&mut data, 0.002, 4);
        let local_sky =
            0.05 + 0.02 * ((40.3f64 - 10.0).powi(2) + (39.7f64 - 70.0).powi(2)) / 6400.0;
        let (out, diag) = fit_with(
            &data,
            w,
            h,
            &[Seed {
                x: 40.3,
                y: 39.7,
                peak: 0.3,
                flux: 1.0,
            }],
            3.0,
        );
        assert_eq!(out.fits.len(), 1);
        assert!(
            (diag[0].background_start - local_sky).abs() < 0.3 * 0.002,
            "b0 {} vs sky {local_sky}",
            diag[0].background_start
        );
        assert!((out.fits[0].background - local_sky).abs() < 0.3 * 0.002);
    }

    #[test]
    fn border_limited_region_refuses_a_broad_star_region_too_small() {
        // PA-18: a FWHM-12 star (α 13.8) in a FWHM-3 field, 10 px from the border: the width-start box (radius 6) fits,
        // the region is clipped at r = 10, and 0.5·FWTM = 12.2 > 10 → RegionTooSmall
        let data = moffat_field(200, 200, &[one(10.0, 100.0, 0.3, 13.8)], 4.0, 0.05);
        let (out, diag) = fit_with(
            &data,
            200,
            200,
            &[Seed {
                x: 10.0,
                y: 100.0,
                peak: 0.3,
                flux: 1.0,
            }],
            3.0,
        );
        assert!(out.fits.is_empty());
        assert_eq!(
            diag[0].outcome,
            Outcome::Refused(RefusalReason::RegionTooSmall)
        );
        assert_eq!(diag[0].region_radius, 10);
    }

    #[test]
    fn a_star_whose_width_start_box_leaves_the_image_is_outside_image() {
        let data = moffat_field(200, 200, &[one(4.0, 100.0, 0.3, 3.0)], 4.0, 0.05);
        let (out, diag) = fit_with(
            &data,
            200,
            200,
            &[Seed {
                x: 4.0,
                y: 100.0,
                peak: 0.3,
                flux: 1.0,
            }],
            3.0,
        );
        assert!(out.fits.is_empty());
        assert_eq!(
            diag[0].outcome,
            Outcome::Refused(RefusalReason::OutsideImage)
        );
    }

    #[test]
    fn nan_border_region_growth_skips_non_finite_pixels() {
        let data = nan_border_field(240, 240, 6, true);
        let m = measure_plane(
            &data,
            240,
            240,
            &MeasureOptions {
                diagnostics: true,
                seeds: SeedSpec::MatchedFilter {
                    threshold: seeds::DEFAULT_THRESHOLD,
                },
                ..Default::default()
            },
            None,
        );
        assert_eq!(m.stars.len(), 40);
        assert!(m
            .stars
            .iter()
            .all(|f| f.background.is_finite() && f.fwhm().is_finite()));
    }
}
