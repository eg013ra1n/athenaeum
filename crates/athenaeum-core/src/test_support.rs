//! Shared test fixtures.
//!
//! Path fixtures in particular. A test that seeds the catalog must seed what
//! production would have written, and production normalises at the write
//! boundary — `normalize_path(canonicalize(...))`, see the invariant in
//! `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md` §3.
//! `canonicalize` alone returns a `\\?\` verbatim path on Windows, which neither
//! compares equal to the stored spelling nor accepts a forward slash as a
//! separator.

use std::path::PathBuf;

/// A temporary directory, plus the spelling production would have stored for it.
///
/// Bind the `TempDir`: dropping it deletes the directory out from under the path.
///
/// ```ignore
/// let (_tmp, base) = crate::test_support::canonical_tempdir();
/// ```
pub(crate) fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    let base = crate::api::scan_roots::normalize_path(&canonical);
    (dir, base)
}

/// The spelling production would have stored for an existing directory.
///
/// For a test that already holds a `TempDir` — from `test_ctx()`, say — and must
/// not create a second one.
pub(crate) fn canonical_path(path: &std::path::Path) -> PathBuf {
    let canonical = path.canonicalize().expect("canonicalize path");
    crate::api::scan_roots::normalize_path(&canonical)
}

/// No fixture may seed a raw `canonicalize()` result.
///
/// This is a drift guard in the same genre as
/// `sync::receiver::tests::every_terminal_writer_announces_or_is_a_named_exemption`.
/// Without it the next fixture repeats the mistake and only a Windows runner
/// notices — which is exactly how 39 tests accumulated unmeasured.
///
/// Deliberately narrower than "no raw canonicalize anywhere": it walks only
/// `athenaeum-core/src` (not `tests/`, not the other crates in the workspace),
/// and it matches the `.canonicalize().unwrap()` / `.canonicalize().expect(`
/// method-call spelling only — the free-function form
/// `std::fs::canonicalize(p).unwrap()` is not caught and is already live at a
/// few sites. A reader should not take a green run of this test as proof the
/// whole tree is clean.
#[test]
fn fixtures_never_seed_a_raw_canonicalized_path() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(&src).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file defines the sanctioned helper, so it necessarily contains
        // the call the guard forbids everywhere else. Exact-path compare (not
        // a bare filename match) so a future src/**/test_support.rs elsewhere
        // in the tree is not silently exempt too.
        if path == src.join("test_support.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("read source");
        // Drop comment lines so prose about the rule never trips it, then drop
        // all whitespace so a multi-line builder chain cannot hide.
        let squashed: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        if squashed.contains("canonicalize().unwrap()")
            || squashed.contains("canonicalize().expect(")
        {
            offenders.push(
                path.strip_prefix(&src)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "these files seed a raw `canonicalize()` result: {offenders:?}\n\
         On Windows that is a `\\\\?\\` verbatim path. It never compares equal to \
         the normalised spelling production stores, and it takes a forward slash \
         as a filename character rather than a separator (error 123, \
         InvalidFilename). Use `crate::test_support::canonical_tempdir()` when the \
         test needs a new temporary directory, or `crate::test_support::canonical_path(path)` \
         when it already holds one (from `test_ctx()`, say) — don't create a second \
         `TempDir` just to canonicalize a path you already have."
    );
}

/// Row-major float plane with Gaussian stars `(x, y, amplitude)` of common
/// `sigma` on a flat `background`. Pixel centres at integer coordinates.
pub(crate) fn gaussian_field(
    w: usize,
    h: usize,
    stars: &[(f64, f64, f64)],
    sigma: f64,
    background: f32,
) -> Vec<f32> {
    let mut data = vec![background; w * h];
    let s2 = 2.0 * sigma * sigma;
    for &(sx, sy, amp) in stars {
        let r = (5.0 * sigma).ceil() as i64;
        let (cx, cy) = (sx.round() as i64, sy.round() as i64);
        for y in (cy - r).max(0)..=(cy + r).min(h as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i64 - 1) {
                let d2 = (x as f64 - sx).powi(2) + (y as f64 - sy).powi(2);
                data[y as usize * w + x as usize] += (amp * (-d2 / s2).exp()) as f32;
            }
        }
    }
    data
}

