//! A frame's photometric zero point from its fitted stars and the installed
//! star catalogue (Gaia G), the number a transparency loss is later taken from.
//!
//! The first half is pure: `measure_zero_point` reads no file and no database.
//! The caller passes the frame's stored star rows, its WCS, a catalogue cone and
//! its exposure, and it projects the catalogue through the WCS, matches stars
//! one to one, and returns the sigma-clipped median of
//! `G + 2.5·log10(flux / exptime)` or a typed refusal (spec §5.1).
//! `catalog_for_frame` is the one catalogue reader (memory-mapped files). Pixel
//! positions on both sides are 0-based in file row order.
//!
//! The second half (`refresh_zero_point_with` and what is built on it) keeps the
//! number current in the catalogue: it reads a frame's analysis, stars and solve,
//! measures, and stores the outcome beside the analysis (`frame_analysis.zp_*`).
//! A zero point is current when it was judged at `ZP_VERSION` against the
//! installed catalogue's identity; a refusal and a colour camera are stored too,
//! so a hopeless frame is not retried on every launch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use solvemyastro::{CatalogStar, LayeredCones, StarCache, WcsSolution};

use crate::db::analysis as db_analysis;
use crate::models::{Frame, StarMetric};
use crate::plate_solve::hints::observation_epoch;
use crate::plate_solve::layers::{catalog_identity, open_catalog_layers, CatalogOpenError};
use crate::plate_solve::storage::get_plate_solve;
use crate::stacking::groups::{color_mode_of, ColorMode};
use crate::star_measure::analysis_mode::{ListMeasurement, ListRequest};
use crate::star_measure::MEASURE_VERSION;

/// Version of the measurement; a stored zero point is current only at this value. Version 2: the validity rule changed
/// (fifty stars and a standard error within 0.035 mag) after version 1 was written to catalogues by development builds.
pub const ZP_VERSION: u32 = 2;

/// Robust scatter of a Gaussian from the median absolute deviation.
const MAD_TO_SIGMA: f64 = 1.4826;
/// Most sigma-clip rounds.
const MAX_CLIP_ROUNDS: usize = 10;
/// Floor of the clip threshold's sigma (mag): noise-free input has MAD 0.
const CLIP_SIGMA_FLOOR_MAG: f64 = 1e-6;
/// Extra radius of the catalogue cone beyond the central region.
const CONE_MARGIN: f64 = 1.10;

/// Constants of the measurement (spec §5.1 starting values, fixed by the probe).
#[derive(Debug, Clone, Copy)]
pub struct ZpParams {
    /// A catalogue star within this many pixels can be an analysis star's match.
    pub match_radius_px: f64,
    /// Stars inside this fraction of the half-diagonal are used.
    pub central_fraction: f64,
    /// Fewest surviving stars of a valid frame. Under 50 matched stars the
    /// frame-to-frame scatter of the zero point was 0.10-0.12 mag: this limit
    /// guards the noise of the median, and the frames it refuses are the ones
    /// measured on a shallow catalogue, where only the brightest stars match.
    pub min_stars: usize,
    /// Largest standard error of the median (mag) of a valid frame. Through a
    /// blue filter the per-star offset to G scatters widely, so a stable frame
    /// still has an error of 0.022-0.030 with ~265 stars; 0.035 keeps those and
    /// refuses a median that is too noisy to compare (acceptance run, 2026-11-15).
    pub max_std_error_mag: f64,
    /// Sigma-clip threshold in robust sigmas.
    pub clip_sigma: f64,
    /// The brightest fraction of the analysis stars is dropped. The brightest
    /// catalogue stars of a field are a different population (red giants read
    /// fainter through a broadband filter than their G says) and may be
    /// saturated.
    pub bright_drop_fraction: f64,
    /// Analysis stars below this fit SNR are not used.
    pub min_snr: f64,
    /// A second catalogue star within this many FWHM of a star makes a blend.
    pub blend_radius_fwhm: f64,
    /// A second catalogue star inside the blend radius counts as a blend only
    /// when it is at most this many magnitudes fainter than the matched star
    /// (a neighbour 4 mag fainter adds 2.5 % of the flux).
    pub blend_dmag: f64,
}

impl Default for ZpParams {
    fn default() -> Self {
        Self {
            match_radius_px: 2.0,
            central_fraction: 0.9,
            min_stars: 50,
            max_std_error_mag: 0.035,
            clip_sigma: 4.0,
            bright_drop_fraction: 0.15,
            min_snr: 10.0,
            blend_radius_fwhm: 3.0,
            blend_dmag: 4.0,
        }
    }
}

/// Everything the measurement reads.
#[derive(Debug, Clone, Copy)]
pub struct ZpInput<'a> {
    pub stars: &'a [StarMetric],
    pub wcs: &'a WcsSolution,
    pub catalog: &'a [CatalogStar],
    pub exptime_s: Option<f64>,
    pub width: usize,
    pub height: usize,
    pub median_fwhm_px: f64,
}

/// A valid zero point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZeroPoint {
    /// Sigma-clipped median of the per-star zero points (mag).
    pub zero_point: f64,
    /// Survivors of the clipping.
    pub stars: u32,
    /// Robust scatter of the survivors (1.4826·MAD, mag).
    pub sigma: f64,
}

/// Why a frame has no zero point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZpRefusal {
    /// `EXPTIME` missing, not finite, or not positive.
    NoExposure,
    /// Fewer survivors than the minimum.
    TooFewStars { matched: u32, kept: u32 },
    /// The median's standard error is above the limit.
    TooScattered { std_error: f64, kept: u32 },
}

impl ZpRefusal {
    /// Stable code of the refusal, the value stored and logged.
    pub fn code(&self) -> &'static str {
        match self {
            ZpRefusal::NoExposure => "no_exposure",
            ZpRefusal::TooFewStars { .. } => "too_few_stars",
            ZpRefusal::TooScattered { .. } => "too_scattered",
        }
    }
}

/// One matched star, for the probe and the logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZpStar {
    /// Index of the analysis star in `ZpInput::stars`.
    pub star_index: usize,
    pub g_mag: f32,
    pub zp: f64,
    /// Distance from the frame centre as a fraction of the half-diagonal.
    pub radius_fraction: f64,
    /// Distance between the analysis star and its catalogue star (px).
    pub match_px: f64,
    /// Survived the sigma clipping.
    pub kept: bool,
}

/// The result with the numbers behind it.
#[derive(Debug, Clone)]
pub struct ZpMeasurement {
    pub result: Result<ZeroPoint, ZpRefusal>,
    /// One-to-one, unblended matches before clipping.
    pub matched: u32,
    pub median_match_px: Option<f64>,
    pub per_star: Vec<ZpStar>,
}

fn refused(r: ZpRefusal) -> ZpMeasurement {
    ZpMeasurement { result: Err(r), matched: 0, median_match_px: None, per_star: Vec::new() }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

/// Median and 1.4826·MAD of `values` (non-empty).
fn median_and_sigma(values: &[f64]) -> (f64, f64) {
    let mut v = values.to_vec();
    let m = median(&mut v);
    let mut dev: Vec<f64> = values.iter().map(|x| (x - m).abs()).collect();
    (m, MAD_TO_SIGMA * median(&mut dev))
}

/// Pixel-space bucket grid over the projected catalogue stars.
struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(points: &[(f64, f64, f32)], cell: f64) -> Self {
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            cells.entry(Self::key(cell, p.0, p.1)).or_default().push(i);
        }
        Self { cell, cells }
    }

    fn key(cell: f64, x: f64, y: f64) -> (i64, i64) {
        ((x / cell).floor() as i64, (y / cell).floor() as i64)
    }

    /// Indices of the points in the 3×3 cells around `(x, y)`; the caller
    /// tests the real distance. Correct for any radius up to `cell`.
    fn near(&self, x: f64, y: f64) -> impl Iterator<Item = usize> + '_ {
        let (cx, cy) = Self::key(self.cell, x, y);
        (-1..=1).flat_map(move |dx| {
            (-1..=1).flat_map(move |dy| {
                self.cells.get(&(cx + dx, cy + dy)).into_iter().flatten().copied()
            })
        })
    }
}

/// Why a clipped median is not a valid zero point.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ClipRefusal {
    /// Fewer survivors than the minimum (or none).
    TooFew { kept: u32 },
    /// The median's standard error is above the limit.
    TooScattered { std_error: f64, kept: u32, sigma: f64 },
}

/// The sigma-clipped median of per-star zero points and its verdict, the one statistic of `measure_zero_point` (the
/// anchor and the 6.5 budget zero point) and of the forced list.
struct Clipped {
    /// Per input value: survived the clipping.
    kept: Vec<bool>,
    verdict: Result<ZeroPoint, ClipRefusal>,
}

fn clipped_median(zps: &[f64], clip_sigma: f64, min_stars: usize, max_std_error_mag: f64) -> Clipped {
    let mut kept = vec![true; zps.len()];
    for _ in 0..MAX_CLIP_ROUNDS {
        let alive: Vec<f64> = zps.iter().zip(&kept).filter(|(_, k)| **k).map(|(z, _)| *z).collect();
        if alive.is_empty() {
            break;
        }
        let (m, sigma) = median_and_sigma(&alive);
        let limit = clip_sigma * sigma.max(CLIP_SIGMA_FLOOR_MAG);
        let mut dropped = false;
        for (z, k) in zps.iter().zip(kept.iter_mut()).filter(|(_, k)| **k) {
            if (z - m).abs() > limit {
                *k = false;
                dropped = true;
            }
        }
        if !dropped {
            break;
        }
    }
    let alive: Vec<f64> = zps.iter().zip(&kept).filter(|(_, k)| **k).map(|(z, _)| *z).collect();
    let n_kept = alive.len() as u32;
    let verdict = if alive.is_empty() || alive.len() < min_stars {
        Err(ClipRefusal::TooFew { kept: n_kept })
    } else {
        let (zero_point, sigma) = median_and_sigma(&alive);
        let std_error = sigma / (alive.len() as f64).sqrt();
        if std_error > max_std_error_mag {
            Err(ClipRefusal::TooScattered { std_error, kept: n_kept, sigma })
        } else {
            Ok(ZeroPoint { zero_point, stars: n_kept, sigma })
        }
    };
    Clipped { kept, verdict }
}

/// Measure a frame's zero point (spec §5.1). Never panics; a frame it cannot
/// measure comes back as a typed refusal with the counts behind it.
///
/// `input.catalog` must be a small cone around the frame (`catalog_for_frame`
/// makes one): a star 90° or more from the tangent point projects to a
/// mirrored position and would match a wrong analysis star.
pub fn measure_zero_point(input: &ZpInput, params: &ZpParams) -> ZpMeasurement {
    // 1. Exposure.
    let exptime = match input.exptime_s {
        Some(t) if t.is_finite() && t > 0.0 => t,
        _ => return refused(ZpRefusal::NoExposure),
    };

    let centre = ((input.width as f64 - 1.0) / 2.0, (input.height as f64 - 1.0) / 2.0);
    let half_diag = 0.5 * (input.width as f64).hypot(input.height as f64);
    let region = params.central_fraction * half_diag;
    let dist_c = |x: f64, y: f64| (x - centre.0).hypot(y - centre.1);

    // 2. Catalogue stars in pixels. The ones just outside the central region
    //    stay: they can still blend a star that lies inside it.
    let mut cat: Vec<(f64, f64, f32)> = Vec::with_capacity(input.catalog.len());
    for c in input.catalog {
        if !c.mag.is_finite() {
            continue;
        }
        let (x, y) = input.wcs.sky_to_pixel(c.ra, c.dec);
        if x.is_finite() && y.is_finite() {
            cat.push((x, y, c.mag));
        }
    }
    let blend_r = (params.blend_radius_fwhm * input.median_fwhm_px).max(0.0);
    let blend_r = if blend_r.is_finite() { blend_r } else { 0.0 };
    // Never below the match radius: a zero or NaN FWHM must not switch the
    // blend rule off.
    let blend_r = blend_r.max(params.match_radius_px);
    let grid = Grid::new(&cat, params.match_radius_px.max(blend_r).max(1.0));

    // 3. Analysis stars: central region, positive finite flux, enough SNR;
    //    the brightest fraction goes (a different population, maybe saturated).
    let mut usable: Vec<usize> = input
        .stars
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.x.is_finite()
                && s.y.is_finite()
                && s.flux.is_finite()
                && s.flux > 0.0
                && s.snr.is_some_and(|q| q.is_finite() && q >= params.min_snr)
                && dist_c(s.x, s.y) <= region
        })
        .map(|(i, _)| i)
        .collect();
    usable.sort_by(|&a, &b| input.stars[b].flux.total_cmp(&input.stars[a].flux).then(a.cmp(&b)));
    let drop = ((usable.len() as f64 * params.bright_drop_fraction) + 1e-9).floor() as usize;
    let usable = &usable[drop.min(usable.len())..];

    // 4. Nearest catalogue star inside the match radius; a blend is refused;
    //    one catalogue star serves one analysis star, the nearer one.
    let mut claims: HashMap<usize, (usize, f64)> = HashMap::new();
    for &si in usable {
        let s = &input.stars[si];
        let mut best: Option<(usize, f64)> = None;
        for ci in grid.near(s.x, s.y) {
            let d = (cat[ci].0 - s.x).hypot(cat[ci].1 - s.y);
            if d <= params.match_radius_px && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((ci, d));
            }
        }
        let Some((ci, d)) = best else { continue };
        let limit_mag = f64::from(cat[ci].2) + params.blend_dmag;
        let blended = grid.near(s.x, s.y).any(|cj| {
            cj != ci
                && f64::from(cat[cj].2) <= limit_mag
                && (cat[cj].0 - s.x).hypot(cat[cj].1 - s.y) <= blend_r
        });
        if blended {
            continue;
        }
        match claims.get(&ci) {
            Some(&(_, bd)) if bd <= d => {}
            _ => {
                claims.insert(ci, (si, d));
            }
        }
    }
    let mut matches: Vec<(usize, usize, f64)> =
        claims.into_iter().map(|(ci, (si, d))| (si, ci, d)).collect();
    matches.sort_by_key(|m| m.0);

    let mut per_star: Vec<ZpStar> = matches
        .iter()
        .map(|&(si, ci, d)| {
            let s = &input.stars[si];
            let g = cat[ci].2;
            ZpStar {
                star_index: si,
                g_mag: g,
                zp: f64::from(g) + 2.5 * (s.flux / exptime).log10(),
                radius_fraction: dist_c(s.x, s.y) / half_diag,
                match_px: d,
                kept: true,
            }
        })
        .collect();
    let matched = per_star.len() as u32;
    let median_match_px = if per_star.is_empty() {
        None
    } else {
        let mut px: Vec<f64> = per_star.iter().map(|p| p.match_px).collect();
        Some(median(&mut px))
    };

    // 5. + 6. Sigma-clipped median and the verdict.
    let zps: Vec<f64> = per_star.iter().map(|p| p.zp).collect();
    let clipped = clipped_median(&zps, params.clip_sigma, params.min_stars, params.max_std_error_mag);
    for (p, kept) in per_star.iter_mut().zip(&clipped.kept) {
        p.kept = *kept;
    }
    let result = match clipped.verdict {
        Ok(z) => Ok(z),
        Err(ClipRefusal::TooFew { kept }) => Err(ZpRefusal::TooFewStars { matched, kept }),
        Err(ClipRefusal::TooScattered { std_error, kept, .. }) => {
            Err(ZpRefusal::TooScattered { std_error, kept })
        }
    };
    ZpMeasurement { result, matched, median_match_px, per_star }
}

