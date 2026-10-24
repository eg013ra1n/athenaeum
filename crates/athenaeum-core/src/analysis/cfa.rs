//! The CFA green-site path of the one star path (Plan C, Task C3, ruling PC-4).
//!
//! A Bayer mosaic is measured on its green sites. Detection runs on the 2x2 sum of the two green sites of each block
//! (G1 + G2, ruling R-PC-3: the fit reads green only, so R / B sky, chromatic focus and R / B hot pixels cannot move a
//! seed; the sum's noise is white); the seeds the matched filter flagged extended are dropped (as the mono path keeps
//! knots out of the statistic) and the rest are mapped to native pixels; the fit runs on the native mosaic with the
//! green-site mask; the plane's sky and noise are read from one green phase. `c` = 1 here (no qualifier, the B4 carry):
//! Analysis star fluxes on an OSC frame are uncorrected, a recorded limit.
//!
//! Positions are in FILE row order (the pattern addresses that order, see `input::RawPlanes`): the caller flips them
//! for display.

use std::sync::Arc;

use astroimage::BayerPattern;
use tracing::{error, warn};

use crate::star_measure::FieldWidth;
use crate::star_measure::{
    self, domain, seeds, MeasureOptions, NoiseSource, PixelNoise, PlaneMeasurement, Seed, SeedSpec,
    DEFAULT_THRESHOLD,
};

/// Native-pixel offset of a super-pixel seed. The module's pixel-centre convention puts a pixel's centre at its integer
/// coordinate, so super-pixel `i` (native columns `2i`, `2i + 1`) is centred at native `2i + 0.5`: a seed at `s` on the
/// super-pixel plane is at `2s + SUPERPIXEL_CENTRE_OFFSET` natively. Pinned at seed level by
/// `the_super_pixel_offset_is_half_a_pixel_at_seed_level` (the fit's position test cannot: the fitted centre is free).
pub const SUPERPIXEL_CENTRE_OFFSET: f64 = 0.5;

/// A super-pixel-plane seed in native pixels: positions x2 plus the centre offset; the peak is a 2-site sum, so one
/// site's is a half (the flux stays: the fit re-measures it).
pub fn native_seed(s: &Seed) -> Seed {
    Seed {
        x: 2.0 * s.x + SUPERPIXEL_CENTRE_OFFSET,
        y: 2.0 * s.y + SUPERPIXEL_CENTRE_OFFSET,
        peak: s.peak / 2.0,
        flux: s.flux,
    }
}

/// `true` where the mosaic pixel `(x, y)` is a green site. RGGB and BGGR carry green where `x + y` is odd, GRBG and
/// GBRG where it is even. `BayerPattern::None` is not a mosaic: every pixel is marked present (with one `warn!`).
pub fn green_mask(w: usize, h: usize, pattern: BayerPattern) -> Vec<bool> {
    let green_parity = match pattern {
        BayerPattern::Rggb | BayerPattern::Bggr => 1,
        BayerPattern::Grbg | BayerPattern::Gbrg => 0,
        BayerPattern::None => {
            warn!(
                width = w,
                height = h,
                "green mask: no Bayer pattern; every pixel kept"
            );
            return vec![true; w * h];
        }
    };
    (0..w * h)
        .map(|i| (i % w + i / w) % 2 == green_parity)
        .collect()
}