/// The stacking Measure stage's 150-star fixture (moved here from
/// `stacking::measure`'s tests in Plan A Task 2, ruling F44, so the
/// `render`-only star-measurement tests build it without `stacking`): 150
/// Gaussian stars (σ 1.8 px) on a jittered 15×10 grid of a 768×512 plane,
/// amplitudes `(0.1..0.3)·amp_scale`, background 0.08, Gaussian noise `noise`
/// seeded by `seed + 100`. Byte-identical to the field the measure pins were
/// recorded on: the σ takes the same FWHM round trip
/// `stacking::test_fixtures::synthetic_star_field` gives it.
pub(crate) fn fixture_field(seed: u64, amp_scale: f64, noise: f32) -> (Vec<f32>, usize, usize) {
    const GAUSSIAN_FWHM_FACTOR: f64 = 2.354_820_045_030_949_3;
    let (w, h) = (768, 512);
    let mut rng = crate::geometry::ransac::SplitMix64(seed);
    let mut stars = Vec::new();
    for j in 0..10 {
        for i in 0..15 {
            let x = 40.0 + i as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
            let y = 40.0 + j as f64 * 48.0 + (rng.next_f64() - 0.5) * 16.0;
            stars.push((x, y, (0.1 + 0.2 * rng.next_f64()) * amp_scale));
        }
    }
    let sigma = 1.8;
    let fwhm = GAUSSIAN_FWHM_FACTOR * sigma;
    let sigma = fwhm / GAUSSIAN_FWHM_FACTOR;
    let mut data = gaussian_field(w, h, &stars, sigma, 0.08);
    if noise > 0.0 {
        add_noise(&mut data, noise, seed + 100);
    }
    (data, w, h)
}