/// The catalogue stars a frame's measurement needs: one cone at the frame
/// centre, as wide as the central region plus a 10 % margin, across every
/// layer, with no magnitude cut. `epoch` is the observation epoch (decimal
/// year); proper motion is applied by the catalogue.
pub fn catalog_for_frame(
    layers: &[&StarCache],
    wcs: &WcsSolution,
    width: usize,
    height: usize,
    central_fraction: f64,
    epoch: f64,
) -> Result<Vec<CatalogStar>> {
    let (cx, cy) = ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
    let (ra, dec) = wcs.pixel_to_sky(cx, cy);
    let region_px = central_fraction * 0.5 * (width as f64).hypot(height as f64);
    // The larger CD column norm (deg/px): the cone must cover the long axis
    // of an anisotropic pixel.
    let scale_deg = (wcs.cd[0][0].hypot(wcs.cd[1][0])).max(wcs.cd[0][1].hypot(wcs.cd[1][1]));
    let radius_deg = region_px * scale_deg * CONE_MARGIN;
    if !(ra.is_finite() && dec.is_finite() && radius_deg.is_finite() && radius_deg > 0.0) {
        bail!(
            "the WCS gives no usable cone (centre {ra}, {dec}, radius {radius_deg} deg)"
        );
    }
    let cones = LayeredCones::new(layers, epoch);
    cones.cone(layers.len(), ra, dec, radius_deg, f32::INFINITY, epoch)
}

// ---------------------------------------------------------------------------
// The forced list (spec `2026-11-16-forced-list-zero-point-design.md` §5.1, §5.3)
// ---------------------------------------------------------------------------
//
// Pure, like `measure_zero_point`: a list of catalogue stars chosen before any pixel is read, from the frame's own
// preliminary zero point (the anchor), then the statistic over the fits the caller measured at those positions. The
// composition that reads a file and fits the list is `analysis::analyzer::measure_with_zero_point`.

/// Constants of the forced list, in one place (plan "Constants"). The probe may move a constant, never a bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedListParams {
    /// Floor of the reference width (px): the rig's PSF scale never goes below it.
    pub fwhm_floor_px: f64,
    /// The reference width in arcsec on a well-sampled rig (the rig's PSF scale).
    pub fwhm_ref_arcsec: f64,
    /// The anchor (6.5's matcher over the budget fits) needs at least this many matches.
    pub anchor_min_matches: usize,
    /// A star whose amplitude reaches this fraction of full scale (at `FWHM_ref`, Moffat beta 4) is the window's
    /// reference.
    pub window_peak_fraction: f64,
    /// The bright edge is this many magnitudes fainter than that reference star.
    pub window_bright_margin_mag: f64,
    /// The faint edge is this many magnitudes fainter than the bright one ...
    pub window_faint_width_mag: f64,
    /// ... deepened up to this many while fewer than `min_isolated` isolated stars lie in the window.
    pub window_faint_max_mag: f64,
    /// Isolated stars the window should hold before it is deepened.
    pub min_isolated: usize,
    /// Stars inside this fraction of the half-diagonal are listed.
    pub region_fraction: f64,
    /// Listed stars are at least this many `FWHM_ref` from every edge (the ring sky fits).
    pub edge_margin_fwhm: f64,
    /// A catalogue neighbour with `G <= G_i + isolation_dmag` within `isolation_radius_fwhm` drops a star.
    pub isolation_dmag: f64,
    pub isolation_radius_fwhm: f64,
    /// A catalogue star with `G <= G_b - bright_dmag` within `bright_radius_fwhm` (halo, bleed, spikes) drops a star.
    pub bright_dmag: f64,
    pub bright_radius_fwhm: f64,
    /// The list is capped at this many stars, by a hash of the catalogue record.
    pub cap: usize,
    /// A fit is kept when centred within `max(centre_tolerance_px, centre_tolerance_fwhm * FWHM_ref)` of its position.
    pub centre_tolerance_px: f64,
    pub centre_tolerance_fwhm: f64,
    /// The aperture correction is over at most this many kept fits.
    pub c_stars: usize,
    /// Sigma-clip threshold, fewest survivors and largest standard error (mag) of a valid zero point.
    pub clip_sigma: f64,
    pub min_stars: usize,
    pub max_std_error_mag: f64,
}

impl Default for ForcedListParams {
    fn default() -> Self {
        Self {
            fwhm_floor_px: 2.0,
            fwhm_ref_arcsec: 2.5,
            anchor_min_matches: 10,
            window_peak_fraction: 0.7,
            window_bright_margin_mag: 1.5,
            window_faint_width_mag: 3.0,
            window_faint_max_mag: 4.5,
            min_isolated: 200,
            region_fraction: 0.9,
            edge_margin_fwhm: 13.0,
            isolation_dmag: 4.0,
            isolation_radius_fwhm: 4.0,
            bright_dmag: 2.0,
            bright_radius_fwhm: 10.0,
            cap: 1000,
            centre_tolerance_px: 1.0,
            centre_tolerance_fwhm: 0.5,
            c_stars: 256,
            clip_sigma: 4.0,
            min_stars: 50,
            max_std_error_mag: 0.035,
        }
    }
}

/// Why a frame has no forced-list zero point. Every code but `catalogue_unreadable` is stored (an outcome a re-read of
/// the same file and solve would repeat); `catalogue_unreadable` is a failure of this run and stores nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ForcedRefusal {
    /// `EXPTIME` missing, not finite, or not positive.
    NoExposure,
    /// The anchor (6.5's matcher over the budget fits) found fewer than the needed matches.
    NoAnchor { matched: u32 },
    /// Fewer than 5 of the list's kept fits qualified for the aperture correction.
    NoApertureCorrection { qualifiers: u32 },
    /// Fewer survivors than the minimum, with the list's size before fitting.
    TooFewStars { list: u32, kept: u32 },
    /// The median's standard error is above the limit.
    TooScattered { std_error: f64, kept: u32, sigma: f64 },
    /// A debayered colour file: no list is measured on a three-plane image.
    ColourCamera,
    /// The stored solve gives no sky-to-pixel transform or no usable cone.
    UnreadableSolve,
    /// The plane has a verdict (trailed, defocused, no stars) that a re-read would repeat; the analysis row carries it.
    FrameVerdict,
    /// The catalogue could not be read this time; nothing is stored, a later run tries again.
    CatalogueUnreadable,
}

impl ForcedRefusal {
    /// Stable code of the refusal, the value stored and logged.
    pub fn code(&self) -> &'static str {
        match self {
            ForcedRefusal::NoExposure => "no_exposure",
            ForcedRefusal::NoAnchor { .. } => "no_anchor",
            ForcedRefusal::NoApertureCorrection { .. } => "no_aperture_correction",
            ForcedRefusal::TooFewStars { .. } => "too_few_stars",
            ForcedRefusal::TooScattered { .. } => "too_scattered",
            ForcedRefusal::ColourCamera => "colour_camera",
            ForcedRefusal::UnreadableSolve => "unreadable_solve",
            ForcedRefusal::FrameVerdict => "frame_verdict",
            ForcedRefusal::CatalogueUnreadable => "catalogue_unreadable",
        }
    }

    /// Whether the outcome is written to the catalogue (false: a failure of this run, retried later).
    pub fn is_stored(&self) -> bool {
        !matches!(self, ForcedRefusal::CatalogueUnreadable)
    }
}

/// The pixel scale of a solve (arcsec/px): the larger CD column norm, as `catalog_for_frame` sizes its cone.
pub fn pixel_scale_arcsec(wcs: &WcsSolution) -> f64 {
    wcs.cd[0][0].hypot(wcs.cd[1][0]).max(wcs.cd[0][1].hypot(wcs.cd[1][1])) * 3600.0
}

/// The reference width `FWHM_ref` (px): set by the rig's pixel scale, never by the night's seeing.
pub fn fwhm_ref_px(scale_arcsec: f64, params: &ForcedListParams) -> f64 {
    (params.fwhm_ref_arcsec / scale_arcsec).max(params.fwhm_floor_px)
}

/// The bright edge `G_b` of the window: 1.5 mag fainter than a Moffat (beta 4, `FWHM_ref`) star whose amplitude is 0.7
/// of `full_scale`, read through the frame's own preliminary zero point `anchor`. `full_scale` is in the fit's working
/// units (`saturation_plane`'s `norm`), the same units as the fluxes the anchor was measured from. A frame that lost
/// `L` mag moves the window by `L`.
pub fn window(anchor: f64, fwhm_ref: f64, full_scale: f64, exptime_s: f64, params: &ForcedListParams) -> f64 {
    // beta 4: FWHM = 2 alpha sqrt(2^(1/4) - 1); total flux = A pi alpha^2 / (beta - 1).
    let alpha = fwhm_ref / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
    let f_sat = params.window_peak_fraction * full_scale * std::f64::consts::PI * alpha * alpha / 3.0;
    let g_sat = anchor - 2.5 * (f_sat / exptime_s).log10();
    g_sat + params.window_bright_margin_mag
}

/// The frame's preliminary zero point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub zero_point: f64,
    /// Survivors of the clipping.
    pub stars: u32,
    /// One-to-one, unblended matches before clipping.
    pub matched: u32,
}

/// The anchor: 6.5's `measure_zero_point` over the budget's fits (their fluxes times `reported_c` on a mosaic, times 1
/// when the mosaic reported none or the plane is mono), valid from `anchor_min_matches` survivors with no standard-error
/// limit. Fewer survivors: `NoAnchor`; no exposure: `NoExposure`.
#[allow(clippy::too_many_arguments)]
pub fn anchor_zero_point(
    stars: &[StarMetric],
    reported_c: Option<f64>,
    wcs: &WcsSolution,
    catalog: &[CatalogStar],
    exptime_s: Option<f64>,
    width: usize,
    height: usize,
    median_fwhm_px: f64,
    params: &ForcedListParams,
) -> Result<Anchor, ForcedRefusal> {
    let scaled: Vec<StarMetric>;
    let stars = match reported_c {
        Some(c) if c.is_finite() && c > 0.0 && c != 1.0 => {
            scaled = stars.iter().map(|s| StarMetric { flux: s.flux * c, ..s.clone() }).collect();
            &scaled[..]
        }
        _ => stars,
    };
    let zp_params = ZpParams {
        min_stars: params.anchor_min_matches,
        max_std_error_mag: f64::INFINITY,
        ..ZpParams::default()
    };
    let input = ZpInput { stars, wcs, catalog, exptime_s, width, height, median_fwhm_px };
    let m = measure_zero_point(&input, &zp_params);
    match m.result {
        Ok(z) => Ok(Anchor { zero_point: z.zero_point, stars: z.stars, matched: m.matched }),
        Err(ZpRefusal::NoExposure) => Err(ForcedRefusal::NoExposure),
        Err(ZpRefusal::TooFewStars { matched, .. } | ZpRefusal::TooScattered { kept: matched, .. }) => {
            Err(ForcedRefusal::NoAnchor { matched })
        }
    }
}

/// Everything the list builder reads. It takes no FWHM: the list follows the rig and the frame's light, never the
/// night's seeing.
#[derive(Debug, Clone, Copy)]
pub struct ForcedListInput<'a> {
    /// The catalogue cone of the frame (`catalog_for_frame`), proper motion applied.
    pub catalog: &'a [CatalogStar],
    pub wcs: &'a WcsSolution,
    pub width: usize,
    pub height: usize,
    /// The frame's preliminary zero point ([`anchor_zero_point`]).
    pub anchor: f64,
    /// The fit's working-unit full scale (`Domain::RawAdu::full_scale`, `ADU_SCALE` for a calibrated plane).
    pub full_scale: f64,
    pub exptime_s: f64,
}

/// One star of the list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedStar {
    /// The cap's hash key of its catalogue record.
    pub key: u64,
    pub g_mag: f32,
    pub ra: f64,
    pub dec: f64,
    /// The projected position, 0-based, file row order.
    pub x: f64,
    pub y: f64,
}

/// The list and the window it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedList {
    /// The capped list, in key order.
    pub stars: Vec<ForcedStar>,
    /// The list's size before the cap.
    pub list_before_cap: usize,
    /// `G_b` and the faint edge of the window.
    pub window_bright: f64,
    pub window_faint: f64,
    pub fwhm_ref_px: f64,
    pub scale_arcsec: f64,
    /// What the fitter is asked: the positions in `stars` order and the centre tolerance.
    pub request: ListRequest,
}

/// The cap's key: xxh3-64 of the little-endian bytes of (`round(G * 1000)` as i32, `round(ra * 3600)` as i64,
/// `round(dec * 3600)` as i64), so the same record has the same key in every frame.
pub fn cap_key(g_mag: f32, ra_deg: f64, dec_deg: f64) -> u64 {
    let mut bytes = [0u8; 20];
    bytes[0..4].copy_from_slice(&((f64::from(g_mag) * 1000.0).round() as i32).to_le_bytes());
    bytes[4..12].copy_from_slice(&((ra_deg * 3600.0).round() as i64).to_le_bytes());
    bytes[12..20].copy_from_slice(&((dec_deg * 3600.0).round() as i64).to_le_bytes());
    xxhash_rust::xxh3::xxh3_64(&bytes)
}

/// 2^64 as f64: a key over it is a fraction in [0, 1).
const KEY_SPACE: f64 = 18_446_744_073_709_551_616.0;

