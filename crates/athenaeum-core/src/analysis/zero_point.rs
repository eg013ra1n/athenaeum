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
use crate::star_measure::analysis_mode::{ListAperture, ListFit, ListMeasurement, ListPlacement, ListRequest};
use crate::star_measure::{Seed, MEASURE_VERSION};

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
// preliminary zero point (the anchor); the map stars among them are fitted, their offsets from their projections give a
// quadratic map of the frame, every list star is summed in a plain aperture at its moved position, and the statistic is
// taken over those sums (spec §5.1-§5.3). The composition that reads a file and measures the list is
// `analysis::analyzer::measure_with_zero_point`.

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
    /// Width of each edge's weight ramp (mag): a star's weight is 0 at an edge and 1 this far inside it.
    pub window_ramp_mag: f64,
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
    /// The frame's own detections isolate too (the catalogue lacks what the image carries: sources dropped for a
    /// high RUWE, stars below the tiers' depth, galaxies). The rule runs AFTER the frame's map, on the list stars at their
    /// MAPPED positions, and takes stars out of the MEMBERS only (the list, the map stars and the gate are the catalogue
    /// rules' alone): a detected seed other than the star's own, within `isolation_radius_fwhm * FWHM_ref` of it, with
    /// `G_seed <= G + isolation_dmag`, `G_seed <= G_b + seed_neighbour_depth_mag` and a peak of at least
    /// `detected_neighbour_min_peak_sigma` plane sigmas, removes the star. `G_seed` is the seed's flux read through a
    /// per-frame calibration to the catalogue G. The catalogue halo rule's mirror: a seed with
    /// `G_seed <= G_b - bright_dmag` within `bright_radius_fwhm * FWHM_ref` removes the star too (no peak floor: a seed that
    /// bright is a halo source whatever its peak). Skipped when the plane handed no seeds.
    pub detected_neighbours: bool,
    /// A seed counts as a neighbour only down to `G_b + seed_neighbour_depth_mag`: the depth the map window reaches, so
    /// a star's membership follows the field and not the night's seeing (a seed fainter than that is detected on some
    /// frames and not on others).
    pub seed_neighbour_depth_mag: f64,
    /// A star's own seed is the nearest one within `max(own_seed_radius_px, FWHM_ref)` of its projection.
    pub own_seed_radius_px: f64,
    /// A neighbouring seed counts from this peak (background-subtracted, in plane sigmas).
    pub detected_neighbour_min_peak_sigma: f64,
    /// The seed flux calibration needs at least this many (star, own seed) pairs; fewer skip the rule for the frame.
    pub seed_calibration_min_pairs: usize,
    /// A guard: a list above this many stars is thinned by a hash of the catalogue record. No rig reaches it.
    pub cap: usize,
    /// The map stars are the list stars with `G <= G_b + map_window_mag`, at most `map_max_stars` of them in key order.
    pub map_window_mag: f64,
    pub map_max_stars: usize,
    /// A map member is accepted, unsaturated, has an SNR of at least this (where it has one) and lies within
    /// `map_radius_fwhm * FWHM_ref` of its seed.
    pub map_min_snr: f64,
    pub map_radius_fwhm: f64,
    /// Rounds of clipping of the map's residuals at `map_clip_sigma` robust sigmas, never below `map_clip_floor_px`.
    pub map_clip_rounds: usize,
    pub map_clip_sigma: f64,
    pub map_clip_floor_px: f64,
    /// A map of fewer members is refused.
    pub map_min_members: usize,
    /// The share of the map stars the second fit must keep (accepted, unsaturated, centred) for the map to stand.
    pub map_min_kept_share: f64,
    /// A refit is kept when centred within `max(centre_tolerance_px, centre_tolerance_fwhm * FWHM_ref)` of its position.
    pub centre_tolerance_px: f64,
    pub centre_tolerance_fwhm: f64,
    /// The aperture radius is `max(aperture_radius_ref * FWHM_ref, aperture_radius_fwhm * F)`, `F` the frame's median
    /// FWHM.
    pub aperture_radius_ref: f64,
    pub aperture_radius_fwhm: f64,
    /// The three radii (in `FWHM_ref`) the sky pedestal is read at: the line of `S(c) - S(b)` against `S(a)`.
    pub pedestal_radii_ref: [f64; 3],
    /// The pedestal line is clipped at this many robust sigmas for this many rounds; fewer points left refuse it.
    pub pedestal_clip_sigma: f64,
    pub pedestal_clip_rounds: usize,
    pub pedestal_min_points: usize,
    /// The sky annulus: its area (px), its start `max(annulus_start_fwhm * F, outer pedestal radius)` and the gap added.
    pub annulus_area_px: f64,
    pub annulus_start_fwhm: f64,
    pub annulus_gap_px: f64,
    /// The highest pixel of a star is read within this many `FWHM_ref`.
    pub peak_radius_ref: f64,
    /// The screen's clip (also the noise allowance, `clip_sigma * n`), the fewest members of non-zero weight and the
    /// largest scatter (mag) of a valid zero point.
    pub clip_sigma: f64,
    pub min_stars: usize,
    pub max_sigma_mag: f64,
    /// The screen's upper rule is a taper: a member `d` mag above `m + clip_sigma * sigma` keeps its own flux at the
    /// weight `clamp(1 - d / (screen_taper_sigma * sigma), 0, 1)`, so a star the jitter of `m` and `sigma` carries across
    /// the limit moves the zero point continuously. `screen_taper_sigma <= 1e-9` is a plain drop, the default (the F5
    /// emulation's rule, measured better than the taper on the thinnest night, G4/G7); the taper stays reachable by the
    /// flag. NaN is refused by [`ForcedListParams::validate`].
    pub screen_taper_sigma: f64,
    /// The window slope needs at least this many members on the bright ramp.
    pub slope_min_ramp_members: usize,
    /// Probe only (RR4's round), the identity by default: a list star stays when `key % decimate == 0`.
    pub decimate: u64,
    /// Probe only (M3), 0 by default: added to the anchor before the window rule.
    pub anchor_shift_mag: f64,
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
            window_ramp_mag: 0.5,
            region_fraction: 0.9,
            edge_margin_fwhm: 13.0,
            isolation_dmag: 4.0,
            isolation_radius_fwhm: 8.0,
            bright_dmag: 2.0,
            bright_radius_fwhm: 10.0,
            detected_neighbours: true,
            seed_neighbour_depth_mag: 2.0,
            own_seed_radius_px: 1.5,
            detected_neighbour_min_peak_sigma: 10.0,
            seed_calibration_min_pairs: 20,
            cap: 12_000,
            map_window_mag: 2.0,
            map_max_stars: 400,
            map_min_snr: 10.0,
            map_radius_fwhm: 2.0,
            map_clip_rounds: 3,
            map_clip_sigma: 3.0,
            map_clip_floor_px: 0.15,
            map_min_members: 30,
            map_min_kept_share: 0.75,
            centre_tolerance_px: 1.0,
            centre_tolerance_fwhm: 0.5,
            aperture_radius_ref: 4.0,
            aperture_radius_fwhm: 2.0,
            pedestal_radii_ref: [3.0, 4.0, 6.0],
            pedestal_clip_sigma: 3.0,
            pedestal_clip_rounds: 4,
            pedestal_min_points: 20,
            annulus_area_px: 2000.0,
            annulus_start_fwhm: 4.0,
            annulus_gap_px: 2.0,
            peak_radius_ref: 1.5,
            clip_sigma: 4.0,
            min_stars: 50,
            max_sigma_mag: 1.0,
            screen_taper_sigma: 0.0,
            slope_min_ramp_members: 10,
            decimate: 1,
            anchor_shift_mag: 0.0,
        }
    }
}

impl ForcedListParams {
    /// A start-up check of the constants a caller may set (the probe's flags): a NaN taper is an error of the params, never
    /// a stored `too_scattered`. Errors are logged by the caller before it exits.
    pub fn validate(&self) -> Result<(), String> {
        if self.screen_taper_sigma.is_nan() {
            return Err("screen_taper_sigma is NaN (0 drops, a positive value tapers)".to_string());
        }
        for (name, v) in [
            ("own_seed_radius_px", self.own_seed_radius_px),
            ("seed_neighbour_depth_mag", self.seed_neighbour_depth_mag),
            ("detected_neighbour_min_peak_sigma", self.detected_neighbour_min_peak_sigma),
        ] {
            if v.is_nan() {
                return Err(format!("{name} is NaN"));
            }
        }
        Ok(())
    }
}

/// Why a frame has no forced-list zero point. Every code but `catalogue_unreadable` and `internal` is stored (an outcome
/// a re-read of the same file and solve would repeat); those two are failures of this run and store nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ForcedRefusal {
    /// `EXPTIME` missing, not finite, or not positive.
    NoExposure,
    /// The anchor (6.5's matcher over the budget fits) left fewer than the needed survivors of its clipping: `kept` is
    /// the survivors, what the rule counts (0 when the matcher found none).
    NoAnchor { kept: u32 },
    /// The stars are not where the plate solve puts them: the map of the map stars' own fits has too few members, or the
    /// second fit kept too small a share of them. `kept_share` is that share (NaN when the map itself failed).
    PoorSolve { kept_share: f64 },
    /// Fewer members than the minimum, with the list's size before fitting.
    TooFewStars { list: u32, kept: u32 },
    /// The sky pedestal cannot be fitted, the sum of the weighted fluxes is not positive, or the scatter of the
    /// members' zero points is above the limit (`sigma`, NaN when none was measured).
    TooScattered { kept: u32, sigma: f64 },
    /// A debayered colour file: no list is measured on a three-plane image.
    ColourCamera,
    /// The stored solve gives no sky-to-pixel transform or no usable cone.
    UnreadableSolve,
    /// The plane has a verdict (trailed, defocused, no stars) that a re-read would repeat; the analysis row carries it.
    FrameVerdict,
    /// The catalogue could not be read this time (an error or a panic of the cone); nothing is stored, a later run tries
    /// again.
    CatalogueUnreadable,
    /// An invariant between this code and the fitter broke (the request was refused although it was built, the fits do
    /// not match the list, the composition panicked): a bug, not a verdict on the frame. Logged, nothing is stored, so a
    /// build that fixes it measures the frame again.
    Internal,
}

impl ForcedRefusal {
    /// Stable code of the refusal, the value stored and logged.
    pub fn code(&self) -> &'static str {
        match self {
            ForcedRefusal::NoExposure => "no_exposure",
            ForcedRefusal::NoAnchor { .. } => "no_anchor",
            ForcedRefusal::PoorSolve { .. } => "poor_solve",
            ForcedRefusal::TooFewStars { .. } => "too_few_stars",
            ForcedRefusal::TooScattered { .. } => "too_scattered",
            ForcedRefusal::ColourCamera => "colour_camera",
            ForcedRefusal::UnreadableSolve => "unreadable_solve",
            ForcedRefusal::FrameVerdict => "frame_verdict",
            ForcedRefusal::CatalogueUnreadable => "catalogue_unreadable",
            ForcedRefusal::Internal => "internal",
        }
    }

    /// Whether the outcome is written to the catalogue (false: a failure of this run, retried later).
    pub fn is_stored(&self) -> bool {
        !matches!(self, ForcedRefusal::CatalogueUnreadable | ForcedRefusal::Internal)
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
/// limit. Fewer survivors: `NoAnchor { kept }` (the survivors after the clip, what the rule counts); no exposure:
/// `NoExposure`.
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
        Err(ZpRefusal::TooFewStars { kept, .. } | ZpRefusal::TooScattered { kept, .. }) => {
            Err(ForcedRefusal::NoAnchor { kept })
        }
    }
}

/// Everything the list builder reads. It takes no FWHM and no detection of the frame: the list follows the catalogue, the
/// rig and the frame's light, never the night's seeing (the frame's own seeds act later, on the members:
/// [`seed_rule_in`]).
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
    /// The list, in key order.
    pub stars: Vec<ForcedStar>,
    /// The list's size before the guard's thinning.
    pub list_before_cap: usize,
    /// `G_b` and the faint edge of the window.
    pub window_bright: f64,
    pub window_faint: f64,
    pub fwhm_ref_px: f64,
    pub scale_arcsec: f64,
    /// The frame's size (px): the map's coordinates are about its centre.
    pub width: usize,
    pub height: usize,
    /// Per list star: removed from the members by the seed rule ([`seed_rule_in`]); empty until the rule ran (the list
    /// itself is the catalogue rules' alone, and the map stars and the gate never see this).
    pub seed_removed: Vec<bool>,
    /// List stars the frame's own detections removed from the members (the count of `seed_removed`).
    pub detected_neighbours_removed: u32,
    /// (star, own seed) pairs the seed flux calibration was taken over (0 when the rule did not run: no seeds, switched
    /// off, the map refused, or the calibration had fewer than `seed_calibration_min_pairs`, in which case this is the
    /// pairs found).
    pub seed_calibration_pairs: u32,
    /// Seeds the peak floor alone kept from removing a star: depth-eligible (`G_seed <= G_b + seed_neighbour_depth_mag`),
    /// within the isolation radius of a list star, no brighter than it plus `isolation_dmag`, with a peak under the floor.
    pub seed_peak_floor_rejected: u32,
    /// The map stars: indices into `stars` (`G <= G_b + map_window_mag`, the first `map_max_stars` in key order).
    pub map: Vec<usize>,
    /// What the first fit pass is asked: the map stars' positions in `map` order and the centre tolerance.
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
/// alone, in the window `[G_b, faint]`, thinned by hash above the guard, and the map stars among them. Reads no pixel
/// and no seeing, and none of the frame's detections.
pub fn forced_list(input: &ForcedListInput, params: &ForcedListParams) -> Result<ForcedList, ForcedRefusal> {
    forced_list_in(input, params, None)
}

