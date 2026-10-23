//! The per-star fit (math reference §1.4; spec 2026-10-07 §2.1 `fit`): elliptical Moffat fits on the seeds, the
//! per-star sampling region, the acceptance rules, each fit's star flux (from F-5, the model's flux in its FWTM
//! ellipse times the plane's aperture correction) and the ±1 px duplicate drop. Moved verbatim from `psf_signal`
//! (Plan A Task 2); the `Auto` β search lives in [`super::beta`]. Coordinates are 0-based pixel centres.
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
//! F-1 with F-7, the fit (Plan A Task 6b, ruling R-OSP-4 as amended by round 1, ruling R1): every star starts from
//! its own half-maximum width where readable (its peak ≥ 10 σ above the rim of a box of radius `⌈2·F_field⌉`, which
//! is also the admission test, PA-18), else from the field width; its region
//! grows from `max(3, ⌈1.5·F_start⌉)` up to `clamp(⌈6·F_start⌉, 6, 96)` px while the region's median stands above its
//! rim's by more than 1 % of the peak (PA-12); the fit's background starts from that rim's median, and its start shape
//! from the moments of the region's starting square (round 3, R9). An annular star is refused `Donut` before any fit
//! (its core about a local iterated centre against a ring at the box moments' radius about it, round 3, R7/R8), and a
//! fit narrower than 0.8 px `NarrowerThanPixel`. The field σ0 from the seeds' flux/peak and the field-level nominal
//! stamp are retired.
//!
//! F-3 (Plan A Task 8, ruling R-OSP-5): on the measure path every seed whose region is gathered gets ONE saturation
//! verdict ([`super::photometry::saturation_verdict`]: the plateau test at the seed pixel, then a censored free-β
//! verdict fit, flagged when its amplitude tops the plane's range), after the field width and before the β sample:
//! seeds → field width → verdicts ([`verdict_pass`]; on a multi-plane image every plane's, then the cross-plane rule)
//! → β sample → fits at β ([`fit_after_passes`]). The `Auto` β sample is the 64 brightest seeds whose verdict fit
//! succeeded unflagged (from Task 10b also centred, and not flagged extended by the knot test); every star is fitted at β and carries its flag in [`StarFlags::SATURATED`], a flagged one
//! (own or cross-plane) without its pixels ≥ its level, ×3 iterations (Q6 as revised 2026-10-09). A seed whose verdict
//! fit keeps no more pixels than its 8 parameters is refused [`RefusalReason::MostlySaturated`].
//!
//! F-4 (Plan A Task 9, ruling R-OSP-6, revised by the owner's E5): `PsfModel::Auto`'s β is continuous — the median
//! of the sample's PIXEL-INTEGRATED bounded free-β fits ([`super::beta`]: on the measure path the sample is the
//! unflagged successful verdicts, each re-fitted on its verdict fit's own pixels, [`sample_fits`]; a caller that ran
//! no verdict fits its own sample, [`own_sample_fits`]), clamped to [`super::beta::BETA_BOUNDS`]; a frame β at a
//! bound flags every star fitted at it [`StarFlags::BETA_AT_CAP`]. The verdict fit and the fits at β stay the point
//! model.
//!
//! F-5 (Plan A Task 10, ruling R-OSP-7): a fit's `signal` is no longer the pixel sum inside its FWTM ellipse (the
//! fraction of a star's light that ellipse holds is 0.75 of a Moffat-2.5's and 0.94 of a Gaussian's): it is
//! [`super::photometry::model_flux_in_fwtm`] times the plane's aperture correction `c`
//! ([`super::photometry::aperture_correction`], measured after the duplicate drop, or the caller's
//! [`super::FluxCorrection::Given`]). No fit, refusal or acceptance rule reads `signal`: every fit is what it was.
//!
//! F-10 (Plan A Task 10b, rulings B1–B4): after the duplicate drop every kept fit carries its aperture-scale sky B_i,
//! noise σ_i and pixel count n_i ([`super::photometry::ring_skies`]: R6's ring at the plane's frame FWHM, or the
//! caller's [`super::FitStarsOptions::ring_fwhm`]), and `c` reads the stored B_i; the fit's own `b` stays free (B1).
//! [`FitOutcome::fit_seeds`] lets a caller re-read both at another ring width without a second fit
//! ([`super::correct_at_ring`], LN's pair-common arm).

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use astroimage::analysis::fitting::{
    fit_moffat_2d_fixed_beta_conditioned, Moffat2DResult, PixelSample,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::beta::BetaSource;
use super::diagnostics::{FitDiagnostics, Outcome, RefusalCounts, RefusalReason};
use super::photometry::{
    integrated_beta, saturation_verdict, SaturationPlane, SaturationVerdict,
    EXCLUDED_ITERATION_FACTOR,
};
use super::seeds::{field_width_of, own_halfmax_width, FieldWidth};
use crate::integration::stats::{mad_about, median_in_place, MAD_TO_SIGMA};

/// `2√(2 ln 2)`: a Gaussian's FWHM per σ (the start width [`FitDiagnostics::width_start`] reports).
const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum PsfModel {
    /// The continuous per-frame β (F-4, Task 9): the median of the pixel-integrated bounded free-β fits of the
    /// [`super::beta::AUTO_SAMPLE`] brightest unsaturated seeds, clamped to [`super::beta::BETA_BOUNDS`]
    /// ([`super::beta`]); 4.0 when fewer than 8 fits are accepted. Through Task 8, the discrete search over
    /// `AUTO_BETAS`.
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    pub x: f64,
    pub y: f64,
    pub peak: f64,
    pub flux: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitParams {
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
/// (`stacking::fits_artifact`). `SATURATED` is set from Task 8 (the measure path's saturation verdict), `BETA_AT_CAP`
/// from Task 9, `EXTENDED` from Task 10b (the matched filter's knot test). A bit this build does not name is kept as it
/// is (a newer writer's flag survives a round trip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StarFlags(pub u8);

impl StarFlags {
    /// The star's core is clipped (F-3).
    pub const SATURATED: StarFlags = StarFlags(1);
    /// The star was fitted at a fitted frame β that sits at a bound of [`super::beta::BETA_BOUNDS`] (F-4, Task 9):
    /// set on every star of such a plane, never under a caller's fixed β.
    pub const BETA_AT_CAP: StarFlags = StarFlags(2);
    /// The star's seed is not a point source (F-10, Task 10b ruling A3: the knot test's zero-sum ratio): it is fitted
    /// and kept, but leaves the β sample, `c`'s qualifiers and the frame statistic, as a saturated star does (P3).
    pub const EXTENDED: StarFlags = StarFlags(4);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: StarFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// [`StarFlags::SATURATED`] is set (F-3): the β sample and the frame statistic skip the star.
    pub const fn saturated(self) -> bool {
        self.contains(StarFlags::SATURATED)
    }

    /// [`StarFlags::BETA_AT_CAP`] is set (F-4): the star was fitted at a frame β that sits at a bound of
    /// [`super::beta::BETA_BOUNDS`].
    pub const fn beta_at_cap(self) -> bool {
        self.contains(StarFlags::BETA_AT_CAP)
    }

    /// [`StarFlags::EXTENDED`] is set (F-10): the β sample, `c`'s qualifiers and the frame statistic skip the star.
    pub const fn extended(self) -> bool {
        self.contains(StarFlags::EXTENDED)
    }

    /// The star is one the frame statistic and `c`'s qualifiers read: neither saturated (F-3) nor extended (F-10, P3).
    pub const fn in_statistic(self) -> bool {
        !self.saturated() && !self.extended()
    }

    /// The named flags set, in bit order, as the probe's star lines write them (`"saturated"`, `"betaAtCap"`,
    /// `"extended"`).
    pub fn names(self) -> Vec<&'static str> {
        [
            (StarFlags::SATURATED, "saturated"),
            (StarFlags::BETA_AT_CAP, "betaAtCap"),
            (StarFlags::EXTENDED, "extended"),
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
    /// The star's flux (F-5, Task 10): the fitted model's flux inside its FWTM ellipse
    /// ([`super::photometry::model_flux_in_fwtm`]) times the plane's aperture correction
    /// ([`FitOutcome::aperture_correction`]; 1 when it is `None`). Before Plan A Task 10, the background-subtracted
    /// pixel sum inside that ellipse.
    pub signal: f64,
    /// Analytic area of that ellipse, `π·(k/2)²·fwtm_x·fwtm_y`.
    pub area: f64,
    /// Mean of the five brightest finite pixels in the fit's sampling
    /// region (spec 2026-10-04 L-12) — the peak the LN star selection
    /// judges SNR on. Not persisted: NaN on a fit decoded from the `fits`
    /// artifact.
    #[serde(skip, default = "not_measured")]
    pub peak5: f64,
    /// B_i (Task 10b, F-10, ruling B2): the star's aperture-scale sky, the plain mean of its present pixels at
    /// `10F ≤ d ≤ 12F` from the fitted centre outside every seed's `3F` disc (R6's ring,
    /// [`super::photometry::ring_sky`]), in the fit's units; `c` reads it, Task 11's SNR and HFR will. Not persisted:
    /// NaN on a decoded fit, and where the ring holds fewer than 8 present pixels.
    #[serde(skip, default = "not_measured")]
    pub sky_ring: f64,
    /// σ_i (ruling B4): the RMS of `v − B_i` over B_i's own pixels. Not persisted: NaN where `sky_ring` is.
    #[serde(skip, default = "not_measured")]
    pub sky_ring_sigma: f64,
    /// n_i: the ring pixels B_i and σ_i read; 0 where the ring is missing. Not persisted.
    #[serde(skip)]
    pub sky_ring_n: u32,
    /// The star's aperture SNR (F-6, Task 11, [`super::photometry::star_snr`]): `S / sigma_S` over the star's own ring
    /// sky and noise. It reads neither `signal` nor `c` and is read BEFORE `c` (the SNR floor of the correction's sample:
    /// `snr_pass` runs before `aperture_correction` inside `ring_and_correction`), and again when a ring is re-read
    /// ([`super::correct_at_ring`]). Every accepted fit carries one, saturated and extended included (R11-5); NaN
    /// without a ring. Not persisted (NaN on a decoded fit), like `peak5`.
    #[serde(skip, default = "not_measured")]
    pub snr: f64,
    /// The star's half-flux radius, px (F-6, Task 11, [`super::photometry::hfr`]): NaN without a ring, or when the `5F`
    /// circle holds less than half of `signal`. Not persisted.
    #[serde(skip, default = "not_measured")]
    pub hfr: f64,
    /// [`StarFlags`]: written and read as the `.athf` flags byte; a fit decoded from a file written before Plan A
    /// Task 4 (whose byte is 0) reads none.
    #[serde(default)]
    pub flags: StarFlags,
}

fn not_measured() -> f64 {
    f64::NAN
}

impl StarFit {
    /// The near/far sky difference `b − B_i` (Task 10b, ruling B1: the fit's free local background against its
    /// aperture-scale ring sky): the structured-sky diagnostic. NaN where the ring is missing.
    pub fn sky_offset(&self) -> f64 {
        self.background - self.sky_ring
    }
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
    /// Where `beta` came from (F-4, Task 9): [`BetaSource::Fixed`] for a caller's β.
    pub beta_source: BetaSource,
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
    /// The plane is defocused (owner ruling 2026-10-08, [`plane_is_defocused`]): `fits` is empty and every seed that
    /// would have been accepted is refused [`RefusalReason::Defocused`].
    pub defocused: bool,
    /// The readable seeds that refused `Donut` (the verdict's `n_donut`); 0 unless the caller asked for the verdict.
    pub defocus_donuts: usize,
    /// The plane is trailed (Task 10b, F-10; rulings A6, Q1, Q5, [`plane_is_trailed`]): `fits` is empty and every seed
    /// that would have been accepted is refused [`RefusalReason::Trailed`]. Never set on a defocused plane (that verdict
    /// is read first) nor when the caller did not ask for it.
    pub trailed: bool,
    /// The share of the judged seeds refused `CentreMoved`, `RegionTooSmall` or `CentreAtRim` ([`trail_share`]), whether or not the
    /// caller asked for the verdict; 0 with no seed.
    pub trail_share: f64,
    /// The seeds whose saturation verdict fit failed for a reason other than n ≤ p (F-3, Q4: no flag, no refusal, out
    /// of the β sample); 0 on every path that runs no verdict.
    pub verdict_fit_failed: usize,
    /// The aperture correction `c` every fit's `signal` carries (F-5, Task 10): the plane's own
    /// ([`super::FluxCorrection::FromPlane`]) or the caller's ([`super::FluxCorrection::Given`]). `None` = `c = 1`:
    /// no fit, or fewer than 5 isolated unsaturated stars qualified.
    pub aperture_correction: Option<f64>,
    /// The qualifying stars the plane's own `aperture_correction` is the median over (at most 64); 0 for a caller's.
    pub aperture_stars: usize,
    /// The standard error of the median `aperture_correction` is, from the ratios' MAD (`1.2533 * 1.4826 * MAD /
    /// sqrt(n)`); `None` where `c` was not measured on the plane's own ratios.
    pub aperture_se: Option<f64>,
    /// What the plane's own `aperture_correction` screened out (the point-source and SNR counts) and the ratios it took
    /// its median over (Plan B B4, PB-12' / PB-13'); empty for a caller's `c` and where no ratio was read.
    pub aperture_screens: super::photometry::ApertureScreens,
    /// The accepted fits without a ring sky B_i (Task 10b, F-10: fewer than 8 present ring pixels, or no ring width on
    /// the plane); their `sky_ring` and `sky_ring_sigma` are NaN and `sky_ring_n` 0.
    pub ring_sky_missing: usize,
    /// The seed each fit came from, in `fits` order (`fits[k]` is seed `fit_seeds[k]`'s): what a later re-read of the
    /// ring sky and `c` at another ring width needs (LN's pair-common arm, [`super::correct_at_ring`]; no second fit).
    pub fit_seeds: Vec<usize>,
    /// The ring width F the fits' ring skies B_i and `c` were read at ([`super::photometry::ring_skies`]: the caller's
    /// `ring_fwhm`, else the plane's frame FWHM over the kept fits neither saturated nor extended); `None` when no ring
    /// width existed (no fit in the statistic). The one owner of that width: LN's pair-common arm reads it here.
    pub ring_fwhm: Option<f64>,
}

/// A plane with this many readable seeds refused `Donut`, and at least [`DEFOCUSED_DONUT_FRACTION`] of its readable
/// seeds, is defocused.
const DEFOCUSED_MIN_DONUTS: usize = 5;
const DEFOCUSED_DONUT_FRACTION: f64 = 0.25;

/// The defocused-frame rule (owner ruling 2026-10-08: heavily defocused stars are not stars). A faint donut cannot be
/// told from a blurred star by its own pixels, but defocus belongs to the whole frame: `n_read` readable seeds (the
/// width start's readability gate passed), `n_donut` of them refused `Donut`.
pub(super) fn plane_is_defocused(n_read: usize, n_donut: usize) -> bool {
    n_donut >= DEFOCUSED_MIN_DONUTS && n_donut as f64 >= DEFOCUSED_DONUT_FRACTION * n_read as f64
}

/// The trailed-frame verdict's line (Task 10b, F-10; ruling Q5, the majority rule): a plane is trailed when MORE than
/// half of its judged seeds were refused `CentreMoved`, `RegionTooSmall` or `CentreAtRim` — most of the frame's detected sources cannot
/// be contained in a fit region, which is what a trail longer than the region does to its fit. A physical criterion,
/// not a number fitted to one frame: the line derived from the clean frames' own distribution (median + 5·1.4826·MAD)
/// did not separate the elongated frames from the trailed one (plan Task 10b phase A4 step 2a: clean real and lab
/// frames ≤ 0.151, the two elongated cases 0.366 / 0.389, the trailed L 0010 0.893). It rests on few real trailed
/// frames; the lab's synthetic long-trail family crosses it near a streak of one fit-region diameter. ecc is never the
/// gate (ruling Q1: an elongated star is a real star, measured as such). Ruling A4-2: the share counts every
/// containment refusal — a streak fit that the half-maximum centre test no longer catches reaches the rim test, so
/// leaving `CentreAtRim` out would change the quantity, not the frame (clean frames ≤ 0.15, G 0003 0.175, trailed
/// frames ≥ 0.88 on the A4 proof set).
pub const TRAIL_REFUSAL_SHARE: f64 = 0.5;
/// Under this many judged seeds there is no trailed verdict, whatever the share: the minimum detection count of the
/// plate solver's own trailed-frame rule, quoted (it is private there), not pinned.
pub const TRAIL_VERDICT_MIN_SEEDS: usize = 100;

/// The share of the `judged` seeds refused `CentreMoved`, `RegionTooSmall` or `CentreAtRim` (Task 10b, F-10, ruling Q1: the refusal
/// structure a trail leaves): `(centreMoved + regionTooSmall + centreAtRim) / judged`, 0 when nothing was judged. `judged` = the
/// seeds the fit call gave an outcome (every seed handed in; Task 12's budget outcomes are not judged).
pub fn trail_share(refused: &RefusalCounts, judged: usize) -> f64 {
    if judged == 0 {
        return 0.0;
    }
    let n = |r| refused.by_reason.get(&r).copied().unwrap_or(0);
    (n(RefusalReason::CentreMoved)
        + n(RefusalReason::RegionTooSmall)
        + n(RefusalReason::CentreAtRim)) as f64
        / judged as f64
}

/// The trailed-frame rule (ruling Q5): at least [`TRAIL_VERDICT_MIN_SEEDS`] judged seeds and a [`trail_share`] above
/// [`TRAIL_REFUSAL_SHARE`].
pub(super) fn plane_is_trailed(judged: usize, share: f64) -> bool {
    judged >= TRAIL_VERDICT_MIN_SEEDS && share > TRAIL_REFUSAL_SHARE
}

/// Full width at tenth maximum of a Moffat profile, `2α√(10^{1/β} − 1)`.
pub fn fwtm_from_alpha(alpha: f64, beta: f64) -> f64 {
    2.0 * alpha * (10f64.powf(1.0 / beta) - 1.0).sqrt()
}

/// The width-start box's radius per field FWHM (PA-12): the box of radius `⌈2·F_field⌉` px in which the width start's
/// readability gate and moments are read; a seed whose box leaves the image is refused `OutsideImage` (PA-18).
const WIDTH_START_BOX_PER_FIELD: f64 = 2.0;
/// The width start reads the star's own half-maximum width only when its peak stands at least this many σ_rim above the
/// width-start box's rim; a fainter star starts from the field width (Task 6b round 1, ruling R1).
pub(super) const WIDTH_START_MIN_PEAK_SNR: f64 = 10.0;
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
/// A region with fewer present pixels than this is refused `TooFewPixels`.
const MIN_REGION_PIXELS: usize = 10;
/// The donut test (Task 6b): a star whose 3×3 core mean is below this fraction of its 1-px ring's mean is refused
/// `Donut`.
const DONUT_CORE_TO_RING: f64 = 0.8;
/// The donut ring's radius floor, px: the ring sits at `max(1.5, √(σ_major·σ_minor))` of the width-start box's moments
/// about the local centre (round 3, ruling R8; the fold check's floor — unfloored, a real star of FWHM ≤ 2.0 would be
/// refused).
const DONUT_RING_MIN_RADIUS_PX: f64 = 1.5;
/// The donut test's local centre (round 3, ruling R7): an iterated centroid of `max(v − b0, 0)` in a window of radius
/// `max(1.5, F_start)` about the current centre, from the seed, at most 3 iterations, stopping on a move under 0.1 px.
const LOCAL_CENTRE_MIN_WINDOW_PX: f64 = 1.5;
const LOCAL_CENTRE_ITERATIONS: usize = 3;
const LOCAL_CENTRE_MIN_MOVE_PX: f64 = 0.1;
/// An accepted fit whose narrower FWHM is below this many px is a hot pixel, not a star (Task 6b,
/// `NarrowerThanPixel`).
const NARROWEST_FWHM_PX: f64 = 0.8;

/// A stamp's background-subtracted second moments (weights `max(v − b0, 0)`) about their own centroid: the axes' σ and
/// the major axis's angle `½·atan2(2μxy, μxx − μyy)`.
#[derive(Debug, Clone, Copy)]
struct Moments {
    sigma_major: f64,
    sigma_minor: f64,
    theta: f64,
}

/// Orientation and axis ratio of a stamp from its background-subtracted
/// second moments ([`Moments`]). `None` when the stamp has no positive signal or
/// degenerate moments.
#[cfg_attr(not(test), allow(dead_code))]
fn moment_ellipse(px: &[PixelSample], b0: f64) -> Option<Moments> {
    moment_ellipse_where(px, b0, |_| true)
}

/// [`moment_ellipse`] over the pixels `keep` admits, in the slice's order.
fn moment_ellipse_where(
    px: &[PixelSample],
    b0: f64,
    keep: impl Fn(&PixelSample) -> bool,
) -> Option<Moments> {
    let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
    for p in px.iter().filter(|p| keep(p)) {
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
    for p in px.iter().filter(|p| keep(p)) {
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
/// the width-start box's first, then the region's). [`fit_all_outcomes`] hands one
/// instance to each rayon worker via `map_init` (a thread-local, one instance
/// per worker for the whole call, not per seed). Every buffer is
/// `clear()`ed at the point it starts being rebuilt for a new seed, so a
/// worker's retained capacity is invisible to the numbers: the content is
/// always exactly what a fresh, empty `Vec` would have accumulated.
///
/// Crate-visible from Task 8 because [`super::photometry::saturation_verdict`] takes it (its fields stay the
/// module's): the verdict pass hands each worker's instance to the verdict, which builds its fit's pixels in `verdict`.
#[derive(Default)]
pub(crate) struct FitScratch {
    pub(super) region: Vec<f32>,
    pub(super) rim: Vec<f32>,
    pub(super) px: Vec<PixelSample>,
    /// The saturation verdict fit's pixels: the region's below the level, conditioned (F-3).
    pub(super) verdict: Vec<PixelSample>,
}

/// What [`fit_one`] records about one seed beside its result: [`FitDiagnostics`] without the seed index and the
/// final outcome, which only the duplicate drop settles.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct FitTrace {
    region_radius: u16,
    background_start: f64,
    width_start: f64,
    growth_steps: u8,
    pixels_used: u32,
    converged: bool,
    /// The width start's readability gate passed (false when the seed was refused before it ran).
    readable: bool,
}

impl FitTrace {
    /// Until the region grows, its radius reads the width-start box's; the start width and background read NaN.
    fn before_region(field_width: f64) -> FitTrace {
        let rb = width_start_box_radius(field_width);
        FitTrace {
            region_radius: rb.min(u16::MAX as i64) as u16,
            background_start: f64::NAN,
            width_start: f64::NAN,
            growth_steps: 0,
            pixels_used: 0,
            converged: false,
            readable: false,
        }
    }

    fn record(
        self,
        seed: usize,
        outcome: Outcome,
        verdict: Option<SaturationVerdict>,
    ) -> FitDiagnostics {
        FitDiagnostics {
            seed,
            outcome,
            region_radius: self.region_radius,
            background_start: self.background_start,
            width_start: self.width_start,
            growth_steps: self.growth_steps,
            pixels_used: self.pixels_used,
            // The bounded verdict fit's (F-4, Task 9): the fixed-β fit at β reports none.
            iterations: verdict.filter(|v| v.fit).map(|v| v.iterations),
            converged: self.converged,
            verdict,
            // A sample star's is set once the frame β's sample is fitted (`fit_after_passes`).
            sample_beta: None,
        }
    }
}

/// The star's width start: its FWHM `F_start` (its own half-maximum width where readable, else the field width —
/// Task 6b round 1, ruling R1) and the width-start box's rim median, the level the donut test's ring radius reads the
/// box's moments above (round 3, ruling R8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct WidthStart {
    fwhm: f64,
    /// The median of the box's outer 1-px rim; NaN when the rim holds no present pixel.
    level: f64,
    /// The A-g readability gate passed: the star's peak stands at least 10 σ_rim above the box's rim.
    readable: bool,
}

/// The radius of a field width's width-start box, `⌈2·F_field⌉` px (PA-12),
/// at least 1.
fn width_start_box_radius(field_width: f64) -> i64 {
    ((WIDTH_START_BOX_PER_FIELD * field_width).ceil() as i64).max(1)
}

/// The width start (PA-12 as amended by Task 6b round 1, ruling R1), in the box of radius `rb` about `(cx, cy)`
/// (the caller has checked that it lies inside the image): `F_start` is the star's own half-maximum width
/// ([`own_halfmax_width`], PA-17's estimator) when the star is readable — its peak stands at least 10 σ above the box's
/// rim, `(max − level)/σ_rim ≥ 10` with `level` the median and `σ_rim` = 1.4826·MAD of the box's outer 1-px rim (a rim
/// with no spread reads any peak above it as readable) — and the field width otherwise, or when the half-maximum width
/// does not read; clamped to [0.6, 40] px. The plan's second moments read every star wide (1.3× on bright stars, 2–3×
/// on faint ones: the clipped noise of the box); the half-maximum width, judged on truth before any bar was read, reads
/// 0.92–1.09× in every Fisher bin of `sharp/m4/f{2.6,8,16}` and `sharp/gauss/f8` and the two populations of a mixed
/// field at their own widths. The start shape is not read here (round 3, ruling R9: [`start_shape`]).
#[allow(clippy::too_many_arguments)]
fn width_start(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    cx: i64,
    cy: i64,
    rb: i64,
    field_width: f64,
    scratch: &mut FitScratch,
) -> WidthStart {
    let mut start = WidthStart {
        fwhm: field_width.clamp(WIDTH_START_MIN_PX, WIDTH_START_MAX_PX),
        level: f64::NAN,
        readable: false,
    };
    let Some(level) = rim_median(data, mask, w, cx, cy, rb, &mut scratch.rim) else {
        return start;
    };
    start.level = level;
    // `rim_median` left the rim's present pixels in `scratch.rim` (reordered, the same multiset).
    let sigma_rim = (MAD_TO_SIGMA * mad_about(&scratch.rim, level as f32)) as f64;
    // The readability gate reads the width-start box maximum, kept by measurement (t06b-m2-check, 2026-10-08): the
    // star's-own-peak alternative lost 547 fits and read same-star fwhm worse.
    let mut peak = f64::NEG_INFINITY;
    for y in cy - rb..=cy + rb {
        for x in cx - rb..=cx + rb {
            let i = y as usize * w + x as usize;
            if present(data, mask, i) {
                peak = peak.max(data[i] as f64);
            }
        }
    }
    let readable = if sigma_rim > 0.0 {
        (peak - level) / sigma_rim >= WIDTH_START_MIN_PEAK_SNR
    } else {
        peak > level
    };
    start.readable = readable;
    if readable {
        if let Some(own) = own_halfmax_width(data, w, h, seed.x, seed.y, mask) {
            start.fwhm = own.clamp(WIDTH_START_MIN_PX, WIDTH_START_MAX_PX);
        }
    }
    start
}

/// The fit's start shape (round 3, ruling R9 — the brief's "moment axis ratio as today"): the second moments of the
/// region's pixels inside PA-12's starting square, `max(|x|, |y|) ≤ r_start` in the stamp-centred coordinates, above 0
/// (the caller has normalised the values to `(v − b0)/a0`); returns `(√clamp(σ_major/σ_minor, 1, 4), θ)`. t06a read
/// them from its region stamp, about 1.5 FWHM; round 0 had moved them to the 2·F_field box, whose clipped noise gave
/// start angles 8.6° → 21.8° off at Fisher 20–40 and a tail of fits settling in a wrong-θ minimum. `None` when the square
/// has no positive signal or degenerate moments.
fn start_shape(px: &[PixelSample], r_start: i64) -> Option<(f64, f64)> {
    let r = r_start as f64;
    moment_ellipse_where(px, 0.0, |p| p.x.abs().max(p.y.abs()) <= r).map(|m| {
        (
            (m.sigma_major / m.sigma_minor).clamp(1.0, 4.0).sqrt(),
            m.theta,
        )
    })
}

/// The donut test's local centre (round 3, ruling R7): from `seed`, the centroid of `max(v − b0, 0)` over the present
/// pixels inside the image whose centres lie within `window` of the current centre, at most 3 times, stopping on a move
/// under 0.1 px; a window with no positive weight keeps the current centre. The window covers the object (`window =
/// max(1.5, F_start)`), so a Peak seed on a donut's annulus walks to its hole, and a bright neighbour outside it cannot
/// pull the centre off the star.
fn local_centre(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: (f64, f64),
    window: f64,
    b0: f64,
) -> (f64, f64) {
    let (mut gx, mut gy) = seed;
    let reach = window.ceil() as i64 + 1;
    for _ in 0..LOCAL_CENTRE_ITERATIONS {
        let (px, py) = (gx.round() as i64, gy.round() as i64);
        let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
        for y in py - reach..=py + reach {
            for x in px - reach..=px + reach {
                if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                    continue;
                }
                if (x as f64 - gx).hypot(y as f64 - gy) > window {
                    continue;
                }
                let i = y as usize * w + x as usize;
                if present(data, mask, i) {
                    let wt = (data[i] as f64 - b0).max(0.0);
                    sw += wt;
                    sx += wt * x as f64;
                    sy += wt * y as f64;
                }
            }
        }
        if !(sw > 0.0) {
            break;
        }
        let (nx, ny) = (sx / sw, sy / sw);
        let moved = (nx - gx).hypot(ny - gy);
        gx = nx;
        gy = ny;
        if moved < LOCAL_CENTRE_MIN_MOVE_PX {
            break;
        }
    }
    (gx, gy)
}

/// The donut ring's radius (round 3, ruling R8): `max(1.5, √(σ_major·σ_minor))` of the second moments of the width-start
/// box's present pixels (radius `rb` about `(cx, cy)`, weights `max(v − level, 0)`) taken about `centre` — the geometry
/// the brief's fold check computed (0.02 / 0.29 on the two donut families). Degenerate moments (no positive weight, no
/// rim level) fall back to `max(1.5, F_start/2.3548)`.
#[allow(clippy::too_many_arguments)]
fn ring_radius(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    cx: i64,
    cy: i64,
    rb: i64,
    level: f64,
    centre: (f64, f64),
    f_start: f64,
) -> f64 {
    let fallback = DONUT_RING_MIN_RADIUS_PX.max(f_start / GAUSSIAN_FWHM_PER_SIGMA);
    if !level.is_finite() {
        return fallback;
    }
    let (gx, gy) = centre;
    let (mut sw, mut mxx, mut myy, mut mxy) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for y in cy - rb..=cy + rb {
        for x in cx - rb..=cx + rb {
            let i = y as usize * w + x as usize;
            if !present(data, mask, i) {
                continue;
            }
            let wt = (data[i] as f64 - level).max(0.0);
            let (dx, dy) = (x as f64 - gx, y as f64 - gy);
            sw += wt;
            mxx += wt * dx * dx;
            myy += wt * dy * dy;
            mxy += wt * dx * dy;
        }
    }
    if !(sw > 0.0) {
        return fallback;
    }
    let (mxx, myy, mxy) = (mxx / sw, myy / sw, mxy / sw);
    let disc = ((mxx - myy).powi(2) + 4.0 * mxy * mxy).sqrt();
    let (l1, l2) = (0.5 * (mxx + myy + disc), 0.5 * (mxx + myy - disc));
    if !(l2 > 0.0) || !l1.is_finite() {
        return fallback;
    }
    DONUT_RING_MIN_RADIUS_PX.max((l1 * l2).sqrt().sqrt())
}

/// The donut test's two means, both minus `b0`: the 3×3 core about the pixel nearest `centre`, and the 1-px ring
/// `R − ½ < d ≤ R + ½` about `centre` itself (round 3, ruling R8: half-open below, so at the radius floor 1.5 the cross
/// neighbours at d = 1, which the core holds, are not in the ring when the centre sits exactly on a pixel centre; an
/// offset of even 1e-12 px puts one of them at d > 1, inside the ring, so a knife edge remains on that one side — the
/// verdict is unharmed, every core/ring ratio sits well clear of 0.8). Only present pixels
/// inside the image count; `None` when the core or the ring holds none.
fn core_and_ring(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    centre: (f64, f64),
    radius: f64,
    b0: f64,
) -> Option<(f64, f64)> {
    let (gx, gy) = centre;
    let (px, py) = (gx.round() as i64, gy.round() as i64);
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
    let core = mean_minus_b0(
        &mut (-1i64..=1).flat_map(|dy| (-1i64..=1).map(move |dx| (px + dx, py + dy))),
    )?;
    let reach = (radius + 0.5).ceil() as i64;
    let on_ring = |&(x, y): &(i64, i64)| {
        let d = (x as f64 - gx).hypot(y as f64 - gy);
        d > radius - 0.5 && d <= radius + 0.5
    };
    let ring = mean_minus_b0(
        &mut (-reach..=reach)
            .flat_map(|dy| (-reach..=reach).map(move |dx| (px + dx, py + dy)))
            .filter(on_ring),
    )?;
    Some((core, ring))
}

/// The donut test (Task 6b; rounds 2–3, rulings R7 and R8): the core about the local centre ([`local_centre`], window
/// `max(1.5, F_start)` from the seed) against the ring at [`ring_radius`] about it ([`core_and_ring`]); a donut when
/// `core < 0.8·ring`. No verdict (`false`) when the core or the ring holds no present pixel. Judged on truth before any
/// bar: round 1's false donuts on `sharp/m4/f8.0` not refused, both donut families refused noise-free whether seeded at
/// the centre or on the annulus (worst core/ring 0.397), every real star of FWHM 1.2–16 at ≥ 1.119.
#[allow(clippy::too_many_arguments)]
fn is_donut(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    cx: i64,
    cy: i64,
    rb: i64,
    start: &WidthStart,
    b0: f64,
) -> bool {
    let window = LOCAL_CENTRE_MIN_WINDOW_PX.max(start.fwhm);
    let centre = local_centre(data, mask, w, h, (seed.x, seed.y), window, b0);
    let radius = ring_radius(data, mask, w, cx, cy, rb, start.level, centre, start.fwhm);
    core_and_ring(data, mask, w, h, centre, radius, b0)
        .is_some_and(|(core, ring)| core < DONUT_CORE_TO_RING * ring)
}

/// Fit one seed with a fixed β (PA-12, spec §3 F-1 and F-7; Plan A Task 6b).
///
/// 1. **Admission:** the width-start box of radius `⌈2·F_field⌉` about the seed's pixel must lie inside the image,
///    else `OutsideImage` (PA-18: a star broader than the field width can still be admitted and meet the border as
///    `RegionTooSmall`).
/// 2. **The width start** `F_start`: the star's own half-maximum width where readable, else the field width
///    ([`width_start`]; round 1, ruling R1).
/// 3. **The region** grows from `max(3, ⌈1.5·F_start⌉)` to at most `clamp(⌈6·F_start⌉, 6, 96)` px, never past the
///    border, while its median stands above its rim's ([`grow_region`]); the background start `b0` is the final
///    rim's median, and fewer than 10 present pixels are `TooFewPixels`.
/// 4. **The donut test** ([`is_donut`]: a local centre, the ring radius from the box moments about it) refuses an
///    annular star `Donut` before any fit.
/// 5. **The fit** starts at `b0`, `a0 = max − b0`, σ `F_start/2.3548` split by the axis ratio of the starting square's
///    moments (`√ratio` clamped to [1, 2]) at their angle ([`start_shape`]); then the acceptance rules ([`accept`],
///    `NarrowerThanPixel` among them). A fit the fitter refuses is `FitFailed` — a solve that never left its start
///    among them (fix round A4: rustafits refuses it unless the start itself converged; before, it was returned at
///    its start as a fit — a censored star whose guard-refused first steps spent the reject budget, a small star whose
///    first five steps all raised the cost).
///
/// `pass` is the seed's verdict-pass record when the caller ran the verdict pass (F-3, Task 8; `None` otherwise): its
/// steps 1–4 are taken from it, not run again (a seed refused there is refused with the pass's reason and trace; the
/// fix wave's cost item, number-neutral), and its verdict is the seed's. `MostlySaturated` refuses the seed after
/// step 4. A flagged star's level — own flag or cross-plane — is the fit's exclusion (Q6 as revised 2026-10-09): the
/// region's pixels at or above it are left out of the fit (its start shape included) and the iteration limit is ×3
/// when any is; its level is `norm` when its own plateau test failed. An unflagged star is fitted with no exclusion,
/// exactly as with no verdict (R3: Q6 as first written censored 218 truth-unsaturated plateau stars at β). An accepted
/// fit copies the verdict's flag into [`StarFlags::SATURATED`].
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
    pass: Option<&SeedPass>,
    scratch: &mut FitScratch,
) -> (Result<StarFit, RefusalReason>, FitTrace) {
    let (reg, mut trace, verdict, peak, top) = match pass {
        // The verdict pass's record (Task 8 fix wave): steps 1–4 are not run again, only the region's pixels are
        // re-gathered; its refusal and its trace are the ones recomputing them would give.
        Some(pass) => {
            let reg = match pass.region {
                Ok(reg) => reg,
                Err(reason) => return (Err(reason), pass.trace),
            };
            let (peak, top) = gather_region(data, mask, w, reg.cx, reg.cy, reg.r, &mut scratch.px);
            (reg, pass.trace, pass.verdict.as_ref(), peak, top)
        }
        None => {
            let mut trace = FitTrace::before_region(field_width);
            match region_steps(data, mask, w, h, seed, field_width, p, scratch, &mut trace) {
                Ok((reg, peak, top)) => (reg, trace, None, peak, top),
                Err(reason) => return (Err(reason), trace),
            }
        }
    };
    let SeedRegion {
        cx,
        cy,
        start,
        r_start,
        r,
        b0,
        ..
    } = reg;
    // F-3 (Task 8, Q6 as revised 2026-10-09): a flagged star's level (own flag or cross-plane) is the fit's exclusion
    // and its flag the fit's; an unflagged star is fitted exactly as with no verdict.
    let (level, saturated) = match verdict {
        Some(Err(reason)) => return (Err(*reason), trace),
        Some(Ok(v)) if v.saturated => (Some(v.level), true),
        Some(Ok(_)) | None => (None, false),
    };
    let mut max_iter = p.max_iter;
    if let Some(level) = level {
        let before = scratch.px.len();
        scratch.px.retain(|s| s.value < level);
        if scratch.px.len() < before {
            max_iter = EXCLUDED_ITERATION_FACTOR * p.max_iter;
        }
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
    // Size from the star's own width start, orientation and axis ratio from the
    // moments of the region's starting square (round 3, ruling R9): a circular
    // θ = 0 start leaves an elongated star in an axis-aligned local minimum.
    let sigma = start.fwhm / GAUSSIAN_FWHM_PER_SIGMA;
    let (q, th0) = start_shape(&scratch.px, r_start).unwrap_or((1.0, 0.0));
    let (sx0, sy0) = (sigma * q, sigma / q);
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
        max_iter,
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
    // F-5 (Task 10): the model's flux in its FWTM ellipse; the plane's aperture correction scales it after the
    // duplicate drop ([`fit_at_width`]). `area` keeps its meaning: the analytic area of the `k·FWTM/2` ellipse.
    let (a, b) = (0.5 * p.growth * f.fwtm_x, 0.5 * p.growth * f.fwtm_y);
    f.area = std::f64::consts::PI * a * b;
    f.signal = super::photometry::model_flux_in_fwtm(&f);
    // At least 10 present pixels were gathered, so all five slots are filled.
    f.peak5 = top.iter().sum::<f64>() / top.len() as f64;
    if saturated {
        f.flags |= StarFlags::SATURATED;
    }
    (Ok(f), trace)
}

/// A seed's region as [`fit_one`] and the verdict pass ([`seed_pass`]) both read it (steps 1–3): the width-start
/// box radius `rb`, the width start, the starting and final radii and the rim median `b0` (PA-12).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SeedRegion {
    cx: i64,
    cy: i64,
    rb: i64,
    start: WidthStart,
    r_start: i64,
    r: i64,
    b0: f64,
}

/// Steps 1–3 of [`fit_one`] (moved out unchanged for the verdict pass, Task 8): the PA-18 box admission (`None` =
/// `OutsideImage`), the width start and the PA-12 region with its rim median, recorded on `trace`.
#[allow(clippy::too_many_arguments)]
fn seed_region(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    field_width: f64,
    p: &FitParams,
    scratch: &mut FitScratch,
    trace: &mut FitTrace,
) -> Option<SeedRegion> {
    let rb = width_start_box_radius(field_width);
    let (cx, cy) = (seed.x.round() as i64, seed.y.round() as i64);
    if cx - rb < 0 || cy - rb < 0 || cx + rb >= w as i64 || cy + rb >= h as i64 {
        return None;
    }
    let start = width_start(data, mask, w, h, seed, cx, cy, rb, field_width, scratch);
    trace.width_start = start.fwhm;
    trace.readable = start.readable;
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
    Some(SeedRegion {
        cx,
        cy,
        rb,
        start,
        r_start,
        r,
        b0,
    })
}

/// The region's present pixels into `px` (cleared first), in stamp-centred coordinates (exact integers) and plane
/// values, row by row; returns the region's maximum and its five largest values, ascending (L-12's `peak5`).
fn gather_region(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    cx: i64,
    cy: i64,
    r: i64,
    px: &mut Vec<PixelSample>,
) -> (f64, [f64; 5]) {
    let cap = ((2 * r + 1) * (2 * r + 1)) as usize;
    px.clear();
    px.reserve(cap);
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
            // normalised by the caller once the start background and
            // amplitude are known.
            px.push(PixelSample {
                x: (x - cx) as f64,
                y: (y - cy) as f64,
                value: v64,
            });
        }
    }
    (peak, top)
}

/// One seed's verdict-pass entry (F-3): `None` when the seed is refused before its region (`OutsideImage`,
/// `TooFewPixels`) or as a `Donut` — [`fit_one`] refuses it again at the same step — else the verdict or
/// `MostlySaturated`.
pub(crate) type SeedVerdict = Option<Result<SaturationVerdict, RefusalReason>>;

/// Steps 1–4 of [`fit_one`] (admission, width start, region, donut test; the verdict pass runs them once per seed and
/// keeps the outcome, [`SeedPass`]): the region ([`seed_region`]), its present pixels gathered into `scratch.px` and
/// counted on `trace.pixels_used`, then `TooFewPixels` and the donut test ([`is_donut`]). Returns the region with its
/// maximum and its five largest values ([`gather_region`]).
#[allow(clippy::too_many_arguments)]
fn region_steps(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    field_width: f64,
    p: &FitParams,
    scratch: &mut FitScratch,
    trace: &mut FitTrace,
) -> Result<(SeedRegion, f64, [f64; 5]), RefusalReason> {
    let Some(reg) = seed_region(data, mask, w, h, seed, field_width, p, scratch, trace) else {
        return Err(RefusalReason::OutsideImage);
    };
    let (peak, top) = gather_region(data, mask, w, reg.cx, reg.cy, reg.r, &mut scratch.px);
    // The region's present pixels: at most (2r+1)² ≤ 193², so a u32 holds it.
    trace.pixels_used = scratch.px.len() as u32;
    if scratch.px.len() < MIN_REGION_PIXELS {
        return Err(RefusalReason::TooFewPixels);
    }
    if is_donut(
        data, mask, w, h, seed, reg.cx, reg.cy, reg.rb, &reg.start, reg.b0,
    ) {
        return Err(RefusalReason::Donut);
    }
    Ok((reg, peak, top))
}

/// One seed's record from the verdict pass (F-3; Task 8 fix wave, review "Cost"): the outcome of [`fit_one`]'s steps
/// 1–4 with the trace they wrote, and the seed's verdict. Steps 1–4 are pure functions of (data, mask, seed, field
/// width, params), so the frame's fits and every β-candidate pass on the same field width reuse the record instead of
/// recomputing the width start, the region's growth and the donut test; the numbers cannot move. They re-gather the
/// region's pixels: keeping them would hold (2r+1)² samples per seed against this record's ≈ 0.2 KB.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct SeedPass {
    /// The region, or the refusal at step 1, 3 or 4 (`OutsideImage`, `TooFewPixels`, `Donut`).
    region: Result<SeedRegion, RefusalReason>,
    /// What steps 1–4 recorded, `pixels_used` included.
    trace: FitTrace,
    /// `None` exactly when `region` is a refusal.
    pub(crate) verdict: SeedVerdict,
}