/// Build the list (spec §5.1): the catalogue stars inside the region and the edge margin, isolated by the catalogue
/// alone, in the window `[G_b, faint]`, capped by hash. Reads no pixel and no seeing.
pub fn forced_list(input: &ForcedListInput, params: &ForcedListParams) -> Result<ForcedList, ForcedRefusal> {
    if !(input.exptime_s.is_finite() && input.exptime_s > 0.0) {
        return Err(ForcedRefusal::NoExposure);
    }
    let scale = pixel_scale_arcsec(input.wcs);
    let fwhm_ref = fwhm_ref_px(scale, params);
    let g_b = window(input.anchor, fwhm_ref, input.full_scale, input.exptime_s, params);
    if !(g_b.is_finite() && fwhm_ref.is_finite()) {
        return Err(ForcedRefusal::NoAnchor { matched: 0 });
    }

    // The cone in pixels (non-finite skipped, as `measure_zero_point` does), with each star's sky record.
    let mut cat: Vec<(f64, f64, f32)> = Vec::with_capacity(input.catalog.len());
    let mut sky: Vec<(f64, f64)> = Vec::with_capacity(input.catalog.len());
    for c in input.catalog {
        if !c.mag.is_finite() {
            continue;
        }
        let (x, y) = input.wcs.sky_to_pixel(c.ra, c.dec);
        if x.is_finite() && y.is_finite() {
            cat.push((x, y, c.mag));
            sky.push((c.ra, c.dec));
        }
    }
    let near_r = params.isolation_radius_fwhm * fwhm_ref;
    let far_r = params.bright_radius_fwhm * fwhm_ref;
    let grid = Grid::new(&cat, far_r.max(near_r).max(1.0));

    let (w, h) = (input.width as f64, input.height as f64);
    let centre = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let region = params.region_fraction * 0.5 * w.hypot(h);
    let margin = params.edge_margin_fwhm * fwhm_ref;
    let faint_limit = g_b + params.window_faint_max_mag;

    // Isolated candidates of the deepest window.
    let mut isolated: Vec<(f64, usize)> = Vec::new();
    for (i, &(x, y, g)) in cat.iter().enumerate() {
        let g = f64::from(g);
        if g < g_b || g > faint_limit {
            continue;
        }
        if (x - centre.0).hypot(y - centre.1) > region
            || x < margin
            || y < margin
            || x > w - 1.0 - margin
            || y > h - 1.0 - margin
        {
            continue;
        }
        let crowded = grid.near(x, y).any(|j| {
            if j == i {
                return false;
            }
            let d = (cat[j].0 - x).hypot(cat[j].1 - y);
            let gj = f64::from(cat[j].2);
            (gj <= g + params.isolation_dmag && d <= near_r) || (gj <= g_b - params.bright_dmag && d <= far_r)
        });
        if !crowded {
            isolated.push((g, i));
        }
    }

    // The faint edge: G_b + 3, deepened by catalogue count up to G_b + 4.5 while the window is thin.
    let shallow_edge = g_b + params.window_faint_width_mag;
    let in_shallow = isolated.iter().filter(|(g, _)| *g <= shallow_edge).count();
    let window_faint = if in_shallow >= params.min_isolated {
        shallow_edge
    } else {
        let mut gs: Vec<f64> = isolated.iter().map(|(g, _)| *g).collect();
        gs.sort_by(|a, b| a.total_cmp(b));
        match gs.get(params.min_isolated.saturating_sub(1)) {
            Some(&g200) if params.min_isolated > 0 => g200.min(faint_limit),
            _ => faint_limit,
        }
    };

    let kept: Vec<usize> =
        isolated.iter().filter(|(g, _)| *g <= window_faint).map(|&(_, i)| i).collect();
    let list_before_cap = kept.len();
    let share = params.cap as f64 / list_before_cap.max(1) as f64;
    let mut stars: Vec<ForcedStar> = kept
        .into_iter()
        .filter_map(|i| {
            let (x, y, g) = cat[i];
            let (ra, dec) = sky[i];
            let key = cap_key(g, ra, dec);
            (key as f64 / KEY_SPACE < share).then_some(ForcedStar { key, g_mag: g, ra, dec, x, y })
        })
        .collect();
    stars.sort_by_key(|s| s.key);

    let tolerance = params.centre_tolerance_px.max(params.centre_tolerance_fwhm * fwhm_ref);
    let mut request = ListRequest::new(stars.iter().map(|s| (s.x, s.y)).collect(), tolerance);
    request.max_c_stars = params.c_stars;
    Ok(ForcedList {
        stars,
        list_before_cap,
        window_bright: g_b,
        window_faint,
        fwhm_ref_px: fwhm_ref,
        scale_arcsec: scale,
        request,
    })
}

/// One list star's contribution to the statistic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedStarZp {
    /// Index into [`ForcedList::stars`] and [`ListMeasurement::fits`].
    pub index: usize,
    /// `G + 2.5·log10(flux / t)`.
    pub zp: f64,
    /// Survived the sigma clipping.
    pub clipped_in: bool,
}

/// A valid forced-list zero point with the figures the catalogue stores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedZeroPoint {
    /// Sigma-clipped median of the per-star zero points (mag).
    pub zero_point: f64,
    /// Survivors of the clipping.
    pub stars: u32,
    /// Robust scatter of the survivors (1.4826·MAD, mag).
    pub sigma: f64,
}

/// The statistic's result with every figure behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedOutcome {
    pub result: Result<ForcedZeroPoint, ForcedRefusal>,
    /// The list before fitting (after the cap).
    pub list_stars: u32,
    /// Fits kept by the fitter (accepted, unsaturated, centred, with a positive finite flux), before the clipping.
    pub fitted_kept: u32,
    /// The list's aperture correction `c` (None when it refused), the qualifiers it is over and its standard error.
    pub aperture_correction: Option<f64>,
    pub aperture_stars: u32,
    pub aperture_se: Option<f64>,
    pub window_bright: f64,
    pub per_star: Vec<ForcedStarZp>,
}

/// The statistic (spec §5.3): `zp_i = G_i + 2.5·log10(flux_i / t)` per kept star, the 4 sigma-clipped median, valid
/// with at least 50 survivors and a standard error within 0.035 mag. `measured` is the fitter's answer for `list`.
pub fn forced_zero_point(
    list: &ForcedList,
    measured: &ListMeasurement,
    exptime_s: f64,
    params: &ForcedListParams,
) -> ForcedOutcome {
    let list_stars = list.stars.len() as u32;
    let base = |result, fitted_kept, per_star| ForcedOutcome {
        result,
        list_stars,
        fitted_kept,
        aperture_correction: measured.aperture_correction,
        aperture_stars: measured.aperture_stars as u32,
        aperture_se: measured.aperture_se,
        window_bright: list.window_bright,
        per_star,
    };
    if !(exptime_s.is_finite() && exptime_s > 0.0) {
        return base(Err(ForcedRefusal::NoExposure), 0, Vec::new());
    }
    if measured.aperture_correction.is_none() {
        return base(
            Err(ForcedRefusal::NoApertureCorrection { qualifiers: measured.aperture_stars as u32 }),
            0,
            Vec::new(),
        );
    }
    if measured.fits.len() != list.stars.len() {
        tracing::error!(
            list = list.stars.len(),
            fits = measured.fits.len(),
            "forced list and its fits differ in length"
        );
        return base(Err(ForcedRefusal::TooFewStars { list: list_stars, kept: 0 }), 0, Vec::new());
    }
    let mut used: Vec<(usize, f64)> = Vec::new();
    for (i, (star, fit)) in list.stars.iter().zip(&measured.fits).enumerate() {
        if fit.kept && fit.flux.is_finite() && fit.flux > 0.0 {
            used.push((i, f64::from(star.g_mag) + 2.5 * (fit.flux / exptime_s).log10()));
        }
    }
    let zps: Vec<f64> = used.iter().map(|u| u.1).collect();
    let clipped = clipped_median(&zps, params.clip_sigma, params.min_stars, params.max_std_error_mag);
    let per_star: Vec<ForcedStarZp> = used
        .iter()
        .zip(&clipped.kept)
        .map(|(&(index, zp), &clipped_in)| ForcedStarZp { index, zp, clipped_in })
        .collect();
    let result = match clipped.verdict {
        Ok(z) => Ok(ForcedZeroPoint { zero_point: z.zero_point, stars: z.stars, sigma: z.sigma }),
        Err(ClipRefusal::TooFew { kept }) => Err(ForcedRefusal::TooFewStars { list: list_stars, kept }),
        Err(ClipRefusal::TooScattered { std_error, kept, sigma }) => {
            Err(ForcedRefusal::TooScattered { std_error, kept, sigma })
        }
    };
    base(result, used.len() as u32, per_star)
}

// ---------------------------------------------------------------------------
// Storage and currency
// ---------------------------------------------------------------------------

/// Frames per chunk of an `ensure_zero_points` lookup and of the startup backfill.
pub const BACKFILL_CHUNK: usize = 200;

/// Why a frame was not judged now. `ColourCamera` is stored like a refusal (owner ruling Z5: colour cameras wait for an
/// aperture sum); every other reason stores nothing, so the frame is picked up when the missing piece arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    /// The frame has no analysis row.
    NoAnalysis,
    /// The analysis was measured by an older star-measurement version; its star fluxes are not comparable.
    StaleAnalysis,
    /// The frame has no plate solve.
    NoSolve,
    /// The stored solve cannot be turned into a sky-to-pixel transform (an unreadable SIP table, or a forward SIP
    /// with no reverse one).
    UnreadableSolve,
    /// No star catalogue is installed.
    NoCatalogue,
    /// The catalogue knows the frame as OSC or CFA.
    ColourCamera,
    /// The analysis reports detected stars but holds no star rows: it arrived through personal sync, which carries the
    /// frame's summary and not its stars. Not "too few stars": nothing was measured, so nothing is stored.
    NoStarMetrics,
    /// The analysis row or the solve changed between the read and the write (a re-analysis, a new or deleted solve);
    /// nothing was stored, and whatever superseded the judgement judges the frame again.
    Superseded,
}

impl NotReady {
    /// Stable code for the logs.
    pub fn code(self) -> &'static str {
        match self {
            NotReady::NoAnalysis => "no_analysis",
            NotReady::StaleAnalysis => "stale_analysis",
            NotReady::NoSolve => "no_solve",
            NotReady::UnreadableSolve => "unreadable_solve",
            NotReady::NoCatalogue => "no_catalogue",
            NotReady::ColourCamera => "colour_camera",
            NotReady::NoStarMetrics => "no_star_metrics",
            NotReady::Superseded => "superseded",
        }
    }
}

/// What a refresh did with a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZpStatus {
    /// A zero point was measured and stored.
    Measured(ZeroPoint),
    /// The measurement refused; the refusal was stored (NULL value, current version and catalogue).
    Refused(ZpRefusal),
    /// Not judged (see [`NotReady`]); `ColourCamera` was stored, the rest stored nothing.
    NotReady(NotReady),
}

impl ZpStatus {
    /// Whether the refresh wrote a row (a value, a refusal or a colour-camera mark).
    pub fn stored(&self) -> bool {
        matches!(
            self,
            ZpStatus::Measured(_) | ZpStatus::Refused(_) | ZpStatus::NotReady(NotReady::ColourCamera)
        )
    }
}

/// The installed star catalogue as a zero point needs it: the opened tiers and their identity.
pub struct ZpCatalog {
    pub layers: Vec<Arc<StarCache>>,
    /// `plate_solve::layers::catalog_identity`: what `frame_analysis.zp_catalog` is compared with.
    pub identity: String,
}

/// Open the installed catalogue under `app_data_dir`. `None` when none is installed (logged at `debug`: a fresh install
/// or a Docker build has none, and that is not an error) or when it cannot be opened (`warn`).
pub fn zero_point_catalog(app_data_dir: &Path) -> Option<ZpCatalog> {
    let identity = match catalog_identity(app_data_dir) {
        Ok(i) => i,
        Err(CatalogOpenError::NotInstalled { .. }) => {
            tracing::debug!("zero point skipped: no star catalogue installed");
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "zero point skipped: star catalogue unreadable");
            return None;
        }
    };
    match open_catalog_layers(app_data_dir) {
        Ok(layers) => Some(ZpCatalog { layers, identity }),
        Err(CatalogOpenError::NotInstalled { .. }) => {
            tracing::debug!("zero point skipped: no star catalogue installed");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "zero point skipped: star catalogue unreadable");
            None
        }
    }
}

/// Whether a frame is a colour frame, which the zero point does not measure yet (owner ruling Z5): the catalogue knows
/// it as OSC or CFA (`BAYERPAT` set, the derivation the stacking group key uses), or its analysis found more than one
/// plane (`frame_analysis.source_channels > 1`: an externally debayered file carries no `BAYERPAT` and three planes).
/// The one test both the measurement and the transparency read use.
pub(crate) fn is_colour_frame(bayerpat: Option<&str>, source_channels: Option<i64>) -> bool {
    color_mode_of(&bayerpat.map(str::to_owned)) == ColorMode::Osc || source_channels.is_some_and(|c| c > 1)
}

/// The frame's own header facts the measurement needs.
struct FrameFacts {
    exptime: Option<f64>,
    date_obs: Option<chrono::DateTime<chrono::Utc>>,
    bayerpat: Option<String>,
}

fn load_facts(conn: &Connection, frame_id: i64) -> Result<Option<FrameFacts>> {
    conn.query_row(
        "SELECT exptime, date_obs, bayerpat FROM frames WHERE id = ?1",
        [frame_id],
        |row| {
            let date_obs: Option<String> = row.get(1)?;
            Ok(FrameFacts {
                exptime: row.get(0)?,
                date_obs: date_obs.as_deref().and_then(parse_date_obs),
                bayerpat: row.get(2)?,
            })
        },
    )
    .optional()
    .with_context(|| format!("reading the header facts of frame {frame_id}"))
}