/// [`forced_list`] with the cone's projection and the isolation test (one independent answer per catalogue star) run on
/// `pool` (the global pool when there is none); the list is the same, star for star and in the same order.
pub fn forced_list_in(
    input: &ForcedListInput,
    params: &ForcedListParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<ForcedList, ForcedRefusal> {
    use rayon::prelude::*;
    fn on<R: Send>(pool: Option<&Arc<rayon::ThreadPool>>, f: impl FnOnce() -> R + Send) -> R {
        match pool {
            Some(p) => p.install(f),
            None => f(),
        }
    }
    if !(input.exptime_s.is_finite() && input.exptime_s > 0.0) {
        return Err(ForcedRefusal::NoExposure);
    }
    let scale = pixel_scale_arcsec(input.wcs);
    let fwhm_ref = fwhm_ref_px(scale, params);
    // M3: a real loss moves the anchor, and each edge then follows its own rule; the probe's shift is added here.
    let g_b = window(input.anchor + params.anchor_shift_mag, fwhm_ref, input.full_scale, input.exptime_s, params);
    if !(g_b.is_finite() && fwhm_ref.is_finite()) {
        return Err(ForcedRefusal::NoAnchor { kept: 0 });
    }

    // The cone in pixels (non-finite skipped, as `measure_zero_point` does), with each star's sky record.
    let projected: Vec<((f64, f64, f32), (f64, f64))> = on(pool, || {
        input
            .catalog
            .par_iter()
            .filter_map(|c| {
                if !c.mag.is_finite() {
                    return None;
                }
                let (x, y) = input.wcs.sky_to_pixel(c.ra, c.dec);
                (x.is_finite() && y.is_finite()).then_some(((x, y, c.mag), (c.ra, c.dec)))
            })
            .collect()
    });
    let (cat, sky): (Vec<(f64, f64, f32)>, Vec<(f64, f64)>) = projected.into_iter().unzip();
    let near_r = params.isolation_radius_fwhm * fwhm_ref;
    let far_r = params.bright_radius_fwhm * fwhm_ref;
    let grid = Grid::new(&cat, far_r.max(near_r).max(1.0));

    let (w, h) = (input.width as f64, input.height as f64);
    let centre = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let region = params.region_fraction * 0.5 * w.hypot(h);
    let margin = params.edge_margin_fwhm * fwhm_ref;
    let faint_limit = g_b + params.window_faint_max_mag;
    let decimate = params.decimate.max(1);

    // Isolated candidates of the deepest window, by the catalogue alone.
    let isolated: Vec<(f64, usize, u64)> = on(pool, || {
        cat.par_iter()
            .enumerate()
            .filter_map(|(i, &(x, y, g))| {
                let g = f64::from(g);
                if g < g_b || g > faint_limit {
                    return None;
                }
                if (x - centre.0).hypot(y - centre.1) > region
                    || x < margin
                    || y < margin
                    || x > w - 1.0 - margin
                    || y > h - 1.0 - margin
                {
                    return None;
                }
                let crowded = grid.near(x, y).any(|j| {
                    if j == i {
                        return false;
                    }
                    let d = (cat[j].0 - x).hypot(cat[j].1 - y);
                    let gj = f64::from(cat[j].2);
                    (gj <= g + params.isolation_dmag && d <= near_r)
                        || (gj <= g_b - params.bright_dmag && d <= far_r)
                });
                if crowded {
                    return None;
                }
                Some((g, i, cap_key(cat[i].2, sky[i].0, sky[i].1)))
            })
            .collect()
    });

    // The probe's decimation thins the candidates (the same share of the catalogue stays in every frame: the thinning is
    // on the key, before the faint edge's count).
    let isolated: Vec<(f64, usize, u64)> = isolated.into_iter().filter(|(_, _, key)| key % decimate == 0).collect();

    // The faint edge: G_b + 3, deepened by catalogue count up to G_b + 4.5 while the window is thin.
    let shallow_edge = g_b + params.window_faint_width_mag;
    let in_shallow = isolated.iter().filter(|(g, _, _)| *g <= shallow_edge).count();
    let window_faint = if in_shallow >= params.min_isolated {
        shallow_edge
    } else {
        let mut gs: Vec<f64> = isolated.iter().map(|(g, _, _)| *g).collect();
        gs.sort_by(|a, b| a.total_cmp(b));
        match gs.get(params.min_isolated.saturating_sub(1)) {
            Some(&g200) if params.min_isolated > 0 => g200.min(faint_limit),
            _ => faint_limit,
        }
    };

    let kept: Vec<(usize, u64)> =
        isolated.iter().filter(|(g, _, _)| *g <= window_faint).map(|&(_, i, key)| (i, key)).collect();
    let list_before_cap = kept.len();
    let share = params.cap as f64 / list_before_cap.max(1) as f64;
    let mut stars: Vec<ForcedStar> = kept
        .into_iter()
        .filter_map(|(i, key)| {
            let (x, y, g) = cat[i];
            let (ra, dec) = sky[i];
            (key as f64 / KEY_SPACE < share).then_some(ForcedStar { key, g_mag: g, ra, dec, x, y })
        })
        .collect();
    stars.sort_by_key(|s| s.key);

    // The map stars: the bright end of the list, the first `map_max_stars` in key order.
    let map_edge = g_b + params.map_window_mag;
    let map: Vec<usize> = stars
        .iter()
        .enumerate()
        .filter(|(_, s)| f64::from(s.g_mag) <= map_edge)
        .map(|(i, _)| i)
        .take(params.map_max_stars)
        .collect();

    let tolerance = params.centre_tolerance_px.max(params.centre_tolerance_fwhm * fwhm_ref);
    let request = ListRequest::new(map.iter().map(|&i| (stars[i].x, stars[i].y)).collect(), tolerance);
    Ok(ForcedList {
        stars,
        list_before_cap,
        window_bright: g_b,
        window_faint,
        fwhm_ref_px: fwhm_ref,
        scale_arcsec: scale,
        width: input.width,
        height: input.height,
        seed_removed: Vec::new(),
        detected_neighbours_removed: 0,
        seed_calibration_pairs: 0,
        seed_peak_floor_rejected: 0,
        map,
        request,
    })
}

/// What the frame's own detections did to the list ([`seed_rule_in`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeedRule {
    /// Per list star: removed from the members by this rule; empty when the rule did not run.
    pub removed: Vec<bool>,
    pub removed_count: u32,
    /// (star, own seed) pairs of the flux calibration.
    pub pairs: u32,
    /// Seeds the peak floor alone kept from removing a star (see [`ForcedList::seed_peak_floor_rejected`]).
    pub peak_floor_rejected: u32,
}

impl SeedRule {
    fn none(pairs: u32) -> Self {
        SeedRule { pairs, ..SeedRule::default() }
    }
}

impl ForcedList {
    /// Record what the seed rule did. The list's stars, its window and its map stars do not change: the rule acts on the
    /// members ([`forced_zero_point`] leaves a removed star out of every sum).
    pub fn apply_seed_rule(&mut self, rule: SeedRule) {
        self.seed_removed = rule.removed;
        self.detected_neighbours_removed = rule.removed_count;
        self.seed_calibration_pairs = rule.pairs;
        self.seed_peak_floor_rejected = rule.peak_floor_rejected;
    }
}

/// The members' isolation from the frame's own detections (spec §5.1). The catalogue misses what the image has (sources
/// dropped at ingest for RUWE >= 1.4, stars under the tiers' depth, galaxies); a list star is removed from the members when
/// a detected seed that is not its own lies within `isolation_radius_fwhm * FWHM_ref` of it with `G_seed <= G +
/// isolation_dmag`, `G_seed <= G_b + seed_neighbour_depth_mag` and a peak of at least `detected_neighbour_min_peak_sigma`
/// plane sigmas: the catalogue rule's own question, asked of the image, to the depth the map window reaches (a seed
/// fainter than that is found on some frames and not on others, and a membership that follows it follows the seeing).
/// The halo rule is mirrored too: a seed with `G_seed <= G_b - bright_dmag` within `bright_radius_fwhm * FWHM_ref` removes
/// the star, whatever its peak.
///
/// It runs on the list stars at their MAPPED positions (`positions`, in list order: where the frame's map put each star),
/// so a frame whose solve is off by a few pixels still finds a star's own seed beside it. A star's own seed is the nearest
/// one within `max(own_seed_radius_px, FWHM_ref)` of that position. A seed's magnitude is read through the frame's
/// calibration `k`, the median of `flux * 10^(0.4 G)` over the stars of the shallow window (`G <= G_b +
/// window_faint_width_mag`) with exactly one seed within that radius, that seed being nearer to no other such star:
/// `G_seed = -2.5 log10(flux / k)`. Fewer pairs than `seed_calibration_min_pairs`, no seeds, or the rule switched off:
/// nothing is removed (never a refusal). The answer for a star is independent of the others, so the pool changes
/// nothing.
pub fn seed_rule_in(
    list: &ForcedList,
    positions: &[(f64, f64)],
    seeds: &[Seed],
    noise_sigma: f64,
    params: &ForcedListParams,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> SeedRule {
    use rayon::prelude::*;
    if !params.detected_neighbours {
        return SeedRule::none(0);
    }
    if positions.len() != list.stars.len() {
        tracing::warn!(
            positions = positions.len(),
            stars = list.stars.len(),
            "forced list: the mapped positions and the list differ in length; the seed rule is skipped"
        );
        return SeedRule::none(0);
    }
    if seeds.is_empty() {
        tracing::debug!("forced list: the plane handed no seeds; no isolation from the frame's own detections");
        return SeedRule::none(0);
    }
    let fwhm_ref = list.fwhm_ref_px;
    let (g_b, near_r, far_r) = (
        list.window_bright,
        params.isolation_radius_fwhm * fwhm_ref,
        params.bright_radius_fwhm * fwhm_ref,
    );
    let own_r = params.own_seed_radius_px.max(fwhm_ref);
    let floor = params.detected_neighbour_min_peak_sigma * noise_sigma;
    let depth_edge = g_b + params.seed_neighbour_depth_mag;
    if !(own_r.is_finite() && near_r.is_finite() && far_r.is_finite() && floor.is_finite() && own_r > 0.0 && depth_edge.is_finite())
    {
        tracing::warn!(own_r, near_r, far_r, floor, depth_edge, "forced list: the seed rule's constants are unusable; the rule is skipped");
        return SeedRule::none(0);
    }
    // Seeds with a usable position and flux, as grid points.
    let seeds: Vec<&Seed> = seeds
        .iter()
        .filter(|s| s.x.is_finite() && s.y.is_finite() && s.flux.is_finite() && s.flux > 0.0 && s.peak.is_finite())
        .collect();
    if seeds.is_empty() {
        tracing::debug!("forced list: no seed has a usable flux; no isolation from the frame's own detections");
        return SeedRule::none(0);
    }
    let cell = own_r.max(near_r).max(far_r).max(1.0);
    let seed_pts: Vec<(f64, f64, f32)> = seeds.iter().map(|s| (s.x, s.y, 0.0)).collect();
    let seed_grid = Grid::new(&seed_pts, cell);

    // The calibration set: the stars of the shallow window, the list's common case, at their mapped positions.
    let shallow = g_b + params.window_faint_width_mag;
    let cal_pts: Vec<(f64, f64, f32)> = list
        .stars
        .iter()
        .zip(positions)
        .filter(|(s, p)| f64::from(s.g_mag) <= shallow && p.0.is_finite() && p.1.is_finite())
        .map(|(s, p)| (p.0, p.1, s.g_mag))
        .collect();
    let cal_grid = Grid::new(&cal_pts, cell);
    let calibrate = || -> Vec<f64> {
        cal_pts
            .par_iter()
            .enumerate()
            .filter_map(|(c, &(x, y, g))| {
                let mut own = None;
                for j in seed_grid.near(x, y) {
                    let d = (seed_pts[j].0 - x).hypot(seed_pts[j].1 - y);
                    if d <= own_r {
                        if own.is_some() {
                            return None; // two seeds in the radius: not a clean pair
                        }
                        own = Some((j, d));
                    }
                }
                let (j, d) = own?;
                // The seed must be nearer to no other star.
                let stolen = cal_grid.near(seed_pts[j].0, seed_pts[j].1).any(|c2| {
                    c2 != c && (cal_pts[c2].0 - seed_pts[j].0).hypot(cal_pts[c2].1 - seed_pts[j].1) <= d
                });
                (!stolen).then(|| seeds[j].flux * 10f64.powf(0.4 * f64::from(g)))
            })
            .collect()
    };
    let ratios = match pool {
        Some(p) => p.install(calibrate),
        None => calibrate(),
    };
    let pairs = ratios.len() as u32;
    if ratios.len() < params.seed_calibration_min_pairs.max(1) {
        tracing::warn!(
            pairs,
            needed = params.seed_calibration_min_pairs,
            seeds = seeds.len(),
            "forced list: too few star-seed pairs to calibrate the seeds' flux; no isolation from the frame's own detections"
        );
        return SeedRule::none(pairs);
    }
    let mut ratios = ratios;
    let k = median(&mut ratios);
    if !(k.is_finite() && k > 0.0) {
        tracing::warn!(k, pairs, "forced list: the seed flux calibration is unusable; no isolation from the frame's own detections");
        return SeedRule::none(pairs);
    }

    // The test, on every list star.
    let floor_hit: Vec<std::sync::atomic::AtomicBool> =
        (0..seeds.len()).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
    let test = || seed_flags(list, positions, &seeds, &seed_pts, &seed_grid, &floor_hit, k, [own_r, near_r, far_r], floor, depth_edge, params);
    let removed: Vec<bool> = match pool {
        Some(p) => p.install(test),
        None => test(),
    };
    let removed_count = removed.iter().filter(|r| **r).count() as u32;
    let peak_floor_rejected =
        floor_hit.iter().filter(|f| f.load(std::sync::atomic::Ordering::Relaxed)).count() as u32;
    SeedRule { removed, removed_count, pairs, peak_floor_rejected }
}

/// The per-star test of [`seed_rule_in`], on the rayon pool the caller is on. `floor_hit[j]` is set when seed `j` passed
/// every test but the peak floor for some star.
#[allow(clippy::too_many_arguments)]
fn seed_flags(
    list: &ForcedList,
    positions: &[(f64, f64)],
    seeds: &[&Seed],
    seed_pts: &[(f64, f64, f32)],
    seed_grid: &Grid,
    floor_hit: &[std::sync::atomic::AtomicBool],
    k: f64,
    [own_r, near_r, far_r]: [f64; 3],
    floor: f64,
    depth_edge: f64,
    params: &ForcedListParams,
) -> Vec<bool> {
    use rayon::prelude::*;
    let halo_g = list.window_bright - params.bright_dmag;
    list.stars
        .par_iter()
        .zip(positions.par_iter())
        .map(|(star, &(x, y))| {
            if !(x.is_finite() && y.is_finite()) {
                return false;
            }
            let g = f64::from(star.g_mag);
            // The star's own seed: the nearest within `own_r` of its mapped position.
            let mut own: Option<(usize, f64)> = None;
            for j in seed_grid.near(x, y) {
                let d = (seed_pts[j].0 - x).hypot(seed_pts[j].1 - y);
                if d <= own_r && own.map_or(true, |(_, best)| d < best) {
                    own = Some((j, d));
                }
            }
            let own = own.map(|(j, _)| j);
            let mut removed = false;
            for j in seed_grid.near(x, y) {
                if Some(j) == own {
                    continue;
                }
                let s = seeds[j];
                let d = (s.x - x).hypot(s.y - y);
                if d > near_r.max(far_r) {
                    continue;
                }
                let g_seed = -2.5 * (s.flux / k).log10();
                // The catalogue halo rule's mirror (a seed that bright needs no peak), and the isolation rule, to the map
                // window's depth, with a peak floor as a guard.
                let halo = d <= far_r && g_seed <= halo_g;
                let neighbour = d <= near_r && g_seed <= g + params.isolation_dmag && g_seed <= depth_edge;
                if halo || (neighbour && s.peak >= floor) {
                    removed = true;
                } else if neighbour {
                    floor_hit[j].store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            removed
        })
        .collect()
}

/// A star's weight in the window: 0 at an edge, 1 `window_ramp_mag` inside it, so a zero point changes continuously as
/// the window moves.
pub fn window_weight(g_mag: f64, list: &ForcedList, params: &ForcedListParams) -> f64 {
    let rho = params.window_ramp_mag.max(f64::MIN_POSITIVE);
    (((g_mag - list.window_bright) / rho).min((list.window_faint - g_mag) / rho)).clamp(0.0, 1.0)
}

// ---- the map ----------------------------------------------------------------------------------------------------

/// Ordinary least squares of six coefficients by the normal equations on column-scaled data, Gauss-Jordan with partial
/// pivoting. `None` for a singular design or non-finite coefficients.
fn least_squares6(rows: &[[f64; 6]], y: &[f64]) -> Option<[f64; 6]> {
    const P: usize = 6;
    if rows.len() <= P || y.len() != rows.len() {
        return None;
    }
    let mut scale = [0.0f64; P];
    for (j, s) in scale.iter_mut().enumerate() {
        *s = rows.iter().map(|r| r[j] * r[j]).sum::<f64>().sqrt();
        if !(*s > 0.0) || !s.is_finite() {
            return None;
        }
    }
    let mut a = [[0.0f64; 2 * P]; P];
    let mut b = [0.0f64; P];
    for (r, yi) in rows.iter().zip(y) {
        for i in 0..P {
            let xi = r[i] / scale[i];
            b[i] += xi * yi;
            for j in i..P {
                a[i][j] += xi * (r[j] / scale[j]);
            }
        }
    }
    for i in 0..P {
        for j in 0..i {
            a[i][j] = a[j][i];
        }
        a[i][P + i] = 1.0;
    }
    for col in 0..P {
        let pivot_row = (col..P).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot_row][col].abs() < 1e-10 {
            return None;
        }
        a.swap(col, pivot_row);
        let pivot = a[col][col];
        for v in a[col].iter_mut() {
            *v /= pivot;
        }
        let pivot_values = a[col];
        for i in 0..P {
            if i != col {
                let f = a[i][col];
                if f != 0.0 {
                    for (v, d) in a[i].iter_mut().zip(pivot_values) {
                        *v -= f * d;
                    }
                }
            }
        }
    }
    let mut beta = [0.0f64; P];
    for i in 0..P {
        beta[i] = (0..P).map(|j| a[i][P + j] * b[j]).sum::<f64>() / scale[i];
    }
    beta.iter().all(|v| v.is_finite()).then_some(beta)
}

/// The frame's own map of `fitted - projected`: a full quadratic of the position in each axis, fitted over the map
/// stars' accepted fits.
#[derive(Debug, Clone, PartialEq)]
pub struct QuadMap {
    cx: f64,
    cy: f64,
    half: f64,
    ax: [f64; 6],
    ay: [f64; 6],
    /// Members of the last fit.
    pub members: usize,
    /// Median |fitted - projected| over the members before the clipping, px.
    pub offset_px: f64,
    /// Median |residual| of the members after the last fit, px.
    pub residual_px: f64,
}

impl QuadMap {
    fn basis(&self, x: f64, y: f64) -> [f64; 6] {
        let (u, v) = ((x - self.cx) / self.half, (y - self.cy) / self.half);
        [1.0, u, v, u * u, u * v, v * v]
    }

    /// `(dx, dy)` to add to a projected position.
    pub fn shift(&self, x: f64, y: f64) -> (f64, f64) {
        let b = self.basis(x, y);
        (b.iter().zip(&self.ax).map(|(p, q)| p * q).sum(), b.iter().zip(&self.ay).map(|(p, q)| p * q).sum())
    }
}

/// The map of the map stars' fits (`fits` in `list.map` order): members are the accepted, unsaturated fits with an SNR of
/// at least `map_min_snr` where one is known (a fit with no ring sky has none and is not refused for it) within
/// `map_radius_fwhm * FWHM_ref` of their seeds; up to `map_clip_rounds` rounds drop the residuals over
/// `max(map_clip_sigma * 1.4826 * median |residual|, map_clip_floor_px)`, stopping when nothing drops or fewer than
/// `map_min_members` would stay. Fewer members: `TooFewStars` on a list of fewer than `ceil(map_min_members /
/// map_min_kept_share)` map stars (a sparse field, not a bad solve), `PoorSolve` otherwise.
pub fn position_map(list: &ForcedList, fits: &[ListFit], params: &ForcedListParams) -> Result<QuadMap, ForcedRefusal> {
    if fits.len() != list.map.len() {
        tracing::warn!(map = list.map.len(), fits = fits.len(), "forced list and its fits differ in length");
        return Err(ForcedRefusal::Internal);
    }
    let radius = params.map_radius_fwhm * list.fwhm_ref_px;
    let mut pts: Vec<(f64, f64, f64, f64)> = list
        .map
        .iter()
        .zip(fits)
        .filter(|(_, f)| {
            f.accepted
                && !f.flags.saturated()
                && f.x.is_finite()
                && f.y.is_finite()
                && f.centre_offset_px.is_finite()
                && f.centre_offset_px <= radius
                && !(f.snr.is_finite() && f.snr < params.map_min_snr)
        })
        .map(|(&i, f)| {
            let s = &list.stars[i];
            (s.x, s.y, f.x - s.x, f.y - s.y)
        })
        .collect();
    let too_few = |members: usize| {
        let sparse_under = if params.map_min_kept_share > 0.0 {
            (params.map_min_members as f64 / params.map_min_kept_share).ceil() as usize
        } else {
            0
        };
        if list.map.len() < sparse_under {
            ForcedRefusal::TooFewStars { list: list.stars.len() as u32, kept: members as u32 }
        } else {
            ForcedRefusal::PoorSolve { kept_share: f64::NAN }
        }
    };
    // Six coefficients need more than six points whatever the minimum says.
    if pts.len() < params.map_min_members.max(7) {
        return Err(too_few(pts.len()));
    }
    let mut map = QuadMap {
        cx: (list.width as f64 - 1.0) / 2.0,
        cy: (list.height as f64 - 1.0) / 2.0,
        half: 0.5 * (list.width as f64).hypot(list.height as f64),
        ax: [0.0; 6],
        ay: [0.0; 6],
        members: 0,
        offset_px: f64::NAN,
        residual_px: f64::NAN,
    };
    let mut before: Vec<f64> = pts.iter().map(|p| p.2.hypot(p.3)).collect();
    map.offset_px = median(&mut before);
    for round in 0..=params.map_clip_rounds {
        let rows: Vec<[f64; 6]> = pts.iter().map(|p| map.basis(p.0, p.1)).collect();
        let dx: Vec<f64> = pts.iter().map(|p| p.2).collect();
        let dy: Vec<f64> = pts.iter().map(|p| p.3).collect();
        let (Some(ax), Some(ay)) = (least_squares6(&rows, &dx), least_squares6(&rows, &dy)) else {
            return Err(ForcedRefusal::PoorSolve { kept_share: f64::NAN });
        };
        map.ax = ax;
        map.ay = ay;
        let res: Vec<f64> = pts
            .iter()
            .map(|p| {
                let (ex, ey) = map.shift(p.0, p.1);
                (p.2 - ex).hypot(p.3 - ey)
            })
            .collect();
        let med = median(&mut res.clone());
        map.residual_px = med;
        map.members = pts.len();
        if round == params.map_clip_rounds {
            break;
        }
        let limit = (params.map_clip_sigma * MAD_TO_SIGMA * med).max(params.map_clip_floor_px);
        let kept: Vec<(f64, f64, f64, f64)> =
            pts.iter().zip(&res).filter(|(_, r)| **r <= limit).map(|(p, _)| *p).collect();
        if kept.len() == pts.len() || kept.len() < params.map_min_members {
            break;
        }
        pts = kept;
    }
    Ok(map)
}

/// What the second fit says of the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapGate {
    /// Refits kept (accepted, unsaturated, centred within the tolerance of their moved seeds) over the map stars asked.
    pub kept_share: f64,
    /// Median distance of an accepted refit from its moved seed, px (NaN when none was accepted): B7's figure.
    pub centre_median_px: f64,
}

/// The gate on the map (spec §5.2 step 3): fewer than `map_min_kept_share` of the map stars kept by the second fit is
/// `PoorSolve`. Refits that do not match the placement and the list are `Internal`.
pub fn map_gate(
    list: &ForcedList,
    refits: &[ListFit],
    placement: &ListPlacement,
    params: &ForcedListParams,
) -> Result<MapGate, ForcedRefusal> {
    if refits.len() != placement.refit_positions.len() || refits.len() != list.map.len() {
        tracing::warn!(
            map = list.map.len(),
            refits = refits.len(),
            placed = placement.refit_positions.len(),
            "forced list and its refits differ in length"
        );
        return Err(ForcedRefusal::Internal);
    }
    let kept = refits.iter().filter(|f| f.kept).count();
    let kept_share = kept as f64 / list.map.len().max(1) as f64;
    let mut off: Vec<f64> =
        refits.iter().filter(|f| f.accepted && f.centre_offset_px.is_finite()).map(|f| f.centre_offset_px).collect();
    let centre_median_px = if off.is_empty() { f64::NAN } else { median(&mut off) };
    if kept_share < params.map_min_kept_share {
        return Err(ForcedRefusal::PoorSolve { kept_share });
    }
    Ok(MapGate { kept_share, centre_median_px })
}

/// The refit seeds and the aperture geometry of the second pass (spec §5.2): every position moved by the map,
/// `R = max(aperture_radius_ref * FWHM_ref, aperture_radius_fwhm * F)`, the pedestal radii, the annulus and the peak
/// radius. `frame_fwhm` is `F`, the frame's Analysis median FWHM (a non-finite one is left out of the maxima).
pub fn placement(list: &ForcedList, map: &QuadMap, frame_fwhm: f64, params: &ForcedListParams) -> ListPlacement {
    let moved = |s: &ForcedStar| {
        let (dx, dy) = map.shift(s.x, s.y);
        (s.x + dx, s.y + dy)
    };
    let refit: Vec<(f64, f64)> = list.map.iter().map(|&i| moved(&list.stars[i])).collect();
    let apertures: Vec<(f64, f64)> = list.stars.iter().map(moved).collect();
    let r = list.fwhm_ref_px;
    let radius = (params.aperture_radius_ref * r).max(params.aperture_radius_fwhm * frame_fwhm);
    let extra: Vec<f64> = params.pedestal_radii_ref.iter().map(|k| k * r).collect();
    let annulus_start =
        (params.annulus_start_fwhm * frame_fwhm).max(params.pedestal_radii_ref[2] * r) + params.annulus_gap_px;
    ListPlacement::new(
        refit,
        apertures,
        radius,
        extra,
        annulus_start,
        params.annulus_area_px,
        params.peak_radius_ref * r,
    )
}

// ---- the statistic ----------------------------------------------------------------------------------------------

/// A straight line `y = a + b x` by least squares; `None` for fewer than two points or no spread in `x`.
fn fit_line(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if pts.len() < 2 {
        return None;
    }
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let spread = (sxx / n).sqrt();
    if !(spread > 1e-9 * mx.abs()) || !spread.is_finite() {
        return None;
    }
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let b = sxy / sxx;
    let a = my - b * mx;
    (a.is_finite() && b.is_finite()).then_some((a, b))
}

/// The sky pedestal's fit with the clip's bookkeeping (G2 review M1: the share the clip keeps is the band that comes
/// before a collapse to `None`, and nothing else shows it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PedestalFit {
    /// The pedestal, `None` when it could not be fitted.
    pub eps: Option<f64>,
    /// Points offered to the line (members whose sums are finite).
    pub points: u32,
    /// Points left after the clipping (at a collapse, the survivors of the round that collapsed it).
    pub kept: u32,
}