/// The `star_probe` example's NaN-border fixture (Plan A Task 6a, ruling F38), so a lib test reaches it: background
/// 0.05; with `stars`, 40 Gaussian stars (σ 1.5 px, FWHM 3.5, amplitude 0.4) on a 25-px grid from (30, 30); a
/// deterministic pseudo-noise of ±0.001 (the MRS noise estimate needs some); every pixel with `x < border` or
/// `y < border` NaN. Byte-identical to the example's own `gaussian_field(w, h, nan_border, stars)`, which an example
/// cannot import from here.
pub(crate) fn nan_border_field(w: usize, h: usize, border: usize, stars: bool) -> Vec<f32> {
    let mut d = vec![0.05f32; w * h];
    if stars {
        for k in 0..40 {
            let (cx, cy) = (30.0 + (k % 8) as f32 * 25.0, 30.0 + (k / 8) as f32 * 25.0);
            for y in 0..h {
                for x in 0..w {
                    let r2 = (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2);
                    d[y * w + x] += 0.4 * (-r2 / (2.0 * 1.5f32.powi(2))).exp();
                }
            }
        }
    }
    for (i, v) in d.iter_mut().enumerate() {
        *v += 0.002 * (((i as u64).wrapping_mul(2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
    }
    for y in 0..h {
        for x in 0..w {
            if x < border || y < border {
                d[y * w + x] = f32::NAN
            }
        }
    }
    d
}

/// Background-subtracted intensity-weighted centroid in a `(2r+1)²` box
/// around `(x0, y0)`. NaN samples are skipped.
pub(crate) fn centroid(
    data: &[f32],
    w: usize,
    x0: f64,
    y0: f64,
    r: usize,
    background: f32,
) -> (f64, f64) {
    let (cx, cy) = (x0.round() as i64, y0.round() as i64);
    let h = data.len() / w;
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in (cy - r as i64).max(0)..=(cy + r as i64).min(h as i64 - 1) {
        for x in (cx - r as i64).max(0)..=(cx + r as i64).min(w as i64 - 1) {
            let v = data[y as usize * w + x as usize];
            if !v.is_finite() {
                continue;
            }
            let v = (v - background).max(0.0) as f64;
            sx += v * x as f64;
            sy += v * y as f64;
            sw += v;
        }
    }
    (sx / sw, sy / sw)
}

/// Background-subtracted flux in the same box (NaN skipped).
pub(crate) fn flux(data: &[f32], w: usize, x0: f64, y0: f64, r: usize, background: f32) -> f64 {
    let (cx, cy) = (x0.round() as i64, y0.round() as i64);
    let h = data.len() / w;
    let mut sum = 0.0f64;
    for y in (cy - r as i64).max(0)..=(cy + r as i64).min(h as i64 - 1) {
        for x in (cx - r as i64).max(0)..=(cx + r as i64).min(w as i64 - 1) {
            let v = data[y as usize * w + x as usize];
            if v.is_finite() {
                sum += (v - background) as f64;
            }
        }
    }
    sum
}

/// Adds zero-mean Gaussian noise of `sigma` in place (Box–Muller over a
/// SplitMix64 stream seeded by `seed`), so a fixture's noise is reproducible.
pub(crate) fn add_noise(data: &mut [f32], sigma: f32, seed: u64) {
    let mut rng = crate::geometry::ransac::SplitMix64(seed);
    let mut i = 0;
    while i < data.len() {
        let u1 = rng.next_f64().max(1e-12);
        let u2 = rng.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        data[i] += (r * c) as f32 * sigma;
        if i + 1 < data.len() {
            data[i + 1] += (r * s) as f32 * sigma;
        }
        i += 2;
    }
}

/// One elliptical Moffat star for [`moffat_field`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct MoffatStar {
    pub x: f64,
    pub y: f64,
    pub amp: f64,
    pub alpha_x: f64,
    pub alpha_y: f64,
    pub theta: f64,
}

/// Row-major float plane of elliptical Moffat stars
/// `A·(1 + (u/αx)² + (v/αy)²)^(−β)` with `u = dx·cosθ + dy·sinθ`,
/// `v = −dx·sinθ + dy·cosθ`, rendered out to `12·max(αx, αy)` on a flat
/// `background`. Pixel centres at integer coordinates. The total flux of
/// one star is `π·A·αx·αy/(β − 1)`; the flux inside its FWTM ellipse is
/// that times `1 − 10^{1/β − 1}`.
pub(crate) fn moffat_field(
    w: usize,
    h: usize,
    stars: &[MoffatStar],
    beta: f64,
    background: f32,
) -> Vec<f32> {
    let mut data = vec![background; w * h];
    for s in stars {
        let r = (12.0 * s.alpha_x.max(s.alpha_y)).ceil() as i64;
        let (cx, cy) = (s.x.round() as i64, s.y.round() as i64);
        let (st, ct) = s.theta.sin_cos();
        for y in (cy - r).max(0)..=(cy + r).min(h as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i64 - 1) {
                let (dx, dy) = (x as f64 - s.x, y as f64 - s.y);
                let u = dx * ct + dy * st;
                let v = -dx * st + dy * ct;
                let q = (u / s.alpha_x).powi(2) + (v / s.alpha_y).powi(2);
                data[y as usize * w + x as usize] += (s.amp * (1.0 + q).powf(-beta)) as f32;
            }
        }
    }
    data
}

/// The profile of one [`integrated_field`]: a round Moffat `(1 + r²/α²)^(−β)` or a round Gaussian, both of FWHM
/// `fwhm` px and peak 1.
#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug)]
pub(crate) enum IntegratedProfile {
    Moffat { fwhm: f64, beta: f64 },
    Gaussian { fwhm: f64 },
}

#[cfg(feature = "render")]
impl IntegratedProfile {
    /// The profile as a function of the squared distance from its centre, peak 1.
    fn eval(self) -> impl Fn(f64) -> f64 + Sync {
        // (1/α², −β) for a Moffat; (1/(2σ²), ∞) marks a Gaussian.
        let (scale, beta) = match self {
            IntegratedProfile::Moffat { fwhm, beta } => {
                let alpha = fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
                (1.0 / (alpha * alpha), -beta)
            }
            IntegratedProfile::Gaussian { fwhm } => {
                let sigma = fwhm / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
                (1.0 / (2.0 * sigma * sigma), f64::INFINITY)
            }
        };
        move |r2: f64| {
            if beta.is_infinite() {
                (-r2 * scale).exp()
            } else {
                (1.0 + r2 * scale).powf(beta)
            }
        }
    }

    fn fwhm(self) -> f64 {
        match self {
            IntegratedProfile::Moffat { fwhm, .. } | IntegratedProfile::Gaussian { fwhm } => fwhm,
        }
    }
}

/// Row-major float plane of round stars `(x, y, amplitude)` of one `profile` on a flat `background`, every pixel the
/// light it COLLECTS: the mean of the profile over `sub × sub` midpoint sub-samples of the pixel (offsets
/// `(i + 0.5)/sub − 0.5` px), where [`moffat_field`] and [`gaussian_field`] sample the profile at the pixel centre.
/// Each star is rendered out to `max(6·FWHM, 6) + 2` px — beyond the largest region the fitter grows (6·`F_start`,
/// at least 6 px) — and its wings past that are dropped. Pixel centres at integer coordinates; rows in parallel.
#[cfg(feature = "render")]
pub(crate) fn integrated_field(
    w: usize,
    h: usize,
    stars: &[(f64, f64, f64)],
    profile: IntegratedProfile,
    sub: usize,
    background: f32,
) -> Vec<f32> {
    use rayon::prelude::*;
    let reach = (6.0 * profile.fwhm()).max(6.0) + 2.0;
    let at = profile.eval();
    let offsets: Vec<f64> = (0..sub)
        .map(|i| (i as f64 + 0.5) / sub as f64 - 0.5)
        .collect();
    let inv = 1.0 / (sub * sub) as f64;
    let mut data = vec![background; w * h];
    data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let yf = y as f64;
        for &(sx, sy, amp) in stars {
            if (yf - sy).abs() > reach {
                continue;
            }
            let x0 = ((sx - reach).ceil().max(0.0)) as usize;
            let x1 = ((sx + reach).floor().min(w as f64 - 1.0)) as usize;
            for (x, v) in row.iter_mut().enumerate().take(x1 + 1).skip(x0) {
                let xf = x as f64;
                let mut sum = 0.0;
                for &oy in &offsets {
                    let dy = yf + oy - sy;
                    for &ox in &offsets {
                        let dx = xf + ox - sx;
                        sum += at(dx * dx + dy * dy);
                    }
                }
                *v += (amp * sum * inv) as f32;
            }
        }
    });
    data
}