/// `frames.date_obs` is stored as RFC 3339; a header-style text without a zone is read as UTC.
fn parse_date_obs(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let text = text.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(t.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok().map(|n| n.and_utc())
}

/// What a judgement read: the analysis row it measured and, for a measurement or a refusal that needed the solve, the
/// solve row. The write is conditional on both still being the frame's (compare-and-set).
#[derive(Clone, Copy)]
struct Judged {
    analysis_id: i64,
    solve_id: Option<i64>,
}

/// Store a judgement and log it with the dictionary fields. `Ok(false)`: the analysis or the solve was replaced since
/// it was read, nothing was written (`debug`, not an error).
fn store_outcome(
    conn: &Connection,
    frame_id: i64,
    judged: Judged,
    catalog: &str,
    value: Option<&ZeroPoint>,
    matched: Option<u32>,
    reason: Option<&str>,
) -> Result<bool> {
    let written = db_analysis::set_zero_point(conn, frame_id, judged.analysis_id, judged.solve_id, value, catalog)
        .with_context(|| format!("storing the zero point of frame {frame_id}"))?;
    if !written {
        tracing::debug!(frame_id, "zero point superseded");
        return Ok(false);
    }
    match value {
        Some(z) => tracing::debug!(
            frame_id,
            zero_point = z.zero_point,
            zp_stars = z.stars,
            zp_sigma = z.sigma,
            zp_matched = matched,
            "zero point measured"
        ),
        None => tracing::debug!(frame_id, zp_matched = matched, zp_reason = reason, "zero point refused"),
    }
    Ok(true)
}

/// Judge one frame and store the outcome. Reads the analysis (it must carry the current `MEASURE_VERSION`), its star
/// rows, the solve and the header's `EXPTIME`; asks `cone` for the catalogue stars (`wcs`, width, height, central
/// fraction, observation epoch); measures; stores through `set_zero_point` with `catalog` (the installed catalogue's
/// identity). Always recomputes: the currency check belongs to the callers that have many frames to skip.
/// The write is a compare-and-set on the analysis row and the solve row it read: if either was replaced meanwhile,
/// nothing is stored and the status is `NotReady(Superseded)`.
/// An `Err` is a database or catalogue failure; a frame that merely cannot be judged is `ZpStatus::NotReady`.
pub fn refresh_zero_point_with<F>(conn: &Connection, frame_id: i64, catalog: &str, cone: F) -> Result<ZpStatus>
where
    F: FnOnce(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    let Some(analysis) = db_analysis::get_frame_analysis(conn, frame_id)
        .with_context(|| format!("reading the analysis of frame {frame_id}"))?
    else {
        return Ok(ZpStatus::NotReady(NotReady::NoAnalysis));
    };
    let Some(analysis_id) = analysis.id else {
        return Ok(ZpStatus::NotReady(NotReady::NoAnalysis));
    };
    if analysis.measure_version != Some(MEASURE_VERSION) {
        return Ok(ZpStatus::NotReady(NotReady::StaleAnalysis));
    }
    let Some(facts) = load_facts(conn, frame_id)? else {
        return Ok(ZpStatus::NotReady(NotReady::NoAnalysis));
    };

    // Z5: a colour camera is not measured yet. Stored like a refusal, so it is not retried.
    if is_colour_frame(facts.bayerpat.as_deref(), Some(analysis.source_channels)) {
        // No solve is read for a colour camera: the analysis id alone is the condition of the write.
        let judged = Judged { analysis_id, solve_id: None };
        let written = store_outcome(conn, frame_id, judged, catalog, None, None, Some(NotReady::ColourCamera.code()))?;
        return Ok(ZpStatus::NotReady(if written { NotReady::ColourCamera } else { NotReady::Superseded }));
    }

    let Some(record) = get_plate_solve(conn, frame_id)
        .with_context(|| format!("reading the solve of frame {frame_id}"))?
    else {
        return Ok(ZpStatus::NotReady(NotReady::NoSolve));
    };
    let Some(solve_id) = record.id else {
        return Ok(ZpStatus::NotReady(NotReady::NoSolve));
    };
    // A frame whose solve cannot be read is said once per pass by the caller (a count at `warn`), not once per frame.
    let wcs = match record.try_solution() {
        Ok(wcs) => wcs,
        Err(why) => {
            tracing::debug!(frame_id, reason = %why, "zero point not measured: the solve cannot be read");
            return Ok(ZpStatus::NotReady(NotReady::UnreadableSolve));
        }
    };
    if wcs.sip_forward.is_some() && wcs.sip_reverse.is_none() {
        tracing::debug!(frame_id, "zero point not measured: the solve has a forward SIP and no reverse one");
        return Ok(ZpStatus::NotReady(NotReady::UnreadableSolve));
    }

    let params = ZpParams::default();

    // A frame with no usable exposure is refused before the catalogue is read.
    let exposure_ok = facts.exptime.is_some_and(|t| t.is_finite() && t > 0.0);
    let (width, height) = (analysis.width.max(0) as usize, analysis.height.max(0) as usize);
    let (m, refusal_only) = if exposure_ok {
        let stars = db_analysis::get_star_metrics(conn, analysis_id)
            .with_context(|| format!("reading the stars of frame {frame_id}"))?;
        if stars.is_empty() && analysis.stars_detected > 0 {
            // A synced analysis: the summary arrived, the star rows did not. Not a measurement of "too few stars".
            return Ok(ZpStatus::NotReady(NotReady::NoStarMetrics));
        }
        let epoch = observation_epoch(&Frame { date_obs: facts.date_obs, ..Frame::default() });
        let cat = cone(&wcs, width, height, params.central_fraction, epoch)
            .with_context(|| format!("reading the catalogue around frame {frame_id}"))?;
        let m = measure_zero_point(
            &ZpInput {
                stars: &stars,
                wcs: &wcs,
                catalog: &cat,
                exptime_s: facts.exptime,
                width,
                height,
                median_fwhm_px: analysis.median_fwhm,
            },
            &params,
        );
        (m, false)
    } else {
        (refused(ZpRefusal::NoExposure), true)
    };
    let matched = (!refusal_only).then_some(m.matched);

    let judged = Judged { analysis_id, solve_id: Some(solve_id) };
    match m.result {
        Ok(z) => {
            let written = store_outcome(conn, frame_id, judged, catalog, Some(&z), matched, None)?;
            Ok(if written { ZpStatus::Measured(z) } else { ZpStatus::NotReady(NotReady::Superseded) })
        }
        Err(r) => {
            let written = store_outcome(conn, frame_id, judged, catalog, None, matched, Some(r.code()))?;
            Ok(if written { ZpStatus::Refused(r) } else { ZpStatus::NotReady(NotReady::Superseded) })
        }
    }
}

/// The production catalogue reader for [`refresh_zero_point_with`]'s `cone`.
pub(crate) fn catalog_cone<'a>(
    layers: &'a [Arc<StarCache>],
) -> impl Fn(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>> + 'a {
    move |wcs, width, height, central_fraction, epoch| {
        let refs: Vec<&StarCache> = layers.iter().map(Arc::as_ref).collect();
        catalog_for_frame(&refs, wcs, width, height, central_fraction, epoch)
    }
}

/// Whether the frame has a current-`MEASURE_VERSION` analysis and a solve and its zero point is not current: judged at
/// another `ZP_VERSION` or against another catalogue.
fn needs_refresh(conn: &Connection, frame_id: i64, catalog: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM frame_analysis fa
             JOIN plate_solves ps ON ps.frame_id = fa.frame_id
             WHERE fa.frame_id = ?1 AND fa.measure_version = ?2
               AND (fa.zp_version IS NOT ?3 OR fa.zp_catalog IS NOT ?4)",
            params![frame_id, MEASURE_VERSION, ZP_VERSION, catalog],
            |r| r.get(0),
        )
        .with_context(|| format!("checking the zero point of frame {frame_id}"))?;
    Ok(n > 0)
}

/// What one pass over many frames did, for the single line it says at the end (never a line per frame).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ZpPassStats {
    /// Frames that wrote a row (a value, a refusal or a colour-camera mark).
    pub stored: usize,
    /// Frames whose solve cannot be read: nothing stored, `debug` per frame.
    pub unreadable: usize,
    /// Frames whose check or refresh failed (a database or catalogue error): left as they were, `debug` per frame.
    pub failed: usize,
    /// The first failure's text, for the pass's one `warn`.
    pub first_error: Option<String>,
}

impl ZpPassStats {
    /// Fold the stats of another chunk into this pass.
    pub fn add(&mut self, other: ZpPassStats) {
        self.stored += other.stored;
        self.unreadable += other.unreadable;
        self.failed += other.failed;
        if self.first_error.is_none() {
            self.first_error = other.first_error;
        }
    }

    /// The pass's one `warn`: the count of frames that could not be measured, when there are any. `pass` names the
    /// caller (`backfill`, `gate`, `trigger`).
    pub fn warn_if_unmeasurable(&self, pass: &'static str) {
        let skipped = self.unreadable + self.failed;
        if skipped > 0 {
            tracing::warn!(
                pass,
                frames = skipped,
                zp_unreadable = self.unreadable,
                zp_failed = self.failed,
                error = self.first_error.as_deref(),
                "zero point: frames could not be measured and were skipped"
            );
        }
    }
}

/// Refresh each of `frame_ids` (`only_stale`: only those that are not current). A frame whose refresh fails is counted,
/// logged at `debug` with its id and left as it was; the rest go on. Each frame is read once. Says nothing at `warn`:
/// the caller prints [`ZpPassStats::warn_if_unmeasurable`] once for its whole pass.
fn refresh_many_stats<F>(conn: &Connection, catalog: &str, frame_ids: &[i64], only_stale: bool, mut cone: F) -> ZpPassStats
where
    F: FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    let mut stats = ZpPassStats::default();
    // Counts a frame's failure into the pass and returns its text; each call site says its own `debug` line.
    let failed = |stats: &mut ZpPassStats, e: anyhow::Error| -> String {
        let text = format!("{e:#}");
        stats.failed += 1;
        stats.first_error.get_or_insert_with(|| text.clone());
        text
    };
    for &frame_id in frame_ids {
        if only_stale {
            match needs_refresh(conn, frame_id, catalog) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    let text = failed(&mut stats, e);
                    tracing::debug!(frame_id, error = %text, "zero point check failed");
                    continue;
                }
            }
        }
        match refresh_zero_point_with(conn, frame_id, catalog, &mut cone) {
            Ok(status) => {
                stats.stored += usize::from(status.stored());
                stats.unreadable += usize::from(status == ZpStatus::NotReady(NotReady::UnreadableSolve));
            }
            Err(e) => {
                let text = failed(&mut stats, e);
                tracing::debug!(frame_id, error = %text, "zero point refresh failed");
            }
        }
    }
    stats
}

/// [`refresh_many_stats`] as one pass of its own: says its one `warn` (`trigger`) and returns how many frames wrote a row.
/// A frame's failure is counted and logged inside the pass; the pass itself cannot fail.
fn refresh_many_with<F>(conn: &Connection, catalog: &str, frame_ids: &[i64], only_stale: bool, cone: F) -> usize
where
    F: FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    let stats = refresh_many_stats(conn, catalog, frame_ids, only_stale, cone);
    stats.warn_if_unmeasurable("trigger");
    stats.stored
}

/// Refresh every one of `frame_ids` (the caller has already selected the stale ones), no currency check per frame; the
/// caller owns the pass's one `warn` ([`ZpPassStats::warn_if_unmeasurable`]).
pub fn refresh_listed_with<F>(conn: &Connection, catalog: &str, frame_ids: &[i64], cone: F) -> ZpPassStats
where
    F: FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    refresh_many_stats(conn, catalog, frame_ids, false, cone)
}

/// Refresh the frames among `frame_ids` whose zero point is not current (a current-`MEASURE_VERSION` analysis, a solve,
/// and `zp_version` / `zp_catalog` other than the current ones): the trigger after an analysis is saved. Returns how many
/// it measured, refused or marked as a colour camera. `cone` as in [`refresh_zero_point_with`]. Never fails.
pub fn ensure_zero_points_with<F>(conn: &Connection, catalog: &str, frame_ids: &[i64], cone: F) -> usize
where
    F: FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    refresh_many_with(conn, catalog, frame_ids, true, cone)
}

/// Recompute the zero point of every one of `frame_ids`, current or not: the trigger after a solve, whose WCS may have
/// moved. Returns how many wrote a row. Never fails.
pub fn recompute_zero_points_with<F>(conn: &Connection, catalog: &str, frame_ids: &[i64], cone: F) -> usize
where
    F: FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>>,
{
    refresh_many_with(conn, catalog, frame_ids, false, cone)
}

/// The stale selections' extra condition (alias `fa` is `frame_analysis`): the frame CAN be measured here. An analysis that
/// arrived through personal sync has `stars_detected > 0` and no `star_metrics` rows; it stores nothing
/// ([`NotReady::NoStarMetrics`]), so it would stay "stale" forever and be re-read by every backfill and gate pass.
const CAN_BE_MEASURED: &str = "(fa.stars_detected = 0
               OR EXISTS (SELECT 1 FROM star_metrics sm WHERE sm.frame_analysis_id = fa.id))";

/// Up to `limit` frame ids above `after` whose zero point is not current and that can be measured ([`CAN_BE_MEASURED`]),
/// in id order: the backfill's cursor. Keyed on
/// the id, so a frame whose refresh keeps failing is skipped, not retried in a loop.
pub fn stale_zero_point_frames(conn: &Connection, catalog: &str, after: i64, limit: usize) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT fa.frame_id FROM frame_analysis fa
         JOIN plate_solves ps ON ps.frame_id = fa.frame_id
         WHERE fa.frame_id > ?1 AND fa.measure_version = ?2
           AND (fa.zp_version IS NOT ?3 OR fa.zp_catalog IS NOT ?4)
           AND {CAN_BE_MEASURED}
         ORDER BY fa.frame_id LIMIT ?5"
    );
    let mut stmt = conn.prepare(&sql).context("preparing the stale zero point query")?;
    let ids = stmt
        .query_map(params![after, MEASURE_VERSION, ZP_VERSION, catalog, limit as i64], |r| r.get(0))
        .context("listing the frames with a stale zero point")?
        .collect::<rusqlite::Result<Vec<i64>>>()
        .context("reading the frames with a stale zero point")?;
    Ok(ids)
}

/// The frames among `frame_ids` whose zero point is not current and that can be measured ([`CAN_BE_MEASURED`]) (a
/// current-`MEASURE_VERSION` analysis and a solve, and `zp_version` / `zp_catalog` other than the current ones), in id order: [`stale_zero_point_frames`] for a known set.
/// One statement per chunk of [`BACKFILL_CHUNK`] ids, never one per frame.
pub fn stale_among_frames(conn: &Connection, catalog: &str, frame_ids: &[i64]) -> Result<Vec<i64>> {
    stale_among_frames_counted(conn, catalog, frame_ids).map(|(ids, _)| ids)
}