impl PedestalFit {
    /// The share of the offered points the clip kept (1.0 for a clean set); `None` with no point offered.
    pub fn kept_share(&self) -> Option<f64> {
        (self.points > 0).then(|| f64::from(self.kept) / f64::from(self.points))
    }
}

/// The sky pedestal `epsilon` (flux per pixel; the sums carry `+ epsilon * pi * r^2`): over `members` (indices of
/// `apertures`, every weight), `D = S(c) - S(b)` against `S(a)` at the three pedestal radii, a straight line with
/// `pedestal_clip_rounds` rounds of clipping the residuals over `pedestal_clip_sigma` robust sigmas (1.4826 times the
/// median deviation of the residuals from their median) about zero;
/// `epsilon = intercept / (pi * (c^2 - b^2))`. `None` for fewer than `pedestal_min_points` left, or no spread in `S(a)`.
pub fn sky_pedestal(
    apertures: &[ListAperture],
    members: &[usize],
    fwhm_ref: f64,
    params: &ForcedListParams,
) -> Option<f64> {
    sky_pedestal_fit(apertures, members, fwhm_ref, params).eps
}

/// [`sky_pedestal`] with the points offered and the points the clip kept.
pub fn sky_pedestal_fit(
    apertures: &[ListAperture],
    members: &[usize],
    fwhm_ref: f64,
    params: &ForcedListParams,
) -> PedestalFit {
    let (r_b, r_c) = (params.pedestal_radii_ref[1] * fwhm_ref, params.pedestal_radii_ref[2] * fwhm_ref);
    let area = std::f64::consts::PI * (r_c * r_c - r_b * r_b);
    let mut pts: Vec<(f64, f64)> = members
        .iter()
        .filter_map(|&i| {
            let s = &apertures.get(i)?.extra_sums;
            let (a, b, c) = (*s.first()?, *s.get(1)?, *s.get(2)?);
            let d = c - b;
            (a.is_finite() && d.is_finite()).then_some((a, d))
        })
        .collect();
    let points = pts.len() as u32;
    let mut fit = PedestalFit { eps: None, points, kept: points };
    if !(area.is_finite() && area > 0.0) || pts.len() < params.pedestal_min_points {
        return fit;
    }
    for _ in 0..params.pedestal_clip_rounds {
        let Some((a, b)) = fit_line(&pts) else {
            return fit;
        };
        let res: Vec<f64> = pts.iter().map(|p| p.1 - (a + b * p.0)).collect();
        let m = median(&mut res.clone());
        let mut dev: Vec<f64> = res.iter().map(|r| (r - m).abs()).collect();
        // A floor, so noise-free input (a MAD of 0 or of rounding dust) does not clip itself away.
        let scale = pts.iter().map(|p| p.0.abs() + p.1.abs()).sum::<f64>() / pts.len() as f64;
        let sigma = (MAD_TO_SIGMA * median(&mut dev)).max(1e-9 * (scale + 1.0));
        let limit = params.pedestal_clip_sigma * sigma;
        // About zero, the fit's own centre: a skewed tail does not move the window it is judged against.
        let kept: Vec<(f64, f64)> = pts.iter().zip(&res).filter(|(_, r)| r.abs() <= limit).map(|(p, _)| *p).collect();
        if kept.len() == pts.len() {
            break;
        }
        fit.kept = kept.len() as u32;
        if kept.len() < params.pedestal_min_points {
            return fit;
        }
        pts = kept;
    }
    fit.eps = fit_line(&pts).map(|(intercept, _)| intercept / area).filter(|e| e.is_finite());
    fit
}

/// One list star's contribution to the statistic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedStarZp {
    /// Index into [`ForcedList::stars`] and [`ListMeasurement::apertures`].
    pub index: usize,
    /// `G + 2.5·log10(f / t)` of a member with a positive flux (NaN otherwise).
    pub zp: f64,
    /// The weight a member enters the sums with: its window weight times the screen's taper (0 for a star that is no
    /// member).
    pub weight: f64,
    /// In the window, on the frame and under the plane's clip floor.
    pub member: bool,
    /// A member the screen kept (a member whose taper reached zero is dropped, as one far below the field is).
    pub screened_in: bool,
}

/// A valid forced-list zero point with the figures the catalogue stores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForcedZeroPoint {
    /// The weighted ratio of sums (mag).
    pub zero_point: f64,
    /// Members of non-zero weight the screen kept.
    pub stars: u32,
    /// Robust scatter of the screen's survivors (1.4826·MAD, mag): the field's per-star scatter.
    pub sigma: f64,
}

/// The statistic's result with every figure behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedOutcome {
    pub result: Result<ForcedZeroPoint, ForcedRefusal>,
    /// The list before fitting (after the guard).
    pub list_stars: u32,
    /// Members: in the window, on the frame, under the clip floor and not removed by the seed rule, whatever their weight,
    /// before the screen.
    pub members: u32,
    /// Members above `m + clip_sigma * sigma`: their weight was tapered (or, at zero, they were dropped).
    pub tapered: u32,
    /// The sky pedestal (working units per pixel), when it was fitted.
    pub pedestal: Option<f64>,
    /// Points offered to the pedestal's line and points its clip kept (0 / 0 before the pedestal was read); also set
    /// when the pedestal collapsed to `None` (G2 review M1).
    pub pedestal_points: u32,
    pub pedestal_kept: u32,
    pub window_bright: f64,
    /// The zero point's change per magnitude the window moves fainter; `None` with fewer than `slope_min_ramp_members`
    /// members on the bright ramp.
    pub window_slope: Option<f64>,
    /// One row per list star, in list order (empty for a refusal before the members were read).
    pub per_star: Vec<ForcedStarZp>,
}

