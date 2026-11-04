//! Tests that hold Athenaeum's own code to `star_measure`, which lives in rustafits (`astroimage::star_measure`)
//! since its port plan's phase 3. They were in the module's own tests while it was Athenaeum's; they need the
//! stacking adapter and the integration engine, so they stayed here when the module moved (bodies unchanged).

use crate::star_measure::{
    measure_plane, seeds, MeasureOptions, PixelNoise, SeedSpec, ADU_SCALE, DEFAULT_THRESHOLD,
};
use crate::test_support::fixture_field;

/// The module's working units of a calibrated plane (its tests' helper).
fn working(data: &[f32]) -> Vec<f32> {
    data.iter().map(|v| v * ADU_SCALE).collect()
}

#[cfg(feature = "solver")]
#[test]
fn module_measure_equals_the_adapter_bit_for_bit() {
    use crate::stacking::measure::{
        measure_plane_detailed, measure_plane_with_seeds, MeasureOptions as AdapterOptions,
        SeedOverride,
    };
    let (data, w, h) = fixture_field(2026_09_19, 1.0, 0.002); // 150 Gaussian stars, σ 1.8 px, bg 0.08, noise 0.002
    let adapter = measure_plane_with_seeds(
        &data,
        w,
        h,
        &AdapterOptions::default(),
        None,
        SeedOverride::Fast,
    );
    let (pm, seeds) = measure_plane_detailed(
        &data,
        w,
        h,
        &AdapterOptions::default(),
        None,
        SeedOverride::Fast,
    );
    let opts = MeasureOptions {
        seeds: SeedSpec::Given(seeds),
        ..Default::default()
    };
    let m = measure_plane(&data, w, h, &opts, None);
    assert_eq!(m.stars, pm.fits);
    assert_eq!(m.beta, adapter.channel.beta);
    assert_eq!(m.frame.fwhm_px.unwrap(), adapter.channel.fwhm_px);
    assert_eq!(m.frame.tflux / ADU_SCALE as f64, adapter.channel.tflux);
    assert_eq!(m.noise.sigma / ADU_SCALE as f64, adapter.channel.noise);
    assert_eq!(m.sky.median, adapter.channel.median);
    assert_eq!(m.sky.scale, adapter.channel.scale);
    assert_eq!(m.sky.noise_scale_high, adapter.channel.noise_scale_high);
}

/// Plan A Task 6b (Task 6a review M3, controller ruling): the adapter's matched-filter arm hands the detector's own
/// field width to the fit with its seeds, so the probe's measure path (`SeedOverride::MatchedFilter`, the seeds
/// handed to the module as `Given`) and the module's own `SeedSpec::MatchedFilter` start every star from the same
/// width and fit the same stars; `field_width_of` serves only seeds another detector found.
///
/// The plane makes the two widths differ: 120 Moffat-4 stars, 60 narrow (FWHM 3, amplitude 0.4) and 60 broad
/// (FWHM 7, amplitude 0.3), and a cap of 40 seeds. Task 10b A2 gave `field_width_of` the matched filter's own rule
/// (I-4: the mode of every readable read), so the widths now differ by the POOL, not the rule (ruling F14, the
/// premise rebuilt; through A1 it was the 100 brightest by peak against the first pass's 100 by flux): the matched
/// filter reads all 120 first-pass candidates (the narrow reads are the denser mode), while the 40 seeds the cap
/// keeps are the broad ones (a broad star's filtered flux reads ≈ 1.7× its amplitude), whose own mode is broad —
/// box radii ⌈2·F⌉ far apart.
#[cfg(feature = "solver")]
#[test]
fn adapter_matched_filter_seeds_fit_exactly_as_the_module_matched_filter() {
    use crate::stacking::measure::{
        measure_plane_diagnosed, MeasureOptions as AdapterOptions, SeedOverride,
    };
    use crate::test_support::{add_noise, moffat_field, MoffatStar};
    let (w, h) = (640usize, 480usize);
    let stars: Vec<MoffatStar> = (0..120)
        .map(|i| {
            let narrow = (i + i / 12) % 2 == 0;
            let alpha = if narrow { 3.449 } else { 8.048 };
            MoffatStar {
                x: 30.0 + (i % 12) as f64 * 50.0,
                y: 30.0 + (i / 12) as f64 * 45.0,
                amp: if narrow { 0.4 } else { 0.3 },
                alpha_x: alpha,
                alpha_y: alpha,
                theta: 0.0,
            }
        })
        .collect();
    let mut data = moffat_field(w, h, &stars, 4.0, 0.05);
    add_noise(&mut data, 0.002, 11);
    let (pm, seeds, _) = measure_plane_diagnosed(
        &data,
        w,
        h,
        &AdapterOptions {
            max_stars: 40,
            ..Default::default()
        },
        None,
        SeedOverride::MatchedFilter {
            threshold: DEFAULT_THRESHOLD,
        },
        false,
        false,
        PixelNoise::Unknown,
        None,
        None,
    );
    let m = measure_plane(
        &data,
        w,
        h,
        &MeasureOptions {
            seeds: SeedSpec::MatchedFilter {
                threshold: DEFAULT_THRESHOLD,
            },
            max_stars: 40,
            ..Default::default()
        },
        None,
    );
    assert_eq!(seeds.len(), m.seeds);
    // The premise: `field_width_of` the same (capped) seeds reads a different width.
    let given = seeds::field_width_of(&working(&data), w, h, &seeds, None);
    assert!(
        (given.fwhm_px - m.field_width.fwhm_px).abs() > 1.0,
        "given {given:?} vs the matched filter's {:?}",
        m.field_width
    );
    assert_eq!(pm.field_width, m.field_width, "the detector's own width");
    assert_eq!(pm.channel.beta, m.beta);
    // Debug strings (Task 10b, ruling F14 edit): a fit without a ring sky carries NaN, which `==` never matches.
    assert_eq!(
        format!("{:?}", pm.fits),
        format!("{:?}", m.stars),
        "the same fits"
    );
}