#[cfg(test)]
impl SeedPass {
    /// Hand-built records for the encoding pin (Plan A Task 11c): every value is an exact binary fraction, so no
    /// platform's libm enters, and NaN / negative infinity ride where the real records hold them. `0` is a fitted seed
    /// (a region, a trace, a verdict), `1` a seed refused at the donut test (no verdict), `2` one refused as mostly
    /// saturated (a verdict that is an `Err`).
    pub(crate) fn for_pin(i: usize) -> SeedPass {
        match i {
            0 => SeedPass {
                region: Ok(SeedRegion {
                    cx: 10,
                    cy: 20,
                    rb: 6,
                    start: WidthStart {
                        fwhm: 3.5,
                        level: 0.125,
                        readable: true,
                    },
                    r_start: 9,
                    r: 12,
                    b0: -0.0625,
                }),
                trace: FitTrace {
                    region_radius: 12,
                    background_start: 0.5,
                    width_start: f64::NAN,
                    growth_steps: 2,
                    pixels_used: 441,
                    converged: true,
                    readable: true,
                },
                verdict: Some(Ok(SaturationVerdict {
                    plateau: false,
                    level: 0.75,
                    excluded: 3,
                    amplitude: 0.5,
                    beta: 2.5,
                    fwhm: 3.25,
                    saturated: true,
                    by_amplitude: false,
                    by_plateau_clip: false,
                    by_near_clip: true,
                    pixel0: f64::NAN,
                    top3x3: f64::NEG_INFINITY,
                    fit: true,
                    iterations: 17,
                    cross: true,
                    centred: false,
                })),
            },
            1 => SeedPass {
                region: Err(RefusalReason::Donut),
                trace: FitTrace::before_region(3.0),
                verdict: None,
            },
            _ => SeedPass {
                region: Err(RefusalReason::TooFewPixels),
                trace: FitTrace::before_region(4.0),
                verdict: Some(Err(RefusalReason::MostlySaturated)),
            },
        }
    }