/// The statistic (spec §5.3) over the apertures of `measured` (one per list star, at the positions `placement` moved).
/// Members: in the window, on the frame, under the clip floor, and not removed by the seed rule
/// ([`ForcedList::seed_removed`]); the sky pedestal is read off them and taken out of every
/// sum; a screen at `clip_sigma` drops the members far below the field's median (a negative flux inside its noise
/// stays) and tapers the weight of one far above it (`screen_taper_sigma`); the zero point is the weighted ratio of sums; valid with at least
/// `min_stars` members of non-zero weight and a scatter within `max_sigma_mag`.
pub fn forced_zero_point(
    list: &ForcedList,
    measured: &ListMeasurement,
    placement: &ListPlacement,
    exptime_s: f64,
    params: &ForcedListParams,
) -> ForcedOutcome {
    let list_stars = list.stars.len() as u32;
    // The pedestal's clip bookkeeping, set once the pedestal has been read.
    let ped_counts = std::cell::Cell::new((0u32, 0u32));
    let outcome = |result, members: u32, tapered: u32, pedestal, window_slope, per_star: Vec<ForcedStarZp>| {
        ForcedOutcome {
            result,
            list_stars,
            members,
            tapered,
            pedestal,
            pedestal_points: ped_counts.get().0,
            pedestal_kept: ped_counts.get().1,
            window_bright: list.window_bright,
            window_slope,
            per_star,
        }
    };
    if !(exptime_s.is_finite() && exptime_s > 0.0) {
        return outcome(Err(ForcedRefusal::NoExposure), 0, 0, None, None, Vec::new());
    }
    if measured.apertures.len() != list.stars.len()
        || measured.fits.len() != list.map.len()
        || measured.refits.len() != placement.refit_positions.len()
        || placement.extra_radii_px.len() != params.pedestal_radii_ref.len()
        || measured.apertures.iter().any(|a| a.on_frame && a.extra_sums.len() != placement.extra_radii_px.len())
    {
        // An invariant of the fitter's answer, not a frame's verdict: nothing is stored (a fixed build measures again).
        tracing::warn!(
            list = list.stars.len(),
            map = list.map.len(),
            fits = measured.fits.len(),
            refits = measured.refits.len(),
            apertures = measured.apertures.len(),
            "forced list and its measurements differ in length"
        );
        return outcome(Err(ForcedRefusal::Internal), 0, 0, None, None, Vec::new());
    }

    // Members.
    let mut per_star: Vec<ForcedStarZp> = (0..list.stars.len())
        .map(|index| ForcedStarZp { index, zp: f64::NAN, weight: 0.0, member: false, screened_in: false })
        .collect();
    let mut members: Vec<usize> = Vec::new();
    for (i, (star, ap)) in list.stars.iter().zip(&measured.apertures).enumerate() {
        // The seed rule's removals are no members: no weight, not in the sums, the pedestal, `zp_stars` or B1's set. Their
        // apertures were measured all the same (the placement does not know the rule).
        if list.seed_removed.get(i).copied().unwrap_or(false) {
            continue;
        }
        let g = f64::from(star.g_mag);
        let under_clip = measured.clip_level.map_or(true, |c| ap.peak < c);
        if g >= list.window_bright && g <= list.window_faint && ap.on_frame && ap.sum.is_finite() && under_clip {
            members.push(i);
            per_star[i].member = true;
            per_star[i].weight = window_weight(g, list, params);
        }
    }
    let n_members = members.len() as u32;
    // What a refusal before the screen reports as `kept`: the members that carry a weight.
    let n_weighted = members.iter().filter(|&&i| per_star[i].weight > 0.0).count() as u32;
    let too_few = |kept: u32| ForcedRefusal::TooFewStars { list: list_stars, kept };
    if members.len() < params.min_stars {
        return outcome(Err(too_few(n_members)), n_members, 0, None, None, per_star);
    }

    // The pedestal, over every member whatever its weight; with none there is no zero point (never one with
    // epsilon = 0, which would carry the annulus' bias).
    let ped = sky_pedestal_fit(&measured.apertures, &members, list.fwhm_ref_px, params);
    ped_counts.set((ped.points, ped.kept));
    let Some(eps) = ped.eps else {
        let r = ForcedRefusal::TooScattered { kept: n_weighted, sigma: f64::NAN };
        return outcome(Err(r), n_members, 0, None, None, per_star);
    };
    let t = exptime_s;
    let area = std::f64::consts::PI * placement.radius_px * placement.radius_px;
    let flux: Vec<f64> = members.iter().map(|&i| measured.apertures[i].sum - eps * area).collect();
    for (&i, &f) in members.iter().zip(&flux) {
        if f > 0.0 {
            per_star[i].zp = f64::from(list.stars[i].g_mag) + 2.5 * (f / t).log10();
        }
    }

    // The screen: the field's median and scatter over the positive fluxes.
    let zps: Vec<f64> = members.iter().filter(|&&i| per_star[i].zp.is_finite()).map(|&i| per_star[i].zp).collect();
    let (m, sigma) = match clipped_median(&zps, params.clip_sigma, 1, f64::INFINITY).verdict {
        Ok(z) => (z.zero_point, z.sigma),
        Err(_) => {
            let r = ForcedRefusal::TooScattered { kept: n_weighted, sigma: f64::NAN };
            return outcome(Err(r), n_members, 0, Some(eps), None, per_star);
        }
    };
    let sig = sigma.max(CLIP_SIGMA_FLOOR_MAG);

    // Per member: dropped far below the median beyond its own noise; far above it (`zp_i > m + clip_sigma * sigma`) it
    // keeps its own flux at a weight that falls to zero over `screen_taper_sigma * sigma`, so a star that the jitter of
    // the median and the scatter carries across the limit moves the zero point continuously.
    struct Kept {
        g: f64,
        /// The weight in the sums: the window weight times the taper.
        w: f64,
        /// The taper alone (what the window's ramps move with).
        taper: f64,
        f: f64,
    }
    let mut left: Vec<Kept> = Vec::with_capacity(members.len());
    let mut tapered = 0u32;
    let upper = m + params.clip_sigma * sig;
    for (&i, &f) in members.iter().zip(&flux) {
        let g = f64::from(list.stars[i].g_mag);
        let noise = measured.apertures[i].sky_scatter * area.sqrt();
        let lo = t * 10f64.powf(0.4 * (m - params.clip_sigma * sig - g));
        if f + params.clip_sigma * noise < lo {
            per_star[i].weight = 0.0;
            continue;
        }
        let mut taper = 1.0;
        if per_star[i].zp > upper {
            tapered += 1;
            taper = if !(params.screen_taper_sigma > 1e-9) {
                0.0
            } else {
                (1.0 - (per_star[i].zp - upper) / (params.screen_taper_sigma * sig)).clamp(0.0, 1.0)
            };
            if taper <= 0.0 {
                per_star[i].weight = 0.0;
                continue;
            }
        }
        let w = per_star[i].weight * taper;
        per_star[i].weight = w;
        per_star[i].screened_in = true;
        left.push(Kept { g, w, taper, f });
    }

    let stars = left.iter().filter(|k| k.w > 0.0).count() as u32;
    if (stars as usize) < params.min_stars {
        return outcome(Err(too_few(stars)), n_members, tapered, Some(eps), None, per_star);
    }
    let sw_f: f64 = left.iter().map(|k| k.w * k.f).sum();
    let sw_c: f64 = left.iter().map(|k| k.w * 10f64.powf(-0.4 * k.g)).sum();
    if !(sw_f > 0.0 && sw_f.is_finite() && sw_c > 0.0) {
        let r = ForcedRefusal::TooScattered { kept: stars, sigma };
        return outcome(Err(r), n_members, tapered, Some(eps), None, per_star);
    }
    let zero_point = 2.5 * (sw_f / (t * sw_c)).log10();

    // The window slope: the zero point's change per magnitude the window moves fainter, from the members on the two
    // ramps (each at its taper: the window moves its window weight, the screen's taper stays). A window the catalogue
    // cut has no faint ramp, and that edge does not move, so its term is zero.
    let rho = params.window_ramp_mag;
    let bright: Vec<&Kept> = left.iter().filter(|k| k.g - list.window_bright < rho).collect();
    let faint: Vec<&Kept> = left.iter().filter(|k| list.window_faint - k.g < rho).collect();
    let window_slope = (bright.len() >= params.slope_min_ramp_members && rho > 0.0).then(|| {
        let sum_f = |v: &[&Kept]| v.iter().map(|k| k.taper * k.f).sum::<f64>();
        let sum_c = |v: &[&Kept]| v.iter().map(|k| k.taper * 10f64.powf(-0.4 * k.g)).sum::<f64>();
        (2.5 / std::f64::consts::LN_10)
            * ((sum_f(&faint) - sum_f(&bright)) / (rho * sw_f) - (sum_c(&faint) - sum_c(&bright)) / (rho * sw_c))
    });

    let result = if sigma > params.max_sigma_mag {
        Err(ForcedRefusal::TooScattered { kept: stars, sigma })
    } else {
        Ok(ForcedZeroPoint { zero_point, stars, sigma })
    };
    outcome(result, n_members, tapered, Some(eps), window_slope, per_star)
}

// ---- the reduction ----------------------------------------------------------------------------------------------

/// The window one set of frames is read in: the median bright edge and the median slope (spec §5.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommonWindow {
    pub pivot: f64,
    pub slope: f64,
}

/// The common window of `(G_b, s)` pairs: the median `G_b`, the median of the slopes that exist (0 when none); `None`
/// for no frame. Non-finite values are left out.
pub fn common_window(frames: &[(f64, Option<f64>)]) -> Option<CommonWindow> {
    let mut g: Vec<f64> = frames.iter().map(|f| f.0).filter(|v| v.is_finite()).collect();
    if g.is_empty() {
        return None;
    }
    let mut s: Vec<f64> = frames.iter().filter_map(|f| f.1).filter(|v| v.is_finite()).collect();
    let slope = if s.is_empty() { 0.0 } else { median(&mut s) };
    Some(CommonWindow { pivot: median(&mut g), slope })
}

impl CommonWindow {
    /// A frame's zero point read in the common window: `zp - slope * (G_b - pivot)`.
    pub fn reduce(&self, zero_point: f64, window_bright: f64) -> f64 {
        zero_point - self.slope * (window_bright - self.pivot)
    }
}