/// A plausible accepted fit at `(x, y)` with `amplitude` (Plan A Task 4): the fixture the `StarFit` literal sites can
/// call instead of spelling every field (ruling F36). Every other field is a round, well-sampled star — FWHM 3 px,
/// FWTM 6 px, β 4, a small residual, a positive signal over its FWTM-ellipse area — with no flags; `peak5` is
/// unmeasured (NaN), as on a fit decoded from a `fits` artifact.
#[cfg(feature = "render")]
impl crate::star_measure::StarFit {
    pub(crate) fn test_at(x: f64, y: f64, amplitude: f64) -> Self {
        let fwtm = 6.0;
        crate::star_measure::StarFit {
            x,
            y,
            background: 100.0,
            amplitude,
            fwhm_x: 3.0,
            fwhm_y: 3.0,
            fwtm_x: fwtm,
            fwtm_y: fwtm,
            theta: 0.0,
            beta: 4.0,
            residual: 0.02,
            signal: 8.0 * amplitude,
            area: std::f64::consts::PI * 0.25 * fwtm * fwtm,
            peak5: f64::NAN,
            sky_ring: f64::NAN,
            sky_ring_sigma: f64::NAN,
            sky_ring_n: 0,
            snr: f64::NAN,
            hfr: f64::NAN,
            flags: crate::star_measure::StarFlags::default(),
        }
    }
}

/// One star of [`spread_moffat_field`]: its centre (pixel centres at integer coordinates), its peak and its FWHM, px.
#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpreadStar {
    pub x: f64,
    pub y: f64,
    pub amp: f64,
    pub fwhm: f64,
}

