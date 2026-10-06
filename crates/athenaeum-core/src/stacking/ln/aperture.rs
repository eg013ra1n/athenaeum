//! The common-aperture photometry of the LN scale's second estimator (spec
//! 2026-10-04 §4.1, L-13): a whole-pixel circle of radius `a·F` at each
//! frame's own fitted centroid, `F` the larger of the two frames' median
//! fitted FWHMs, and a local sky from the `[6F, 8F]` annulus median. A pair
//! is valid when its circle is entirely present on both frames, at least
//! half of each annulus is present, and both fluxes are positive.

use crate::integration::stats::median_in_place;
use crate::stacking::integrate::LnAperture;

pub const SKY_INNER_FWHM: f64 = 6.0;
pub const SKY_OUTER_FWHM: f64 = 8.0;
pub const GROWTH_OUTER_FWHM: f64 = 5.0;
pub const GROWTH_PAIRS: usize = 100;

pub fn aperture_multiple(a: LnAperture) -> f64 {
    match a {
        LnAperture::Fwhm2 | LnAperture::GrowthCurve => 2.0,
        LnAperture::Fwhm3 => 3.0,
    }
}

/// Which pixels of a plane are data (1 bit per pixel; 7.6 MB at 61 Mpx).
#[derive(Debug, Clone, PartialEq)]
pub struct FiniteMask {
    width: usize,
    height: usize,
    bits: Vec<u64>,
}

impl FiniteMask {
    pub fn from_plane(plane: &[f32], width: usize, height: usize) -> FiniteMask {
        debug_assert_eq!(plane.len(), width * height);
        Self::from_fn(width, height, |k| plane[k].is_finite())
    }

    pub fn from_bools(mask: &[bool], width: usize, height: usize) -> FiniteMask {
        debug_assert_eq!(mask.len(), width * height);
        Self::from_fn(width, height, |k| mask[k])
    }

    pub fn all(width: usize, height: usize) -> FiniteMask {
        Self::from_fn(width, height, |_| true)
    }

    /// Bit `k` (row-major) set where `f(k)`. Shared with the selection's
    /// reference-frames saturation mask (L-23), the same one-bit storage.
    pub(crate) fn from_fn(width: usize, height: usize, f: impl Fn(usize) -> bool) -> FiniteMask {
        let n = width * height;
        let mut bits = vec![0u64; n.div_ceil(64)];
        for k in 0..n {
            if f(k) {
                bits[k / 64] |= 1 << (k % 64);
            }
        }
        FiniteMask {
            width,
            height,
            bits,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// `false` outside the plane.
    pub fn get(&self, x: isize, y: isize) -> bool {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return false;
        }
        let k = y as usize * self.width + x as usize;
        (self.bits[k / 64] >> (k % 64)) & 1 == 1
    }
}

/// One frame's plane (values are read where `present` is set) and its mask.
#[derive(Clone, Copy)]
pub struct ApertureSide<'a> {
    pub plane: &'a [f32],
    pub present: &'a FiniteMask,
}