    /// A record carrying only `verdict` (the cross-plane rule reads nothing else of it).
    pub(crate) fn for_test(verdict: SeedVerdict) -> SeedPass {
        SeedPass {
            region: Err(RefusalReason::OutsideImage),
            trace: FitTrace::before_region(3.0),
            verdict,
        }
    }
}

/// One seed's saturation verdict (spec §2.1, PA-23): [`fit_one`]'s steps 1–4 ([`region_steps`]) on the same field
/// width, then [`saturation_verdict`] on the unnormalised region in stamp coordinates, from the region's rim median
/// `b0` and the star's own `F_start`; kept with the steps' outcome ([`SeedPass`]).
#[allow(clippy::too_many_arguments)]
fn seed_pass(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seed: &Seed,
    field_width: f64,
    plane: &SaturationPlane,
    p: &FitParams,
    scratch: &mut FitScratch,
) -> SeedPass {
    let mut trace = FitTrace::before_region(field_width);
    let reg = match region_steps(data, mask, w, h, seed, field_width, p, scratch, &mut trace) {
        Ok((reg, _, _)) => reg,
        Err(reason) => {
            return SeedPass {
                region: Err(reason),
                trace,
                verdict: None,
            }
        }
    };
    let region = std::mem::take(&mut scratch.px);
    let verdict = saturation_verdict(
        &region,
        seed.x - reg.cx as f64,
        seed.y - reg.cy as f64,
        reg.b0,
        reg.start.fwhm,
        reg.r as f64,
        plane,
        p,
        scratch,
    );
    scratch.px = region;
    SeedPass {
        region: Ok(reg),
        trace,
        verdict: Some(verdict),
    }
}