/// The 2x2 sum of the two green sites (G1 + G2) of each block, `(w / 2) x (h / 2)`; an odd last row or column is left
/// out. Their centroid is the block centre, so the plane is a super-pixel plane for the 0.5 offset. `BayerPattern::None`
/// is read as the GRBG / GBRG phase (the caller never passes it: [`green_mask`] warns for it).
pub fn green_sum(
    mosaic: &[f32],
    w: usize,
    h: usize,
    pattern: BayerPattern,
) -> (Vec<f32>, usize, usize) {
    // (dx, dy) of the two greens in a block: (1, 0) + (0, 1) for RGGB / BGGR, (0, 0) + (1, 1) for GRBG / GBRG.
    let ((ax, ay), (bx, by)) = match pattern {
        BayerPattern::Rggb | BayerPattern::Bggr => ((1, 0), (0, 1)),
        _ => ((0, 0), (1, 1)),
    };
    let (sw, sh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(sw * sh);
    for j in 0..sh {
        for i in 0..sw {
            out.push(mosaic[(2 * j + ay) * w + 2 * i + ax] + mosaic[(2 * j + by) * w + 2 * i + bx]);
        }
    }
    (out, sw, sh)
}

/// The green sites of one phase, `(w / 2) x (h / 2)`: the green pixel of each 2x2 block (the one at `(1, 0)` of an
/// RGGB / BGGR block, at `(0, 0)` of a GRBG / GBRG block). A subsample of the mosaic with white noise, read for the
/// plane's sky and noise.
pub fn green_phase(
    mosaic: &[f32],
    w: usize,
    h: usize,
    pattern: BayerPattern,
) -> (Vec<f32>, usize, usize) {
    let dx = match pattern {
        BayerPattern::Rggb | BayerPattern::Bggr => 1,
        _ => 0,
    };
    let (sw, sh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(sw * sh);
    for j in 0..sh {
        for i in 0..sw {
            out.push(mosaic[2 * j * w + 2 * i + dx]);
        }
    }
    (out, sw, sh)
}

/// Measure a Bayer mosaic on its green sites (PC-4); `base` carries the domain, PSF model, threshold
/// ([`SeedSpec::MatchedFilter`], else the default), detection cap, fit parameters and mode. `aperture_correction` of the
/// result is `None` (`c` = 1). Every seed the green-sum matched filter found is handed to the fit in native pixels with
/// its `extended` flag (R-PC-8); `PlaneMeasurement::detected` stays empty (given seeds), `seeds` / `extended_seeds`
/// count them.
///
/// # Panics
/// When `mosaic.len() != w * h` (the reader guarantees it).
pub fn measure_green_sites(
    mosaic: &[f32],
    w: usize,
    h: usize,
    pattern: BayerPattern,
    base: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> PlaneMeasurement {
    assert_eq!(mosaic.len(), w * h, "mosaic does not match its geometry");
    let threshold = match base.seeds {
        SeedSpec::MatchedFilter { threshold } => threshold,
        _ => DEFAULT_THRESHOLD,
    };

    // Sky and noise: one green phase, in the working units.
    let (phase, pw, ph) = green_phase(mosaic, w, h, pattern);
    let sky = domain::sky_level(&phase, pw, ph, &base.domain, base.scale_estimator);
    let phase_working = domain::to_working_units(&phase, &base.domain);
    let (noise, _) = domain::noise_model(&phase_working, pw, ph, pool);
    if noise.source == NoiseSource::None {
        error!(
            width = w,
            height = h,
            "cfa green sites: no noise estimate on the green phase"
        );
    }

    // Detection: the G1 + G2 sum, white noise by construction. The `fieldWidthAtFloor` warn fires in `detect`, in
    // super-pixel units; the handed width does not carry the flag.
    let (sum, sw, sh) = green_sum(mosaic, w, h, pattern);
    let sum_working = domain::to_working_units(&sum, &base.domain);
    let (sum_noise, _) = domain::noise_model(&sum_working, sw, sh, pool);
    let (found, width, report) = seeds::detect(
        &sum_working,
        sw,
        sh,
        &sum_noise,
        threshold,
        base.max_stars,
        None,
        pool,
        PixelNoise::White,
    );
    // Knots (ruling A3, amended by R-PC-8) ride with their flag: `SeedSpec::Given` + `given_extended` fits them like the
    // mono path and keeps them out of `c`'s qualifiers and the frame statistic. Then native pixels, `native_seed`.
    let native: Vec<Seed> = found.iter().map(native_seed).collect();
    let given_extended = report.extended;
    let field_width = FieldWidth {
        fwhm_px: 2.0 * width.fwhm_px,
        from_stars: width.from_stars,
    };

    let opts = MeasureOptions {
        domain: base.domain,
        psf_model: base.psf_model,
        max_stars: base.max_stars,
        seeds: SeedSpec::Given(native),
        fit: base.fit,
        noise: Some(noise),
        scale_estimator: base.scale_estimator,
        diagnostics: base.diagnostics,
        mask: Some(green_mask(w, h, pattern)),
        field_width: Some(field_width),
        colour: false,
        pixel_noise: PixelNoise::White,
        cfa_site_share: None,
        mode: base.mode,
        sky: Some(sky),
        given_extended,
    };
    let pm = star_measure::measure_plane(mosaic, w, h, &opts, pool);
    pm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::star_measure::{Domain, Mode, PsfModel};
    use crate::test_support::{add_noise, moffat_field, MoffatStar};

    const LAYOUTS: [BayerPattern; 4] = [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ];

    /// 'R', 'G' or 'B' at `(x, y)`.
    fn site(p: BayerPattern, x: usize, y: usize) -> char {
        let (ex, ey) = (x % 2 == 0, y % 2 == 0);
        match (p, ex, ey) {
            (BayerPattern::Rggb, true, true) | (BayerPattern::Grbg, false, true) => 'R',
            (BayerPattern::Rggb, false, false) | (BayerPattern::Grbg, true, false) => 'B',
            (BayerPattern::Bggr, true, true) | (BayerPattern::Gbrg, false, true) => 'B',
            (BayerPattern::Bggr, false, false) | (BayerPattern::Gbrg, true, false) => 'R',
            _ => 'G',
        }
    }

    fn star(x: f64, y: f64, amp: f64, fwhm: f64) -> MoffatStar {
        // Moffat beta 4: FWHM = 2 alpha sqrt(2^(1/4) - 1).
        let alpha = fwhm / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
        MoffatStar {
            x,
            y,
            amp,
            alpha_x: alpha,
            alpha_y: alpha,
            theta: 0.0,
        }
    }

    /// A 256x256-style mosaic: scene stars (flux 1.0 on G) x site gains + sky per site + Gaussian noise.
    fn mosaic(p: BayerPattern, w: usize, h: usize, stars: &[MoffatStar], seed: u64) -> Vec<f32> {
        let scene = moffat_field(w, h, stars, 4.0, 0.0);
        let mut m: Vec<f32> = (0..w * h)
            .map(|i| {
                let (gain, sky) = match site(p, i % w, i / w) {
                    'R' => (0.5, 900.0),
                    'B' => (0.4, 800.0),
                    _ => (1.0, 1000.0),
                };
                scene[i] * gain + sky
            })
            .collect();
        add_noise(&mut m, 6.0, seed);
        m
    }

    fn field_stars(bright: (f64, f64), w: usize, h: usize, seed: u64) -> Vec<MoffatStar> {
        let mut rng = crate::geometry::ransac::SplitMix64(seed);
        let mut stars = vec![star(bright.0, bright.1, 3000.0, 4.0)];
        while stars.len() < 41 {
            let x = 16.0 + rng.next_f64() * (w as f64 - 32.0);
            let y = 16.0 + rng.next_f64() * (h as f64 - 32.0);
            if stars.iter().any(|s| (s.x - x).hypot(s.y - y) < 20.0) {
                continue;
            }
            stars.push(star(
                x,
                y,
                250.0 + rng.next_f64() * 500.0,
                3.6 + rng.next_f64() * 0.8,
            ));
        }
        stars
    }

    fn base() -> MeasureOptions {
        MeasureOptions {
            domain: Domain::RawAdu {
                clip: 65535.0,
                full_scale: 65535.0,
                gain_e_per_adu: None,
            },
            psf_model: PsfModel::Moffat4,
            seeds: SeedSpec::MatchedFilter {
                threshold: DEFAULT_THRESHOLD,
            },
            mode: Mode::Budget { budget: 500 },
            ..Default::default()
        }
    }

    #[test]
    fn green_mask_marks_the_green_sites_of_each_layout() {
        let at = |m: &[bool], x: usize, y: usize| m[y * 4 + x];
        for (p, g00) in [
            (BayerPattern::Rggb, false),
            (BayerPattern::Bggr, false),
            (BayerPattern::Grbg, true),
            (BayerPattern::Gbrg, true),
        ] {
            let m = green_mask(4, 6, p);
            assert_eq!(m.len(), 24);
            assert_eq!(at(&m, 0, 0), g00, "{p:?} (0,0)");
            assert_eq!(at(&m, 1, 0), !g00, "{p:?} (1,0)");
            assert_eq!(at(&m, 0, 1), !g00, "{p:?} (0,1)");
            assert_eq!(at(&m, 1, 1), g00, "{p:?} (1,1)");
            assert_eq!(m.iter().filter(|&&b| b).count(), 12);
            for (i, &g) in m.iter().enumerate() {
                assert_eq!(
                    g,
                    site(p, i % 4, i / 4) == 'G',
                    "{p:?} site ({}, {})",
                    i % 4,
                    i / 4
                );
            }
        }
    }

    #[test]
    fn a_star_on_a_mosaic_is_fitted_at_its_native_position() {
        let truth = (100.3, 80.7);
        for p in LAYOUTS {
            let stars = field_stars(truth, 256, 256, 11);
            let m = mosaic(p, 256, 256, &stars, 5);
            let pm = measure_green_sites(&m, 256, 256, p, &base(), None);
            let best = pm
                .stars
                .iter()
                .min_by(|a, b| {
                    (a.x - truth.0)
                        .hypot(a.y - truth.1)
                        .total_cmp(&(b.x - truth.0).hypot(b.y - truth.1))
                })
                .unwrap_or_else(|| panic!("{p:?}: no accepted fit; refused {:?}", pm.refused));
            let d = (best.x - truth.0).hypot(best.y - truth.1);
            assert!(
                d < 0.1,
                "{p:?}: fit at ({}, {}) is {d} px from the star",
                best.x,
                best.y
            );
            let fwhm = 0.5 * (best.fwhm_x + best.fwhm_y);
            assert!((fwhm / 4.0 - 1.0).abs() < 0.02, "{p:?}: FWHM {fwhm}");
        }
    }

    #[test]
    fn the_cfa_sky_and_noise_read_only_green_sites() {
        let p = BayerPattern::Rggb;
        let stars = field_stars((100.3, 80.7), 256, 256, 11);
        let a = mosaic(p, 256, 256, &stars, 5);
        let mut b = a.clone();
        for (i, v) in b.iter_mut().enumerate() {
            if site(p, i % 256, i / 256) != 'G' {
                *v += 5000.0;
            }
        }
        let (ma, mb) = (
            measure_green_sites(&a, 256, 256, p, &base(), None),
            measure_green_sites(&b, 256, 256, p, &base(), None),
        );
        assert_eq!(ma.sky.median.to_bits(), mb.sky.median.to_bits());
        assert_eq!(ma.noise.sigma.to_bits(), mb.noise.sigma.to_bits());
        assert!(!ma.stars.is_empty());
        // Seeds come from the two green sites only (R-PC-3), so R / B never reach a seed, the fit, the sky or the noise:
        // the accepted fits are identical bit for bit.
        assert_eq!(ma.seeds, mb.seeds);
        assert_eq!(ma.field_width, mb.field_width);
        assert_eq!(ma.stars, mb.stars);
    }

    #[test]
    fn a_hot_red_site_is_not_a_star() {
        let p = BayerPattern::Rggb;
        let mut m = mosaic(p, 128, 128, &[], 3);
        let (hx, hy) = (60, 40); // both even: an R site in RGGB
        assert_eq!(site(p, hx, hy), 'R');
        m[hy * 128 + hx] = 60000.0;
        let pm = measure_green_sites(&m, 128, 128, p, &base(), None);
        assert!(
            !pm.stars
                .iter()
                .any(|s| (s.x - hx as f64).hypot(s.y - hy as f64) < 3.0),
            "accepted fits near the hot red site: {:?}",
            pm.stars.iter().map(|s| (s.x, s.y)).collect::<Vec<_>>()
        );
        // A pin: the green-only seeds never see the site, so no seed was even attempted there (not merely refused
        // `NarrowerThanPixel`).
        assert!(
            !native_seeds(&m, 128, 128, p)
                .0
                .iter()
                .any(|s| (s.x - hx as f64).hypot(s.y - hy as f64) < 3.0),
            "a seed at the hot red site"
        );
    }

    #[test]
    fn an_odd_mosaic_leaves_its_last_row_and_column_out_of_detection_only() {
        let (w, h) = (257, 255);
        let (sum, sw, sh) = green_sum(&vec![1.0; w * h], w, h, BayerPattern::Rggb);
        assert_eq!((sw, sh, sum.len()), (128, 127, 128 * 127));
        assert!(sum.iter().all(|&v| v == 2.0));
        // A pin of the stride (`w`, not `2 * sw`) and of the unshifted grid: a wrong stride would shear every row and
        // shift this star's seed. The star sits ~10 px from the odd column (the module's border rule), so the test does
        // not use the last column itself; the fit lands at its native position.
        let p = BayerPattern::Rggb;
        let truth = (246.2, 100.4);
        let mut stars = field_stars((100.3, 80.7), w, h, 11);
        stars.push(star(truth.0, truth.1, 3000.0, 4.0));
        let m = mosaic(p, w, h, &stars, 5);
        let pm = measure_green_sites(&m, w, h, p, &base(), None);
        assert!(
            pm.stars
                .iter()
                .any(|s| (s.x - truth.0).hypot(s.y - truth.1) < 0.3),
            "no fit at the last-column star; refused {:?}",
            pm.refused
        );
    }

    /// The detection source of `measure_green_sites`: seeds on the G1 + G2 sum, in native pixels.
    fn native_seeds(m: &[f32], w: usize, h: usize, p: BayerPattern) -> (Vec<Seed>, Vec<bool>) {
        let b = base();
        let (sum, sw, sh) = green_sum(m, w, h, p);
        let working = domain::to_working_units(&sum, &b.domain);
        let (noise, _) = domain::noise_model(&working, sw, sh, None);
        let (found, _, report) = seeds::detect(
            &working,
            sw,
            sh,
            &noise,
            DEFAULT_THRESHOLD,
            b.max_stars,
            None,
            None,
            PixelNoise::White,
        );
        (found.iter().map(native_seed).collect(), report.extended)
    }

    /// I2: the 0.5 offset pinned where it is applied. A star at a block centre (2i + 0.5) or at a block corner
    /// (2i + 1.5) maps to within 0.15 px of its true position; an offset of 0.0 or 1.0 misses by 0.5 px per axis.
    #[test]
    fn the_super_pixel_offset_is_half_a_pixel_at_seed_level() {
        let truths = [
            (40.5, 40.5), // block centres
            (90.5, 50.5),
            (50.5, 90.5),
            (91.5, 91.5), // block corners
            (41.5, 71.5),
            (71.5, 41.5),
        ];
        let stars: Vec<MoffatStar> = truths
            .iter()
            .map(|&(x, y)| star(x, y, 3000.0, 4.0))
            .collect();
        for p in LAYOUTS {
            let m = mosaic(p, 128, 128, &stars, 5);
            let (seeds, _) = native_seeds(&m, 128, 128, p);
            for &(tx, ty) in &truths {
                let near = seeds
                    .iter()
                    .map(|s| (s.x - tx).abs().max((s.y - ty).abs()))
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    near < 0.15,
                    "{p:?}: nearest seed to ({tx}, {ty}) is {near} px off"
                );
            }
        }
    }

    /// I1 / R-PC-8: a knot (an extended blob, ruling A3) is flagged by the matched filter on the green sum, HANDED to the
    /// fit flagged and fitted (as on the mono path), and never enters the frame statistic.
    #[test]
    fn a_knot_does_not_enter_the_cfa_statistic() {
        use crate::test_support::gaussian_field;
        let p = BayerPattern::Rggb;
        let (w, h) = (400, 300);
        let mut stars = Vec::new();
        for j in 0..3 {
            for i in 0..5 {
                stars.push(star(
                    50.0 + 80.0 * f64::from(i),
                    50.0 + 80.0 * f64::from(j) + 3.3,
                    700.0,
                    4.0,
                ));
            }
        }
        let plain = mosaic(p, w, h, &stars, 5);
        // A Gaussian blob of FWHM 12 px (3 F), ~40 px from its neighbours.
        let blob = (90.3, 94.8);
        let g = gaussian_field(w, h, &[(blob.0, blob.1, 300.0)], 12.0 / 2.354_820_045, 0.0);
        let mut knotted = plain.clone();
        for (i, v) in knotted.iter_mut().enumerate() {
            if site(p, i % w, i / w) == 'G' {
                *v += g[i];
            }
        }
        let near = |x: f64, y: f64| (x - blob.0).hypot(y - blob.1) < 8.0;
        // Premise: the matched filter seeds the blob and flags it.
        let (seeds, extended) = native_seeds(&knotted, w, h, p);
        let flagged = seeds
            .iter()
            .zip(&extended)
            .any(|(s, &e)| e && near(s.x, s.y));
        assert!(flagged, "premise: the blob is seeded and flagged extended");
        let (a, b) = (
            measure_green_sites(&plain, w, h, p, &base(), None),
            measure_green_sites(&knotted, w, h, p, &base(), None),
        );
        assert!(!a.stars.is_empty());
        assert!(b.detected.is_empty(), "given seeds leave `detected` empty");
        assert_eq!(b.seeds, seeds.len(), "every seed is handed to the fit");
        assert_eq!(b.extended_seeds, extended.iter().filter(|&&e| e).count());
        assert!(b.extended_seeds >= 1);
        let knot = b
            .stars
            .iter()
            .find(|s| near(s.x, s.y))
            .expect("the knot is handed and fitted");
        assert!(
            knot.flags.contains(crate::star_measure::StarFlags::EXTENDED),
            "{:?}",
            knot.flags
        );
        let (fa, fb) = (a.frame.fwhm_px.unwrap(), b.frame.fwhm_px.unwrap());
        assert!(
            (fb / fa - 1.0).abs() < 0.01,
            "frame FWHM {fb} vs {fa} without the knot"
        );
    }

    /// R-PC-8: a bright broad star (1.3 F at SNR 300, the injector's rung that the significance test flags) among F-width
    /// stars is fitted and flagged `extended`; the frame statistic is within 1 % of the run without it.
    #[test]
    fn a_bright_broad_star_on_a_mosaic_is_fitted_and_flagged() {
        let p = BayerPattern::Rggb;
        let (w, h) = (400, 300);
        let mut stars = Vec::new();
        for j in 0..3 {
            for i in 0..5 {
                stars.push(star(
                    50.0 + 80.0 * f64::from(i),
                    50.0 + 80.0 * f64::from(j) + 3.3,
                    700.0,
                    4.0,
                ));
            }
        }
        let plain = mosaic(p, w, h, &stars, 5);
        let broad = (90.3, 94.8);
        stars.push(star(broad.0, broad.1, 20000.0, 1.3 * 4.0));
        let with = mosaic(p, w, h, &stars, 5);
        let near = |x: f64, y: f64| (x - broad.0).hypot(y - broad.1) < 4.0;
        let (a, b) = (
            measure_green_sites(&plain, w, h, p, &base(), None),
            measure_green_sites(&with, w, h, p, &base(), None),
        );
        let star_b = b
            .stars
            .iter()
            .find(|s| near(s.x, s.y))
            .expect("the broad star is in pm.stars");
        assert!(
            star_b.flags.extended(),
            "premise: the significance test flags it ({:?}, extended seeds {})",
            star_b.flags,
            b.extended_seeds
        );
        let (fa, fb) = (a.frame.fwhm_px.unwrap(), b.frame.fwhm_px.unwrap());
        assert!(
            (fb / fa - 1.0).abs() < 0.01,
            "frame FWHM {fb} vs {fa} without it"
        );
    }

    #[test]
    fn a_cfa_plane_has_no_aperture_correction() {
        let p = BayerPattern::Rggb;
        let stars = field_stars((100.3, 80.7), 256, 256, 11);
        let m = mosaic(p, 256, 256, &stars, 5);
        let pm = measure_green_sites(&m, 256, 256, p, &base(), None);
        assert!(!pm.stars.is_empty());
        assert_eq!(pm.aperture_correction, None);
    }
}