/// `Σ_circle − N·sky` (spec §4.1), `None` when invalid: a circle pixel
/// absent or outside the plane, fewer than half of the annulus pixels
/// present, or a flux ≤ 0. Pixels whose centre lies within `radius` count
/// whole; the annulus is `sky_inner ≤ d ≤ sky_outer`.
pub fn circle_flux(
    side: &ApertureSide<'_>,
    cx: f64,
    cy: f64,
    radius: f64,
    sky_inner: f64,
    sky_outer: f64,
    scratch: &mut Vec<f32>,
) -> Option<f64> {
    let w = side.present.width();
    debug_assert_eq!(side.plane.len(), w * side.present.height());
    let reach = sky_outer.max(radius).ceil() as isize;
    let (ix, iy) = (cx.round() as isize, cy.round() as isize);
    let (r2, in2, out2) = (
        radius * radius,
        sky_inner * sky_inner,
        sky_outer * sky_outer,
    );
    let (mut sum, mut n) = (0f64, 0usize);
    let mut annulus_total = 0usize;
    scratch.clear();
    for y in iy - reach..=iy + reach {
        for x in ix - reach..=ix + reach {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let d2 = dx * dx + dy * dy;
            let present = side.present.get(x, y);
            if d2 <= r2 {
                if !present {
                    return None;
                }
                sum += side.plane[y as usize * w + x as usize] as f64;
                n += 1;
            } else if d2 >= in2 && d2 <= out2 {
                annulus_total += 1;
                if present {
                    scratch.push(side.plane[y as usize * w + x as usize]);
                }
            }
        }
    }
    if n == 0 || scratch.is_empty() || 2 * scratch.len() < annulus_total {
        return None;
    }
    let sky = median_in_place(scratch) as f64;
    let flux = sum - n as f64 * sky;
    (flux > 0.0).then_some(flux)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKY: f32 = 0.1;

    #[derive(Clone, Copy)]
    enum Profile {
        Gaussian { fwhm: f64 },
        Moffat { fwhm: f64, beta: f64 },
    }

    impl Profile {
        /// Pixel-centre value of a unit-flux star at squared distance `d2`.
        fn value(self, d2: f64) -> f64 {
            match self {
                Profile::Gaussian { fwhm } => {
                    let s2 = (fwhm / 2.354_820_045_030_949).powi(2);
                    (-d2 / (2.0 * s2)).exp() / (2.0 * std::f64::consts::PI * s2)
                }
                Profile::Moffat { fwhm, beta } => {
                    let a2 = (fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt())).powi(2);
                    (beta - 1.0) / (std::f64::consts::PI * a2) * (1.0 + d2 / a2).powf(-beta)
                }
            }
        }
    }

    /// A flat sky plus stars `(cx, cy, flux)` of one profile, summed over a
    /// ±80 px window around each (the wings beyond are far below 1e-6).
    fn render(w: usize, h: usize, stars: &[(f64, f64, f64)], profile: Profile) -> Vec<f32> {
        let mut plane = vec![SKY as f64; w * h];
        for &(cx, cy, flux) in stars {
            let (x0, x1) = (
                (cx - 80.0).max(0.0) as usize,
                ((cx + 80.0) as usize).min(w - 1),
            );
            let (y0, y1) = (
                (cy - 80.0).max(0.0) as usize,
                ((cy + 80.0) as usize).min(h - 1),
            );
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (dx, dy) = (x as f64 - cx, y as f64 - cy);
                    plane[y * w + x] += flux * profile.value(dx * dx + dy * dy);
                }
            }
        }
        plane.into_iter().map(|v| v as f32).collect()
    }

    /// 30 stars on a 6 × 5 lattice of a 768 × 512 plane: 110-px spacing in x
    /// (border 60), 100-px in y (border 56; five rows at 110 px plus a 60-px
    /// border need 560 px, the plane has 512). Every border exceeds the 8F =
    /// 36 px annulus reach, every neighbour is farther than 2 x 36 px.
    /// Fractional offsets and fluxes vary per star, deterministically.
    fn lattice() -> Vec<(f64, f64, f64)> {
        let mut stars = Vec::new();
        for j in 0..5 {
            for i in 0..6 {
                let k = (j * 6 + i) as f64;
                let fx = (k * 0.37).fract();
                let fy = (k * 0.61).fract();
                let flux = 20.0 + (k * 7.0) % 40.0;
                stars.push((
                    60.0 + 110.0 * i as f64 + fx,
                    56.0 + 100.0 * j as f64 + fy,
                    flux,
                ));
            }
        }
        stars
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    fn star_plane() -> (Vec<f32>, f64, f64, f64) {
        let (cx, cy, flux) = (64.3, 63.6, 50.0);
        let plane = render(128, 128, &[(cx, cy, flux)], Profile::Gaussian { fwhm: 4.0 });
        (plane, cx, cy, flux)
    }

    #[test]
    fn circle_flux_recovers_a_gaussian_star() {
        let (plane, cx, cy, flux) = star_plane();
        let mask = FiniteMask::all(128, 128);
        let side = ApertureSide {
            plane: &plane,
            present: &mask,
        };
        let mut scratch = Vec::new();
        let r = 3.0 * 4.0;
        let got = circle_flux(&side, cx, cy, r, 24.0, 32.0, &mut scratch).expect("valid circle");
        let sigma = 4.0 / 2.354_820_045_030_949;
        let want = flux * (1.0 - (-r * r / (2.0 * sigma * sigma)).exp());
        let rel = (got - want).abs() / want;
        assert!(rel < 0.002, "got {got}, want {want}, rel {rel}");
    }

    #[test]
    fn a_circle_touching_an_absent_or_outside_pixel_is_invalid() {
        let (plane, cx, cy, _) = star_plane();
        let mut scratch = Vec::new();
        let r = 12.0;
        // One pixel inside the circle absent.
        let mut bools = vec![true; 128 * 128];
        bools[(63 * 128) + 70] = false; // 5.7 px from the centre
        let mask = FiniteMask::from_bools(&bools, 128, 128);
        let side = ApertureSide {
            plane: &plane,
            present: &mask,
        };
        assert_eq!(
            circle_flux(&side, cx, cy, r, 24.0, 32.0, &mut scratch),
            None
        );
        // A star whose circle crosses the image edge.
        let edge = render(
            128,
            128,
            &[(6.0, 64.0, 50.0)],
            Profile::Gaussian { fwhm: 4.0 },
        );
        let all = FiniteMask::all(128, 128);
        let side = ApertureSide {
            plane: &edge,
            present: &all,
        };
        assert_eq!(
            circle_flux(&side, 6.0, 64.0, r, 24.0, 32.0, &mut scratch),
            None
        );
    }

    /// A NaN pixel inside the circle, with the mask built from the plane
    /// itself, makes the pair invalid; without it the same circle is valid.
    #[test]
    fn a_nan_pixel_inside_the_circle_is_invalid_through_a_plane_mask() {
        let (mut plane, cx, cy, _) = star_plane();
        let mut scratch = Vec::new();
        let clean = FiniteMask::from_plane(&plane, 128, 128);
        let side = ApertureSide {
            plane: &plane,
            present: &clean,
        };
        assert!(circle_flux(&side, cx, cy, 8.0, 24.0, 32.0, &mut scratch).is_some());
        plane[64 * 128 + 66] = f32::NAN;
        let holed = FiniteMask::from_plane(&plane, 128, 128);
        let side = ApertureSide {
            plane: &plane,
            present: &holed,
        };
        assert_eq!(
            circle_flux(&side, cx, cy, 8.0, 24.0, 32.0, &mut scratch),
            None
        );
    }

    #[test]
    fn an_annulus_under_half_present_is_invalid() {
        let (plane, cx, cy, _) = star_plane();
        let mut scratch = Vec::new();
        let (inner, outer) = (24.0f64, 32.0f64);
        let annulus: Vec<usize> = (0..128usize * 128)
            .filter(|k| {
                let (x, y) = ((k % 128) as f64, (k / 128) as f64);
                let d2 = (x - cx).powi(2) + (y - cy).powi(2);
                d2 >= inner * inner && d2 <= outer * outer
            })
            .collect();
        let n = annulus.len();
        let mut run = |absent: usize| {
            let mut bools = vec![true; 128 * 128];
            for &k in annulus.iter().take(absent) {
                bools[k] = false;
            }
            let mask = FiniteMask::from_bools(&bools, 128, 128);
            let side = ApertureSide {
                plane: &plane,
                present: &mask,
            };
            circle_flux(&side, cx, cy, 12.0, inner, outer, &mut scratch)
        };
        assert_eq!(run((n as f64 * 0.51).ceil() as usize), None, "51 % absent");
        assert!(
            run((n as f64 * 0.49).floor() as usize).is_some(),
            "49 % absent"
        );
    }

    #[test]
    fn a_non_positive_flux_is_invalid() {
        let mut plane = vec![SKY; 128 * 128];
        plane[64 * 128 + 64] = -0.5;
        let mask = FiniteMask::all(128, 128);
        let side = ApertureSide {
            plane: &plane,
            present: &mask,
        };
        let mut scratch = Vec::new();
        assert_eq!(
            circle_flux(&side, 64.0, 64.0, 12.0, 24.0, 32.0, &mut scratch),
            None
        );
    }

    fn pair_planes(
        profile_r: Profile,
        profile_t: Profile,
        ratio: f64,
    ) -> (Vec<f32>, Vec<f32>, Vec<(f64, f64, f64)>) {
        let stars_r = lattice();
        let stars_t: Vec<_> = stars_r.iter().map(|&(x, y, f)| (x, y, f / ratio)).collect();
        (
            render(768, 512, &stars_r, profile_r),
            render(768, 512, &stars_t, profile_t),
            stars_r,
        )
    }

    /// The point of F1: one circle sized to the broader star measures both
    /// frames alike, whatever the seeing. Reference FWHM 3 px, target FWHM
    /// 4.5 px, target = 0.8 × reference: z = 1.25 on every star.
    #[test]
    fn a_common_circle_reads_the_same_ratio_whatever_the_seeing() {
        let (pr, pt, stars) = pair_planes(
            Profile::Gaussian { fwhm: 3.0 },
            Profile::Gaussian { fwhm: 4.5 },
            1.25,
        );
        let mask = FiniteMask::all(768, 512);
        let (sr, st) = (
            ApertureSide {
                plane: &pr,
                present: &mask,
            },
            ApertureSide {
                plane: &pt,
                present: &mask,
            },
        );
        let f = 4.5;
        let mut scratch = Vec::new();
        let ratios: Vec<f64> = stars
            .iter()
            .map(|&(x, y, _)| {
                let a = circle_flux(
                    &sr,
                    x,
                    y,
                    3.0 * f,
                    SKY_INNER_FWHM * f,
                    SKY_OUTER_FWHM * f,
                    &mut scratch,
                )
                .unwrap();
                let b = circle_flux(
                    &st,
                    x,
                    y,
                    3.0 * f,
                    SKY_INNER_FWHM * f,
                    SKY_OUTER_FWHM * f,
                    &mut scratch,
                )
                .unwrap();
                a / b
            })
            .collect();
        assert_eq!(ratios.len(), 30);
        let z = median(ratios);
        assert!((z - 1.25).abs() < 1e-3, "z = {z}");
    }

    /// The growth-curve correction recovers the light beyond 2F: Moffat
    /// β = 4, FWHM 3 px vs 4.5 px, scale 1.25. Closed form (review C-2):
    /// z(2F) misses by 1.34 %; c · z(2F) = 1.25 × 1.00011. (At β = 2.5 the
    /// wings beyond 5F leave +0.37 %, so that profile cannot meet 0.3 %.)
    #[test]
    fn the_growth_correction_recovers_the_light_beyond_two_fwhm() {
        let (pr, pt, stars) = pair_planes(
            Profile::Moffat {
                fwhm: 3.0,
                beta: 4.0,
            },
            Profile::Moffat {
                fwhm: 4.5,
                beta: 4.0,
            },
            1.25,
        );
        let mask = FiniteMask::all(768, 512);
        let (sr, st) = (
            ApertureSide {
                plane: &pr,
                present: &mask,
            },
            ApertureSide {
                plane: &pt,
                present: &mask,
            },
        );
        let f = 4.5;
        let (lo, hi) = (SKY_INNER_FWHM * f, SKY_OUTER_FWHM * f);
        let mut scratch = Vec::new();
        let (mut z2, mut growth) = (Vec::new(), Vec::new());
        for &(x, y, _) in &stars {
            let r2 = circle_flux(&sr, x, y, 2.0 * f, lo, hi, &mut scratch).unwrap();
            let t2 = circle_flux(&st, x, y, 2.0 * f, lo, hi, &mut scratch).unwrap();
            let r5 = circle_flux(&sr, x, y, GROWTH_OUTER_FWHM * f, lo, hi, &mut scratch).unwrap();
            let t5 = circle_flux(&st, x, y, GROWTH_OUTER_FWHM * f, lo, hi, &mut scratch).unwrap();
            z2.push(r2 / t2);
            growth.push((r5 / r2) / (t5 / t2));
        }
        let z = median(z2);
        let c = median(growth);
        assert!(
            (z - 1.25).abs() / 1.25 > 0.01,
            "z(2F) = {z} should miss 1.25 by > 1 %"
        );
        let corrected = c * z;
        assert!(
            (corrected - 1.25).abs() / 1.25 < 0.003,
            "z(2F) = {z}, c = {c}, c·z = {corrected}"
        );
    }
}