/// Every seed's verdict-pass record, in seed order (F-3). Runs on `pool` when given one, one [`FitScratch`] per worker
/// as [`fit_all_outcomes`] does. On [`SaturationPlane::UNTESTED`] each record's verdict is the bare bounded free-β
/// fit (the verdict path; a caller that ran no verdict takes its frame β from the pixel-integrated re-fit, F-4 E5,
/// [`integrated_beta_on_region`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn seed_passes(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
    field_width: f64,
    plane: &SaturationPlane,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<SeedPass> {
    let body = || {
        seeds
            .par_iter()
            .map_init(FitScratch::default, |scratch, s| {
                seed_pass(data, mask, w, h, s, field_width, plane, p, scratch)
            })
            .collect()
    };
    super::install_in_span(pool, body)
}

/// The frame β sample's pixel-integrated fits on the measure path (F-4 as revised by the owner's ruling E5,
/// 2026-10-09; ruling R-OSP-6): for each seed index in `sample` (the seeds [`super::beta::sample_seeds`] chose from
/// `passes`), the verdict fit's own problem re-fitted with the integrated model
/// ([`super::photometry::integrated_beta`]) — the region its record holds, re-gathered (steps 1–4 are pure, so the
/// pixels are the verdict's), below the verdict's level. In `sample` order; `None` where the fit failed (or where the
/// record holds no region or no verdict, which a sample seed never lacks). Runs on `pool` when given one.
#[allow(clippy::too_many_arguments)]
pub(super) fn sample_fits(
    data: &[f32],
    w: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
    p: &FitParams,
    passes: &[SeedPass],
    sample: &[usize],
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Option<f64>> {
    let body = || {
        sample
            .par_iter()
            .map_init(FitScratch::default, |scratch, &i| {
                let (Ok(reg), Some(Ok(v))) = (&passes[i].region, passes[i].verdict) else {
                    return None;
                };
                gather_region(data, mask, w, reg.cx, reg.cy, reg.r, &mut scratch.px);
                integrated_beta_on_region(&seeds[i], reg, v.level, p, scratch)
            })
            .collect()
    };
    super::install_in_span(pool, body)
}

/// Q7 under E5 (a caller that ran no verdict, [`super::beta::frame_beta`]): for each of `seeds` (the caller's
/// brightest), [`fit_one`]'s steps 1–4 on `field_width` ([`region_steps`]) and the pixel-integrated fit of the whole
/// region — no saturation test, no exclusion (the level of [`SaturationPlane::UNTESTED`]). In seed order; `None` where
/// the seed was refused before its fit or the fit failed. Runs on `pool` when given one.
#[allow(clippy::too_many_arguments)]
pub(super) fn own_sample_fits(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    mask: Option<&[bool]>,
    field_width: f64,
    p: &FitParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Option<f64>> {
    let body = || {
        seeds
            .par_iter()
            .map_init(FitScratch::default, |scratch, s| {
                let mut trace = FitTrace::before_region(field_width);
                let (reg, _, _) =
                    region_steps(data, mask, w, h, s, field_width, p, scratch, &mut trace).ok()?;
                integrated_beta_on_region(s, &reg, SaturationPlane::UNTESTED.norm, p, scratch)
            })
            .collect()
    };
    super::install_in_span(pool, body)
}

/// [`integrated_beta`] on the region `scratch.px` holds (gathered for `reg`), in stamp coordinates from `reg`'s rim
/// median and the star's own `F_start`, below `level`.
fn integrated_beta_on_region(
    seed: &Seed,
    reg: &SeedRegion,
    level: f64,
    p: &FitParams,
    scratch: &mut FitScratch,
) -> Option<f64> {
    let region = std::mem::take(&mut scratch.px);
    let beta = integrated_beta(
        &region,
        seed.x - reg.cx as f64,
        seed.y - reg.cy as f64,
        reg.b0,
        reg.start.fwhm,
        level,
        p,
        scratch,
    );
    scratch.px = region;
    beta
}

/// Is the offset `(dx, dy)` from a fit's centre inside its half-maximum contour (Task 10b ruling A4-2)? The contour is
/// the ellipse of semi-axes `(semi_x, semi_y)` (half the FWHMs) turned by `theta` — `(u/α_x)² + (v/α_y)² ≤ 2^(1/β) − 1`
/// in the fit's axes. One definition: the acceptance test, the verdict fit's `centred` and LN's seed link all read it.
pub(crate) fn inside_half_max(dx: f64, dy: f64, semi_x: f64, semi_y: f64, theta: f64) -> bool {
    let (c, s) = (theta.cos(), theta.sin());
    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
    (u / semi_x).powi(2) + (v / semi_y).powi(2) <= 1.0
}

/// [`accept`]'s centre and region tests (math reference §1.4), shared with the verdict fit's `centred` (Task 10b ruling
/// A4) so the two read one definition: `CentreMoved` when the seed `(seed_x, seed_y)` lies outside the fitted star's own
/// half-maximum contour ([`inside_half_max`]; ruling A4-2: "is this fit the seed's star?" read at the FWHM's own
/// definition — a detection lies on its star's core, so its error is bounded by the core's half-widths, not by a fixed
/// number; at a round FWHM-3 star the contour is 1.5 px); `CentreAtRim` when the fitted centre sits outside the region
/// about `(cx, cy)` of radius `r` shrunk by `inner_margin` per side; `RegionTooSmall` when half the grown major FWTM
/// exceeds `r`. The fitted centre `(x0, y0)`, the seed and the region centre in one frame (image or stamp
/// coordinates); `alpha` the fitted `(α_x, α_y)` at `beta`, turned by `theta`.
pub(super) fn placement(
    (x0, y0): (f64, f64),
    (alpha_x, alpha_y): (f64, f64),
    beta: f64,
    (seed_x, seed_y): (f64, f64),
    (cx, cy, r): (f64, f64, f64),
    theta: f64,
    p: &FitParams,
) -> Result<(), RefusalReason> {
    let k = (2f64.powf(1.0 / beta) - 1.0).sqrt();
    if !inside_half_max(seed_x - x0, seed_y - y0, alpha_x * k, alpha_y * k, theta) {
        return Err(RefusalReason::CentreMoved);
    }
    // The fitted centre must sit inside the sampling region shrunk by
    // `inner_margin` per side (math reference §1.4): a star whose region
    // stayed narrow is one whose light the region barely contains.
    let inner = (1.0 - 2.0 * p.inner_margin) * r;
    if (x0 - cx).abs() > inner || (y0 - cy).abs() > inner {
        return Err(RefusalReason::CentreAtRim);
    }
    let fwtm = fwtm_from_alpha(alpha_x, beta).max(fwtm_from_alpha(alpha_y, beta));
    if 0.5 * p.growth * fwtm > r {
        return Err(RefusalReason::RegionTooSmall);
    }
    Ok(())
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
    placement(
        (m.x0, m.y0),
        (m.alpha_x, m.alpha_y),
        m.beta,
        (seed.x, seed.y),
        (cx, cy, r),
        m.theta,
        p,
    )?;
    let (fwtm_x, fwtm_y) = (
        fwtm_from_alpha(m.alpha_x, m.beta),
        fwtm_from_alpha(m.alpha_y, m.beta),
    );
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
        sky_ring: f64::NAN,
        sky_ring_sigma: f64::NAN,
        sky_ring_n: 0,
        snr: f64::NAN,
        hfr: f64::NAN,
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
/// Every seed's result and trace, in seed order (M-4): the parallel chain is
/// the one `fit_all` ran (retired with the discrete `Auto` search, Task 9), collected per seed instead of filtered in
/// flight — rayon's indexed `collect` keeps the order, so the accepted fits
/// read off it sequentially are exactly the list the filtered chain gave.
/// `passes` (F-3): one per seed, in seed order, when the caller ran the verdict pass — each seed is fitted from its
/// own record, its region outcome and its verdict ([`fit_one`]); `None` fits every seed as before.
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
    passes: Option<&[SeedPass]>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<(Result<StarFit, RefusalReason>, FitTrace)> {
    debug_assert!(passes.map_or(true, |v| v.len() == seeds.len()));
    let body = || {
        seeds
            .par_iter()
            .enumerate()
            // `map_init` calls `FitScratch::default` once per worker (not
            // per seed) and hands that same instance to every seed the
            // worker processes — `fit_one` clears each buffer itself, so
            // this is only the allocation that disappears, never the data.
            .map_init(FitScratch::default, |scratch, (i, s)| {
                let pass = passes.map(|v| &v[i]);
                fit_one(data, mask, w, h, s, field_width, beta, p, pass, scratch)
            })
            .collect()
    };
    super::install_in_span(pool, body)
}

/// Fit every seed with the chosen model. `Auto` resolves the continuous frame β from its own sample — the bounded
/// free-β fits of the brightest `AUTO_SAMPLE` seeds, no saturation test (F-4, Q7; [`super::beta::frame_beta`]; β = 4
/// when fewer than 8 fits are accepted) — then fits all seeds with it, as [`fit_stars_with_beta`] does once β is
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
    fit_stars_diagnosed(
        data,
        w,
        h,
        seeds,
        model,
        p,
        None,
        None,
        pool,
        false,
        false,
        false,
        Correction::default(),
    )
}

/// What a fit call's aperture correction reads from its caller (F-5, Task 10; one struct from round 4): where `c`
/// comes from (`flux`), the clip screen's range (`saturation`, the measure path's plane inputs; else the plane's own
/// under `full_scale`, [`super::photometry::unverdicted_clip_range`]), the footprint a qualifier's `12F` disc must
/// lie in and (Task 10b) the ring width every fit's B_i is read at. `Default`: measured on the plane, uncapped, the
/// whole plane, the plane's own frame FWHM.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Correction<'a> {
    pub(super) flux: super::FluxCorrection,
    pub(super) saturation: Option<&'a SaturationPlane>,
    pub(super) full_scale: Option<f64>,
    pub(super) footprint: Option<&'a [bool]>,
    /// The ring width every fit's B_i is read at and `c`'s growth radius scales with (Task 10b, ruling B3's arm):
    /// `None` = the plane's own frame FWHM.
    pub(super) ring_fwhm: Option<f64>,
    /// The plane's gain in e- per working ADU (F-6, Task 11): the SNR's shot term; `None` omits it. Never read by a fit.
    pub(super) gain: Option<f64>,
    /// The plane's pixel-noise correlation and shot spreading the SNR's aperture noise reads (F-6, Task 11); the
    /// default is white (LN's and every refit's: `kappa` 1).
    pub(super) noise: super::photometry::ApertureNoise,
    /// The seeds the ring sky masks and `c`'s isolation reads when the fitted seeds are a subset of the detected ones
    /// (the budget's candidates, Task 12, R12-5); `None` = the fitted seeds themselves.
    pub(super) neighbours: Option<Neighbours<'a>>,
    /// Population (a) among the fit call's seeds (Plan A Task 13b, [`super::population`]): the ring width reads its
    /// fits alone and `c` is the median of the 64 brightest qualifying fits of (a). `None` = every fit counts, as before.
    pub(super) population: Option<super::population::PopulationView<'a>>,
    /// Every OTHER plane's seeds on an image of more than one plane (Plan B B4, PB-12'): a `c` candidate with none of
    /// them within `max(`[`super::photometry::POINT_SOURCE_MATCH_PX`]`, 0.5 F)` px of its fitted centre (R-PB4-2) is screened out. `None` = no screen
    /// (one plane, LN's refits, every caller but the stacking halves).
    pub(super) siblings: Option<&'a [Seed]>,
    /// The plane's sub-step timers (Plan A Task 13b, 13-P): the ring sky, `c` and the SNR / HFR pass report into it.
    /// Timers only; `None` for every caller but the measure path.
    pub(super) timers: Option<&'a super::SubSteps>,
}

/// Every detected seed and where the fitted ones sit among them (Task 12, R12-5): `of_fit[k]` is the index into `seeds`
/// of the fit call's seed `k`. The ring sky leaves out the `3F` disc of every one of `seeds`, and `c`'s isolation test
/// reads every one, exactly as when all of them are fitted.
#[derive(Debug, Clone, Copy)]
pub(super) struct Neighbours<'a> {
    pub(super) seeds: &'a [Seed],
    pub(super) of_fit: &'a [usize],
}