/// [`stale_among_frames`] and the number of statements it prepared (what its test reads: one per chunk).
fn stale_among_frames_counted(conn: &Connection, catalog: &str, frame_ids: &[i64]) -> Result<(Vec<i64>, usize)> {
    let mut out = Vec::new();
    let mut statements = 0;
    for chunk in frame_ids.chunks(BACKFILL_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT fa.frame_id FROM frame_analysis fa
             JOIN plate_solves ps ON ps.frame_id = fa.frame_id
             WHERE fa.measure_version = ? AND (fa.zp_version IS NOT ? OR fa.zp_catalog IS NOT ?)
               AND {CAN_BE_MEASURED}
               AND fa.frame_id IN ({marks})
             ORDER BY fa.frame_id"
        );
        statements += 1;
        let mut stmt = conn.prepare(&sql).context("preparing the stale zero point selection")?;
        let mut bound: Vec<rusqlite::types::Value> =
            vec![i64::from(MEASURE_VERSION).into(), i64::from(ZP_VERSION).into(), catalog.to_owned().into()];
        bound.extend(chunk.iter().map(|id| rusqlite::types::Value::from(*id)));
        let ids = stmt
            .query_map(rusqlite::params_from_iter(bound.iter()), |r| r.get::<_, i64>(0))
            .context("selecting the frames with a stale zero point")?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .context("reading the frames with a stale zero point")?;
        out.extend(ids);
    }
    out.sort_unstable();
    Ok((out, statements))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPTIME: f64 = 120.0;
    const TRUE_ZP: f64 = 21.30;
    const W: usize = 4000;
    const H: usize = 3000;

    struct Field {
        stars: Vec<StarMetric>,
        catalog: Vec<CatalogStar>,
        wcs: WcsSolution,
    }

    /// Deterministic standard normal draws (xorshift64* + Box-Muller).
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn unit(&mut self) -> f64 {
            ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn normal(&mut self) -> f64 {
            let (u, v) = (self.unit(), self.unit());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }
    }

    fn wcs() -> WcsSolution {
        let s = 1.0 / 3600.0;
        WcsSolution {
            crpix: ((W as f64 - 1.0) / 2.0, (H as f64 - 1.0) / 2.0),
            crval: (150.0, 20.0),
            cd: [[-s, 0.0], [0.0, s]],
            sip_forward: None,
            sip_reverse: None,
        }
    }

    fn star(x: f64, y: f64, flux: f64) -> StarMetric {
        StarMetric {
            id: None,
            frame_analysis_id: 0,
            x,
            y,
            peak: 1000.0,
            flux,
            fwhm: 4.0,
            fwhm_x: 4.0,
            fwhm_y: 4.0,
            eccentricity: 0.1,
            snr: Some(100.0),
            hfr: None,
            theta: 0.0,
            beta: None,
            fit_method: "test".to_string(),
            fit_residual: 0.0,
        }
    }

    fn flux_for(g: f64, zp: f64) -> f64 {
        EXPTIME * 10f64.powf(0.4 * (zp - g))
    }

    fn cat_at(w: &WcsSolution, x: f64, y: f64, g: f32) -> CatalogStar {
        let (ra, dec) = w.pixel_to_sky(x, y);
        CatalogStar { ra, dec, mag: g }
    }

    /// `n` stars on a 25-column grid (50 px apart) around the frame centre,
    /// catalogue magnitudes spread evenly over 10..16 (index 0 the brightest),
    /// the per-star offset from `zp` Gaussian with `scatter_mag`.
    fn field(n: usize, zp: f64, scatter_mag: f64, seed: u64) -> Field {
        let w = wcs();
        let mut rng = Rng(seed);
        let (mut stars, mut catalog) = (Vec::new(), Vec::new());
        for i in 0..n {
            // The catalogue holds G as f32; the fluxes are made from that value.
            let g = f64::from((10.0 + 6.0 * i as f64 / (n.max(2) - 1) as f64) as f32);
            let x = 1999.5 - 600.0 + (i % 25) as f64 * 50.0;
            let y = 1499.5 - 400.0 + (i / 25) as f64 * 50.0;
            let eps = if scatter_mag > 0.0 { scatter_mag * rng.normal() } else { 0.0 };
            stars.push(star(x, y, flux_for(g, zp + eps)));
            catalog.push(cat_at(&w, x, y, g as f32));
        }
        Field { stars, catalog, wcs: w }
    }

    fn input<'a>(f: &'a Field, exptime_s: Option<f64>) -> ZpInput<'a> {
        ZpInput {
            stars: &f.stars,
            wcs: &f.wcs,
            catalog: &f.catalog,
            exptime_s,
            width: W,
            height: H,
            median_fwhm_px: 4.0,
        }
    }

    fn measure(f: &Field) -> ZpMeasurement {
        measure_zero_point(&input(f, Some(EXPTIME)), &ZpParams::default())
    }

    #[test]
    fn a_synthetic_field_recovers_its_zero_point() {
        let f = field(200, TRUE_ZP, 0.0, 1);
        let m = measure(&f);
        let zp = m.result.expect("a clean field is valid");
        assert!((zp.zero_point - TRUE_ZP).abs() < 1e-9, "zp {}", zp.zero_point);
        assert!(zp.sigma < 1e-9, "sigma {}", zp.sigma);
        assert!(zp.stars >= 50, "stars {}", zp.stars);
    }

    #[test]
    fn saturated_bright_stars_do_not_move_the_median() {
        let mut f = field(200, TRUE_ZP, 0.0, 2);
        // The brightest 5 % (index 0 is the brightest) carry half their flux.
        for s in f.stars.iter_mut().take(10) {
            s.flux *= 0.5;
        }
        let m = measure(&f);
        // 200 stars, the brightest 15 % (30) dropped: the halved ones go with them.
        assert_eq!(m.matched, 170);
        let zp = m.result.expect("valid");
        assert!((zp.zero_point - TRUE_ZP).abs() < 0.01, "zp {}", zp.zero_point);
    }

    #[test]
    fn a_star_between_two_catalogue_stars_is_not_used() {
        let mut f = field(100, TRUE_ZP, 0.0, 3);
        let (x, y) = (1999.5, 1499.5 - 450.0);
        f.stars.push(star(x, y, flux_for(12.345, TRUE_ZP)));
        f.catalog.push(cat_at(&f.wcs, x, y, 12.345));
        f.catalog.push(cat_at(&f.wcs, x + 4.0, y, 13.0));
        let m = measure(&f);
        assert!(m.result.is_ok());
        assert!(
            !m.per_star
                .iter()
                .any(|s| s.kept && (s.g_mag - 12.345).abs() < 1e-3),
            "the blended star was used"
        );
    }

    /// A G 12.0 star at a free spot of a 60-star field with one catalogue
    /// neighbour 8 px away (inside the 12 px blend radius of FWHM 4).
    fn field_with_neighbour(neighbour_g: f32) -> Field {
        let mut f = field(60, TRUE_ZP, 0.0, 12);
        let (x, y) = (1999.5, 1499.5 - 450.0);
        f.stars.push(star(x, y, flux_for(12.0, TRUE_ZP)));
        f.catalog.push(cat_at(&f.wcs, x, y, 12.0));
        f.catalog.push(cat_at(&f.wcs, x + 8.0, y, neighbour_g));
        f
    }

    fn used_at(m: &ZpMeasurement, g: f32) -> bool {
        m.per_star.iter().any(|s| (s.g_mag - g).abs() < 1e-3)
    }

    #[test]
    fn a_faint_neighbour_is_not_a_blend() {
        let m = measure(&field_with_neighbour(19.0));
        assert!(m.result.is_ok());
        assert!(used_at(&m, 12.0), "a 7 mag fainter neighbour blocked the star");
        assert!(m.per_star.iter().any(|s| s.kept && (s.g_mag - 12.0).abs() < 1e-3));
    }

    #[test]
    fn a_bright_neighbour_is_a_blend() {
        let m = measure(&field_with_neighbour(13.0));
        assert!(m.result.is_ok());
        assert!(!used_at(&m, 12.0), "the blended star was used");
    }

    #[test]
    fn a_zero_fwhm_still_refuses_two_catalogue_stars_inside_the_match_radius() {
        let mut f = field(60, TRUE_ZP, 0.0, 13);
        let (x, y) = (1999.5, 1499.5 - 450.0);
        f.stars.push(star(x, y, flux_for(12.5, TRUE_ZP)));
        f.catalog.push(cat_at(&f.wcs, x + 0.5, y, 12.5));
        f.catalog.push(cat_at(&f.wcs, x - 1.5, y, 12.7));
        let i = ZpInput { median_fwhm_px: 0.0, ..input(&f, Some(EXPTIME)) };
        let m = measure_zero_point(&i, &ZpParams::default());
        assert!(m.result.is_ok());
        assert!(!used_at(&m, 12.5) && !used_at(&m, 12.7), "a star with two candidates was used");
    }

    #[test]
    fn a_star_below_the_snr_floor_is_not_used() {
        let mut f = field(60, TRUE_ZP, 0.0, 14);
        for (k, (snr, g)) in
            [(Some(9.0), 11.111_f32), (None, 11.222_f32), (Some(100.0), 11.333_f32)]
                .into_iter()
                .enumerate()
        {
            let (x, y) = (1999.5 + k as f64 * 100.0, 1499.5 + 450.0);
            let mut s = star(x, y, flux_for(f64::from(g), TRUE_ZP));
            s.snr = snr;
            f.stars.push(s);
            f.catalog.push(cat_at(&f.wcs, x, y, g));
        }
        let m = measure(&f);
        assert!(m.result.is_ok());
        assert!(!used_at(&m, 11.111), "a star at SNR 9 was used");
        assert!(!used_at(&m, 11.222), "a star without SNR was used");
        assert!(used_at(&m, 11.333), "the good star beside them was not used");
    }

    #[test]
    fn stars_outside_the_central_region_are_not_used() {
        let mut f = field(100, TRUE_ZP, 0.0, 4);
        let half_diag = 0.5 * (W as f64).hypot(H as f64);
        for k in 0..5 {
            // Corner stars, 0.94 of the half-diagonal from the centre: beyond 0.9.
            let (x, y) = (3900.0 + k as f64 * 20.0, 100.0);
            let r = (x - 1999.5).hypot(y - 1499.5) / half_diag;
            assert!(r > 0.9, "the fixture star is inside the region: {r}");
            f.stars.push(star(x, y, flux_for(11.5, TRUE_ZP)));
            f.catalog.push(cat_at(&f.wcs, x, y, 11.5));
        }
        let m = measure(&f);
        assert!(m.result.is_ok());
        assert!(!m.per_star.is_empty());
        assert!(
            m.per_star.iter().all(|s| s.radius_fraction <= 0.9),
            "a star beyond the central region was used"
        );
        assert!(!m.per_star.iter().any(|s| (s.g_mag - 11.5).abs() < 1e-3));
    }

    #[test]
    fn a_match_is_one_to_one() {
        let mut f = field(60, TRUE_ZP, 0.0, 5);
        let (x, y) = (1999.5, 1499.5 - 450.0);
        f.catalog.push(cat_at(&f.wcs, x, y, 12.5));
        f.stars.push(star(x - 0.5, y, flux_for(12.5, TRUE_ZP)));
        f.stars.push(star(x + 1.0, y, flux_for(12.5, TRUE_ZP)));
        let m = measure(&f);
        let at_12_5: Vec<&ZpStar> =
            m.per_star.iter().filter(|s| (s.g_mag - 12.5).abs() < 1e-3).collect();
        assert_eq!(at_12_5.len(), 1, "one catalogue star served {} analysis stars", at_12_5.len());
        assert!((at_12_5[0].match_px - 0.5).abs() < 1e-9, "match_px {}", at_12_5[0].match_px);
        assert_eq!(
            at_12_5[0].star_index,
            f.stars.len() - 2,
            "the surviving entry is the 0.5 px star, the second to last pushed"
        );
    }

    #[test]
    fn fewer_than_the_minimum_refuses_with_counts() {
        // 10 stars, the brightest one dropped (15 % of 10 floors to 1): 9 matched,
        // far under the 50 a valid frame needs.
        let f = field(10, TRUE_ZP, 0.0, 6);
        let m = measure(&f);
        assert_eq!(m.matched, 9);
        match m.result {
            Err(ZpRefusal::TooFewStars { matched, kept }) => {
                assert_eq!((matched, kept), (9, 9));
            }
            other => panic!("expected TooFewStars, got {other:?}"),
        }
    }

    #[test]
    fn forty_nine_stars_refuse_and_fifty_measure() {
        // No scatter, so every matched star is kept. Brightest 15 % dropped:
        // 57 stars drop 8 (8.55 floors to 8) and leave 49; 58 drop 8 (8.7) and
        // leave 50.
        let m = measure(&field(57, TRUE_ZP, 0.0, 15));
        assert_eq!(m.matched, 49);
        match m.result {
            Err(ZpRefusal::TooFewStars { matched, kept }) => assert_eq!((matched, kept), (49, 49)),
            other => panic!("expected TooFewStars at 49 stars, got {other:?}"),
        }
        let m = measure(&field(58, TRUE_ZP, 0.0, 16));
        assert_eq!(m.matched, 50);
        let zp = m.result.expect("50 kept stars are enough");
        assert_eq!(zp.stars, 50);
        assert!((zp.zero_point - TRUE_ZP).abs() < 1e-9, "zp {}", zp.zero_point);
    }

    #[test]
    fn a_wide_scatter_of_many_enough_stars_refuses() {
        // 100 stars, 85 after the bright drop: at least 50 are kept, so the
        // refusal is the scatter (standard error ~1.0 / sqrt(85) = 0.11), not
        // the star count.
        let f = field(100, TRUE_ZP, 1.0, 7);
        let m = measure(&f);
        match m.result {
            Err(ZpRefusal::TooScattered { std_error, kept }) => {
                assert!(std_error > 0.035, "std_error {std_error}");
                assert!(kept >= 50, "kept {kept}");
            }
            other => panic!("expected TooScattered, got {other:?}"),
        }
    }

    #[test]
    fn a_wide_scatter_of_many_stars_is_valid() {
        // 800 stars (680 after the bright drop): the standard error of 0.30 mag
        // scatter is about 0.012, clearly under the 0.035 limit.
        let f = field(800, TRUE_ZP, 0.30, 8);
        let zp = measure(&f).result.expect("many stars average the scatter out");
        assert!((zp.zero_point - TRUE_ZP).abs() < 0.05, "zp {}", zp.zero_point);
        assert!(zp.sigma > 0.1, "sigma {}", zp.sigma);
    }

    #[test]
    fn the_defaults_are_the_probes() {
        let d = ZpParams::default();
        assert_eq!(d.match_radius_px, 2.0);
        assert_eq!(d.central_fraction, 0.9);
        assert_eq!(d.min_stars, 50);
        assert_eq!(d.max_std_error_mag, 0.035);
        assert_eq!(d.clip_sigma, 4.0);
        assert_eq!(d.bright_drop_fraction, 0.15);
        assert_eq!(d.min_snr, 10.0);
        assert_eq!(d.blend_radius_fwhm, 3.0);
        assert_eq!(d.blend_dmag, 4.0);
    }

    #[test]
    fn zero_exposure_refuses() {
        let f = field(200, TRUE_ZP, 0.0, 9);
        for exp in [Some(0.0), None] {
            let m = measure_zero_point(&input(&f, exp), &ZpParams::default());
            assert_eq!(m.result.unwrap_err().code(), "no_exposure");
        }
    }

    #[test]
    fn a_non_positive_flux_is_skipped() {
        let mut f = field(100, TRUE_ZP, 0.0, 10);
        for (k, (flux, g)) in [(0.0, 11.111_f32), (-5.0, 11.222_f32)].into_iter().enumerate() {
            let (x, y) = (1999.5 + k as f64 * 100.0, 1499.5 + 450.0);
            f.stars.push(star(x, y, flux));
            f.catalog.push(cat_at(&f.wcs, x, y, g));
        }
        let m = measure(&f);
        let zp = m.result.expect("valid");
        assert!(zp.zero_point.is_finite() && zp.sigma.is_finite());
        assert!(m.per_star.iter().all(|s| s.zp.is_finite()));
        assert!(!m
            .per_star
            .iter()
            .any(|s| (s.g_mag - 11.111).abs() < 1e-3 || (s.g_mag - 11.222).abs() < 1e-3));
    }

    #[test]
    fn a_singular_wcs_refuses_without_panic() {
        let mut f = field(200, TRUE_ZP, 0.0, 11);
        f.wcs.cd = [[0.0, 0.0], [0.0, 0.0]];
        let m = measure(&f);
        assert_eq!(m.matched, 0);
        match m.result {
            Err(ZpRefusal::TooFewStars { matched: 0, kept: 0 }) => {}
            other => panic!("expected TooFewStars with no matches, got {other:?}"),
        }
    }

    // ---- storage and currency -------------------------------------------------------------------------------------

    use crate::models::FrameAnalysis;
    use crate::plate_solve::storage::{insert_plate_solve, PlateSolveRecord};

    fn catalog_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    /// What a seeded frame is made of; the defaults are a measurable mono light.
    struct Seed {
        exptime: Option<f64>,
        bayerpat: Option<&'static str>,
        measure_version: Option<u32>,
    }

    impl Default for Seed {
        fn default() -> Self {
            Seed { exptime: Some(EXPTIME), bayerpat: None, measure_version: Some(MEASURE_VERSION) }
        }
    }

    fn fresh_analysis(id: i64, measure_version: Option<u32>) -> FrameAnalysis {
        FrameAnalysis {
            id: None,
            frame_id: id,
            file_id: id,
            stars_detected: 200,
            median_fwhm: 4.0,
            median_eccentricity: 0.1,
            median_snr: 100.0,
            median_hfr: 2.0,
            frame_snr: 30.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 100.0,
            noise: 5.0,
            detection_threshold: 5.0,
            width: W as i64,
            height: H as i64,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: false,
            median_beta: None,
            quality_score: None,
            config_hash: Some("t".into()),
            analyzed_at: "2026-03-01T23:00:00Z".into(),
            measure_version,
            stars_refused: None,
            frame_verdict: None,
            zero_point: None,
            zp_stars: None,
            zp_sigma: None,
            zp_version: None,
            zp_catalog: None,
        }
    }

    /// Store `wcs` as the frame's solve. `forward_only_sip` adds a forward SIP pair and no reverse one.
    fn seed_solve(conn: &Connection, id: i64, wcs: &WcsSolution, forward_only_sip: bool) {
        let table = |c: f64| serde_json::to_string(&vec![vec![c; 3]; 3]).unwrap();
        insert_plate_solve(
            conn,
            &PlateSolveRecord {
                id: None,
                frame_id: id,
                crpix1: wcs.crpix.0,
                crpix2: wcs.crpix.1,
                crval1: wcs.crval.0,
                crval2: wcs.crval.1,
                cd1_1: wcs.cd[0][0],
                cd1_2: wcs.cd[0][1],
                cd2_1: wcs.cd[1][0],
                cd2_2: wcs.cd[1][1],
                sip_order: forward_only_sip.then_some(2),
                sip_a_coeffs: forward_only_sip.then(|| table(1e-9)),
                sip_b_coeffs: forward_only_sip.then(|| table(2e-9)),
                sip_ap_coeffs: None,
                sip_bp_coeffs: None,
                matched_stars: 300,
                total_detected: 400,
                rms_residual_px: 0.3,
                rms_residual_arcsec: 0.3,
                pixel_scale_arcsec: 1.0,
                field_rotation_deg: 0.0,
                solve_time_ms: 10,
                catalog_used: "test".into(),
                algorithm_used: "test".into(),
                solved_at: "2026-03-01T23:00:00Z".into(),
                expected_catalog_stars_in_fov: None,
                inlier_ratio: None,
            },
        )
        .unwrap();
    }

    /// A light with an analysis, its 200 stars and (when `solve`) a solve; frame id = file id = `id`.
    fn seed_frame(conn: &Connection, id: i64, f: &Field, seed: &Seed, solve: bool) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
             VALUES (?1, ?2, ?3, 0, '2026-03-01', 'FITS', '2026-03-01')",
            params![id, format!("/x/{id}.fits"), format!("{id}.fits")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, exptime, date_obs, bayerpat)
             VALUES (?1, ?1, 'Light', ?2, '2026-03-01T22:00:00+00:00', ?3)",
            params![id, seed.exptime, seed.bayerpat],
        )
        .unwrap();
        let analysis_id = db_analysis::upsert_frame_analysis(conn, &fresh_analysis(id, seed.measure_version)).unwrap();
        let stars: Vec<StarMetric> =
            f.stars.iter().map(|s| StarMetric { frame_analysis_id: analysis_id, ..s.clone() }).collect();
        db_analysis::upsert_star_metrics(conn, analysis_id, &stars).unwrap();
        if solve {
            seed_solve(conn, id, &f.wcs, false);
        }
    }

    /// The catalogue of `f` as a `cone` closure; counts its calls.
    fn cone_of<'a>(
        f: &'a Field,
        calls: &'a std::cell::Cell<usize>,
    ) -> impl FnMut(&WcsSolution, usize, usize, f64, f64) -> Result<Vec<CatalogStar>> + 'a {
        move |_, _, _, _, _| {
            calls.set(calls.get() + 1);
            Ok(f.catalog.clone())
        }
    }

    fn zp_columns(conn: &Connection, id: i64) -> (Option<f64>, Option<i64>, Option<u32>, Option<String>) {
        let a = db_analysis::get_frame_analysis(conn, id).unwrap().unwrap();
        (a.zero_point, a.zp_stars, a.zp_version, a.zp_catalog)
    }

    #[test]
    fn refresh_measures_and_stores() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 21);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        let ZpStatus::Measured(z) = status else { panic!("expected Measured, got {status:?}") };
        assert!((z.zero_point - TRUE_ZP).abs() < 1e-9, "zp {}", z.zero_point);
        let (value, stars, version, catalog) = zp_columns(&conn, 1);
        assert!((value.unwrap() - TRUE_ZP).abs() < 1e-9);
        assert_eq!((stars, version, catalog.as_deref()), (Some(i64::from(z.stars)), Some(ZP_VERSION), Some("A")));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_stale_analysis_is_not_measured() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 22);
        let seed = Seed { measure_version: Some(MEASURE_VERSION - 1), ..Seed::default() };
        seed_frame(&conn, 1, &f, &seed, true);
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::StaleAnalysis));
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn a_frame_without_a_solve_or_an_analysis_is_not_ready() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 23);
        seed_frame(&conn, 1, &f, &Seed::default(), false);
        let calls = std::cell::Cell::new(0);
        assert_eq!(
            refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap(),
            ZpStatus::NotReady(NotReady::NoSolve)
        );
        assert_eq!(
            refresh_zero_point_with(&conn, 99, "A", cone_of(&f, &calls)).unwrap(),
            ZpStatus::NotReady(NotReady::NoAnalysis)
        );
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None));
    }

    #[test]
    fn a_missing_exposure_stores_a_refusal() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 24);
        seed_frame(&conn, 1, &f, &Seed { exptime: None, ..Seed::default() }, true);
        // The catalogue is never read for a frame with no exposure: a closure that fails would show it.
        let status = refresh_zero_point_with(&conn, 1, "A", |_, _, _, _, _| bail!("must not be called")).unwrap();
        assert_eq!(status, ZpStatus::Refused(ZpRefusal::NoExposure));
        assert_eq!(zp_columns(&conn, 1), (None, None, Some(ZP_VERSION), Some("A".to_string())));
    }

    #[test]
    fn a_colour_camera_frame_is_not_measured() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 25);
        seed_frame(&conn, 1, &f, &Seed { bayerpat: Some("RGGB"), ..Seed::default() }, true);
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::ColourCamera));
        assert!(status.stored());
        assert_eq!(zp_columns(&conn, 1), (None, None, Some(ZP_VERSION), Some("A".to_string())));
        assert_eq!(calls.get(), 0, "no catalogue read for a colour camera");
        // Stored like a refusal: the next pass does not retry it.
        assert_eq!(ensure_zero_points_with(&conn, "A", &[1], cone_of(&f, &calls)), 0);
    }

    #[test]
    fn a_debayered_colour_frame_is_not_measured() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 40);
        // No BAYERPAT in the header, but the analysis found three planes: an externally debayered file.
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        conn.execute("UPDATE frame_analysis SET source_channels = 3 WHERE frame_id = 1", []).unwrap();
        assert!(is_colour_frame(None, Some(3)));
        assert!(is_colour_frame(Some("RGGB"), Some(1)));
        assert!(!is_colour_frame(None, Some(1)));
        assert!(!is_colour_frame(Some("  "), None));
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::ColourCamera));
        assert!(status.stored());
        assert_eq!(zp_columns(&conn, 1), (None, None, Some(ZP_VERSION), Some("A".to_string())));
        assert_eq!(calls.get(), 0, "no catalogue read for a colour camera");
    }

    #[test]
    fn a_forward_only_sip_is_not_measured() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 26);
        seed_frame(&conn, 1, &f, &Seed::default(), false);
        seed_solve(&conn, 1, &f.wcs, true);
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::UnreadableSolve));
        assert!(!status.stored());
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None), "nothing is written");
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn a_new_analysis_clears_the_zero_point_until_the_trigger_runs() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 27);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert!(zp_columns(&conn, 1).0.is_some());
        // The analyzer's row carries no zero point: the REPLACE drops the old one with the old stars.
        db_analysis::upsert_frame_analysis(&conn, &fresh_analysis(1, Some(MEASURE_VERSION))).unwrap();
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None));
        // ...and `ensure` picks the frame up again once its stars are back.
        let analysis_id = db_analysis::get_frame_analysis(&conn, 1).unwrap().unwrap().id.unwrap();
        let stars: Vec<StarMetric> =
            f.stars.iter().map(|s| StarMetric { frame_analysis_id: analysis_id, ..s.clone() }).collect();
        db_analysis::upsert_star_metrics(&conn, analysis_id, &stars).unwrap();
        assert_eq!(ensure_zero_points_with(&conn, "A", &[1], cone_of(&f, &calls)), 1);
        assert!(zp_columns(&conn, 1).0.is_some());
    }

    #[test]
    fn a_new_solve_recomputes_the_zero_point() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 28);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        let before = zp_columns(&conn, 1);
        assert!(before.0.is_some());
        // A better solve, 50 px away: storing it clears the old value (a zero point never outlives its solve)...
        let mut shifted = f.wcs.clone();
        shifted.crpix.0 += 50.0;
        seed_solve(&conn, 1, &shifted, false);
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None), "the old zero point survived a new solve");
        // ...and the solve trigger recomputes it against the new WCS.
        crate::api::analysis::zero_points_after_solve_with(&conn, "A", &[1], cone_of(&f, &calls));
        let after = zp_columns(&conn, 1);
        assert_ne!(after, before, "the new solve measured the same as the old");
        assert_eq!((after.2, after.3.as_deref()), (Some(ZP_VERSION), Some("A")));
    }

    #[test]
    fn the_solve_trigger_recomputes_a_current_zero_point_and_the_analysis_trigger_skips_it() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 41);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        calls.set(0);
        crate::api::analysis::zero_points_after_analysis_with(&conn, "A", &[1], cone_of(&f, &calls));
        assert_eq!(calls.get(), 0, "a current zero point is left alone after an analysis");
        crate::api::analysis::zero_points_after_solve_with(&conn, "A", &[1], cone_of(&f, &calls));
        assert_eq!(calls.get(), 1, "after a solve every frame is recomputed");
    }

    fn zp_row_raw(conn: &Connection, id: i64) -> (Option<f64>, Option<i64>, Option<f64>, Option<u32>, Option<String>) {
        conn.query_row(
            "SELECT zero_point, zp_stars, zp_sigma, zp_version, zp_catalog FROM frame_analysis WHERE frame_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    }

    #[test]
    fn deleting_a_solve_clears_the_zero_point() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 42);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert!(zp_row_raw(&conn, 1).0.is_some());
        crate::plate_solve::storage::delete_plate_solve(&conn, 1).unwrap();
        assert_eq!(zp_row_raw(&conn, 1), (None, None, None, None, None));
    }

    #[test]
    fn a_new_solve_clears_the_zero_point_even_when_the_analysis_is_stale() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 43);
        let seed = Seed { measure_version: Some(MEASURE_VERSION - 1), ..Seed::default() };
        seed_frame(&conn, 1, &f, &seed, true);
        // A value stored by an earlier pass, under an analysis that has since gone stale.
        conn.execute(
            "UPDATE frame_analysis SET zero_point = 20.0, zp_stars = 80, zp_sigma = 0.03, zp_version = ?1, zp_catalog = 'A'
             WHERE frame_id = 1",
            [ZP_VERSION],
        )
        .unwrap();
        seed_solve(&conn, 1, &f.wcs, false);
        assert_eq!(zp_row_raw(&conn, 1), (None, None, None, None, None));
        let calls = std::cell::Cell::new(0);
        crate::api::analysis::zero_points_after_solve_with(&conn, "A", &[1], cone_of(&f, &calls));
        assert_eq!(calls.get(), 0, "a stale analysis is not measured");
        assert_eq!(
            refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap(),
            ZpStatus::NotReady(NotReady::StaleAnalysis)
        );
        assert_eq!(zp_row_raw(&conn, 1), (None, None, None, None, None));
    }

    #[test]
    fn a_write_after_a_reanalysis_is_dropped() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 44);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        // The ids a judgement would have read.
        let old_analysis = db_analysis::get_frame_analysis(&conn, 1).unwrap().unwrap().id.unwrap();
        let old_solve = get_plate_solve(&conn, 1).unwrap().unwrap().id.unwrap();
        // The frame is re-analysed: the REPLACE gives the row a new id.
        db_analysis::upsert_frame_analysis(&conn, &fresh_analysis(1, Some(MEASURE_VERSION))).unwrap();
        let z = ZeroPoint { zero_point: 21.0, stars: 50, sigma: 0.02 };
        let written = db_analysis::set_zero_point(&conn, 1, old_analysis, Some(old_solve), Some(&z), "A").unwrap();
        assert!(!written, "the write after a re-analysis must not land");
        assert_eq!(zp_row_raw(&conn, 1), (None, None, None, None, None), "the new row's zero point stays NULL");
        // The same through the refresh: the cone replaces the analysis (a re-analysis landing mid-judgement).
        let f2 = field(200, TRUE_ZP, 0.0, 45);
        seed_frame(&conn, 2, &f2, &Seed::default(), true);
        let status = refresh_zero_point_with(&conn, 2, "A", |_, _, _, _, _| {
            db_analysis::upsert_frame_analysis(&conn, &fresh_analysis(2, Some(MEASURE_VERSION))).unwrap();
            Ok(f2.catalog.clone())
        })
        .unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::Superseded));
        assert!(!status.stored());
        assert_eq!(zp_row_raw(&conn, 2), (None, None, None, None, None));
        // ...and a new solve landing mid-judgement.
        let f3 = field(200, TRUE_ZP, 0.0, 46);
        seed_frame(&conn, 3, &f3, &Seed::default(), true);
        let status = refresh_zero_point_with(&conn, 3, "A", |_, _, _, _, _| {
            seed_solve(&conn, 3, &f3.wcs, false);
            Ok(f3.catalog.clone())
        })
        .unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::Superseded));
        assert_eq!(zp_row_raw(&conn, 3), (None, None, None, None, None));
    }

    #[test]
    fn a_synced_analysis_without_star_rows_is_not_measured() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 47);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        // The summary says 200 stars were detected; the rows are not there (personal sync carries no star rows).
        conn.execute("DELETE FROM star_metrics", []).unwrap();
        let calls = std::cell::Cell::new(0);
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(status, ZpStatus::NotReady(NotReady::NoStarMetrics));
        assert!(!status.stored());
        assert_eq!(zp_row_raw(&conn, 1), (None, None, None, None, None), "nothing is stored");
        assert_eq!(calls.get(), 0);
        // A frame that really detected no stars is a measurement of too few.
        conn.execute("UPDATE frame_analysis SET stars_detected = 0 WHERE frame_id = 1", []).unwrap();
        let status = refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert!(matches!(status, ZpStatus::Refused(ZpRefusal::TooFewStars { .. })), "{status:?}");
    }

    #[test]
    fn ensure_skips_frames_that_are_current() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 29);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        seed_frame(&conn, 2, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        calls.set(0);
        assert_eq!(ensure_zero_points_with(&conn, "A", &[1, 2], cone_of(&f, &calls)), 1);
        assert_eq!(calls.get(), 1, "only the stale frame read the catalogue");
        assert!(zp_columns(&conn, 2).0.is_some());
        assert_eq!(ensure_zero_points_with(&conn, "A", &[1, 2], cone_of(&f, &calls)), 0);
    }

    #[test]
    fn a_zero_point_from_another_catalogue_is_recomputed() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 30);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 1, "A", cone_of(&f, &calls)).unwrap();
        assert_eq!(ensure_zero_points_with(&conn, "A", &[1], cone_of(&f, &calls)), 0, "skipped under A");
        assert_eq!(ensure_zero_points_with(&conn, "B", &[1], cone_of(&f, &calls)), 1, "refreshed under B");
        assert_eq!(zp_columns(&conn, 1).3.as_deref(), Some("B"));
        assert_eq!(ensure_zero_points_with(&conn, "B", &[1], cone_of(&f, &calls)), 0);
    }

    #[test]
    fn the_backfill_cursor_walks_the_stale_frames_in_chunks() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 31);
        for id in 1..=5 {
            seed_frame(&conn, id, &f, &Seed::default(), true);
        }
        let calls = std::cell::Cell::new(0);
        refresh_zero_point_with(&conn, 3, "A", cone_of(&f, &calls)).unwrap();
        let first = stale_zero_point_frames(&conn, "A", 0, 2).unwrap();
        assert_eq!(first, vec![1, 2]);
        let second = stale_zero_point_frames(&conn, "A", 2, 2).unwrap();
        assert_eq!(second, vec![4, 5], "the current frame 3 is not listed");
        assert!(stale_zero_point_frames(&conn, "A", 5, 2).unwrap().is_empty());
    }

    #[test]
    fn a_trigger_error_does_not_fail_the_analysis() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 32);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        // The catalogue read fails; the trigger returns (it cannot fail its host), the analysis row is intact and
        // its zero point columns stay NULL.
        crate::api::analysis::zero_points_after_analysis_with(&conn, "A", &[1], |_, _, _, _, _| bail!("catalogue unreadable"));
        let a = db_analysis::get_frame_analysis(&conn, 1).unwrap().expect("the analysis row exists");
        assert_eq!(a.stars_detected, 200);
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None));
    }

    #[test]
    fn a_panicking_cone_does_not_fail_the_trigger() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 48);
        seed_frame(&conn, 1, &f, &Seed::default(), true);
        crate::api::analysis::zero_points_after_analysis_with(&conn, "A", &[1], |_, _, _, _, _| -> Result<Vec<CatalogStar>> {
            panic!("the catalogue reader exploded")
        });
        crate::api::analysis::zero_points_after_solve_with(&conn, "A", &[1], |_, _, _, _, _| -> Result<Vec<CatalogStar>> {
            panic!("the catalogue reader exploded")
        });
        let a = db_analysis::get_frame_analysis(&conn, 1).unwrap().expect("the analysis row is intact");
        assert_eq!(a.stars_detected, 200);
        assert_eq!(zp_columns(&conn, 1), (None, None, None, None));
    }

    /// A zero point judged at an older `ZP_VERSION` (version 1 was written by development builds under another validity
    /// rule) is stale, for the gate's selection and the backfill's cursor alike; one at the current version is not.
    #[test]
    fn a_zero_point_of_an_older_version_is_stale() {
        let conn = catalog_db();
        let f = Field { stars: Vec::new(), catalog: Vec::new(), wcs: wcs() };
        for id in 1..=2 {
            seed_frame(&conn, id, &f, &Seed::default(), true);
        }
        conn.execute("UPDATE frame_analysis SET stars_detected = 0", []).unwrap();
        assert_eq!(ZP_VERSION, 2);
        conn.execute("UPDATE frame_analysis SET zp_version = 1, zp_catalog = 'A' WHERE frame_id = 1", []).unwrap();
        conn.execute("UPDATE frame_analysis SET zp_version = ?1, zp_catalog = 'A' WHERE frame_id = 2", [ZP_VERSION]).unwrap();
        assert_eq!(stale_among_frames(&conn, "A", &[1, 2]).unwrap(), vec![1]);
        assert_eq!(stale_zero_point_frames(&conn, "A", 0, 10).unwrap(), vec![1]);
    }

    /// The gate selects its stale frames with the module's statement count in hand: one statement per chunk of
    /// `BACKFILL_CHUNK` ids, not one per frame (structural: the count is read off the selection itself).
    #[test]
    fn the_gates_stale_selection_is_one_query_per_chunk() {
        let conn = catalog_db();
        let f = Field { stars: Vec::new(), catalog: Vec::new(), wcs: wcs() };
        let total = 2 * BACKFILL_CHUNK + 50;
        let mut ids = Vec::new();
        for id in 1..=total as i64 {
            seed_frame(&conn, id, &f, &Seed::default(), true);
            ids.push(id);
        }
        // The frames carry no star rows, so they count as measurable only when they claim no stars either.
        conn.execute("UPDATE frame_analysis SET stars_detected = 0", []).unwrap();
        // Frame 3 is current for "A"; frame 4 has no solve and is never selected.
        conn.execute("UPDATE frame_analysis SET zp_version = ?1, zp_catalog = 'A' WHERE frame_id = 3", [ZP_VERSION]).unwrap();
        conn.execute("DELETE FROM plate_solves WHERE frame_id = 4", []).unwrap();
        let (stale, statements) = stale_among_frames_counted(&conn, "A", &ids).unwrap();
        assert_eq!(statements, 3, "{total} ids are three chunks");
        assert_eq!(stale.len(), total - 2);
        assert!(!stale.contains(&3) && !stale.contains(&4));
        assert!(stale.windows(2).all(|w| w[0] < w[1]), "id order");
        // Another catalogue makes frame 3 stale too; nothing asked is one empty read.
        assert!(stale_among_frames(&conn, "B", &ids).unwrap().contains(&3));
        assert_eq!(stale_among_frames_counted(&conn, "A", &[]).unwrap(), (Vec::new(), 0));
    }

    /// An analysis that arrived through personal sync (stars counted, no star rows) can never be measured here: it is
    /// no stale frame, for the backfill's cursor and for the gate's selection alike.
    #[test]
    fn a_synced_analysis_without_star_rows_is_never_stale() {
        let conn = catalog_db();
        let f = field(200, TRUE_ZP, 0.0, 61);
        let bare = Field { stars: Vec::new(), catalog: Vec::new(), wcs: wcs() };
        // Frames 1..=300 are synced analyses (stars_detected 200, no star rows); 301..=310 are real stale frames.
        for id in 1..=300 {
            seed_frame(&conn, id, &bare, &Seed::default(), true);
        }
        for id in 301..=310 {
            seed_frame(&conn, id, &f, &Seed::default(), true);
        }
        let real: Vec<i64> = (301..=310).collect();
        assert_eq!(stale_zero_point_frames(&conn, "A", 0, 500).unwrap(), real, "the backfill's cursor");
        assert_eq!(stale_zero_point_frames(&conn, "A", 0, 5).unwrap(), (301..=305).collect::<Vec<i64>>(), "a limit is not crowded out");
        let all: Vec<i64> = (1..=310).collect();
        assert_eq!(stale_among_frames(&conn, "A", &all).unwrap(), real, "the gate's selection");
        // A frame that counts no stars is still selected: its refresh stores the "too few stars" refusal.
        conn.execute("UPDATE frame_analysis SET stars_detected = 0 WHERE frame_id = 1", []).unwrap();
        assert_eq!(stale_among_frames(&conn, "A", &[1]).unwrap(), vec![1]);
    }
}

