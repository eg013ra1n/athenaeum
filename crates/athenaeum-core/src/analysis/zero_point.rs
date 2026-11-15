//! A frame's photometric zero point from its fitted stars and the installed
//! star catalogue (Gaia G), the number a transparency loss is later taken from.
//!
//! Pure: no database, no file, no logging. The caller passes the frame's stored
//! star rows, its WCS, a catalogue cone and its exposure; this module projects
//! the catalogue through the WCS, matches stars one to one, and returns the
//! sigma-clipped median of `G + 2.5·log10(flux / exptime)` or a typed refusal
//! (spec §5.1). Pixel positions on both sides are 0-based in file row order.

use std::collections::HashMap;

use anyhow::{bail, Result};
use solvemyastro::{CatalogStar, LayeredCones, StarCache, WcsSolution};

use crate::models::StarMetric;

/// Version of the measurement; a stored zero point is current only at this value.
pub const ZP_VERSION: u32 = 1;

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
    /// Fewest surviving stars of a valid frame.
    pub min_stars: usize,
    /// Largest standard error of the median (mag) of a valid frame.
    pub max_std_error_mag: f64,
    /// Sigma-clip threshold in robust sigmas.
    pub clip_sigma: f64,
    /// The brightest fraction of the analysis stars is dropped (saturation guard).
    pub bright_drop_fraction: f64,
    /// Analysis stars below this fit SNR are not used.
    pub min_snr: f64,
    /// A second catalogue star within this many FWHM of a star makes a blend.
    pub blend_radius_fwhm: f64,
}

impl Default for ZpParams {
    fn default() -> Self {
        Self {
            match_radius_px: 2.0,
            central_fraction: 0.6,
            min_stars: 15,
            max_std_error_mag: 0.05,
            clip_sigma: 2.5,
            bright_drop_fraction: 0.10,
            min_snr: 20.0,
            blend_radius_fwhm: 3.0,
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

/// Measure a frame's zero point (spec §5.1). Never panics; a frame it cannot
/// measure comes back as a typed refusal with the counts behind it.
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
    let grid = Grid::new(&cat, params.match_radius_px.max(blend_r).max(1.0));

    // 3. Analysis stars: central region, positive finite flux, enough SNR;
    //    the brightest fraction goes (the saturation guard).
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
        let blended = grid.near(s.x, s.y).any(|cj| {
            cj != ci && (cat[cj].0 - s.x).hypot(cat[cj].1 - s.y) <= blend_r
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

    // 5. Sigma-clipped median.
    for _ in 0..MAX_CLIP_ROUNDS {
        let alive: Vec<f64> = per_star.iter().filter(|p| p.kept).map(|p| p.zp).collect();
        if alive.is_empty() {
            break;
        }
        let (m, sigma) = median_and_sigma(&alive);
        let limit = params.clip_sigma * sigma.max(CLIP_SIGMA_FLOOR_MAG);
        let mut dropped = false;
        for p in per_star.iter_mut().filter(|p| p.kept) {
            if (p.zp - m).abs() > limit {
                p.kept = false;
                dropped = true;
            }
        }
        if !dropped {
            break;
        }
    }

    // 6. Verdict.
    let alive: Vec<f64> = per_star.iter().filter(|p| p.kept).map(|p| p.zp).collect();
    let kept = alive.len() as u32;
    let result = if alive.is_empty() || alive.len() < params.min_stars {
        Err(ZpRefusal::TooFewStars { matched, kept })
    } else {
        let (zero_point, sigma) = median_and_sigma(&alive);
        let std_error = sigma / (alive.len() as f64).sqrt();
        if std_error > params.max_std_error_mag {
            Err(ZpRefusal::TooScattered { std_error, kept })
        } else {
            Ok(ZeroPoint { zero_point, stars: kept, sigma })
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
    let radius_deg = region_px * wcs.pixel_scale_arcsec() / 3600.0 * CONE_MARGIN;
    if !(ra.is_finite() && dec.is_finite() && radius_deg.is_finite() && radius_deg > 0.0) {
        bail!(
            "the WCS gives no usable cone (centre {ra}, {dec}, radius {radius_deg} deg)"
        );
    }
    let cones = LayeredCones::new(layers, epoch);
    cones.cone(layers.len(), ra, dec, radius_deg, f32::INFINITY, epoch)
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
        assert!(zp.stars >= 15, "stars {}", zp.stars);
    }

    #[test]
    fn saturated_bright_stars_do_not_move_the_median() {
        let mut f = field(200, TRUE_ZP, 0.0, 2);
        // The brightest 5 % (index 0 is the brightest) carry half their flux.
        for s in f.stars.iter_mut().take(10) {
            s.flux *= 0.5;
        }
        let zp = measure(&f).result.expect("valid");
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

    #[test]
    fn stars_outside_the_central_region_are_not_used() {
        let mut f = field(100, TRUE_ZP, 0.0, 4);
        for k in 0..5 {
            let (x, y) = (3800.0 + k as f64 * 40.0, 150.0);
            f.stars.push(star(x, y, flux_for(11.5, TRUE_ZP)));
            f.catalog.push(cat_at(&f.wcs, x, y, 11.5));
        }
        let m = measure(&f);
        assert!(m.result.is_ok());
        assert!(!m.per_star.is_empty());
        assert!(
            m.per_star.iter().all(|s| s.radius_fraction <= 0.6),
            "a star beyond the central region was used"
        );
        assert!(!m.per_star.iter().any(|s| (s.g_mag - 11.5).abs() < 1e-3));
    }

    #[test]
    fn a_match_is_one_to_one() {
        let mut f = field(60, TRUE_ZP, 0.0, 5);
        let (x, y) = (1999.5, 1499.5 - 450.0);
        f.catalog.push(cat_at(&f.wcs, x, y, 12.5));
        f.stars.push(star(x - 1.0, y, flux_for(12.5, TRUE_ZP)));
        f.stars.push(star(x + 1.0, y, flux_for(12.5, TRUE_ZP)));
        let m = measure(&f);
        let at_12_5 = m.per_star.iter().filter(|s| (s.g_mag - 12.5).abs() < 1e-3).count();
        assert_eq!(at_12_5, 1, "one catalogue star served {at_12_5} analysis stars");
    }

    #[test]
    fn fewer_than_the_minimum_refuses_with_counts() {
        let f = field(9, TRUE_ZP, 0.0, 6);
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
    fn a_wide_scatter_of_few_stars_refuses() {
        let f = field(16, TRUE_ZP, 1.0, 7);
        let m = measure(&f);
        match m.result {
            Err(ZpRefusal::TooScattered { std_error, kept }) => {
                assert!(std_error > 0.05, "std_error {std_error}");
                assert!(kept >= 15, "kept {kept}");
            }
            other => panic!("expected TooScattered, got {other:?}"),
        }
    }

    #[test]
    fn a_wide_scatter_of_many_stars_is_valid() {
        let f = field(400, TRUE_ZP, 0.30, 8);
        let zp = measure(&f).result.expect("many stars average the scatter out");
        assert!((zp.zero_point - TRUE_ZP).abs() < 0.05, "zp {}", zp.zero_point);
        assert!(zp.sigma > 0.1, "sigma {}", zp.sigma);
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
}