/// The field width a fit call starts from and reports (Task 6a; read by every star's width-start box from Task 6b):
/// the caller's `Some(f)` with `from_stars = 0`, else [`field_width_of`] the seeds. A given width that is not a
/// positive number is refused with a `warn!` and the seeds' own is used.
pub(super) fn resolve_field_width(
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
/// `mask`, `field_width` and the `correction` inputs as [`super::FitStarsOptions`] carries them. No verdict, no flag,
/// the brightest seeds as the β sample: the measure path runs [`verdict_pass`] and [`fit_after_passes`] instead (F-3,
/// Task 8).
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
    defocus_verdict: bool,
    trail_verdict: bool,
    correction: Correction<'_>,
) -> FitOutcome {
    let field = resolve_field_width(data, w, h, seeds, mask, field_width);
    fit_after_passes(
        data,
        w,
        h,
        seeds,
        model,
        p,
        mask,
        field,
        None,
        correction,
        pool,
        diagnostics,
        defocus_verdict,
        trail_verdict,
        &[],
    )
}

/// The measure path's head (F-3, Task 8; [`super::prepare_plane`]): the fit's field width ([`resolve_field_width`]
/// of `field_width`) and every seed's verdict-pass record ([`seed_passes`]) on `saturation`, the plane's saturation
/// inputs. A multi-plane caller runs it on every plane and applies the cross-plane rule
/// ([`super::cross_plane_flags`]) before [`fit_after_passes`].
#[allow(clippy::too_many_arguments)]
pub(super) fn verdict_pass(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    p: &FitParams,
    mask: Option<&[bool]>,
    field_width: Option<f64>,
    saturation: &SaturationPlane,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (FieldWidth, Vec<SeedPass>) {
    let field = resolve_field_width(data, w, h, seeds, mask, field_width);
    let passes = seed_passes(data, w, h, seeds, mask, field.fwhm_px, saturation, p, pool);
    (field, passes)
}

/// β, then every seed fitted at it on `field`'s width. `passes` (the measure path's, [`verdict_pass`], cross-plane
/// flags applied): the order is seeds → field width → verdicts → β sample (`Auto`: the seeds whose verdict fit
/// succeeded unflagged, each re-fitted pixel-integrated on its verdict fit's pixels, the median β,
/// [`super::beta::verdict_beta`]) → fits at β, each from its seed's record. `None` (every other caller): no verdict,
/// no flag, and `Auto` fits its own sample of the brightest seeds ([`super::beta::sample_beta`], Q7). A fitted β at a
/// bound flags every accepted fit [`StarFlags::BETA_AT_CAP`]; with `diagnostics`, each sample star's record carries
/// its integrated β ([`FitDiagnostics::sample_beta`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn fit_after_passes(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    mask: Option<&[bool]>,
    field: FieldWidth,
    passes: Option<&[SeedPass]>,
    correction: Correction<'_>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
    defocus_verdict: bool,
    trail_verdict: bool,
    extended: &[bool],
) -> FitOutcome {
    let sample = resolve_beta(
        data, w, h, seeds, model, p, mask, field, passes, extended, pool,
    );
    let (beta, beta_source) = (sample.beta, sample.source);
    let mut outcome = fit_at_width(
        data,
        w,
        h,
        seeds,
        beta,
        p,
        mask,
        field,
        passes,
        pool,
        diagnostics,
        defocus_verdict,
        trail_verdict,
        correction,
        extended,
    );
    outcome.beta_source = beta_source;
    if matches!(beta_source, BetaSource::Fitted { .. }) && super::beta::at_bound(beta) {
        for f in &mut outcome.fits {
            f.flags |= StarFlags::BETA_AT_CAP;
        }
    }
    // One record per seed, in seed order: a sample star's record carries its integrated fit's β.
    for &(seed, b) in &sample.fits {
        if let Some(d) = outcome.diagnostics.get_mut(seed) {
            debug_assert_eq!(d.seed, seed);
            d.sample_beta = Some(b);
        }
    }
    outcome
}

/// The β [`fit_after_passes`] fits at, resolved from `model` — the ONE place it is resolved, so the measure path's own
/// β ([`super::plane_beta`], Task 11b pass 1) and its fits are never two computations: `Moffat4` 4.0; `Fixed(b)` b,
/// fit exactly like `Moffat4`'s fixed 4.0 (Tier C Task 1: a caller-resolved β, the stacking group β of Task 11b
/// among them); `Auto` the verdict sample's ([`super::beta::verdict_beta`]) given `passes`, else the brightest
/// seeds' own sample ([`super::beta::sample_beta`], Q7).
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_beta(
    data: &[f32],
    w: usize,
    h: usize,
    seeds: &[Seed],
    model: PsfModel,
    p: &FitParams,
    mask: Option<&[bool]>,
    field: FieldWidth,
    passes: Option<&[SeedPass]>,
    extended: &[bool],
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> super::beta::SampleBeta {
    match model {
        PsfModel::Moffat4 => super::beta::SampleBeta::fixed(4.0),
        PsfModel::Fixed(b) => super::beta::SampleBeta::fixed(b),
        PsfModel::Auto => match passes {
            Some(v) => super::beta::verdict_beta(data, w, seeds, p, mask, v, extended, pool),
            None => super::beta::sample_beta(data, w, h, seeds, p, mask, field.fwhm_px, pool),
        },
    }
}

/// Fit every seed at a caller-chosen, already-concrete β — the tail
/// [`fit_stars`] itself runs once `Auto`'s sample (or `Moffat4`'s fixed
/// 4.0) has resolved a model to a number: the field width from the seeds
/// (the same [`field_width_of`] `fit_stars` reads), every star from its own
/// width start (Task 6b), then the duplicate drop over [`fit_all_outcomes`]. `beta` is echoed back
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
    fit_stars_with_beta_diagnosed(
        data,
        w,
        h,
        seeds,
        beta,
        p,
        None,
        None,
        pool,
        false,
        false,
        false,
        Correction::default(),
    )
}

/// [`fit_stars_with_beta`], also keeping one [`FitDiagnostics`] per seed, in
/// seed order, in [`FitOutcome::diagnostics`] when `diagnostics` is set
/// (empty otherwise).
///
/// The outcomes (M-4): every seed's result comes off [`fit_all_outcomes`]
/// in seed order; the accepted fits are read off it sequentially — the very
/// list the filtered chain collected before — and handed to [`dedupe`], so
/// the kept fits, their order and every sum over them are unchanged. A seed
/// whose fit the duplicate drop removed is refused `Duplicate`. `mask`,
/// `field_width` and the `correction` inputs as [`super::FitStarsOptions`]
/// carries them.
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
    defocus_verdict: bool,
    trail_verdict: bool,
    correction: Correction<'_>,
) -> FitOutcome {
    let field = resolve_field_width(data, w, h, seeds, mask, field_width);
    fit_at_width(
        data,
        w,
        h,
        seeds,
        beta,
        p,
        mask,
        field,
        None,
        pool,
        diagnostics,
        defocus_verdict,
        trail_verdict,
        correction,
        &[],
    )
}