#[cfg(test)]
mod forced_tests {
    use super::*;
    use crate::star_measure::analysis_mode::ListFit;

    const EXPTIME: f64 = 120.0;
    const ANCHOR: f64 = 21.3;
    const FULL: f64 = 65535.0;

    fn params() -> ForcedListParams {
        ForcedListParams::default()
    }

    /// A TAN solve at `scale_arcsec` per pixel for a `w` x `h` frame; `shift` moves the reference pixel (a dither) and
    /// `rot_deg` rotates the axes.
    fn wcs_at(w: usize, h: usize, scale_arcsec: f64, shift: (f64, f64), rot_deg: f64) -> WcsSolution {
        let s = scale_arcsec / 3600.0;
        let (sn, cs) = rot_deg.to_radians().sin_cos();
        WcsSolution {
            crpix: ((w as f64 - 1.0) / 2.0 + shift.0, (h as f64 - 1.0) / 2.0 + shift.1),
            crval: (150.0, 20.0),
            cd: [[-s * cs, s * sn], [s * sn, s * cs]],
            sip_forward: None,
            sip_reverse: None,
        }
    }

    fn at(wcs: &WcsSolution, x: f64, y: f64, g: f32) -> CatalogStar {
        let (ra, dec) = wcs.pixel_to_sky(x, y);
        CatalogStar { ra, dec, mag: g }
    }