/// A field of round Moffat stars of one `beta` whose widths SPREAD (Task 10b A2b's truth tests): `n` stars at seeded
/// random positions at least `min_sep` px from each other and `margin` px from every edge, each FWHM drawn from
/// N(`fwhm`, `fwhm_sd`) cut at ±3σ, each peak `sigma` × an SNR log-uniform in `snr` (`sigma` is the noise the caller
/// adds). Every pixel is the light it collects — the mean of the profile over 5 × 5 midpoint sub-samples, as
/// [`integrated_field`] — each star rendered out to `max(6·FWHM, 6) + 2` px, on a flat `background`. Returns the
/// noiseless plane and the stars.
#[cfg(feature = "render")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn spread_moffat_field(
    (w, h): (usize, usize),
    n: usize,
    (fwhm, fwhm_sd): (f64, f64),
    beta: f64,
    sigma: f64,
    snr: (f64, f64),
    (min_sep, margin): (f64, f64),
    background: f32,
    seed: u64,
) -> (Vec<f32>, Vec<SpreadStar>) {
    let mut rng = crate::geometry::ransac::SplitMix64(seed);
    let mut stars: Vec<SpreadStar> = Vec::with_capacity(n);
    let mut attempts = 0usize;
    while stars.len() < n {
        attempts += 1;
        assert!(
            attempts < 1000 * n,
            "{n} stars do not fit {min_sep} px apart"
        );
        let x = margin + rng.next_f64() * (w as f64 - 2.0 * margin);
        let y = margin + rng.next_f64() * (h as f64 - 2.0 * margin);
        if stars.iter().any(|s| (s.x - x).hypot(s.y - y) < min_sep) {
            continue;
        }
        let z = loop {
            let u1 = rng.next_f64().max(1e-12);
            let u2 = rng.next_f64();
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            if z.abs() <= 3.0 {
                break z;
            }
        };
        let ln_snr = snr.0.ln() + rng.next_f64() * (snr.1 / snr.0).ln();
        stars.push(SpreadStar {
            x,
            y,
            amp: sigma * ln_snr.exp(),
            fwhm: fwhm + fwhm_sd * z,
        });
    }
    const SUB: usize = 5;
    let offsets: Vec<f64> = (0..SUB)
        .map(|i| (i as f64 + 0.5) / SUB as f64 - 0.5)
        .collect();
    let inv = 1.0 / (SUB * SUB) as f64;
    let mut data = vec![background; w * h];
    for s in &stars {
        let alpha = s.fwhm / (2.0 * (2f64.powf(1.0 / beta) - 1.0).sqrt());
        let reach = (6.0 * s.fwhm).max(6.0) + 2.0;
        let (x0, x1) = (
            (s.x - reach).ceil().max(0.0) as usize,
            (s.x + reach).floor().min(w as f64 - 1.0) as usize,
        );
        let (y0, y1) = (
            (s.y - reach).ceil().max(0.0) as usize,
            (s.y + reach).floor().min(h as f64 - 1.0) as usize,
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let mut sum = 0.0;
                for &oy in &offsets {
                    let dy = y as f64 + oy - s.y;
                    for &ox in &offsets {
                        let dx = x as f64 + ox - s.x;
                        sum += (1.0 + (dx * dx + dy * dy) / (alpha * alpha)).powf(-beta);
                    }
                }
                data[y * w + x] += (s.amp * sum * inv) as f32;
            }
        }
    }
    (data, stars)
}

/// Task 10b A2b's width-spread field (truth tests 1 and 2, one field): 600 Moffat β 2.5 stars on a 2048² plane, FWHM
/// N(3.0, 0.24) px cut at ±3σ, peak SNR log-uniform in [20, 2000] over the noise σ = 0.0004 the caller adds, ≥ 30 px
/// apart and ≥ 40 px from every edge, on a background of 0.05. Returns the noiseless plane, the stars and σ.
#[cfg(feature = "render")]
pub(crate) fn width_spread_field() -> (Vec<f32>, Vec<SpreadStar>, f64) {
    width_spread_field_beta(2.5)
}

/// [`width_spread_field`] with the stars' Moffat β `beta` (truth test 2 sweeps the product's β range).
#[cfg(feature = "render")]
pub(crate) fn width_spread_field_beta(beta: f64) -> (Vec<f32>, Vec<SpreadStar>, f64) {
    const SIGMA: f64 = 0.0004;
    let (data, stars) = spread_moffat_field(
        (2048, 2048),
        600,
        (3.0, 0.24),
        beta,
        SIGMA,
        (20.0, 2000.0),
        (30.0, 40.0),
        0.05,
        101,
    );
    (data, stars, SIGMA)
}