/// The body both entry points share once β and the field width are resolved. `passes`: the measure path's verdict
/// pass (F-3), `None` on every other path. `correction` (F-5): the kept fits' model fluxes are scaled by the plane's
/// aperture correction (measured here, after the duplicate drop: the clip screen reads `correction.saturation`'s range
/// on the measure path, else the plane's own under `correction.full_scale`, owner ruling R6 / round 4; a qualifier's
/// `12F` disc lies in `correction.footprint`) or the caller's (`correction.flux`). `extended[i]` (the measure path's
/// knot flags, empty elsewhere) flags seed `i`'s kept fit [`StarFlags::EXTENDED`] before the correction reads them.
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
    passes: Option<&[SeedPass]>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    diagnostics: bool,
    defocus_verdict: bool,
    trail_verdict: bool,
    correction: Correction<'_>,
    extended: &[bool],
) -> FitOutcome {
    let per_seed = fit_all_outcomes(
        data,
        w,
        h,
        seeds,
        beta,
        p,
        mask,
        field.fwhm_px,
        passes,
        pool,
    );
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(per_seed.len());
    let mut traces: Vec<FitTrace> =
        Vec::with_capacity(if diagnostics { per_seed.len() } else { 0 });
    let mut accepted: Vec<(usize, StarFit)> = Vec::with_capacity(per_seed.len());
    let n_read = per_seed.iter().filter(|(_, t)| t.readable).count();
    let n_donut = per_seed
        .iter()
        .filter(|(r, t)| t.readable && matches!(r, Err(RefusalReason::Donut)))
        .count();
    // The plane rule is the measure path's alone (Task 6b fix wave A, review I1): LN and every other caller keep the
    // per-star `Donut` refusal and never see the plane verdict. The event is the adapter's (it knows the path).
    let defocused = defocus_verdict && plane_is_defocused(n_read, n_donut);
    // The trailed verdict (Task 10b, F-10; rulings A6, Q1, Q5) is the measure path's alone too, read from the refusal
    // structure of the same fits: the share of the judged seeds whose fit could not be contained (`CentreMoved`,
    // `RegionTooSmall`, `CentreAtRim`). A seed it pre-empts is refused `Trailed`, so the F-0 identity holds. A defocused plane keeps
    // its own verdict (read first).
    let judged = per_seed.len();
    let n_uncontained = per_seed
        .iter()
        .filter(|(r, _)| {
            matches!(
                r,
                Err(RefusalReason::CentreMoved)
                    | Err(RefusalReason::RegionTooSmall)
                    | Err(RefusalReason::CentreAtRim)
            )
        })
        .count();
    let share = if judged == 0 {
        0.0
    } else {
        n_uncontained as f64 / judged as f64
    };
    let trailed = trail_verdict && !defocused && plane_is_trailed(judged, share);
    for (i, (result, trace)) in per_seed.into_iter().enumerate() {
        match result {
            Ok(_) if defocused => outcomes.push(Outcome::Refused(RefusalReason::Defocused)),
            Ok(_) if trailed => outcomes.push(Outcome::Refused(RefusalReason::Trailed)),
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
    let mut kept = dedupe(accepted);
    for (star, &(seed, _)) in kept.iter().enumerate() {
        outcomes[seed] = Outcome::Accepted { star };
    }
    // F-10 (Task 10b, P3): a fit of a seed the knot test flagged extended carries the flag, so `c`'s qualifiers and the
    // frame statistic skip it as they skip a saturated star.
    for (seed, f) in &mut kept {
        if extended.get(*seed).copied().unwrap_or(false) {
            f.flags |= StarFlags::EXTENDED;
        }
    }
    // F-10 (Task 10b, ruling B2) then F-6 and F-5 (Task 10, Plan B B4): every kept fit's ring sky B_i and σ_i, then its
    // SNR (the `c` sample's floor reads it), then the aperture correction from them, after the duplicate drop and
    // before any total reads `signal`; none moves a fit, `c` only every kept fit's flux.
    let (aperture_correction, aperture_stars, aperture_se, ring_sky_missing, ring_fwhm, screens) =
        ring_and_correction(
            data,
            mask,
            w,
            h,
            seeds,
            &mut kept,
            field.fwhm_px,
            &correction,
            pool,
        );
    if let Some(c) = aperture_correction {
        for (_, f) in &mut kept {
            f.signal *= c;
        }
    }
    // F-6 (Task 11): every kept fit's HFR, from the stored ring and the corrected `signal` (its SNR is read before `c`).
    hfr_pass(data, mask, w, h, &mut kept, ring_fwhm, &correction, pool);
    let mut refused = RefusalCounts::default();
    for o in &outcomes {
        if let Outcome::Refused(reason) = *o {
            refused.add(reason);
        }
    }
    // A fit at β the fitter refused (a solve that never left its start among them, fix round A4) is counted, not
    // silent.
    let failed = refused
        .by_reason
        .get(&RefusalReason::FitFailed)
        .copied()
        .unwrap_or(0);
    if failed > 0 {
        debug!(
            count = seeds.len(),
            failed, beta, "fit at beta: seeds whose fit failed are refused fitFailed"
        );
    }
    let fit_seeds: Vec<usize> = kept.iter().map(|&(seed, _)| seed).collect();
    let fits: Vec<StarFit> = kept.into_iter().map(|(_, f)| f).collect();
    debug_assert_eq!(
        refused.accounted(fits.len()),
        seeds.len(),
        "every seed has exactly one outcome (ruling F13)"
    );
    debug_assert_eq!(
        share,
        trail_share(&refused, judged),
        "the verdict's share is the refusal counts' (Task 10b)"
    );
    let verdict_of = |seed: usize| -> Option<SaturationVerdict> {
        passes.and_then(|v| v[seed].verdict.as_ref().and_then(|r| r.ok()))
    };
    let diagnostics = traces
        .into_iter()
        .zip(outcomes)
        .enumerate()
        .map(|(seed, (trace, outcome))| trace.record(seed, outcome, verdict_of(seed)))
        .collect();
    let verdict_fit_failed = passes.map_or(0, |v| {
        v.iter()
            .filter(|x| matches!(x.verdict, Some(Ok(sv)) if !sv.fit))
            .count()
    });
    if verdict_fit_failed > 0 {
        debug!(
            count = seeds.len(),
            failed = verdict_fit_failed,
            "saturation verdict: seeds whose verdict fit failed are unflagged by amplitude and counted"
        );
    }
    FitOutcome {
        fits,
        beta,
        beta_source: BetaSource::Fixed,
        seeds: seeds.len(),
        refused,
        field_width: field,
        diagnostics,
        defocused,
        defocus_donuts: if defocus_verdict { n_donut } else { 0 },
        trailed,
        trail_share: share,
        verdict_fit_failed,
        aperture_correction,
        aperture_stars,
        aperture_se,
        aperture_screens: screens,
        ring_sky_missing,
        fit_seeds,
        ring_fwhm,
    }
}

/// Every kept fit's ring sky (Task 10b, F-10, ruling B2: [`super::photometry::ring_skies`] at `correction.ring_fwhm`,
/// else the plane's own frame FWHM), then the aperture correction from the stored B_i (F-5,
/// [`super::photometry::aperture_correction`] at the same ring width) or the caller's (`correction.flux`). Every fit's
/// SNR is read between the two (Plan B B4: the `c` sample's floor, [`super::photometry::CORRECTION_MIN_SNR`], reads it;
/// the SNR reads the ring sky and the pixels, never `c` or `signal`). Returns
/// `(c, qualifiers, se, ring_sky_missing, ring_fwhm, screens)`; no `signal` is touched. One body for the fit call and for a later re-read at
/// another ring width ([`correct_at_ring`]).
#[allow(clippy::too_many_arguments)]
fn ring_and_correction(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seeds: &[Seed],
    kept: &mut [(usize, StarFit)],
    field_fwhm: f64,
    correction: &Correction<'_>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> (
    Option<f64>,
    usize,
    Option<f64>,
    usize,
    Option<f64>,
    super::photometry::ApertureScreens,
) {
    // R12-5: a budgeted call fits a subset; the discs and the isolation read every detected seed.
    let (all_seeds, own) = match correction.neighbours {
        Some(n) => (n.seeds, Some(n.of_fit)),
        None => (seeds, None),
    };
    let (ring_fwhm, missing) = {
        let _s = super::substeps::span(correction.timers, super::Step::RingSky);
        super::photometry::ring_skies(
            data,
            mask,
            correction.footprint,
            w,
            h,
            all_seeds,
            kept,
            correction.ring_fwhm,
            correction.population.map(|p| p.statistic),
            pool,
            correction.timers,
        )
    };
    let given = match correction.flux {
        super::FluxCorrection::Given(c) if c.is_finite() && c > 0.0 => Some(c),
        super::FluxCorrection::Given(bad) => {
            warn!(
                aperture_correction = bad,
                count = kept.len(),
                "fit: the given aperture correction is not a positive number; the plane's own is measured"
            );
            None
        }
        super::FluxCorrection::FromPlane => None,
    };
    snr_pass(data, mask, w, h, kept, correction, pool);
    let (c, stars, se, screens) = match given {
        Some(c) => (
            Some(c),
            0,
            None,
            super::photometry::ApertureScreens::default(),
        ),
        None => {
            let _s = super::substeps::span(correction.timers, super::Step::ApertureC);
            super::photometry::aperture_correction(
                data,
                mask,
                w,
                h,
                kept,
                all_seeds,
                own,
                field_fwhm,
                ring_fwhm,
                correction.saturation,
                correction.full_scale,
                correction.footprint,
                correction.population,
                correction.siblings,
            )
        }
    };
    (c, stars, se, missing, ring_fwhm, screens)
}

/// Every kept fit's aperture SNR (F-6, Task 11, [`super::photometry::star_snr`]) from its stored ring sky and noise
/// and the pixels about its fitted centre: it reads neither `signal` nor `c`, so it is read BEFORE the aperture
/// correction (Plan B B4, PB-13': the `c` sample's floor reads it). `correction` carries the plane's gain and noise
/// correlation (default: no gain, white). Nothing the fit measured moves. `pool` as the fit's.
#[allow(clippy::too_many_arguments)]
fn snr_pass(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    kept: &mut [(usize, StarFit)],
    correction: &Correction<'_>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) {
    let body = || {
        let _s = super::substeps::span(correction.timers, super::Step::SnrHfr);
        kept.par_iter_mut().for_each(|(_, f)| {
            let _leaf = super::substeps::leaf(correction.timers, super::Step::SnrHfr);
            f.snr = super::photometry::star_snr(
                data,
                w,
                h,
                f,
                correction.gain,
                mask,
                &correction.noise,
            )
            .0;
        })
    };
    super::install_in_span(pool, body);
}

/// Every kept fit's half-flux radius (F-6, Task 11, [`super::photometry::hfr`]) from its stored ring sky and its
/// (aperture-corrected) `signal`, at the ring width `ring_fwhm` the rings were read at (`None` = no ring anywhere:
/// every fit reads NaN). Read AFTER the aperture correction: the half-flux target is half of `signal`. `pool` as the
/// fit's.
#[allow(clippy::too_many_arguments)]
fn hfr_pass(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    kept: &mut [(usize, StarFit)],
    ring_fwhm: Option<f64>,
    correction: &Correction<'_>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) {
    let body = || {
        let _s = super::substeps::span(correction.timers, super::Step::SnrHfr);
        kept.par_iter_mut().for_each(|(_, f)| {
            let _leaf = super::substeps::leaf(correction.timers, super::Step::SnrHfr);
            f.hfr = ring_fwhm.map_or(f64::NAN, |r| super::photometry::hfr(data, w, h, f, r, mask));
        })
    };
    super::install_in_span(pool, body);
}

/// LN's pair-common ring arm (Task 10b, ruling B3) without a second fit: every fit of `outcome` (a fit call's result
/// on `data` with `seeds`) re-reads its ring sky at `correction.ring_fwhm` and the plane's `c` is measured again from
/// them, exactly as the fit call itself would have with that ring width ([`ring_and_correction`], the same seeds,
/// field width and qualifier rule); every fit's `signal` becomes its model flux ([`super::model_flux_in_fwtm`], the
/// value the fit call started from) times the new `c`. The fits themselves (centres, widths, β, flags) do not move.
#[allow(clippy::too_many_arguments)]
pub(super) fn correct_at_ring(
    data: &[f32],
    mask: Option<&[bool]>,
    w: usize,
    h: usize,
    seeds: &[Seed],
    outcome: &mut FitOutcome,
    correction: &Correction<'_>,
    pool: Option<&Arc<rayon::ThreadPool>>,
) {
    debug_assert_eq!(outcome.fit_seeds.len(), outcome.fits.len());
    let mut kept: Vec<(usize, StarFit)> = outcome
        .fit_seeds
        .iter()
        .copied()
        .zip(outcome.fits.drain(..))
        .collect();
    let (c, stars, se, missing, ring_fwhm, screens) = ring_and_correction(
        data,
        mask,
        w,
        h,
        seeds,
        &mut kept,
        outcome.field_width.fwhm_px,
        correction,
        pool,
    );
    for (_, f) in &mut kept {
        f.signal = super::photometry::model_flux_in_fwtm(f);
        if let Some(c) = c {
            f.signal *= c;
        }
    }
    // F-6 (Task 11): B_i and `signal` moved, so the HFR is read again (the SNR, read from the new B_i, is already in).
    hfr_pass(data, mask, w, h, &mut kept, ring_fwhm, correction, pool);
    outcome.fits = kept.into_iter().map(|(_, f)| f).collect();
    outcome.aperture_correction = c;
    outcome.aperture_stars = stars;
    outcome.aperture_se = se;
    outcome.aperture_screens = screens;
    outcome.ring_sky_missing = missing;
    outcome.ring_fwhm = ring_fwhm;
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
        // F-5 (Task 10): fewer than 5 stars qualify for the plane's aperture correction, so `c` is None and every
        // `signal` below is the bare model flux in the FWTM ellipse (0.8222 of the total).
        assert_eq!(out.aperture_correction, None);
        assert!(out.aperture_stars < 5, "{}", out.aperture_stars);
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
            let expected = total * (1.0 - 10f64.powf(0.25 - 1.0)); // 0.8222·total (F-5: c = None, the bare model flux)
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

    /// F-4 (Task 9, ruling F14): `Auto` is continuous, so the Moffat-4 field reads within 0.2 of its generating β 4
    /// (was `== 4.0`, a ladder pick) and the Gaussian field at the large-β limit, ≥ 60 (was 10.0, the ladder's top).
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
        assert!((out.beta - 4.0).abs() < 0.2, "{}", out.beta);
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
        assert!(
            out.beta >= 60.0,
            "a Gaussian field is the large-β limit: {}",
            out.beta
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
        // uses, which resolves to a NON-default β under `Auto` (at or near
        // the cap from F-4; 10.0 on the discrete ladder) — so this pins
        // `fit_stars_with_beta` reproducing an arbitrary resolved β exactly,
        // not just the β = 4.0 default.
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
            "signal {} vs {expected} (the bare model flux, c = None under F-5): the aperture rotation must follow the fitter's θ convention",
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
    // Ruling A4-2: the 'wander' arm of this test (a seed 3.5 px off whose fit walked back > 1.5 px) read the retired fixed
    // centroid tolerance. A seed off a star now fits a broad blob (FWHM 15 × 12 at a 6-px offset) whose own half-maximum
    // contour reaches it, which is accepted by the rule; the contour test itself is pinned on `placement` by
    // `a_round_star_keeps_the_seed_within_half_its_fwhm` and `an_elongated_fit_owns_the_seeds_along_its_major_axis`.
    fn fits_are_rejected_when_they_wander_duplicate_or_touch_the_border() {
        let stars = [round(60.0, 60.0, 0.5), round(8.0, 100.0, 0.5)];
        let data = moffat_field(160, 160, &stars, 4.0, 0.05);
        let good = Seed {
            x: 60.0,
            y: 60.0,
            peak: 0.5,
            flux: 13.1,
        };
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
            &[good, dup, border],
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
        // The seed sits on the star (the centre test is not what refuses here, ruling A4-2); the REGION is off-centre.
        let seed = Seed {
            x: 100.0,
            y: 100.0,
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
        // floor, by 20 %). A power of two scales every f32 value exactly. The seed's peak AND flux scale with it (the
        // width start, a half-maximum width since Task 6b, is scale-free either way).
        const K: f64 = 1024.0;
        // Plan A Task 6b round 1 (ruling F14): the sub-pixel phases are binary-exact (.25, .75). With 30.3 / 3000.3
        // the seed's offset from its pixel, `x − round(x)`, differs by 1.8e-13 between the two copies (3000.3 is not
        // 2970 + 30.3 in f64), so the two fits started 1.8e-13 apart and the residual, at the f32 rounding floor
        // (8.2e-9), read 1.3e-9 apart once the width start became the star's own half-maximum width. With exact phases
        // the far copy is an exact translate: the same pixels, the same start.
        let near = moffat_field(80, 80, &[star(30.25, 20.75, 0.3)], 4.0, 0.05);
        let far = moffat_field(3100, 2100, &[star(3000.25, 2000.75, 0.3)], 4.0, 0.05);
        let bright = moffat_field(80, 80, &[star(30.25, 20.75, 0.3 * K)], 4.0, 0.05 * K as f32);
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
        let a = f(&near, 80, 80, 30.25, 20.75, 0.3, 1.0);
        let b = f(&far, 3100, 2100, 3000.25, 2000.75, 0.3, 1.0);
        let c = f(&bright, 80, 80, 30.25, 20.75, 0.3 * K, K);
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
        fit_with_verdict(data, w, h, seeds, field, true)
    }

    /// [`fit_with`] with the plane verdict chosen: `true` is the measure path's call, `false` every other caller's.
    fn fit_with_verdict(
        data: &[f32],
        w: usize,
        h: usize,
        seeds: &[Seed],
        field: f64,
        defocus_verdict: bool,
    ) -> (FitOutcome, Vec<FitDiagnostics>) {
        let opts = FitStarsOptions {
            field_width: Some(field),
            diagnostics: true,
            defocus_verdict,
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

    /// Adds an annulus (outer 6, obstruction 2.4) blurred by a Gaussian of σ 1.1 at `(cx, cy)` to `data`, the way
    /// `donut_is_refused_with_its_reason` renders one, over the 25×25 pixels about it only.
    fn add_donut(data: &mut [f32], w: usize, cx: usize, cy: usize) {
        add_donut_gain(data, w, cx, cy, 1.0);
    }

    /// [`add_donut`] with its amplitude scaled by `gain`.
    fn add_donut_gain(data: &mut [f32], w: usize, cx: usize, cy: usize, gain: f64) {
        let ring = ring_samples(cx as f64, cy as f64, 2.4, 6.0);
        for y in cy - 12..=cy + 12 {
            for x in cx - 12..=cx + 12 {
                let mut acc = 0.0f64;
                for &(ux, uy) in &ring {
                    let d2 = (x as f64 - ux).powi(2) + (y as f64 - uy).powi(2);
                    acc += (-d2 / (2.0 * 1.1 * 1.1)).exp();
                }
                data[y * w + x] += (gain * 0.3 * acc / 2000.0) as f32;
            }
        }
    }

    /// Owner ruling 2026-10-08: a plane whose readable seeds are mostly donuts is defocused. Eight donuts and two
    /// sharp stars: the donuts keep their `Donut` refusal, the two stars (which fit on their own) are refused
    /// `Defocused`, and the plane carries no fit.
    #[test]
    fn a_donut_field_plane_is_defocused_and_has_no_fits() {
        let (data, w, h, seeds) = donut_field();
        // The field width of a defocused frame is the donuts' (their box must clear the ring: radius 2·8 = 16 > 6).
        let (out, diag) = fit_with(&data, w, h, &seeds, 8.0);
        assert!(out.defocused, "{:?}", out.refused);
        assert!(out.fits.is_empty(), "{:?}", out.fits);
        for d in &diag[..8] {
            assert_eq!(d.outcome, Outcome::Refused(RefusalReason::Donut), "{d:?}");
        }
        for d in &diag[8..] {
            assert_eq!(
                d.outcome,
                Outcome::Refused(RefusalReason::Defocused),
                "{d:?}"
            );
        }
        assert_eq!(out.refused.by_reason[&RefusalReason::Donut], 8);
        assert_eq!(out.refused.by_reason[&RefusalReason::Defocused], 2);
        assert_eq!(out.refused.accounted(0), seeds.len());
        assert_eq!(out.defocus_donuts, 8);
    }

    /// Task 6b fix wave A, review I1 (RED before the fix: `fit_stars_at` re-decided the plane verdict on every call).
    /// An LN-style call (`defocus_verdict` false) on the plane whose donuts are most of its readable stars keeps its
    /// two non-donut fits; the donuts keep their per-star `Donut` refusal and the plane is not defocused.
    #[test]
    fn an_ln_style_call_on_a_donut_field_keeps_its_non_donut_fits() {
        let (data, w, h, seeds) = donut_field();
        let (out, diag) = fit_with_verdict(&data, w, h, &seeds, 8.0, false);
        assert!(!out.defocused, "{:?}", out.refused);
        assert_eq!(out.fits.len(), 2, "{:?}", out.refused);
        for d in &diag[..8] {
            assert_eq!(d.outcome, Outcome::Refused(RefusalReason::Donut), "{d:?}");
        }
        assert!(!out
            .refused
            .by_reason
            .contains_key(&RefusalReason::Defocused));
        assert_eq!(out.defocus_donuts, 0);
    }

    /// The measure path (`measure_plane`) asks for the verdict and marks the same plane defocused, with no star.
    #[test]
    fn the_measure_path_marks_the_donut_field_plane_defocused() {
        use crate::star_measure::{measure_plane, MeasureOptions, SeedSpec};
        let (data, w, h, seeds) = donut_field();
        let m = measure_plane(
            &data,
            w,
            h,
            &MeasureOptions {
                seeds: SeedSpec::Given(seeds),
                field_width: Some(FieldWidth {
                    fwhm_px: 8.0,
                    from_stars: 0,
                }),
                ..Default::default()
            },
            None,
        );
        assert!(m.defocused, "{:?}", m.refused);
        assert!(m.stars.is_empty());
        assert_eq!(m.defocus_donuts, 8);
    }

    /// Eight donuts and two sharp stars on a flat plane (330 x 100), the donuts' seeds first.
    fn donut_field() -> (Vec<f32>, usize, usize, Vec<Seed>) {
        let (w, h) = (330usize, 100usize);
        let mut data = vec![0.05f32; w * h];
        let donuts: Vec<(usize, usize)> = (0..8).map(|i| (30 + 35 * i, 30)).collect();
        for &(x, y) in &donuts {
            add_donut(&mut data, w, x, y);
        }
        let alpha = 2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        let stars = [one(100.0, 75.0, 0.3, alpha), one(220.0, 75.0, 0.3, alpha)];
        let sharp = moffat_field(w, h, &stars, 4.0, 0.0);
        for (d, v) in data.iter_mut().zip(&sharp) {
            *d += *v;
        }
        crate::test_support::add_noise(&mut data, 0.002, 5);
        let mut seeds: Vec<Seed> = donuts
            .iter()
            .map(|&(x, y)| Seed {
                x: x as f64,
                y: y as f64,
                peak: 0.1,
                flux: 1.0,
            })
            .collect();
        seeds.extend(stars.iter().map(seed));
        (data, w, h, seeds)
    }

    /// The rule's two thresholds, exactly: 5 donuts are the minimum and a quarter of the readable seeds the fraction.
    #[test]
    fn the_defocused_rule_needs_five_donuts_and_a_quarter_of_the_readable_seeds() {
        assert!(
            !plane_is_defocused(4, 4),
            "four donuts are under the minimum"
        );
        assert!(plane_is_defocused(5, 5));
        assert!(plane_is_defocused(20, 5), "exactly a quarter");
        assert!(!plane_is_defocused(21, 5), "just under a quarter");
        assert!(!plane_is_defocused(0, 0));
    }

    /// Ruling Q5's rule, exactly: at least 100 judged seeds and MORE than half of them uncontained (exactly half is not
    /// a majority); the share counts `CentreMoved`, `RegionTooSmall` and `CentreAtRim`, over every judged seed.
    #[test]
    fn the_trailed_rule_needs_a_hundred_judged_seeds_and_a_majority() {
        assert!(
            !plane_is_trailed(99, 1.0),
            "99 judged seeds are under the minimum"
        );
        assert!(plane_is_trailed(100, 0.51));
        assert!(!plane_is_trailed(100, 0.5), "exactly half");
        assert!(!plane_is_trailed(0, 0.0));
        let mut c = RefusalCounts::default();
        for r in [
            RefusalReason::CentreMoved,
            RefusalReason::CentreMoved,
            RefusalReason::CentreMoved,
            RefusalReason::RegionTooSmall,
            RefusalReason::CentreAtRim,
            RefusalReason::Duplicate,
        ] {
            c.add(r);
        }
        // 3 centreMoved + 1 regionTooSmall + 1 centreAtRim (ruling A4-2) of 8; the duplicate is not a containment refusal.
        assert_eq!(trail_share(&c, 8), 0.625);
        assert_eq!(trail_share(&RefusalCounts::default(), 0), 0.0);
    }

    /// Ruling A4-2: the trail share counts every containment refusal — `centreAtRim` with `centreMoved` and
    /// `regionTooSmall` — over the judged seeds; every other refusal stays out.
    #[test]
    fn the_trail_share_counts_every_containment_refusal() {
        let mut c = RefusalCounts::default();
        for r in [
            RefusalReason::CentreAtRim,
            RefusalReason::CentreAtRim,
            RefusalReason::CentreMoved,
            RefusalReason::RegionTooSmall,
            RefusalReason::Donut,
        ] {
            c.add(r);
        }
        assert_eq!(trail_share(&c, 10), 0.4);
        let mut rim = RefusalCounts::default();
        rim.add(RefusalReason::CentreAtRim);
        assert_eq!(trail_share(&rim, 4), 0.25, "a rim refusal alone is a share");
    }

    fn alpha_of_fwhm(fwhm: f64, beta: f64) -> f64 {
        fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt())
    }

    /// Ruling A4-2 on a round star (Moffat FWHM 3, β 4): the half-maximum contour is today's 1.5 px — a seed 1.4 px
    /// off along x is the star's, 1.6 px is not.
    #[test]
    fn a_round_star_keeps_the_seed_within_half_its_fwhm() {
        let (beta, a) = (4.0, alpha_of_fwhm(3.0, 4.0));
        let p = FitParams::default();
        let at = |dx: f64| {
            placement(
                (50.0, 50.0),
                (a, a),
                beta,
                (50.0 + dx, 50.0),
                (50.0, 50.0, 20.0),
                0.0,
                &p,
            )
        };
        assert_eq!(at(1.4), Ok(()));
        assert_eq!(at(-1.4), Ok(()));
        assert_eq!(at(1.6), Err(RefusalReason::CentreMoved));
    }

    /// Ruling A4-2 on an elongated fit (FWHM 21 × 7, turned by 0.6 rad): a seed 3 px along the major axis is the
    /// star's, a seed 4 px across it (half-width 3.5) is not — the contour, not a fixed box.
    #[test]
    fn an_elongated_fit_owns_the_seeds_along_its_major_axis() {
        let beta = 4.0;
        let (ax, ay) = (alpha_of_fwhm(21.0, beta), alpha_of_fwhm(7.0, beta));
        let theta = 0.6f64;
        let p = FitParams::default();
        let off = |u: f64, v: f64| {
            // (u, v) in the fit's axes, back to image offsets.
            let (c, s) = (theta.cos(), theta.sin());
            placement(
                (100.0, 100.0),
                (ax, ay),
                beta,
                (100.0 + u * c - v * s, 100.0 + u * s + v * c),
                (100.0, 100.0, 40.0),
                theta,
                &p,
            )
        };
        assert_eq!(off(3.0, 0.0), Ok(()));
        assert_eq!(off(9.0, 0.0), Ok(()));
        assert_eq!(off(0.0, 4.0), Err(RefusalReason::CentreMoved));
        assert_eq!(off(0.0, 3.0), Ok(()));
    }

    /// A sharp plane with 3 donut-shaped ghosts among 100 stars is not defocused: the ghosts keep `Donut`, the stars
    /// are measured.
    #[test]
    fn a_sharp_plane_with_three_donut_ghosts_is_not_defocused() {
        let (w, h) = (330usize, 330usize);
        let alpha = 2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        let stars: Vec<MoffatStar> = (0..100)
            .map(|i| {
                one(
                    30.0 + (i % 10) as f64 * 30.3,
                    30.0 + (i / 10) as f64 * 30.3,
                    0.3,
                    alpha,
                )
            })
            .collect();
        let mut data = moffat_field(w, h, &stars, 4.0, 0.05);
        // Between grid stars, 21 px from the nearest: clear of their regions.
        let ghosts = [(45usize, 45usize), (166, 166), (257, 105)];
        for &(x, y) in &ghosts {
            add_donut(&mut data, w, x, y);
        }
        crate::test_support::add_noise(&mut data, 0.002, 9);
        let mut seeds: Vec<Seed> = stars.iter().map(seed).collect();
        seeds.extend(ghosts.iter().map(|&(x, y)| Seed {
            x: x as f64,
            y: y as f64,
            peak: 0.1,
            flux: 1.0,
        }));
        let (out, diag) = fit_with(&data, w, h, &seeds, 2.6);
        assert!(!out.defocused, "{:?}", out.refused);
        assert!(out.fits.len() >= 95, "{:?}", out.refused);
        for d in &diag[100..] {
            assert_eq!(d.outcome, Outcome::Refused(RefusalReason::Donut), "{d:?}");
        }
        assert!(!out
            .refused
            .by_reason
            .contains_key(&RefusalReason::Defocused));
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
        // Task 6b round 1 (ruling R1; reported to the controller): the brief asserted b0 itself within 0.3 σ. With the
        // width start the star's own half-maximum width (2.81 px here), PA-12's region starts and stops at
        // r = ⌈1.5·2.81⌉ = 5 (1.9 FWHM; `(m_r − s_r)` is already below 1 % of the peak), where the rim still holds
        // ≈ 0.3 % of this bright star's peak, +0.59 σ (b0 0.056911 vs the sky 0.055738); the moments' inflated width
        // had started the region wider. The F-7 bar reads the fit's background (row 5), which stays within 0.3 σ
        // (−0.08 σ), and the rim start is still the better start: closer to the sky than the plain median of the same
        // square, the start it replaced.
        let r = diag[0].region_radius as i64;
        let mut square: Vec<f32> = Vec::new();
        for y in 40 - r..=40 + r {
            for x in 40 - r..=40 + r {
                square.push(data[y as usize * w + x as usize]);
            }
        }
        let plain = median_in_place(&mut square) as f64;
        assert!(
            (diag[0].background_start - local_sky).abs() < (plain - local_sky).abs(),
            "b0 {} vs the square median {plain}, sky {local_sky}",
            diag[0].background_start
        );
        assert!(
            (out.fits[0].background - local_sky).abs() < 0.3 * 0.002,
            "fitted background {} vs sky {local_sky}",
            out.fits[0].background
        );
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

    /// Task 6b round 1, ruling R1: the width start is the star's own half-maximum width where its peak stands at least
    /// 10 σ_rim above the width-start box's rim, else the field width. Six bright FWHM-3 stars and one bright FWHM-7 star
    /// (a broad star among sharp ones) start from their own widths; a faint FWHM-3 star (peak ≈ 5 σ) starts from the
    /// field width exactly. The candidates were judged on truth before any bar was read
    /// (`star-lab-a/t06b-f1/diag/fstart_candidates.py`): the half-maximum width read 1.02–1.04 (FWHM 3) and 0.99–1.01
    /// (FWHM 7) of the truth on the two-population plane.
    #[test]
    fn width_start_is_the_own_width_where_readable_else_the_field_width() {
        let mut stars: Vec<MoffatStar> = (0..6)
            .map(|i| {
                one(
                    40.0 + (i % 3) as f64 * 60.0,
                    50.0 + (i / 3) as f64 * 100.0,
                    0.3,
                    3.449,
                )
            })
            .collect();
        stars.push(one(250.3, 49.6, 0.3, 8.048)); // FWHM 7
        stars.push(one(250.0, 150.0, 0.010, 3.449)); // FWHM 3 at ≈ 5 σ
        let mut data = moffat_field(320, 200, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 21);
        let seeds: Vec<Seed> = stars.iter().map(seed).collect();
        let (_, diag) = fit_with(&data, 320, 200, &seeds, 3.0);
        for d in &diag[..6] {
            assert!(
                (d.width_start / 3.0 - 1.0).abs() < 0.1,
                "a sharp star's own width: {d:?}"
            );
        }
        assert!(
            (diag[6].width_start / 7.0 - 1.0).abs() < 0.1,
            "a broad star among sharp ones starts from its own width: {:?}",
            diag[6]
        );
        assert_eq!(
            diag[7].width_start, 3.0,
            "a faint star starts from the field width: {:?}",
            diag[7]
        );
    }

    /// t06b-m2-check (2026-10-08): the own-peak gate lost 547 fits and read same-star fwhm +0.3 % worse (10-20, 20-40);
    /// the box-maximum gate is kept by measurement. A 5 sigma FWHM-3 star beside a 50x brighter neighbour 5 px away,
    /// inside its box, reads readable by the box maximum, so it starts from its own half-maximum width, not the field's.
    #[test]
    fn a_star_near_a_bright_neighbour_is_readable_by_the_box_maximum() {
        let stars = [
            one(100.0, 100.0, 0.010, 3.449),
            one(105.0, 100.0, 0.5, 1.725),
        ];
        let mut data = moffat_field(200, 200, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 33);
        let seeds: Vec<Seed> = stars.iter().map(seed).collect();
        let (_, diag) = fit_with(&data, 200, 200, &seeds, 3.0);
        assert_ne!(
            diag[0].width_start, 3.0,
            "the neighbour's wing makes the box maximum clear the gate: {:?}",
            diag[0]
        );
    }

    // --- Task 6b round 3 (rulings R7/R8 final, R9 final) ---

    /// The brief's donut, noise-free: an annulus (outer 6, obstruction 2.4) blurred by a Gaussian of σ 1.1.
    fn donut_plane() -> (Vec<f32>, usize, usize) {
        let (w, h) = (80usize, 80usize);
        let mut data = vec![0.05f32; w * h];
        let ring = ring_samples(40.0, 40.0, 2.4, 6.0);
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
        (data, w, h)
    }

    /// A pin, not RED (ruling F28): round 1's box-moment centre also refuses this donut at the field's own width (its
    /// 33-px box covers the annulus); the pin guards R7's window, which a window of `F_start/2` (round 2's first ruling)
    /// fails — the centre stays on the annulus, core/ring 2.48 (`star-lab-a/t06b-f1/diag`).
    ///
    /// R7 (round 3): a Peak seed sits on the annulus, not at its centre; the local centre's window (radius
    /// `max(1.5, F_start)`) covers the object, so the iterated centroid walks to the hole and the donut is refused. The
    /// field width is the donut field's own (the lab's `donut/r6` matched filter reads ≈ 8 px, the truth check's
    /// geometry): its width start reads 6.7 px, the centre walks from 4 px out to 0.3 px, core/ring 0.26. (A lone donut
    /// in a field of width 3 — a box of radius 6 whose rim crosses the annulus — fails the readability gate, starts from
    /// the narrow field width and keeps its centre on the annulus; it is refused `CentreMoved` there, never fitted.)
    #[test]
    fn a_peak_seeded_donut_is_refused() {
        let (data, w, h) = donut_plane();
        let mut best = (0usize, 0usize, f32::MIN);
        for y in 30..=50 {
            for x in 30..=50 {
                if data[y * w + x] > best.2 {
                    best = (x, y, data[y * w + x]);
                }
            }
        }
        let peak = Seed {
            x: best.0 as f64,
            y: best.1 as f64,
            peak: 0.1,
            flux: 1.0,
        };
        assert!(
            (peak.x - 40.0).hypot(peak.y - 40.0) > 3.0,
            "the Peak seed is on the annulus: {peak:?}"
        );
        let (out, diag) = fit_with(&data, w, h, &[peak], 8.0);
        assert!(out.fits.is_empty(), "{:?}", out.fits);
        assert_eq!(diag[0].outcome, Outcome::Refused(RefusalReason::Donut));
    }

    /// R7 (round 3): a faint star (FWHM 8, Fisher SNR ≈ 30) with a 60× brighter neighbour 24 px away — the geometry of
    /// round 1's `sharp/m4/f8.0` false donuts (neighbours 25–35 px away, 5–277× brighter). The neighbour's wing inside
    /// the width-start box pulled the box moments' centroid off the star, so the core missed it and the star was
    /// refused `Donut` (RED on round 1's rule); the local centre stays on the star.
    #[test]
    fn a_star_beside_a_bright_neighbour_is_not_refused_as_a_donut() {
        let alpha = 8.0 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        let stars = [
            one(100.3, 100.2, 0.0098, alpha),
            one(124.3, 100.2, 0.588, alpha),
        ];
        let mut data = moffat_field(260, 200, &stars, 4.0, 0.05);
        add_noise(&mut data, 0.002, 31);
        let seeds = [seed(&stars[0]), seed(&stars[1])];
        let (_, diag) = fit_with(&data, 260, 200, &seeds, 8.0);
        assert_ne!(
            diag[0].outcome,
            Outcome::Refused(RefusalReason::Donut),
            "{:?}",
            diag[0]
        );
    }

    /// R8 (round 3): the ring `(R − ½, R + ½]`. At R = 1.5 its bounds are the integers 1 and 2; a pixel-centred
    /// Gaussian of FWHM 3.5 has its four cross neighbours at exactly d = 1, which the half-open-below ring of round 1
    /// let in or out by a 1e-16 offset of the centre (core/ring 1.044 in, 1.21 out). The fold check's real-star bar is
    /// ≥ 1.08; it holds at a 0 and at a 1e-12 offset.
    #[test]
    fn the_ring_excludes_the_cross_neighbours_at_a_pixel_centre() {
        let sigma = 3.5 / GAUSSIAN_FWHM_PER_SIGMA;
        let data = crate::test_support::gaussian_field(120, 120, &[(60.0, 60.0, 0.3)], sigma, 0.05);
        for dx in [0.0, 1e-12] {
            let (core, ring) = core_and_ring(&data, None, 120, 120, (60.0 + dx, 60.0), 1.5, 0.05)
                .expect("core and ring present");
            assert!(
                core / ring >= 1.08,
                "offset {dx}: core {core} ring {ring} ({})",
                core / ring
            );
        }
    }

    /// R9 (round 3, the brief's "moment axis ratio as today"): the start shape is read from PA-12's starting square
    /// only. An elongated star (axis ratio 2 at 30°) with a bright round neighbour 12 px along x: the starting square of
    /// radius 5 reads the star's own angle, the whole 31×31 stamp is pulled toward the neighbour.
    #[test]
    fn the_start_shape_is_read_from_the_starting_square() {
        let star = MoffatStar {
            x: 15.0,
            y: 15.0,
            amp: 0.3,
            alpha_x: 3.0,
            alpha_y: 1.5,
            theta: 30f64.to_radians(),
        };
        let data = moffat_field(31, 31, &[star, one(27.0, 15.0, 0.3, 2.0)], 4.0, 0.0);
        let px: Vec<PixelSample> = (0..31i64)
            .flat_map(|y| (0..31i64).map(move |x| (x, y)))
            .map(|(x, y)| PixelSample {
                x: (x - 15) as f64,
                y: (y - 15) as f64,
                value: data[y as usize * 31 + x as usize] as f64,
            })
            .collect();
        let (q, theta) = start_shape(&px, 5).expect("the starting square has moments");
        assert!(
            (theta.to_degrees() - 30.0).abs() < 3.0,
            "θ {}",
            theta.to_degrees()
        );
        assert!(q > 1.2, "q {q}");
        let whole = moment_ellipse(&px, 0.0).unwrap();
        assert!(
            (whole.theta.to_degrees() - 30.0).abs() > 10.0,
            "the whole stamp is pulled: {}",
            whole.theta.to_degrees()
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