    fn g_b(wcs: &WcsSolution) -> f64 {
        let fwhm_ref = fwhm_ref_px(pixel_scale_arcsec(wcs), &params());
        window(ANCHOR, fwhm_ref, FULL, EXPTIME, &params())
    }

    fn input<'a>(catalog: &'a [CatalogStar], wcs: &'a WcsSolution, w: usize, h: usize) -> ForcedListInput<'a> {
        ForcedListInput { catalog, wcs, width: w, height: h, anchor: ANCHOR, full_scale: FULL, exptime_s: EXPTIME }
    }

    /// xorshift64* uniform draws.
    struct Rng(u64);
    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn normal(&mut self) -> f64 {
            let (u, v) = (self.unit(), self.unit());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }
    }

    /// `n` catalogue stars uniform over the pixel box of frame A (a margin beyond it), G uniform from `g0` to `g1`; the
    /// catalogue holds the sky positions A's solve gives them.
    fn sky_catalogue(n: usize, wcs_a: &WcsSolution, w: usize, h: usize, g0: f64, g1: f64, seed: u64) -> Vec<CatalogStar> {
        let mut rng = Rng(seed);
        (0..n)
            .map(|_| {
                let x = -150.0 + rng.unit() * (w as f64 + 300.0);
                let y = -150.0 + rng.unit() * (h as f64 + 300.0);
                at(wcs_a, x, y, (g0 + rng.unit() * (g1 - g0)) as f32)
            })
            .collect()
    }

    fn keys(l: &ForcedList) -> std::collections::HashSet<u64> {
        l.stars.iter().map(|s| s.key).collect()
    }

    fn share(a: &ForcedList, b: &ForcedList) -> f64 {
        let kb = keys(b);
        a.stars.iter().filter(|s| kb.contains(&s.key)).count() as f64 / a.stars.len() as f64
    }

    const W: usize = 4000;
    const H: usize = 3000;

    #[test]
    fn a_dithered_field_keeps_ninety_five_percent_of_its_list() {
        let a = wcs_at(W, H, 1.0, (0.0, 0.0), 0.0);
        let b = wcs_at(W, H, 1.0, (60.0, 40.0), 0.05);
        let gb = g_b(&a);
        let cat = sky_catalogue(5000, &a, W, H, gb - 1.0, gb + 6.0, 11);
        let la = forced_list(&input(&cat, &a, W, H), &params()).unwrap();
        let lb = forced_list(&input(&cat, &b, W, H), &params()).unwrap();
        assert!(la.stars.len() > 300, "a usable list: {}", la.stars.len());
        let s = share(&la, &lb);
        assert!(s >= 0.95, "share {s}");
        let s2 = share(&lb, &la);
        assert!(s2 >= 0.95, "reverse share {s2}");
    }

    #[test]
    fn a_loss_moves_the_window_by_the_loss() {
        let wcs = wcs_at(W, H, 1.0, (0.0, 0.0), 0.0);
        let fwhm_ref = fwhm_ref_px(pixel_scale_arcsec(&wcs), &params());
        let g0 = window(ANCHOR, fwhm_ref, FULL, EXPTIME, &params());
        let g1 = window(ANCHOR - 0.5, fwhm_ref, FULL, EXPTIME, &params());
        assert!((g1 - (g0 - 0.5)).abs() < 1e-12, "{g0} {g1}");
        // Through the builder as well.
        let cat = sky_catalogue(800, &wcs, W, H, g0 - 3.0, g0 + 6.0, 5);
        let mut i = input(&cat, &wcs, W, H);
        let l0 = forced_list(&i, &params()).unwrap();
        i.anchor = ANCHOR - 0.5;
        let l1 = forced_list(&i, &params()).unwrap();
        assert!((l1.window_bright - (l0.window_bright - 0.5)).abs() < 1e-12);
    }

    #[test]
    fn a_capped_list_keeps_the_same_stars() {
        let a = wcs_at(W, H, 1.0, (0.0, 0.0), 0.0);
        let b = wcs_at(W, H, 1.0, (45.0, -30.0), -0.05);
        let gb = g_b(&a);
        let cat = sky_catalogue(10_000, &a, W, H, gb, gb + 3.0, 23);
        let uncapped = ForcedListParams { cap: usize::MAX, ..params() };
        let (ca, cb) = (
            forced_list(&input(&cat, &a, W, H), &params()).unwrap(),
            forced_list(&input(&cat, &b, W, H), &params()).unwrap(),
        );
        let ua = forced_list(&input(&cat, &a, W, H), &uncapped).unwrap();
        assert!(ua.list_before_cap > 2000, "candidates {}", ua.list_before_cap);
        assert_eq!(ua.stars.len(), ua.list_before_cap, "no cap, no loss");
        assert!(ca.stars.len() > 700 && ca.stars.len() < 1300, "capped to about 1000: {}", ca.stars.len());
        assert_eq!(ca.list_before_cap, ua.list_before_cap);
        assert!(share(&ca, &cb) >= 0.95, "share {}", share(&ca, &cb));
        let med = |l: &ForcedList| {
            let mut g: Vec<f64> = l.stars.iter().map(|s| f64::from(s.g_mag)).collect();
            g.sort_by(|x, y| x.total_cmp(y));
            g[g.len() / 2]
        };
        assert!((med(&ca) - med(&ua)).abs() < 0.05, "{} {}", med(&ca), med(&ua));
    }

    #[test]
    fn the_cap_key_is_pinned() {
        // A change of the hash or of the record's bytes changes every stored list: bump on purpose only.
        assert_eq!(cap_key(15.123, 150.5, 20.25), PINNED_KEY);
        // Rounding: a position within 0.4 arcsec of the same record gives the same key.
        assert_eq!(cap_key(15.123, 150.5 + 0.0001 / 3600.0, 20.25), PINNED_KEY);
    }
    const PINNED_KEY: u64 = 7_866_977_722_513_562_110;

    /// A target at the frame centre with G = G_b + 1 and one neighbour; is the target in the list?
    fn target_survives(neighbour_dg: f64, neighbour_dx_fwhm: f64, neighbour_g_abs: Option<f64>) -> bool {
        let (w, h) = (1000usize, 1000usize);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let fwhm_ref = fwhm_ref_px(pixel_scale_arcsec(&wcs), &params());
        let gb = g_b(&wcs);
        let (x0, y0) = (499.5, 499.5);
        let gt = gb + 1.0;
        let gn = neighbour_g_abs.unwrap_or(gt + neighbour_dg);
        let cat = vec![
            at(&wcs, x0, y0, gt as f32),
            at(&wcs, x0 + neighbour_dx_fwhm * fwhm_ref, y0, gn as f32),
        ];
        let l = forced_list(&input(&cat, &wcs, w, h), &params()).unwrap();
        l.stars.iter().any(|s| (s.x - x0).abs() < 0.01 && (s.y - y0).abs() < 0.01)
    }

    #[test]
    fn isolation_reads_the_catalogue_only() {
        // dG 3.9 at 3.9 FWHM_ref drops the star, dG 4.1 keeps it.
        assert!(!target_survives(3.9, 3.9, None));
        assert!(target_survives(4.1, 3.9, None));
        // Beyond 4 FWHM_ref a close companion is no longer a neighbour.
        assert!(target_survives(1.0, 4.1, None));
        // A star 2.1 mag brighter than G_b at 9.9 FWHM_ref drops it, at 10.1 keeps it.
        let gb = g_b(&wcs_at(1000, 1000, 1.0, (0.0, 0.0), 0.0));
        assert!(!target_survives(0.0, 9.9, Some(gb - 2.1)));
        assert!(target_survives(0.0, 10.1, Some(gb - 2.1)));
        // 1.9 mag brighter than G_b is not a halo source (the dG rule alone, at that distance, keeps it).
        assert!(target_survives(0.0, 9.9, Some(gb - 1.9)));
    }

    #[test]
    fn the_list_does_not_follow_the_seeing() {
        // The builder's input has no width: what can differ is the anchor's blend radius, and on a field with no
        // close neighbours it does not move the anchor, so the list is the same.
        let (w, h) = (1000usize, 1000usize);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let mut stars = Vec::new();
        let mut cat = Vec::new();
        for i in 0..400 {
            let (x, y) = (60.0 + (i % 20) as f64 * 44.0, 60.0 + (i / 20) as f64 * 44.0);
            let g = 14.0 + (i % 17) as f64 * 0.1;
            let flux = EXPTIME * 10f64.powf(0.4 * (ANCHOR - g));
            stars.push(metric(x, y, flux));
            cat.push(at(&wcs, x, y, g as f32));
        }
        let lists: Vec<ForcedList> = [3.0, 6.0]
            .iter()
            .map(|&fwhm| {
                let a = anchor_zero_point(&stars, None, &wcs, &cat, Some(EXPTIME), w, h, fwhm, &params()).unwrap();
                forced_list(&ForcedListInput { anchor: a.zero_point, ..input(&cat, &wcs, w, h) }, &params()).unwrap()
            })
            .collect();
        assert_eq!(lists[0], lists[1]);
        assert!(!lists[0].stars.is_empty());
    }

    fn metric(x: f64, y: f64, flux: f64) -> StarMetric {
        StarMetric {
            id: None,
            frame_analysis_id: 0,
            x,
            y,
            peak: 1000.0,
            flux,
            fwhm: 4.0,
            fwhm_x: 4.0,
            fwhm_y: 4.0,
            eccentricity: 0.1,
            snr: Some(100.0),
            hfr: None,
            theta: 0.0,
            beta: None,
            fit_method: "test".to_string(),
            fit_residual: 0.0,
        }
    }

    #[test]
    fn fwhm_ref_floors_at_two_pixels() {
        assert_eq!(fwhm_ref_px(1.87, &params()), 2.0);
        assert_eq!(fwhm_ref_px(0.5, &params()), 5.0);
        // The scale is the larger CD column norm.
        let mut w = wcs_at(100, 100, 1.0, (0.0, 0.0), 0.0);
        w.cd = [[-1.0 / 3600.0, 0.0], [0.0, 2.0 / 3600.0]];
        assert!((pixel_scale_arcsec(&w) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_edge_margin_and_region_hold() {
        let (w, h) = (1000usize, 1000usize);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let gb = g_b(&wcs);
        let g = (gb + 1.0) as f32;
        let margin = 13.0 * 2.5; // 32.5 px at 1 arcsec/px
        let probes: [(f64, f64, bool, &str); 8] = [
            (margin - 0.1, 500.0, false, "left, inside the margin"),
            (margin + 0.1, 500.0, true, "left, outside the margin"),
            (999.0 - margin + 0.1, 500.0, false, "right, inside the margin"),
            (999.0 - margin - 0.1, 500.0, true, "right, outside the margin"),
            (500.0, margin - 0.1, false, "top, inside the margin"),
            (500.0, 999.0 - margin + 0.1, false, "bottom, inside the margin"),
            // Along the diagonal: region radius 0.9 * 707.1 = 636.4 px; centre (499.5, 499.5).
            (499.5 + 445.0, 499.5 + 445.0, true, "diagonal, 629 px"),
            (499.5 + 455.0, 499.5 + 455.0, false, "diagonal, 643 px"),
        ];
        // One star per probe, far from each other so isolation never fires (the probes sit on different sides).
        for (x, y, listed, what) in probes {
            let cat = vec![at(&wcs, x, y, g)];
            let l = forced_list(&input(&cat, &wcs, w, h), &params()).unwrap();
            let found = l.stars.iter().any(|s| (s.x - x).abs() < 0.01 && (s.y - y).abs() < 0.01);
            assert_eq!(found, listed, "{what}");
        }
    }

    /// Isolated stars on a 40 px grid of a 1400 x 1400 frame (spacing beyond every isolation radius), their G from `gs`.
    fn grid_catalogue(wcs: &WcsSolution, gs: &[f64]) -> Vec<CatalogStar> {
        let mut out = Vec::new();
        for (i, &g) in gs.iter().enumerate() {
            let (x, y) = (120.0 + (i % 30) as f64 * 40.0, 120.0 + (i / 30) as f64 * 40.0);
            out.push(at(wcs, x, y, g as f32));
        }
        out
    }

    #[test]
    fn a_sparse_field_deepens_the_faint_edge_to_four_and_a_half_at_most() {
        let (w, h) = (1400usize, 1400usize);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let gb = g_b(&wcs);
        let edge = |gs: Vec<f64>| {
            let cat = grid_catalogue(&wcs, &gs);
            forced_list(&input(&cat, &wcs, w, h), &params()).unwrap()
        };
        // (a) 100 stars spread over G_b..G_b + 6: fewer than 200, the edge goes to 4.5 and no deeper.
        let sparse: Vec<f64> = (0..120).map(|i| gb + 6.0 * i as f64 / 119.0).collect();
        let l = edge(sparse);
        assert!((l.window_faint - (gb + 4.5)).abs() < 1e-9, "{}", l.window_faint);
        assert!(l.stars.iter().all(|s| f64::from(s.g_mag) <= gb + 4.5 + 1e-6));
        assert!(l.stars.iter().any(|s| f64::from(s.g_mag) > gb + 3.5), "the deepening admitted fainter stars");
        // (b) 250 stars within G_b + 3 and some beyond: the edge stays at 3.
        let dense: Vec<f64> =
            (0..250).map(|i| gb + 2.99 * i as f64 / 249.0).chain((0..50).map(|i| gb + 3.1 + 0.02 * i as f64)).collect();
        let l = edge(dense);
        assert!((l.window_faint - (gb + 3.0)).abs() < 1e-9, "{}", l.window_faint);
        assert_eq!(l.stars.len(), 250);
        // (c) 150 within G_b + 3 and 200 more up to G_b + 4.4: the edge is the 200th brightest's G.
        let mid: Vec<f64> =
            (0..150).map(|i| gb + 2.99 * i as f64 / 149.0).chain((0..200).map(|i| gb + 3.01 + 1.39 * i as f64 / 199.0)).collect();
        let l = edge(mid);
        let g200 = gb + 3.01 + 1.39 * 49.0 / 199.0;
        assert!((l.window_faint - g200).abs() < 1e-5, "{} vs {g200}", l.window_faint);
        assert_eq!(l.stars.len(), 200);
    }

    fn rich_field(n: usize, zp: f64, c_flux_scale: f64) -> (WcsSolution, Vec<StarMetric>, Vec<CatalogStar>) {
        let (w, h) = (1000usize, 1000usize);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let (mut stars, mut cat) = (Vec::new(), Vec::new());
        for i in 0..n {
            let (x, y) = (100.0 + (i % 20) as f64 * 40.0, 100.0 + (i / 20) as f64 * 40.0);
            let g = f64::from((14.0 + (i % 23) as f64 * 0.1) as f32);
            stars.push(metric(x, y, EXPTIME * 10f64.powf(0.4 * (zp - g)) / c_flux_scale));
            cat.push(at(&wcs, x, y, g as f32));
        }
        (wcs, stars, cat)
    }

    #[test]
    fn the_anchor_needs_ten_matches() {
        // 11 stars: the 6.5 matcher drops the brightest 15 % (one), 10 are matched.
        for (n, ok) in [(11usize, true), (10, false)] {
            let (wcs, stars, cat) = rich_field(n, ANCHOR, 1.0);
            let r = anchor_zero_point(&stars, None, &wcs, &cat, Some(EXPTIME), 1000, 1000, 3.0, &params());
            match (ok, r) {
                (true, Ok(a)) => assert!((a.zero_point - ANCHOR).abs() < 1e-9 && a.matched == 10, "{a:?}"),
                (false, Err(ForcedRefusal::NoAnchor { matched: 9 })) => {}
                (_, other) => panic!("{n} stars: {other:?}"),
            }
        }
        let (wcs, stars, cat) = rich_field(60, ANCHOR, 1.0);
        let r = anchor_zero_point(&stars, None, &wcs, &cat, None, 1000, 1000, 3.0, &params());
        assert_eq!(r, Err(ForcedRefusal::NoExposure));
    }

    #[test]
    fn the_anchor_takes_the_reported_c_on_a_mosaic() {
        // The mosaic's fits carry no aperture correction (their signal is the FWTM model flux); the reported c scales them.
        let (wcs, stars, cat) = rich_field(80, ANCHOR, 1.25);
        let without = anchor_zero_point(&stars, None, &wcs, &cat, Some(EXPTIME), 1000, 1000, 3.0, &params()).unwrap();
        let with = anchor_zero_point(&stars, Some(1.25), &wcs, &cat, Some(EXPTIME), 1000, 1000, 3.0, &params()).unwrap();
        assert!((with.zero_point - ANCHOR).abs() < 1e-9, "{}", with.zero_point);
        assert!((with.zero_point - without.zero_point - 2.5 * 1.25f64.log10()).abs() < 1e-9);
    }

    /// A list of `n` stars (G 14..17) and the fitter's answer for it: fluxes from `zp` with `scatter` mag of noise, `kept`
    /// of them kept, `c` as given.
    fn fitted(n: usize, zp: f64, scatter: f64, kept: usize, c: Option<f64>) -> (ForcedList, ListMeasurement) {
        let mut rng = Rng(77);
        let stars: Vec<ForcedStar> = (0..n)
            .map(|i| ForcedStar {
                key: i as u64,
                g_mag: (14.0 + 3.0 * i as f64 / n as f64) as f32,
                ra: 150.0,
                dec: 20.0,
                x: 100.0 + i as f64,
                y: 100.0,
            })
            .collect();
        let fits: Vec<ListFit> = stars
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let flux = EXPTIME * 10f64.powf(0.4 * (zp + scatter * rng.normal() - f64::from(s.g_mag)));
                let mut f = ListFit::test_kept(s.x, s.y, flux);
                f.kept = i < kept;
                f
            })
            .collect();
        let list = ForcedList {
            request: ListRequest::new(stars.iter().map(|s| (s.x, s.y)).collect(), 1.0),
            stars,
            list_before_cap: n,
            window_bright: 14.0,
            window_faint: 17.0,
            fwhm_ref_px: 2.5,
            scale_arcsec: 1.0,
        };
        (list, ListMeasurement::test_new(fits, c, 40))
    }

    #[test]
    fn forced_zero_point_recovers_the_truth() {
        let (list, m) = fitted(600, 21.8, 0.1, 600, Some(1.2));
        let o = forced_zero_point(&list, &m, EXPTIME, &params());
        let z = o.result.expect("600 stars are valid");
        assert!((z.zero_point - 21.8).abs() < 0.01, "{}", z.zero_point);
        assert!((z.sigma - 0.1).abs() < 0.02, "{}", z.sigma);
        assert!(z.stars >= 580, "{}", z.stars);
        assert_eq!((o.list_stars, o.aperture_correction, o.aperture_stars), (600, Some(1.2), 40));
        assert_eq!(o.per_star.len(), 600);
        assert!(o.per_star.iter().filter(|p| p.clipped_in).count() as u32 == z.stars);
    }

    #[test]
    fn forty_nine_kept_refuse_with_the_list_size() {
        let (list, m) = fitted(600, 21.8, 0.05, 49, Some(1.2));
        let o = forced_zero_point(&list, &m, EXPTIME, &params());
        assert_eq!(o.result, Err(ForcedRefusal::TooFewStars { list: 600, kept: 49 }));
        assert_eq!(o.result.unwrap_err().code(), "too_few_stars");
        let (list, m) = fitted(600, 21.8, 0.05, 50, Some(1.2));
        assert!(forced_zero_point(&list, &m, EXPTIME, &params()).result.is_ok(), "fifty survive");
    }

    #[test]
    fn a_wide_scatter_refuses() {
        let (list, m) = fitted(100, 21.8, 0.5, 100, Some(1.2));
        match forced_zero_point(&list, &m, EXPTIME, &params()).result {
            Err(ForcedRefusal::TooScattered { std_error, kept, sigma }) => {
                assert!(std_error > 0.035 && kept > 90 && sigma > 0.3, "{std_error} {kept} {sigma}");
            }
            other => panic!("expected TooScattered, got {other:?}"),
        }
    }

    #[test]
    fn no_c_refuses() {
        let (list, m) = fitted(300, 21.8, 0.05, 300, None);
        let o = forced_zero_point(&list, &m, EXPTIME, &params());
        assert_eq!(o.result, Err(ForcedRefusal::NoApertureCorrection { qualifiers: 40 }));
        assert_eq!(o.result.unwrap_err().code(), "no_aperture_correction");
        assert_eq!(o.aperture_correction, None);
    }

    #[test]
    fn the_refusal_codes_are_stable_and_one_is_not_stored() {
        let all = [
            (ForcedRefusal::NoExposure, "no_exposure"),
            (ForcedRefusal::NoAnchor { matched: 3 }, "no_anchor"),
            (ForcedRefusal::NoApertureCorrection { qualifiers: 1 }, "no_aperture_correction"),
            (ForcedRefusal::TooFewStars { list: 1, kept: 1 }, "too_few_stars"),
            (ForcedRefusal::TooScattered { std_error: 1.0, kept: 1, sigma: 1.0 }, "too_scattered"),
            (ForcedRefusal::ColourCamera, "colour_camera"),
            (ForcedRefusal::UnreadableSolve, "unreadable_solve"),
            (ForcedRefusal::FrameVerdict, "frame_verdict"),
            (ForcedRefusal::CatalogueUnreadable, "catalogue_unreadable"),
        ];
        for (r, code) in all {
            assert_eq!(r.code(), code);
            assert_eq!(r.is_stored(), code != "catalogue_unreadable", "{code}");
        }
    }

    #[test]
    fn an_unfitted_or_unkept_star_is_left_out_of_the_statistic() {
        let (list, mut m) = fitted(200, 21.8, 0.02, 200, Some(1.2));
        m.fits[3] = ListFit::test_refused();
        m.fits[4].flux = f64::NAN;
        m.fits[5].flux = -1.0;
        let o = forced_zero_point(&list, &m, EXPTIME, &params());
        assert_eq!(o.fitted_kept, 197);
        assert!(o.per_star.iter().all(|p| ![3, 4, 5].contains(&p.index)));
    }
}