/// `star_measure::robust` against the integration engine's own copies.
mod robust {
    use crate::star_measure::robust::{erfc, erfinv, rcr};

    /// Gaussian samples of σ 1 (`star_measure::robust`'s test helper).
    fn gaussian(n: usize, seed: u64) -> Vec<f64> {
        let mut v = vec![0.0f32; n];
        crate::test_support::add_noise(&mut v, 1.0, seed);
        v.iter().map(|&x| x as f64).collect()
    }

    /// M4c Task 1: `integration::combine` carries a SECOND implementation of
    /// this module's `erfc`, `erfinv` and `rcr` — the integration tree is
    /// ungated and cannot import `stacking`, which is gated behind
    /// `render + solver`. Hold the two copies to the same answers so they
    /// cannot drift apart silently: the error functions bit-for-bit, and the
    /// RCR rejection over 50 random contaminated samples (reached through
    /// the public weighted combiner, whose survivor mask names exactly the
    /// samples the pixel-stack routine kept).
    #[test]
    fn integration_copies_agree_with_this_module() {
        use crate::integration::combine::{
            combine_pixel_weighted, mask_get, mask_words, IntegrationRecipe, Rejection,
        };
        use crate::integration::student_t;

        for i in -500..=500 {
            let x = i as f64 / 100.0;
            assert_eq!(
                erfc(x).to_bits(),
                student_t::erfc(x).to_bits(),
                "erfc({x}): {} vs {}",
                erfc(x),
                student_t::erfc(x)
            );
        }
        for i in -99..=99 {
            let x = i as f64 / 100.0;
            assert_eq!(
                erfinv(x).to_bits(),
                student_t::erfinv(x).to_bits(),
                "erfinv({x}): {} vs {}",
                erfinv(x),
                student_t::erfinv(x)
            );
        }

        let limit = 0.5;
        let mut scratch: Vec<f32> = Vec::new();
        for seed in 0..50u64 {
            // A clean Gaussian plus four distinct planted outliers.
            //
            // The two paths do NOT see the same numbers: `values` is
            // `Vec<f64>` and neither the Gaussian draws nor
            // `6.0 + 0.5·k + 0.01·seed` is f32-exact, so the pixel path's
            // `v as f32` rounds every sample. What makes the comparison
            // sound is that RCR's SURVIVOR SET is robust to that rounding
            // on these samples — the outliers sit whole units clear of the
            // clean population's spread and the survivors are nowhere near
            // the rejection boundary, so a ULP of f32 rounding cannot move
            // a sample across it. A disagreement here is therefore the
            // algorithm drifting apart, not the width; a fixture whose
            // samples crowded the boundary would need matched widths
            // instead.
            let mut values = gaussian(60, 1000 + seed);
            for k in 0..4 {
                values.push(6.0 + 0.5 * k as f64 + 0.01 * seed as f64);
            }
            let n = values.len();
            let mine = rcr(&values, limit);

            let out: Vec<f32> = values.iter().map(|&v| v as f32).collect();
            let mut work: Vec<(f32, u16)> = out
                .iter()
                .enumerate()
                .map(|(i, &v)| (v, i as u16))
                .collect();
            let weights = vec![1.0f32; n];
            let mut mask = vec![0u64; mask_words(n)];
            let (_, rejected, _) = combine_pixel_weighted(
                &mut work,
                &out,
                &weights,
                IntegrationRecipe::average(Rejection::Rcr { limit }),
                &mut mask,
                &mut scratch,
            );
            let theirs: Vec<bool> = (0..n).map(|i| mask_get(&mask, i)).collect();
            assert_eq!(rejected, mine.rejected, "seed {seed}: rejected count");
            assert_eq!(theirs, mine.kept, "seed {seed}: survivor set");
        }
    }
}