/// Reduce a set of `(zero point, G_b, slope)` frames in place to their common window (the probe's form of the same
/// rule; a set with no frame is left as it is). A frame whose zero point is not finite (a refused frame) takes no part
/// in the pivot or the slope: it would put its own window into the median.
pub fn reduce_to_common_window(frames: &mut [(f64, f64, Option<f64>)]) {
    let pairs: Vec<(f64, Option<f64>)> = frames.iter().filter(|f| f.0.is_finite()).map(|f| (f.1, f.2)).collect();
    let Some(window) = common_window(&pairs) else {
        return;
    };
    for f in frames.iter_mut() {
        f.0 = window.reduce(f.0, f.1);
    }
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
    use crate::star_measure::analysis_mode::{ListAperture, ListFit};

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
        ForcedListInput {
            catalog,
            wcs,
            width: w,
            height: h,
            anchor: ANCHOR,
            full_scale: FULL,
            exptime_s: EXPTIME,
        }
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

    /// The projection and the isolation test run per catalogue star, so the list is the same whatever pool (or none) runs
    /// them: the same stars in the same order, the same window and map.
    #[test]
    fn the_list_does_not_depend_on_the_pool_that_builds_it() {
        let wcs = wcs_at(W, H, 1.0, (0.0, 0.0), 0.0);
        let gb = g_b(&wcs);
        let cat = sky_catalogue(6000, &wcs, W, H, gb - 1.0, gb + 6.0, 31);
        let pool = |n: usize| Arc::new(rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap());
        let (one, six) = (pool(1), pool(6));
        let i = input(&cat, &wcs, W, H);
        let base = forced_list_in(&i, &params(), Some(&one)).unwrap();
        assert!(base.stars.len() > 300 && !base.map.is_empty(), "a usable list: {}", base.stars.len());
        assert_eq!(base, forced_list_in(&i, &params(), Some(&six)).unwrap());
        assert_eq!(base, forced_list_in(&i, &params(), None).unwrap());
        assert_eq!(base, forced_list(&i, &params()).unwrap());
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
        // The guard, set low so a catalogue this size reaches it (the default 12 000 is a guard no rig here reaches).
        let guard = ForcedListParams { cap: 1500, ..params() };
        let uncapped = ForcedListParams { cap: usize::MAX, ..params() };
        let (ca, cb) = (
            forced_list(&input(&cat, &a, W, H), &guard).unwrap(),
            forced_list(&input(&cat, &b, W, H), &guard).unwrap(),
        );
        let ua = forced_list(&input(&cat, &a, W, H), &uncapped).unwrap();
        assert!(ua.list_before_cap > 2000, "candidates {}", ua.list_before_cap);
        assert_eq!(ua.stars.len(), ua.list_before_cap, "no cap, no loss");
        assert!(ca.stars.len() > 1200 && ca.stars.len() < 1800, "capped to about 1500: {}", ca.stars.len());
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
        assert_eq!(cap_key(15.123, 150.5 + 0.4 / 3600.0, 20.25), PINNED_KEY);
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
        // dG 3.9 at 7.9 FWHM_ref drops the star, dG 4.1 keeps it.
        assert!(!target_survives(3.9, 7.9, None));
        assert!(target_survives(4.1, 7.9, None));
        // Beyond 8 FWHM_ref a close companion is no longer a neighbour.
        assert!(target_survives(1.0, 8.1, None));
        // A star 2.1 mag brighter than G_b at 9.9 FWHM_ref drops it, at 10.1 keeps it.
        let gb = g_b(&wcs_at(1000, 1000, 1.0, (0.0, 0.0), 0.0));
        assert!(!target_survives(0.0, 9.9, Some(gb - 2.1)));
        assert!(target_survives(0.0, 10.1, Some(gb - 2.1)));
        // 1.9 mag brighter than G_b is not a halo source (the dG rule alone, at that distance, keeps it).
        assert!(target_survives(0.0, 9.9, Some(gb - 1.9)));
    }

    // ---- isolation from the frame's own detections (the members' rule, after the map) -------------------------------

    const K0: f64 = 4.0e9;

    /// A seed of a star of magnitude `g` at `(x, y)`, flux read through the test's calibration `K0`.
    fn seed_of(x: f64, y: f64, g: f64, peak: f64) -> Seed {
        Seed { x, y, peak, flux: K0 * 10f64.powf(-0.4 * g) }
    }

    /// A 1000 x 1000 frame holding a lattice of 36 px of catalogue stars, all at `G_b + 1`, each with its own seed 0.3 px
    /// off its position, plus nothing else. Returns the catalogue, the seeds, the frame's solve, the lattice's positions
    /// and the stars' magnitude.
    #[allow(clippy::type_complexity)]
    fn seeded_field(side: usize) -> (Vec<CatalogStar>, Vec<Seed>, WcsSolution, Vec<(f64, f64)>, f64) {
        let wcs = wcs_at(1000, 1000, 1.0, (0.0, 0.0), 0.0);
        let g = g_b(&wcs) + 1.0;
        let (mut cat, mut seeds, mut pos) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..side {
            for j in 0..side {
                let (x, y) = (300.0 + 36.0 * i as f64, 300.0 + 36.0 * j as f64);
                cat.push(at(&wcs, x, y, g as f32));
                seeds.push(seed_of(x + 0.3, y - 0.2, g, 100.0));
                pos.push((x, y));
            }
        }
        (cat, seeds, wcs, pos, g)
    }

    /// The catalogue's list of a seeded field: the seeds take no part in it.
    fn seeded_list(cat: &[CatalogStar], wcs: &WcsSolution, p: &ForcedListParams) -> ForcedList {
        forced_list(&input(cat, wcs, 1000, 1000), p).unwrap()
    }

    /// The seed rule on a list at the positions the catalogue projected it to (a map that moves nothing), plane sigma 1.
    fn ruled(l: &ForcedList, seeds: &[Seed], p: &ForcedListParams) -> SeedRule {
        let at_projection: Vec<(f64, f64)> = l.stars.iter().map(|s| (s.x, s.y)).collect();
        seed_rule_in(l, &at_projection, seeds, 1.0, p, None)
    }

    /// Whether the rule removed the list star at `(x, y)` (the catalogue's projection).
    fn removed_at(l: &ForcedList, r: &SeedRule, x: f64, y: f64) -> bool {
        let i = l.stars.iter().position(|s| (s.x - x).abs() < 0.01 && (s.y - y).abs() < 0.01).expect("a listed star");
        r.removed[i]
    }

    #[test]
    fn a_bright_star_absent_from_the_cone_removes_its_neighbours_from_the_members_and_no_others() {
        let (cat, mut seeds, wcs, pos, g) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        assert_eq!(list.stars.len(), 64);
        let base = ruled(&list, &seeds, &params());
        assert_eq!((base.removed_count, base.pairs, base.peak_floor_rejected), (0, 64, 0));
        // A G_b - 3 star between two lattice stars, 18 px from each (the radius is 8 * 2.5 = 20 px), 36 px from the rest.
        let (bx, by) = (pos[0].0 + 18.0, pos[0].1);
        seeds.push(seed_of(bx, by, g - 4.0, 1000.0));
        let r = ruled(&list, &seeds, &params());
        let near: Vec<(f64, f64)> = pos.iter().copied().filter(|p| (p.0 - bx).hypot(p.1 - by) <= 20.0).collect();
        assert_eq!(near.len(), 2, "the premise: two stars in the radius");
        assert_eq!((r.removed_count, r.pairs), (2, 64), "every lattice star has exactly one seed in its radius");
        for &(x, y) in &pos {
            assert_eq!(removed_at(&list, &r, x, y), near.contains(&(x, y)), "star at ({x}, {y})");
        }
        // The list is the catalogue's whatever the seeds say: a rule applied to it changes the members' bookkeeping only.
        let mut applied = list.clone();
        applied.apply_seed_rule(r.clone());
        assert_eq!((applied.stars.len(), applied.map.len()), (64, list.map.len()));
        assert_eq!((applied.stars.clone(), applied.request.clone(), applied.window_faint), (list.stars.clone(), list.request.clone(), list.window_faint));
        assert_eq!((applied.detected_neighbours_removed, applied.seed_calibration_pairs), (2, 64));
        assert_eq!(applied.seed_removed.iter().filter(|r| **r).count(), 2);
        // Switched off, nothing is removed and nothing is said.
        let off = ruled(&list, &seeds, &ForcedListParams { detected_neighbours: false, ..params() });
        assert_eq!(off, SeedRule::default());
    }

    #[test]
    fn a_stars_own_seed_never_removes_it() {
        let (cat, seeds, wcs, _, _) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        let r = ruled(&list, &seeds, &params());
        assert_eq!((r.removed_count, r.pairs), (0, 64));
        // Its own seed 1.4 px off (a proper-motion error, inside the 2.5 px radius) is still its own.
        let shifted: Vec<Seed> = seeds.iter().map(|s| Seed { x: s.x + 1.1, y: s.y + 0.9, ..*s }).collect();
        let r = ruled(&list, &shifted, &params());
        assert_eq!(r.removed_count, 0);
    }

    #[test]
    fn a_seed_fainter_than_the_depth_never_removes_and_the_peak_floor_counts_what_it_spares() {
        let (cat, seeds, wcs, pos, g) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        let gb = g - 1.0;
        let (bx, by) = (pos[0].0 + 18.0, pos[0].1);
        let with = |g_seed: f64, peak: f64, p: &ForcedListParams| {
            let mut s = seeds.clone();
            s.push(seed_of(bx, by, g_seed, peak));
            let r = ruled(&list, &s, p);
            (r.removed_count, r.peak_floor_rejected)
        };
        // The depth: G_b + 2 (the default). A seed at G_b + 1.9 (peak well above the floor) removes its two neighbours, one
        // at G_b + 2.1 never does, however bright its peak.
        assert_eq!(with(gb + 1.9, 1000.0, &params()), (2, 0));
        assert_eq!(with(gb + 2.1, 1000.0, &params()), (0, 0));
        assert_eq!(with(gb + 3.9, 1.0e6, &params()), (0, 0));
        // The floor is 10 sigma (sigma 1): 9.9 spares, 10 removes; the seed it spares is counted (once, however many stars
        // it was near), a seed past the depth is not (it is no neighbour at all).
        assert_eq!(with(g - 1.0, 9.9, &params()), (0, 1));
        assert_eq!(with(g - 1.0, 10.0, &params()), (2, 0));
        assert_eq!(with(gb + 2.1, 9.9, &params()), (0, 0));
        // dG: with the depth taken out of the way, a neighbour 3.9 mag fainter than the star removes it, 4.1 mag fainter
        // keeps it (the isolation rule's own limit, which the depth now sits inside).
        let deep = ForcedListParams { seed_neighbour_depth_mag: 8.0, ..params() };
        assert_eq!(with(g + 3.9, 50.0, &deep), (2, 0));
        assert_eq!(with(g + 4.1, 50.0, &deep), (0, 0));
        // Distance: a seed 27 px from the nearest lattice stars (20.5 px along, 18 px across) is outside 8 FWHM_ref (it is
        // G_b - 0.5, no halo source either).
        let mut far = seeds.clone();
        far.push(seed_of(pos[0].0 + 20.5, pos[0].1 + 18.0, gb - 0.5, 1000.0));
        assert!(pos.iter().all(|p| (p.0 - (pos[0].0 + 20.5)).hypot(p.1 - (pos[0].1 + 18.0)) > 20.0), "the premise");
        assert_eq!(ruled(&list, &far, &params()).removed_count, 0);
        // And the same seed 19.5 px from a star removes it.
        let mut near = seeds.clone();
        near.push(seed_of(pos[0].0 + 19.5, pos[0].1, gb - 0.5, 1000.0));
        assert!(ruled(&list, &near, &params()).removed_count >= 1);
        // A lower peak floor lets the faint peak through: the constant is the rule's.
        let lax = ForcedListParams { detected_neighbour_min_peak_sigma: 5.0, ..params() };
        assert_eq!(with(g - 1.0, 9.9, &lax), (2, 0));
    }

    /// The catalogue halo rule's mirror: a seed with `G_seed <= G_b - bright_dmag` within `bright_radius_fwhm * FWHM_ref`
    /// (25 px here) removes a list star the isolation rule (20 px) does not reach, whatever its peak.
    #[test]
    fn a_bright_seed_in_the_halo_radius_removes_the_member_as_the_catalogue_halo_rule_does() {
        let (cat, seeds, wcs, pos, g) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        let gb = g - 1.0;
        // A seed left of the lattice's corner star (the rest are 36 px further): only that star is near it.
        let (x0, y0) = pos[0];
        let with = |dx: f64, g_seed: f64, peak: f64, p: &ForcedListParams| {
            let mut s = seeds.clone();
            s.push(seed_of(x0 - dx, y0, g_seed, peak));
            let r = ruled(&list, &s, p);
            (r.removed_count, removed_at(&list, &r, x0, y0))
        };
        // The edges, as `target_survives` asks the catalogue: 24.75 px (9.9 F) removes it, 25.25 px (10.1 F) keeps it.
        assert_eq!(with(24.75, gb - 2.1, 1000.0, &params()), (1, true));
        assert_eq!(with(25.25, gb - 2.1, 1000.0, &params()), (0, false));
        // 1.9 mag brighter than G_b is no halo source, and 24.75 px is outside the 20 px of the isolation rule.
        assert_eq!(with(24.75, gb - 1.9, 1000.0, &params()), (0, false));
        // The same seed at 19 px is the isolation rule's (dG <= 4, inside the depth, a peak above the floor).
        assert_eq!(with(19.0, gb - 1.9, 1000.0, &params()), (1, true));
        // No peak floor on the halo rule: a 1 sigma peak that bright still removes the star.
        assert_eq!(with(24.75, gb - 2.1, 1.0, &params()), (1, true));
        // Switched off, nothing is removed; the halo constants are the catalogue rule's own.
        let off = ForcedListParams { detected_neighbours: false, ..params() };
        let mut s = seeds.clone();
        s.push(seed_of(x0 - 24.75, y0, gb - 2.1, 1000.0));
        assert_eq!(ruled(&list, &s, &off), SeedRule::default());
        let wide = ForcedListParams { bright_radius_fwhm: 11.0, ..params() };
        assert_eq!(with(26.0, gb - 2.1, 1000.0, &wide), (1, true));
    }

    #[test]
    fn under_twenty_pairs_the_rule_is_skipped_and_the_members_are_the_catalogues() {
        // Four lattice stars: 4 x 4 = 16 stars, 16 pairs, with a bright seed beside one of them.
        let (cat, mut seeds, wcs, pos, g) = seeded_field(4);
        seeds.push(seed_of(pos[0].0 + 18.0, pos[0].1, g - 4.0, 1000.0));
        let list = seeded_list(&cat, &wcs, &params());
        let r = ruled(&list, &seeds, &params());
        assert_eq!((r.removed.len(), r.removed_count, r.pairs), (0, 0, 16), "the rule did not run: no flags, the pairs found");
        // The pairs are the number found, and the floor of 20 is the params' constant.
        let few = ForcedListParams { seed_calibration_min_pairs: 16, ..params() };
        let r = ruled(&list, &seeds, &few);
        assert_eq!((r.removed_count, r.pairs), (2, 16));
    }

    #[test]
    fn a_plane_with_no_seeds_skips_the_rule() {
        let (cat, _, wcs, _, _) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        assert_eq!(ruled(&list, &[], &params()), SeedRule::default());
    }

    #[test]
    fn a_seed_that_two_stars_share_is_no_calibration_pair() {
        // Two catalogue stars 3 px apart (the catalogue rule would drop them, so one is made faint-isolated by dG): a seed
        // between them is within the radius of both and nearer to neither alone: it calibrates nothing.
        let (cat, mut seeds, wcs, pos, g) = seeded_field(8);
        let mut cat = cat;
        cat.push(at(&wcs, pos[63].0 + 3.0, pos[63].1, (g + 4.5) as f32));
        seeds.push(seed_of(pos[63].0 + 1.5, pos[63].1, g, 100.0));
        let list = seeded_list(&cat, &wcs, &params());
        let r = ruled(&list, &seeds, &params());
        assert!(r.pairs <= 63, "{}", r.pairs);
    }

    #[test]
    fn the_seed_rule_runs_at_the_mapped_positions() {
        // The frame's map moved every star 3 px (a solve that is off): the seeds sit at the moved positions. Asked at the
        // projections no star has its own seed in the radius (3.3 px > 2.5 px: no pair, the rule skips); asked at the
        // mapped positions every star has its own seed and the bright star's neighbours are the ones it removes.
        let (cat, seeds, wcs, pos, g) = seeded_field(8);
        let list = seeded_list(&cat, &wcs, &params());
        let moved = |dx: f64| -> Vec<Seed> { seeds.iter().map(|s| Seed { x: s.x + dx, ..*s }).collect() };
        let mut shifted = moved(3.0);
        let (bx, by) = (pos[0].0 + 3.0 + 18.0, pos[0].1);
        shifted.push(seed_of(bx, by, g - 4.0, 1000.0));
        let skipped = ruled(&list, &shifted, &params());
        assert_eq!((skipped.removed.len(), skipped.pairs), (0, 0), "at the projections the rule has nothing to calibrate on");
        let mapped: Vec<(f64, f64)> = list.stars.iter().map(|s| (s.x + 3.0, s.y)).collect();
        let r = seed_rule_in(&list, &mapped, &shifted, 1.0, &params(), None);
        assert_eq!((r.removed_count, r.pairs), (2, 64));
        let near = |x: f64, y: f64| (x + 3.0 - bx).hypot(y - by) <= 20.0;
        for (s, flag) in list.stars.iter().zip(&r.removed) {
            assert_eq!(*flag, near(s.x, s.y), "star at ({}, {})", s.x, s.y);
        }
        // A bad solve does not turn a star's own seed into a neighbour: shifted seeds, mapped positions, no bright star.
        let r = seed_rule_in(&list, &mapped, &moved(3.0), 1.0, &params(), None);
        assert_eq!(r.removed_count, 0);
    }

    #[test]
    fn the_seed_rule_does_not_depend_on_the_pool() {
        let (cat, mut seeds, wcs, pos, g) = seeded_field(8);
        seeds.push(seed_of(pos[10].0 + 18.0, pos[10].1, g - 4.0, 1000.0));
        seeds.push(seed_of(pos[40].0 + 18.0, pos[40].1, g - 1.0, 5.0));
        let list = seeded_list(&cat, &wcs, &params());
        let at_projection: Vec<(f64, f64)> = list.stars.iter().map(|s| (s.x, s.y)).collect();
        let alone = seed_rule_in(&list, &at_projection, &seeds, 1.0, &params(), None);
        let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap());
        let pooled = seed_rule_in(&list, &at_projection, &seeds, 1.0, &params(), Some(&pool));
        assert_eq!(alone, pooled);
        assert!(alone.removed_count > 0 && alone.peak_floor_rejected == 1, "{alone:?}");
    }

    /// A star the rule removed is no member: no weight, not in the sums, the pedestal or `zp_stars`; the others are as if
    /// it were not there.
    #[test]
    fn a_member_the_seed_rule_removed_is_in_no_sum() {
        let mut list = hand_list(&even(600, 14.0, 17.0));
        let m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 7);
        let all = statistic(&list, &m);
        assert_eq!((all.members, all.pedestal_points), (600, 600));
        // The rule removed 50 stars in the middle of the window.
        let mut removed = vec![false; 600];
        removed[250..300].iter_mut().for_each(|r| *r = true);
        list.apply_seed_rule(SeedRule { removed, removed_count: 50, pairs: 500, peak_floor_rejected: 0 });
        let o = statistic(&list, &m);
        assert_eq!((o.members, o.pedestal_points, o.list_stars), (550, 550, 600));
        for (i, p) in o.per_star.iter().enumerate() {
            let gone = (250..300).contains(&i);
            assert_eq!((p.member, p.screened_in, p.weight == 0.0), (!gone, !gone && p.screened_in, gone || p.weight == 0.0), "star {i}");
            if gone {
                assert!(p.zp.is_nan(), "star {i}: a removed star has no zero point of its own");
            }
        }
        assert!(o.result.expect("550 members") .stars <= 550);
        // The map stars and the placement are the list's: the rule did not touch them.
        assert_eq!(list.map.len(), hand_list(&even(600, 14.0, 17.0)).map.len());
    }

    #[test]
    fn the_screen_taper_defaults_to_a_drop_and_a_nan_one_is_a_params_error() {
        assert_eq!(params().screen_taper_sigma, 0.0);
        assert!(params().validate().is_ok());
        let nan = ForcedListParams { screen_taper_sigma: f64::NAN, ..params() };
        let e = nan.validate().unwrap_err();
        assert!(e.contains("screen_taper_sigma"), "{e}");
        // The statistic never stores it as a scatter: a NaN width reads as a drop.
        let edge = crossing();
        let over = with_probe_under(14.6, edge + 0.03, &nan);
        assert!(over.result.is_ok(), "{:?}", over.result);
        assert_eq!(zp_of(&over), zp_of(&with_probe_under(14.6, edge + 0.03, &params())));
    }

    /// The pedestal's clip is about zero (a port of the lab's reference: sigma from the median deviation about the
    /// fit's median residual, the window about ZERO, four rounds, a break when nothing drops, the fit refitted after the
    /// last round). A one-sided tail is where the two centres differ.
    #[test]
    fn the_pedestal_clip_is_about_zero_not_about_the_median() {
        let mut rng = Rng(77);
        let apertures: Vec<ListAperture> = (0..200)
            .map(|i| {
                let a = 100.0 + 10.0 * i as f64;
                let d = 2.0 + 0.01 * a + rng.normal() + if i % 10 == 3 { 30.0 } else { 0.0 };
                ListAperture::test_new(0.0, vec![a, 0.0, d], 1000.0, 4.0, 1000.0)
            })
            .collect();
        let members: Vec<usize> = (0..200).collect();
        let p = ForcedListParams { pedestal_clip_rounds: 10, ..params() };
        let area = std::f64::consts::PI * (15.0f64 * 15.0 - 10.0 * 10.0);
        let reference = |about_median: bool| -> f64 {
            let mut pts: Vec<(f64, f64)> =
                apertures.iter().map(|a| (a.extra_sums[0], a.extra_sums[2] - a.extra_sums[1])).collect();
            let ols = |pts: &[(f64, f64)]| {
                let n = pts.len() as f64;
                let (sx, sy) = (pts.iter().map(|q| q.0).sum::<f64>(), pts.iter().map(|q| q.1).sum::<f64>());
                let sxx: f64 = pts.iter().map(|q| q.0 * q.0).sum();
                let sxy: f64 = pts.iter().map(|q| q.0 * q.1).sum();
                let b = (n * sxy - sx * sy) / (n * sxx - sx * sx);
                ((sy - b * sx) / n, b)
            };
            let med = |v: &mut Vec<f64>| {
                v.sort_by(|a, b| a.total_cmp(b));
                let n = v.len();
                if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
            };
            for _ in 0..10 {
                let (a, b) = ols(&pts);
                let res: Vec<f64> = pts.iter().map(|q| q.1 - (a + b * q.0)).collect();
                let m = med(&mut res.clone());
                let sigma = 1.4826 * med(&mut res.iter().map(|r| (r - m).abs()).collect());
                let centre = if about_median { m } else { 0.0 };
                let kept: Vec<(f64, f64)> =
                    pts.iter().zip(&res).filter(|(_, r)| (**r - centre).abs() <= 3.0 * sigma).map(|(q, _)| *q).collect();
                if kept.len() == pts.len() {
                    break;
                }
                pts = kept;
            }
            ols(&pts).0 / area
        };
        let (about_zero, about_median) = (reference(false), reference(true));
        let got = sky_pedestal(&apertures, &members, 2.5, &p).expect("a pedestal");
        assert!((got - about_zero).abs() < 1e-9 * about_zero.abs().max(1e-12) + 1e-12, "{got} against the port {about_zero}");
        assert!(
            (about_zero - about_median).abs() > 0.05 / area,
            "the premise: the two centres give different intercepts ({about_zero} {about_median})"
        );
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
        // (a) 120 stars spread over G_b..G_b + 6: fewer than 200, the edge goes to 4.5 and no deeper.
        let sparse: Vec<f64> = (0..120).map(|i| gb + 0.001 + 5.99 * i as f64 / 119.0).collect();
        let l = edge(sparse);
        assert!((l.window_faint - (gb + 4.5)).abs() < 1e-9, "{}", l.window_faint);
        assert!(l.stars.iter().all(|s| f64::from(s.g_mag) <= gb + 4.5 + 1e-6));
        assert!(l.stars.iter().any(|s| f64::from(s.g_mag) > gb + 3.5), "the deepening admitted fainter stars");
        // (b) 250 stars within G_b + 3 and some beyond: the edge stays at 3.
        let dense: Vec<f64> =
            (0..250).map(|i| gb + 0.001 + 2.98 * i as f64 / 249.0).chain((0..50).map(|i| gb + 3.1 + 0.02 * i as f64)).collect();
        let l = edge(dense);
        assert!((l.window_faint - (gb + 3.0)).abs() < 1e-9, "{}", l.window_faint);
        assert_eq!(l.stars.len(), 250);
        // (c) 150 within G_b + 3 and 200 more up to G_b + 4.4: the edge is the 200th brightest's G.
        let mid: Vec<f64> =
            (0..150).map(|i| gb + 0.001 + 2.98 * i as f64 / 149.0).chain((0..200).map(|i| gb + 3.01 + 1.39 * i as f64 / 199.0)).collect();
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
                (false, Err(ForcedRefusal::NoAnchor { kept: 9 })) => {}
                (_, other) => panic!("{n} stars: {other:?}"),
            }
        }
        let (wcs, stars, cat) = rich_field(60, ANCHOR, 1.0);
        let r = anchor_zero_point(&stars, None, &wcs, &cat, None, 1000, 1000, 3.0, &params());
        assert_eq!(r, Err(ForcedRefusal::NoExposure));
    }

    #[test]
    fn no_anchor_reports_the_survivors_of_the_clip_not_the_matches() {
        // 12 stars: the brightest is dropped, 11 are matched; five of them read three magnitudes faint, so the 4 sigma
        // clip leaves six: the rule (at least ten survivors) refuses, and the refusal says six, not eleven.
        let (wcs, mut stars, cat) = rich_field(12, ANCHOR, 1.0);
        for s in &mut stars[6..11] {
            s.flux *= 10f64.powf(-1.2);
        }
        let m = measure_zero_point(
            &ZpInput {
                stars: &stars,
                wcs: &wcs,
                catalog: &cat,
                exptime_s: Some(EXPTIME),
                width: 1000,
                height: 1000,
                median_fwhm_px: 3.0,
            },
            &ZpParams { min_stars: 1, max_std_error_mag: f64::INFINITY, ..ZpParams::default() },
        );
        assert_eq!(m.matched, 11, "premise: eleven matches");
        let r = anchor_zero_point(&stars, None, &wcs, &cat, Some(EXPTIME), 1000, 1000, 3.0, &params());
        assert_eq!(r, Err(ForcedRefusal::NoAnchor { kept: 6 }));
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

    // ---- the map, the apertures, the statistic: hand-made lists and the fitter's answers for them -------------------

    const RADIUS: f64 = 10.0;
    const RADII: [f64; 3] = [7.5, 10.0, 15.0];

    fn frac(v: f64) -> f64 {
        v - v.floor()
    }

    /// A list of stars at the given G, spread over a 4000 x 3000 frame (a low-discrepancy sequence), window `[14, 17]`,
    /// `FWHM_ref` 2.5 px; the map is the stars with `G <= 16` (at most 400), as `forced_list` picks it.
    fn hand_list(gs: &[f64]) -> ForcedList {
        let stars: Vec<ForcedStar> = gs
            .iter()
            .enumerate()
            .map(|(i, &g)| ForcedStar {
                key: i as u64,
                g_mag: g as f32,
                ra: 150.0,
                dec: 20.0,
                x: 150.0 + 3700.0 * frac(i as f64 * 0.618_033_988_7),
                y: 150.0 + 2700.0 * frac(i as f64 * 0.754_877_666_2),
            })
            .collect();
        let map: Vec<usize> =
            stars.iter().enumerate().filter(|(_, s)| f64::from(s.g_mag) <= 16.0).map(|(i, _)| i).take(400).collect();
        ForcedList {
            request: ListRequest::new(map.iter().map(|&i| (stars[i].x, stars[i].y)).collect(), 1.25),
            list_before_cap: stars.len(),
            stars,
            window_bright: 14.0,
            window_faint: 17.0,
            fwhm_ref_px: 2.5,
            scale_arcsec: 1.0,
            width: 4000,
            height: 3000,
            seed_removed: Vec::new(),
            detected_neighbours_removed: 0,
            seed_calibration_pairs: 0,
            seed_peak_floor_rejected: 0,
            map,
        }
    }

    /// The placement of a list with no map shift: `R` 10 px, the pedestal radii 3, 4 and 6 `FWHM_ref`.
    fn plain_placement(list: &ForcedList) -> ListPlacement {
        ListPlacement::new(
            list.map.iter().map(|&i| (list.stars[i].x, list.stars[i].y)).collect(),
            list.stars.iter().map(|s| (s.x, s.y)).collect(),
            RADIUS,
            RADII.to_vec(),
            40.0,
            2000.0,
            3.75,
        )
    }

    /// What the fitter answers for `list`: a star's flux is its truth `zp_of(index, G)` plus `scatter` mag of noise,
    /// every sum carries `eps * pi * r^2` of sky pedestal and a little noise of its own, the sky scatter is 4.
    fn measured_for(
        list: &ForcedList,
        zp_of: &dyn Fn(usize, f64) -> f64,
        scatter: f64,
        eps: f64,
        seed: u64,
    ) -> ListMeasurement {
        let mut rng = Rng(seed);
        let disc = |r: f64| eps * std::f64::consts::PI * r * r;
        let apertures: Vec<ListAperture> = list
            .stars
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let g = f64::from(s.g_mag);
                let flux = EXPTIME * 10f64.powf(0.4 * (zp_of(i, g) + scatter * rng.normal() - g));
                let sum = flux + disc(RADIUS) + 40.0 * rng.normal();
                let extra: Vec<f64> = RADII.iter().map(|&r| flux + disc(r) + 20.0 * rng.normal()).collect();
                ListAperture::test_new(sum, extra, 1000.0, 4.0, 1000.0)
            })
            .collect();
        let fits: Vec<ListFit> =
            list.map.iter().map(|&i| ListFit::test_kept(list.stars[i].x, list.stars[i].y, 1000.0)).collect();
        ListMeasurement::test_new(fits.clone(), fits, apertures)
    }

    fn statistic(list: &ForcedList, m: &ListMeasurement) -> ForcedOutcome {
        forced_zero_point(list, m, &plain_placement(list), EXPTIME, &params())
    }

    /// The default with the screen's taper switched on (the default is a plain drop).
    fn tapered() -> ForcedListParams {
        ForcedListParams { screen_taper_sigma: 1.0, ..params() }
    }

    fn even(n: usize, g0: f64, g1: f64) -> Vec<f64> {
        (0..n).map(|i| g0 + (g1 - g0) * (i as f64 + 0.5) / n as f64).collect()
    }

    #[test]
    fn forced_zero_point_recovers_the_truth() {
        let list = hand_list(&even(600, 14.0, 17.0));
        let m = measured_for(&list, &|_, _| 21.8, 0.1, 0.0, 7);
        let o = statistic(&list, &m);
        let z = o.result.expect("600 members are valid");
        assert!((z.zero_point - 21.8).abs() < 0.01, "{}", z.zero_point);
        assert!((z.sigma - 0.1).abs() < 0.02, "{}", z.sigma);
        assert!(z.stars >= 580, "{}", z.stars);
        assert_eq!((o.list_stars, o.members, o.per_star.len()), (600, 600, 600));
        assert!(o.pedestal.is_some_and(|e| e.abs() < 0.05), "{:?}", o.pedestal);
        // The pedestal's clip bookkeeping rides in the outcome: 600 points offered, almost all kept.
        assert_eq!(o.pedestal_points, 600);
        assert!(o.pedestal_kept >= 570 && o.pedestal_kept <= 600, "{}", o.pedestal_kept);
        assert_eq!(o.window_bright, 14.0);
    }

    /// The members of a 600-star list kept to `keep`, the others off the frame.
    fn members_only(list: &ForcedList, m: &mut ListMeasurement, keep: std::ops::Range<usize>) {
        assert_eq!(list.stars.len(), m.apertures.len());
        for (i, a) in m.apertures.iter_mut().enumerate() {
            a.on_frame = keep.contains(&i);
        }
    }

    #[test]
    fn forty_nine_members_refuse_with_the_list_size() {
        let list = hand_list(&even(600, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 9);
        // Stars 200..249 sit at G 15.0-15.25: weight 1.
        members_only(&list, &mut m, 200..249);
        let o = statistic(&list, &m);
        assert_eq!(o.result, Err(ForcedRefusal::TooFewStars { list: 600, kept: 49 }));
        assert_eq!(o.result.unwrap_err().code(), "too_few_stars");
        members_only(&list, &mut m, 200..250);
        assert!(statistic(&list, &m).result.is_ok(), "fifty members are valid");
    }

    #[test]
    fn sixty_members_are_valid_and_forty_are_too_few_stars() {
        let list = hand_list(&even(400, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 13);
        members_only(&list, &mut m, 150..210);
        let o = statistic(&list, &m);
        assert!(o.result.is_ok(), "{:?}", o.result);
        assert_eq!(o.members, 60);
        assert!(o.result.unwrap().stars >= 58, "the screen may drop a star or two");
        members_only(&list, &mut m, 150..190);
        assert_eq!(statistic(&list, &m).result, Err(ForcedRefusal::TooFewStars { list: 400, kept: 40 }));
    }

    #[test]
    fn a_wide_scatter_refuses() {
        let list = hand_list(&even(200, 14.0, 17.0));
        let m = measured_for(&list, &|_, _| 21.8, 1.3, 0.0, 21);
        match statistic(&list, &m).result {
            Err(ForcedRefusal::TooScattered { kept, sigma }) => {
                assert!(sigma > 1.0 && kept > 100, "{sigma} {kept}");
            }
            other => panic!("expected TooScattered, got {other:?}"),
        }
    }

    #[test]
    fn an_off_frame_or_clipped_aperture_is_no_member() {
        let list = hand_list(&even(300, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.02, 0.0, 5);
        m.clip_level = Some(60_000.0);
        m.apertures[3].on_frame = false;
        m.apertures[4].peak = 65_535.0;
        m.apertures[5].peak = 59_999.0;
        let o = statistic(&list, &m);
        assert_eq!(o.members, 298);
        assert!(!o.per_star[3].member && !o.per_star[4].member && o.per_star[5].member);
        assert!(o.per_star.iter().filter(|p| p.member).all(|p| p.weight >= 0.0));
        // No clip floor on the plane: the same peak is no evidence of clipping.
        m.clip_level = None;
        assert_eq!(statistic(&list, &m).members, 299);
    }

    #[test]
    fn a_length_mismatch_is_internal_and_not_stored() {
        let list = hand_list(&even(300, 14.0, 17.0));
        let good = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 77);
        assert!(statistic(&list, &good).result.is_ok());
        for what in ["fits", "refits", "apertures"] {
            let mut m = good.clone();
            match what {
                "fits" => drop(m.fits.pop()),
                "refits" => drop(m.refits.pop()),
                _ => drop(m.apertures.pop()),
            };
            let o = statistic(&list, &m);
            assert_eq!(o.result, Err(ForcedRefusal::Internal), "{what}");
            assert_ne!(o.result.unwrap_err().code(), "too_few_stars");
        }
        assert!(!ForcedRefusal::Internal.is_stored());
    }

    #[test]
    fn the_refusal_codes_are_stable_and_one_is_not_stored() {
        let all = [
            (ForcedRefusal::NoExposure, "no_exposure"),
            (ForcedRefusal::NoAnchor { kept: 3 }, "no_anchor"),
            (ForcedRefusal::PoorSolve { kept_share: 0.5 }, "poor_solve"),
            (ForcedRefusal::TooFewStars { list: 1, kept: 1 }, "too_few_stars"),
            (ForcedRefusal::TooScattered { kept: 1, sigma: 1.0 }, "too_scattered"),
            (ForcedRefusal::ColourCamera, "colour_camera"),
            (ForcedRefusal::UnreadableSolve, "unreadable_solve"),
            (ForcedRefusal::FrameVerdict, "frame_verdict"),
            (ForcedRefusal::CatalogueUnreadable, "catalogue_unreadable"),
            (ForcedRefusal::Internal, "internal"),
        ];
        for (r, code) in all {
            assert_eq!(r.code(), code);
            assert_eq!(r.is_stored(), !["catalogue_unreadable", "internal"].contains(&code), "{code}");
        }
        assert!(ForcedRefusal::PoorSolve { kept_share: f64::NAN }.is_stored(), "an outcome a re-read would repeat");
        // The list's aperture correction is gone, and its code with it.
        assert!(all.iter().all(|(_, code)| *code != "no_aperture_correction"));
    }

    // ---- the pedestal ---------------------------------------------------------------------------------------------

    /// `n_good` apertures whose pedestal points `(S(a), S(c) - S(b))` lie on `2 + 0.01 a` with unit noise, and `n_bad`
    /// gross outliers of +/-40 (alternating) among them; the list indices in step with the apertures.
    fn pedestal_points(n_good: usize, n_bad: usize) -> (Vec<ListAperture>, Vec<usize>) {
        let mut rng = Rng(41);
        let total = n_good + n_bad;
        let stride = total / n_bad.max(1);
        let mut bad = 0usize;
        let apertures: Vec<ListAperture> = (0..total)
            .map(|i| {
                let a = 100.0 + 10.0 * i as f64;
                let mut d = 2.0 + 0.01 * a + rng.normal();
                if n_bad > 0 && i % stride == stride / 2 && bad < n_bad {
                    d += if bad % 2 == 0 { 40.0 } else { -40.0 };
                    bad += 1;
                }
                ListAperture::test_new(0.0, vec![a, 0.0, d], 1000.0, 4.0, 1000.0)
            })
            .collect();
        assert_eq!(bad, n_bad, "every outlier was placed");
        (apertures, (0..total).collect())
    }

    #[test]
    fn a_pedestal_left_with_too_few_points_after_clipping_is_none() {
        let (aps, members) = pedestal_points(24, 6);
        // The outer radii are 4 and 6 FWHM_ref (10 and 15 px at 2.5).
        let area = std::f64::consts::PI * (15.0f64 * 15.0 - 10.0 * 10.0);
        let eps = sky_pedestal(&aps, &members, 2.5, &params()).expect("24 good points of 30");
        assert!((eps - 2.0 / area).abs() < 3.0 / area, "the outliers are clipped: {eps} against {}", 2.0 / area);
        // Thirty points are plenty, but the six outliers go and 24 are left: under 25 is no pedestal.
        let strict = ForcedListParams { pedestal_min_points: 25, ..params() };
        assert_eq!(sky_pedestal(&aps, &members, 2.5, &strict), None);
    }

    #[test]
    fn the_pedestal_reports_the_share_its_clip_kept() {
        // A clean set (every point on the line): nothing clipped, a share of exactly 1.
        let apertures: Vec<ListAperture> = (0..30)
            .map(|i| {
                let a = 100.0 + 10.0 * i as f64;
                ListAperture::test_new(0.0, vec![a, 0.0, 2.0 + 0.01 * a], 1000.0, 4.0, 1000.0)
            })
            .collect();
        let members: Vec<usize> = (0..30).collect();
        let clean = sky_pedestal_fit(&apertures, &members, 2.5, &params());
        assert!(clean.eps.is_some());
        assert_eq!((clean.points, clean.kept, clean.kept_share()), (30, 30, Some(1.0)));
        // Six one-sided-in-pairs outliers among 24 good points: they are clipped, the share is what is left.
        let (aps, members) = pedestal_points(24, 6);
        let fit = sky_pedestal_fit(&aps, &members, 2.5, &params());
        assert_eq!(fit.points, 30);
        assert!((24..=26).contains(&fit.kept), "{fit:?}");
        let share = fit.kept_share().unwrap();
        assert!((0.8..=0.87).contains(&share), "{share}");
        assert_eq!(fit.eps, sky_pedestal(&aps, &members, 2.5, &params()), "one fit, two doors");
        // At a collapse the figure is still there: 24 left of 30 and under the minimum of 25.
        let strict = ForcedListParams { pedestal_min_points: 25, ..params() };
        let collapsed = sky_pedestal_fit(&aps, &members, 2.5, &strict);
        assert_eq!((collapsed.eps, collapsed.points), (None, 30));
        assert!(collapsed.kept < 25, "{collapsed:?}");
        // Nothing offered: no share.
        assert_eq!(sky_pedestal_fit(&[], &[], 2.5, &params()).kept_share(), None);
    }

    #[test]
    fn the_pedestal_clip_reads_its_rounds() {
        let (aps, members) = pedestal_points(24, 6);
        let none = ForcedListParams { pedestal_clip_rounds: 0, ..params() };
        let many = ForcedListParams { pedestal_clip_rounds: 10, ..params() };
        let (a, b, c) = (
            sky_pedestal(&aps, &members, 2.5, &none),
            sky_pedestal(&aps, &members, 2.5, &params()),
            sky_pedestal(&aps, &members, 2.5, &many),
        );
        assert_ne!(a, b, "no clipping is another fit");
        assert!((b.unwrap() - c.unwrap()).abs() < 1e-3, "four rounds have converged here: {b:?} {c:?}");
        assert_eq!(ForcedListParams::default().pedestal_clip_rounds, 4);
    }

    #[test]
    fn a_pedestal_added_under_every_aperture_is_recovered_and_leaves_the_zero_point_unmoved() {
        let list = hand_list(&even(600, 14.0, 17.0));
        let plain = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 31);
        let lifted = measured_for(&list, &|_, _| 21.8, 0.05, 0.7, 31);
        let (a, b) = (statistic(&list, &plain), statistic(&list, &lifted));
        let (za, zb) = (a.result.unwrap().zero_point, b.result.unwrap().zero_point);
        assert!(b.pedestal.is_some_and(|e| (e - 0.7).abs() < 0.05), "{:?}", b.pedestal);
        assert!((za - 21.8).abs() < 0.01 && (zb - za).abs() < 0.002, "{za} {zb}");
    }

    #[test]
    fn an_unfittable_pedestal_refuses_too_scattered() {
        let list = hand_list(&even(600, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 31);
        // No spread in S(3 FWHM_ref): the line has no slope to stand on.
        for a in &mut m.apertures {
            a.extra_sums[0] = 1000.0;
        }
        let o = statistic(&list, &m);
        assert!(
            matches!(o.result, Err(ForcedRefusal::TooScattered { kept: 600, sigma }) if sigma.is_nan()),
            "{:?}",
            o.result
        );
        assert_eq!(o.pedestal, None);
    }

    // ---- the screen and the weights ----------------------------------------------------------------------------------

    /// 300 stars from 14.5 to 16.5 with 0.05 mag of scatter and one probe star last, at `g_probe` and `delta` mag too
    /// bright; the statistic of it.
    fn with_probe(g_probe: f64, delta: f64) -> ForcedOutcome {
        with_probe_under(g_probe, delta, &params())
    }

    fn with_probe_under(g_probe: f64, delta: f64, p: &ForcedListParams) -> ForcedOutcome {
        let mut gs = even(300, 14.5, 16.5);
        gs.push(g_probe);
        let list = hand_list(&gs);
        let m = measured_for(&list, &|i, _| if i == 300 { 21.8 + delta } else { 21.8 }, 0.05, 0.0, 99);
        forced_zero_point(&list, &m, &plain_placement(&list), EXPTIME, p)
    }

    fn with_probe_tapered(g_probe: f64, delta: f64) -> ForcedOutcome {
        with_probe_under(g_probe, delta, &tapered())
    }

    fn zp_of(o: &ForcedOutcome) -> f64 {
        o.result.as_ref().expect("a valid zero point").zero_point
    }

    #[test]
    fn a_star_at_a_window_edge_changes_the_zero_point_continuously() {
        for (inside, outside) in [(14.0005, 13.9995), (16.9995, 17.0005)] {
            let across = (zp_of(&with_probe(inside, 0.15)) - zp_of(&with_probe(outside, 0.15))).abs();
            assert!(across < 2e-4, "a star crossing the edge moved the zero point by {across}");
        }
        // A star well inside the window with the same error moves it by far more.
        let inside = (zp_of(&with_probe(14.6, 0.15)) - zp_of(&with_probe(14.6, 0.0))).abs();
        assert!(inside > 5e-4, "{inside}");
    }

    /// The probe star's `delta` (mag too bright) at which it crosses `m + 4 sigma`, read off the screen's own figures.
    fn crossing() -> f64 {
        let o = with_probe(14.6, 0.0);
        let sigma = o.result.expect("valid").sigma;
        // The field's median is the truth (21.8) within the scatter's sampling: the limit is 4 sigma above it.
        4.0 * sigma
    }

    #[test]
    fn a_member_above_the_screen_is_tapered_and_the_zero_point_moves_continuously() {
        // The probe star rises through m + 4 sigma (about 0.2 mag). Fine steps across the limit and the taper (sigma
        // wide, 0.05 mag here): the largest step is small beside the jump of a plain drop at the same place.
        let edge = crossing();
        let delta_at = |k: i32| edge - 0.1 + f64::from(k) * 0.002;
        let largest_step = |p: &ForcedListParams| {
            let curve: Vec<f64> = (0..=100).map(|k| zp_of(&with_probe_under(14.6, delta_at(k), p))).collect();
            curve.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max)
        };
        let step = largest_step(&tapered());
        let jump = largest_step(&ForcedListParams { screen_taper_sigma: 0.0, ..params() });
        assert!(jump > 0.001, "a plain drop jumps by {jump}");
        assert!(step < jump / 8.0, "largest step of the taper {step}, the drop's jump {jump}");
        // Through the whole taper and beyond: no step at the start (the weight is 1 at the limit) or at the end (0).
        let o = with_probe_tapered(14.6, edge + 0.025);
        assert_eq!(o.tapered, 1);
        let w = o.per_star[300].weight;
        assert!(o.per_star[300].screened_in && w > 0.0 && w < 1.0, "inside the taper: weight {w}");
        // Past the taper's end the star is gone, and the zero point is that of the other 300.
        let (far, farther) = (with_probe_tapered(14.6, 0.8), with_probe_tapered(14.6, 1.5));
        assert!(!far.per_star[300].screened_in && far.per_star[300].member && far.per_star[300].weight == 0.0);
        assert_eq!((far.tapered, farther.tapered), (1, 1));
        let list = hand_list(&even(300, 14.5, 16.5));
        let others = statistic(&list, &measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99));
        // (The star still sits in the pedestal's fit, whatever its flux, so the others differ by a hair.)
        let (d_far, d_others) = ((zp_of(&far) - zp_of(&farther)).abs(), (zp_of(&far) - zp_of(&others)).abs());
        assert!(d_far < 1e-4 && d_others < 1e-4, "{d_far} {d_others}");
        // One far below (2 mag, beyond its noise) is dropped as well: the low side stays a drop.
        let low = with_probe_tapered(14.6, -2.0);
        assert!(!low.per_star[300].screened_in && low.per_star[300].member && low.tapered == 0);
        assert!((zp_of(&low) - zp_of(&others)).abs() < 1e-5);
    }

    #[test]
    fn a_zero_taper_is_a_plain_drop_with_no_division_by_zero() {
        let drop = ForcedListParams { screen_taper_sigma: 0.0, ..params() };
        let edge = crossing();
        let (under, over) = (with_probe_under(14.6, edge - 0.03, &drop), with_probe_under(14.6, edge + 0.03, &drop));
        assert!(under.per_star[300].screened_in && under.per_star[300].weight == 1.0 && under.tapered == 0);
        assert!(!over.per_star[300].screened_in && over.per_star[300].weight == 0.0 && over.tapered == 1);
        let list = hand_list(&even(300, 14.5, 16.5));
        let others = statistic(&list, &measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99));
        assert!(zp_of(&over).is_finite() && (zp_of(&over) - zp_of(&others)).abs() < 1e-5);
        // A negative width is no width either, and the default is no taper.
        let negative = ForcedListParams { screen_taper_sigma: -1.0, ..params() };
        assert_eq!(zp_of(&with_probe_under(14.6, edge + 0.03, &negative)), zp_of(&over));
        assert_eq!(params().screen_taper_sigma, 0.0, "the default is the plain drop");
        assert_eq!(tapered().screen_taper_sigma, 1.0);
        // A wider taper keeps the same star at a larger weight.
        let wide = ForcedListParams { screen_taper_sigma: 3.0, ..params() };
        let (w1, w3) = (with_probe_under(14.6, edge + 0.025, &tapered()), with_probe_under(14.6, edge + 0.025, &wide));
        assert!(w3.per_star[300].weight > w1.per_star[300].weight);
    }

    #[test]
    fn the_screen_keeps_a_negative_flux_inside_its_noise() {
        let mut gs = even(300, 14.5, 16.5);
        gs.push(16.9);
        let list = hand_list(&gs);
        let base = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99);
        let judged = |sky_scatter: f64| {
            let mut m = base.clone();
            m.apertures[300] = ListAperture::test_new(-3000.0, vec![-3000.0; 3], 1000.0, sky_scatter, 1000.0);
            statistic(&list, &m)
        };
        // n = scatter * sqrt(pi R^2): 282 * 17.7 = 5000, four of them reach over the limit (about 9 000).
        let noisy = judged(282.0);
        assert!(noisy.per_star[300].screened_in, "a negative flux inside its noise stays");
        assert!(noisy.result.is_ok());
        // The same flux where the sky is quiet is a real deficit: dropped.
        assert!(!judged(4.0).per_star[300].screened_in);
    }

    #[test]
    fn a_non_positive_sum_refuses_too_scattered() {
        // Half the members sit at their truth, the other half at -1.5 times it with a sky so noisy that the low rule keeps
        // them: the screen has a field (the positive half), and the weighted sum of fluxes is below zero.
        let list = hand_list(&even(600, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99);
        for (i, a) in m.apertures.iter_mut().enumerate() {
            if i % 2 == 1 {
                let v = -1.5 * EXPTIME * 10f64.powf(0.4 * (21.8 - f64::from(list.stars[i].g_mag)));
                *a = ListAperture::test_new(v, vec![v; 3], 1000.0, 1e5, 1000.0);
            }
        }
        let o = statistic(&list, &m);
        assert!(
            matches!(o.result, Err(ForcedRefusal::TooScattered { kept: 600, sigma }) if sigma.is_finite() && sigma < 0.2),
            "{:?}",
            o.result
        );
        assert_eq!(o.result.unwrap_err().code(), "too_scattered");
        // All 300 negative members were kept by the screen, so it was the sum that refused.
        assert!(o.per_star.iter().filter(|p| p.screened_in).count() == 600);
        // Every flux negative: no field at all, so no scatter either.
        let mut all_neg = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99);
        for (i, a) in all_neg.apertures.iter_mut().enumerate() {
            let v = -(1000.0 + 37.0 * i as f64);
            a.sum = v;
            a.extra_sums = vec![v; 3];
        }
        assert!(matches!(
            statistic(&list, &all_neg).result,
            Err(ForcedRefusal::TooScattered { sigma, .. }) if sigma.is_nan()
        ));
    }

    #[test]
    fn too_few_stars_after_the_screen_reports_the_survivors() {
        let list = hand_list(&even(300, 14.0, 17.0));
        let mut m = measured_for(&list, &|_, _| 21.8, 0.05, 0.0, 99);
        // Sixty members (weight 1, G 15.0 to 15.6); twenty of them sit 3 mag low under a quiet sky: the screen drops them.
        members_only(&list, &mut m, 100..160);
        for a in &mut m.apertures[100..120] {
            let k = 10f64.powf(-1.2);
            a.sum *= k;
            a.extra_sums.iter_mut().for_each(|v| *v *= k);
        }
        let o = statistic(&list, &m);
        assert_eq!(o.members, 60);
        assert_eq!(o.result, Err(ForcedRefusal::TooFewStars { list: 300, kept: 40 }));
        assert_eq!(o.per_star.iter().filter(|p| p.member && !p.screened_in).count(), 20);
    }

    // ---- the window slope and the reduction -----------------------------------------------------------------------

    fn slope_of(gs: &[f64]) -> (Option<f64>, bool) {
        let list = hand_list(gs);
        let o = statistic(&list, &measured_for(&list, &|_, g| 22.0 - 0.15 * (g - 15.0), 0.0, 0.0, 17));
        (o.window_slope, o.result.is_ok())
    }

    #[test]
    fn fewer_than_ten_bright_ramp_members_give_no_slope() {
        // The bright ramp is [14.0, 14.5): nine stars in it, then 300 above.
        let mut gs = even(9, 14.0, 14.5);
        gs.extend(even(300, 14.6, 16.4));
        assert_eq!(slope_of(&gs), (None, true), "nine bright-ramp members: a valid zero point, no slope");
        let mut gs = even(10, 14.0, 14.5);
        gs.extend(even(300, 14.6, 16.4));
        let (s, ok) = slope_of(&gs);
        assert!(ok && s.is_some_and(f64::is_finite), "ten are enough: {s:?}");
    }

    #[test]
    fn a_window_cut_by_the_catalogue_keeps_its_slope() {
        // The catalogue ends at 15.6, 1.4 mag before the faint edge: the faint ramp [16.5, 17] is empty.
        let list = hand_list(&even(400, 14.0, 15.6));
        let m = measured_for(&list, &|_, g| 22.0 - 0.15 * (g - 15.0), 0.0, 0.0, 17);
        let o = statistic(&list, &m);
        let s = o.window_slope.expect("a slope from the bright ramp alone");
        // The slope against both edges moved by hand, 0.1 mag each way (the faint one moves nothing here).
        let at = |shift: f64| {
            let mut l = list.clone();
            l.window_bright += shift;
            l.window_faint += shift;
            zp_of(&forced_zero_point(&l, &m, &plain_placement(&l), EXPTIME, &params()))
        };
        let moved = (at(0.1) - at(-0.1)) / 0.2;
        assert!(moved < -0.02, "the field's offsets fall with G, so a fainter bright edge lowers the zero point: {moved}");
        assert!((s - moved).abs() < 0.01, "analytic {s}, both edges moved {moved}");
    }

    #[test]
    fn a_tapered_member_moves_the_window_slope_as_it_moves_the_window() {
        // Every 40th star is 0.6 mag too bright: far enough above m + 4 sigma (sigma is the trend's own 0.13) to be
        // tapered. The slope reads those members at their taper, and the moved window agrees with it.
        let list = hand_list(&even(600, 14.0, 17.0));
        let trend = |i: usize, g: f64| 22.0 - 0.15 * (g - 15.0) + if i % 40 == 11 { 0.6 } else { 0.0 };
        let m = measured_for(&list, &trend, 0.0, 0.0, 3);
        let o = forced_zero_point(&list, &m, &plain_placement(&list), EXPTIME, &tapered());
        assert!(o.tapered > 0 && o.per_star.iter().any(|p| p.screened_in && p.weight > 0.0 && p.weight < 1.0), "{}", o.tapered);
        let at = |shift: f64| {
            let mut l = list.clone();
            l.window_bright += shift;
            l.window_faint += shift;
            zp_of(&forced_zero_point(&l, &m, &plain_placement(&l), EXPTIME, &tapered()))
        };
        let (s, moved) = (o.window_slope.expect("a slope"), (at(0.1) - at(-0.1)) / 0.2);
        assert!((s - moved).abs() < 0.02, "analytic {s}, moved {moved}");
    }

    #[test]
    fn the_analytic_window_slope_matches_a_moved_window() {
        let (w, h) = (W, H);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let gb = g_b(&wcs);
        let cat = sky_catalogue(4000, &wcs, w, h, gb - 1.0, gb + 6.0, 61);
        let trend = |_: usize, g: f64| 22.0 - 0.15 * (g - 15.0);
        let at_shift = |shift: f64| {
            let p = ForcedListParams { anchor_shift_mag: shift, ..params() };
            let list = forced_list(&input(&cat, &wcs, w, h), &p).unwrap();
            assert!(list.stars.len() > 400, "{}", list.stars.len());
            let m = measured_for(&list, &trend, 0.0, 0.0, 3);
            let o = forced_zero_point(&list, &m, &plain_placement(&list), EXPTIME, &p);
            (list.window_bright, zp_of(&o), o.window_slope)
        };
        let (g0, _, s) = at_shift(0.0);
        let (g_lo, z_lo, _) = at_shift(-0.1);
        let (g_hi, z_hi, _) = at_shift(0.1);
        assert!((g_hi - g_lo - 0.2).abs() < 1e-9 && (g_lo - (g0 - 0.1)).abs() < 1e-9, "the shift moves the edge");
        let moved = (z_hi - z_lo) / 0.2;
        let s = s.expect("a slope");
        assert!(s < -0.05 && (s - moved).abs() < 0.01, "analytic {s}, moved window {moved}");
    }

    #[test]
    fn a_loss_of_half_a_magnitude_reads_half_after_the_reduction() {
        // The field's offsets fall 0.15 mag per mag; the lost frame sees 0.5 mag less light, its window 0.5 mag brighter.
        let frame = |g_b: f64, loss: f64| {
            let mut list = hand_list(&even(400, g_b, g_b + 3.0));
            list.window_bright = g_b;
            list.window_faint = g_b + 3.0;
            let m = measured_for(&list, &|_, g| 21.8 - 0.15 * (g - 15.0) - loss, 0.0, 0.0, 3);
            let o = forced_zero_point(&list, &m, &plain_placement(&list), EXPTIME, &params());
            (zp_of(&o), g_b, o.window_slope)
        };
        let mut frames = vec![frame(14.0, 0.0), frame(13.5, 0.5)];
        let before = frames[0].0 - frames[1].0;
        assert!((before - 0.425).abs() < 0.015, "read without the reduction: {before}");
        reduce_to_common_window(&mut frames);
        let after = frames[0].0 - frames[1].0;
        assert!((after - 0.5).abs() < 0.02, "read after it: {after}");
    }

    #[test]
    fn common_window_of_one_frame_reads_its_zero_point() {
        let w = common_window(&[(13.7, Some(-0.12))]).unwrap();
        assert_eq!(w, CommonWindow { pivot: 13.7, slope: -0.12 });
        assert_eq!(w.reduce(21.9, 13.7), 21.9);
        assert_eq!(common_window(&[]), None);
        let mut one = [(21.9, 13.7, None)];
        reduce_to_common_window(&mut one);
        assert_eq!(one[0].0, 21.9);
    }

    #[test]
    fn a_refused_frame_takes_no_part_in_the_pivot() {
        // The NaN frame's window (G_b 20) would drag the median pivot from 14.0 to 14.5.
        let mut frames = [(21.9, 13.0, Some(-0.2)), (21.8, 14.0, Some(-0.2)), (f64::NAN, 20.0, None), (21.7, 15.0, Some(-0.2))];
        reduce_to_common_window(&mut frames);
        assert!((frames[1].0 - 21.8).abs() < 1e-12, "the median window of the three valid frames is its own: {}", frames[1].0);
        assert!((frames[0].0 - (21.9 + 0.2 * (13.0 - 14.0))).abs() < 1e-12);
        assert!(frames[2].0.is_nan(), "a refused frame stays refused");
    }

    #[test]
    fn a_set_without_a_slope_is_not_reduced() {
        let mut frames = [(21.9, 13.7, None), (21.5, 13.2, None), (21.7, 14.1, None)];
        let before = frames;
        reduce_to_common_window(&mut frames);
        assert_eq!(frames, before);
        // The pivot is the median edge; the slope the median of the slopes that exist.
        let w = common_window(&[(13.0, None), (14.0, Some(-0.2)), (15.0, Some(-0.1))]).unwrap();
        assert_eq!(w.pivot, 14.0);
        assert!((w.slope + 0.15).abs() < 1e-12);
    }

    // ---- the map ----------------------------------------------------------------------------------------------------

    /// The first-pass fits of the map stars: each sits at its projected position plus `truth` there plus `noise` px.
    fn first_pass(list: &ForcedList, truth: &dyn Fn(f64, f64) -> (f64, f64), noise: f64, seed: u64) -> Vec<ListFit> {
        let mut rng = Rng(seed);
        list.map
            .iter()
            .map(|&i| {
                let s = &list.stars[i];
                let (dx, dy) = truth(s.x, s.y);
                let (dx, dy) = (dx + noise * rng.normal(), dy + noise * rng.normal());
                let mut f = ListFit::test_kept(s.x + dx, s.y + dy, 1000.0);
                f.centre_offset_px = dx.hypot(dy);
                f
            })
            .collect()
    }

    /// A shift and a quadratic distortion, up to about 3 px at a corner.
    fn distortion(x: f64, y: f64) -> (f64, f64) {
        let (u, v) = ((x - 1999.5) / 2500.0, (y - 1499.5) / 2500.0);
        (
            1.5 + 1.2 * u - 0.8 * v + 1.5 * u * u + 0.9 * u * v - 1.0 * v * v,
            -1.0 + 0.7 * u + 1.1 * v - 0.9 * u * u + 1.2 * u * v + 0.6 * v * v,
        )
    }

    #[test]
    fn a_shifted_and_quadratically_distorted_field_is_mapped_to_under_five_hundredths_of_a_pixel() {
        let list = hand_list(&vec![15.0; 400]);
        assert_eq!(list.map.len(), 400);
        let map = position_map(&list, &first_pass(&list, &distortion, 0.03, 5), &params()).unwrap();
        assert!(map.members >= 380, "{}", map.members);
        assert!(map.offset_px > 1.0 && map.residual_px < 0.05, "{} {}", map.offset_px, map.residual_px);
        let mut rng = Rng(8);
        for _ in 0..2000 {
            let (x, y) = (150.0 + 3700.0 * rng.unit(), 150.0 + 2700.0 * rng.unit());
            let ((tx, ty), (mx, my)) = (distortion(x, y), map.shift(x, y));
            assert!((tx - mx).hypot(ty - my) < 0.05, "at ({x:.0}, {y:.0}): {} {}", tx - mx, ty - my);
        }
    }

    #[test]
    fn the_placement_moves_every_position_and_sets_the_geometry() {
        let list = hand_list(&even(500, 14.0, 17.0));
        let map = position_map(&list, &first_pass(&list, &distortion, 0.03, 5), &params()).unwrap();
        let p = placement(&list, &map, 8.0, &params());
        assert_eq!((p.refit_positions.len(), p.aperture_positions.len()), (list.map.len(), 500));
        for (s, &(x, y)) in list.stars.iter().zip(&p.aperture_positions) {
            let (dx, dy) = distortion(s.x, s.y);
            assert!((x - s.x - dx).hypot(y - s.y - dy) < 0.05);
        }
        for (&i, &(x, y)) in list.map.iter().zip(&p.refit_positions) {
            assert_eq!((x, y), p.aperture_positions[i], "a refit seed is its aperture's position");
        }
        // F = 8 px: R = max(4 * 2.5, 2 * 8), the annulus starts at max(4 F, 6 FWHM_ref) + 2.
        assert_eq!(
            (p.radius_px, p.annulus_start_px, p.annulus_area_px, p.peak_radius_px),
            (16.0, 34.0, 2000.0, 3.75)
        );
        assert_eq!(p.extra_radii_px, vec![7.5, 10.0, 15.0]);
        // A narrow frame: the reference scale sets R and the annulus.
        let narrow = placement(&list, &map, 2.0, &params());
        assert_eq!((narrow.radius_px, narrow.annulus_start_px), (10.0, 17.0));
    }

    #[test]
    fn the_annulus_clears_every_disc_for_every_frame_fwhm() {
        // rustafits refuses a placement whose annulus starts closer than its largest radius plus 0.75 px.
        let mut list = hand_list(&even(100, 14.0, 17.0));
        let map = QuadMap { cx: 0.0, cy: 0.0, half: 1.0, ax: [0.0; 6], ay: [0.0; 6], members: 400, offset_px: 0.0, residual_px: 0.0 };
        for fwhm_ref in [2.0, 2.5, 5.0] {
            list.fwhm_ref_px = fwhm_ref;
            for f in [0.5, 1.0, 2.0, 3.0, 4.5, 6.0, 8.0, 12.0, 20.0, 40.0, f64::NAN] {
                let p = placement(&list, &map, f, &params());
                let r_max = p.extra_radii_px.iter().copied().fold(p.radius_px, f64::max);
                assert!(
                    p.annulus_start_px >= r_max + 0.75,
                    "FWHM_ref {fwhm_ref}, F {f}: annulus at {} against radius {r_max}",
                    p.annulus_start_px
                );
            }
        }
    }

    #[test]
    fn the_maps_clip_drops_gross_outliers() {
        let list = hand_list(&vec![15.0; 400]);
        // A wide radius, so it is the clip and not the radius rule that drops them.
        let wide = ForcedListParams { map_radius_fwhm: 20.0, ..params() };
        let mut fits = first_pass(&list, &distortion, 0.03, 5);
        for (k, f) in fits.iter_mut().enumerate().filter(|(k, _)| k % 20 == 7) {
            let (ex, ey) = [(6.0, 0.0), (0.0, 6.0), (-6.0, 0.0), (0.0, -6.0)][(k / 20) % 4];
            f.x += ex;
            f.y += ey;
            let s = &list.stars[list.map[k]];
            f.centre_offset_px = (f.x - s.x).hypot(f.y - s.y);
        }
        let worst = |map: &QuadMap| {
            let mut rng = Rng(8);
            (0..2000)
                .map(|_| {
                    let (x, y) = (150.0 + 3700.0 * rng.unit(), 150.0 + 2700.0 * rng.unit());
                    let ((tx, ty), (mx, my)) = (distortion(x, y), map.shift(x, y));
                    (tx - mx).hypot(ty - my)
                })
                .fold(0.0, f64::max)
        };
        let map = position_map(&list, &fits, &wide).unwrap();
        assert!((370..=380).contains(&map.members), "the 20 outliers go: {}", map.members);
        assert!(worst(&map) < 0.05, "the clipped map follows the truth: {}", worst(&map));
        let unclipped = position_map(&list, &fits, &ForcedListParams { map_clip_rounds: 0, ..wide }).unwrap();
        assert_eq!(unclipped.members, 400);
        assert!(worst(&unclipped) > 0.05, "without the clip they pull it: {}", worst(&unclipped));
    }

    /// The second pass over a placement for a truth: each refit sits on its star, `kept` by the tolerance from its seed.
    fn second_pass(list: &ForcedList, p: &ListPlacement, truth: &dyn Fn(f64, f64) -> (f64, f64)) -> Vec<ListFit> {
        list.map
            .iter()
            .zip(&p.refit_positions)
            .map(|(&i, &(px, py))| {
                let s = &list.stars[i];
                let (dx, dy) = truth(s.x, s.y);
                let (cx, cy) = (s.x + dx, s.y + dy);
                let mut f = ListFit::test_kept(cx, cy, 1000.0);
                f.centre_offset_px = (cx - px).hypot(cy - py);
                f.kept = f.centre_offset_px <= 1.25;
                f
            })
            .collect()
    }

    #[test]
    fn a_field_shifted_three_pixels_over_a_third_of_the_frame_is_poor_solve() {
        let list = hand_list(&vec![15.0; 400]);
        // Every third 500 px stripe of the frame (a third of it) sits 3 px off its projections, the rest is where the
        // solve puts it: no smooth map follows that. (One third as a single block does not fail: a quadratic map
        // follows a block that size, 0.90 of the stars kept, and the stars are then where the map puts them.)
        let truth = |x: f64, _y: f64| if ((x / 500.0).floor() as i64) % 3 == 0 { (3.0, 0.0) } else { (0.0, 0.0) };
        // (One third as a single block passes the gate on purpose: see below.)
        let verdict = position_map(&list, &first_pass(&list, &truth, 0.03, 5), &params()).and_then(|map| {
            let p = placement(&list, &map, 8.0, &params());
            map_gate(&list, &second_pass(&list, &p, &truth), &p, &params())
        });
        assert!(matches!(verdict, Err(ForcedRefusal::PoorSolve { .. })), "{verdict:?}");
        assert_eq!(verdict.unwrap_err().code(), "poor_solve");
        // A single 3 px block over a third of the frame passes (a quadratic map follows a block that size, 0.80 to 0.95 of
        // the stars kept): its stars stay inside the aperture radius R, so the zero point moves by at most 0.002 mag
        // (the G2 review's simulation at F 2 to 4.4 px). The gate is for the frames whose apertures miss, like the
        // eight bad frames (0.31 to 0.64 kept), and stripes refuse it.
        let block = |x: f64, _y: f64| if x < 1333.0 { (3.0, 0.0) } else { (0.0, 0.0) };
        let through = position_map(&list, &first_pass(&list, &block, 0.03, 5), &params()).and_then(|map| {
            let p = placement(&list, &map, 8.0, &params());
            map_gate(&list, &second_pass(&list, &p, &block), &p, &params())
        });
        assert!(through.as_ref().is_ok_and(|g| (0.80..=0.95).contains(&g.kept_share)), "{through:?}");
        // The same field with one smooth shift passes both.
        let smooth = |_: f64, _: f64| (3.0, -1.0);
        let ok = position_map(&list, &first_pass(&list, &smooth, 0.03, 5), &params()).and_then(|map| {
            let p = placement(&list, &map, 8.0, &params());
            map_gate(&list, &second_pass(&list, &p, &smooth), &p, &params())
        });
        assert!(ok.is_ok_and(|g| g.kept_share == 1.0), "a uniform shift is a good solve");
    }

    #[test]
    fn the_gate_asks_three_quarters_of_the_map_stars() {
        let list = hand_list(&vec![15.0; 400]);
        let map = position_map(&list, &first_pass(&list, &distortion, 0.03, 5), &params()).unwrap();
        let p = placement(&list, &map, 8.0, &params());
        let mut refits = second_pass(&list, &p, &distortion);
        for f in refits.iter_mut().take(100) {
            f.kept = false;
        }
        let g = map_gate(&list, &refits, &p, &params()).expect("300 of 400 is 0.75");
        assert_eq!(g.kept_share, 0.75);
        assert!(g.centre_median_px < 0.1, "{}", g.centre_median_px);
        refits[100].kept = false;
        assert!(matches!(
            map_gate(&list, &refits, &p, &params()),
            Err(ForcedRefusal::PoorSolve { kept_share }) if (kept_share - 0.7475).abs() < 1e-12
        ));
        refits.pop();
        assert_eq!(map_gate(&list, &refits, &p, &params()), Err(ForcedRefusal::Internal));
    }

    #[test]
    fn fewer_than_thirty_map_members_is_poor_solve() {
        let list = hand_list(&vec![15.0; 100]);
        let mut fits = first_pass(&list, &distortion, 0.03, 5);
        for f in fits.iter_mut().skip(29) {
            *f = ListFit::test_refused();
        }
        assert_eq!(position_map(&list, &fits, &params()).unwrap_err().code(), "poor_solve");
        // Thirty are a map.
        fits = first_pass(&list, &distortion, 0.03, 5);
        for f in fits.iter_mut().skip(30) {
            *f = ListFit::test_refused();
        }
        assert!(position_map(&list, &fits, &params()).is_ok());
    }

    #[test]
    fn a_list_with_fewer_than_forty_map_stars_is_too_few_stars_not_poor_solve() {
        let few = |n: usize| {
            let list = hand_list(&vec![15.0; n]);
            let mut fits = first_pass(&list, &distortion, 0.03, 5);
            for f in fits.iter_mut().skip(25) {
                *f = ListFit::test_refused();
            }
            position_map(&list, &fits, &params())
        };
        assert_eq!(few(39).unwrap_err(), ForcedRefusal::TooFewStars { list: 39, kept: 25 });
        assert_eq!(few(40).unwrap_err().code(), "poor_solve");
    }

    #[test]
    fn a_map_member_needs_snr_and_no_saturation() {
        let list = hand_list(&vec![15.0; 100]);
        let mut fits = first_pass(&list, &distortion, 0.03, 5);
        for f in fits.iter_mut().take(40) {
            f.snr = 9.9;
        }
        for f in fits.iter_mut().skip(40).take(30) {
            f.flags = crate::star_measure::StarFlags::SATURATED;
        }
        // A fit with no ring sky has no SNR and is not refused for it.
        for f in fits.iter_mut().skip(70).take(10) {
            f.snr = f64::NAN;
        }
        let map = position_map(&list, &fits, &params()).unwrap();
        assert_eq!(map.members, 30);
    }

    #[test]
    fn the_map_stars_are_the_bright_end_of_the_list_in_key_order() {
        let (w, h) = (W, H);
        let wcs = wcs_at(w, h, 1.0, (0.0, 0.0), 0.0);
        let gb = g_b(&wcs);
        let cat = sky_catalogue(5000, &wcs, w, h, gb - 1.0, gb + 6.0, 11);
        let l = forced_list(&input(&cat, &wcs, w, h), &params()).unwrap();
        assert!(l.map.len() > 100 && l.map.len() <= 400, "{}", l.map.len());
        assert!(l.map.windows(2).all(|p| p[0] < p[1]), "key order");
        assert!(l.map.iter().all(|&i| f64::from(l.stars[i].g_mag) <= l.window_bright + 2.0));
        let bright = l.stars.iter().filter(|s| f64::from(s.g_mag) <= l.window_bright + 2.0).count();
        assert_eq!(l.map.len(), bright.min(400));
        assert_eq!(l.request.fit_positions.len(), l.map.len());
        assert_eq!((l.width, l.height), (w, h));
        // The first `map_max_stars` in key order.
        let few = forced_list(&input(&cat, &wcs, w, h), &ForcedListParams { map_max_stars: 7, ..params() }).unwrap();
        assert_eq!(few.map, l.map[..7].to_vec());
    }

    #[test]
    fn decimate_keeps_the_same_tenth_in_every_frame() {
        let a = wcs_at(W, H, 1.0, (0.0, 0.0), 0.0);
        let b = wcs_at(W, H, 1.0, (60.0, 40.0), 0.05);
        let gb = g_b(&a);
        let cat = sky_catalogue(9000, &a, W, H, gb - 1.0, gb + 6.0, 11);
        let tenth = ForcedListParams { decimate: 10, ..params() };
        let full = forced_list(&input(&cat, &a, W, H), &params()).unwrap();
        let (da, db) =
            (forced_list(&input(&cat, &a, W, H), &tenth).unwrap(), forced_list(&input(&cat, &b, W, H), &tenth).unwrap());
        assert!(da.stars.iter().all(|s| s.key % 10 == 0));
        let share_of_full = da.stars.len() as f64 / full.stars.len() as f64;
        assert!((0.06..0.14).contains(&share_of_full), "{share_of_full}");
        assert!(share(&da, &db) >= 0.95 && share(&db, &da) >= 0.95, "the same tenth in both frames");
        assert_eq!(forced_list(&input(&cat, &a, W, H), &ForcedListParams { decimate: 1, ..params() }).unwrap(), full);
    }
}
