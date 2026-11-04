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

// --- Plan C C1: the trailed / defocused plane fixtures, shared with C5 / C6 (moved from `star_measure/mod.rs`'s
// `trailed` tests, and the donut field the defocused verdict reads) ---

/// A horizontal trail `len` px long centred at (x, y): Gaussian points of FWHM 2.6 every 0.25 px (peak ≈ 11·amp).
#[cfg(feature = "render")]
pub(crate) fn trail_points(points: &mut Vec<(f64, f64, f64)>, x: f64, y: f64, len: f64, amp: f64) {
    let n = (len / 0.25) as usize + 1;
    points.extend((0..n).map(|k| (x - len / 2.0 + k as f64 * 0.25, y, amp)));
}

/// 20-px trails (amplitude 0.018 per point) at `trails` and round β-4 Moffat stars of FWHM 2.6 and peak 0.2 at
/// `round`, on sky 0.05 with noise 0.002; one seed (peak 0.2) at every position, the round ones first.
#[cfg(feature = "render")]
fn trail_fixture(
    w: usize,
    h: usize,
    round: &[(f64, f64)],
    trails: &[(f64, f64)],
    seed: u64,
) -> (Vec<f32>, Vec<crate::star_measure::Seed>) {
    let mut points = Vec::new();
    for &(x, y) in trails {
        trail_points(&mut points, x, y, 20.0, 0.018);
    }
    let mut data = gaussian_field(w, h, &points, 2.6 / 2.354_820_045, 0.05);
    let a = 2.6 / (2.0 * (2f64.powf(0.25) - 1.0).sqrt());
    let stars: Vec<MoffatStar> = round
        .iter()
        .map(|&(x, y)| MoffatStar {
            x,
            y,
            amp: 0.2,
            alpha_x: a,
            alpha_y: a,
            theta: 0.0,
        })
        .collect();
    for (d, m) in data.iter_mut().zip(moffat_field(w, h, &stars, 4.0, 0.0)) {
        *d += m;
    }
    add_noise(&mut data, 0.002, seed);
    let seeds = round
        .iter()
        .chain(trails)
        .map(|&(x, y)| crate::star_measure::Seed {
            x,
            y,
            peak: 0.2,
            flux: 1.0,
        })
        .collect();
    (data, seeds)
}

/// [`trail_fixture`] on a 40-px grid from (30, 30) with a 30-px margin on every side: 13 columns on a 540-px plane (the
/// previous version of these tests), as many as fit on a narrower one. Of the `round + trails` grid slots in order,
/// every `(round + trails) / round`-th holds a round star (`round` of them), the rest trails.
#[cfg(feature = "render")]
pub(crate) fn trail_plane_fixture(
    w: usize,
    h: usize,
    round: usize,
    trails: usize,
    seed: u64,
) -> (Vec<f32>, Vec<crate::star_measure::Seed>) {
    let n = round + trails;
    let cols = 13.min((w - 60) / 40 + 1);
    assert!(
        30 + 40 * ((n - 1) / cols) <= h - 30,
        "{n} slots of {cols} columns do not fit {w} x {h}"
    );
    let at = |i: usize| {
        (
            30.0 + 40.0 * (i % cols) as f64,
            30.0 + 40.0 * (i / cols) as f64,
        )
    };
    let stride = n / round;
    let is_round = |i: usize| i % stride == 0 && i / stride < round;
    let round_at: Vec<(f64, f64)> = (0..n).filter(|&i| is_round(i)).map(at).collect();
    let trails_at: Vec<(f64, f64)> = (0..n).filter(|&i| !is_round(i)).map(at).collect();
    assert_eq!((round_at.len(), trails_at.len()), (round, trails));
    trail_fixture(w, h, &round_at, &trails_at, seed)
}

/// A trailed plane for the frame verdict: 5 round stars among 120 trails of 20 px ([`trail_plane_fixture`]'s mix), so
/// `measure_plane` reads it `trailed` and keeps no star. Needs a plane of at least 540 x 420 (125 grid slots).
#[cfg(feature = "render")]
pub(crate) fn trailed_field(w: usize, h: usize, seed: u64) -> Vec<f32> {
    trail_plane_fixture(w, h, 5, 120, seed).0
}

/// A defocused plane for the frame verdict: annular stars (outer radius 6, obstruction 2.4, blurred by a Gaussian of
/// sigma 1.1) on a 35-px grid from (30, 30) with a 30-px margin, on sky 0.05 with noise 0.002. `measure_plane` reads
/// it `defocused` and keeps no star. Needs room for at least 8 donuts (a 330 x 100 plane holds exactly 8).
#[cfg(feature = "render")]
pub(crate) fn donut_field(w: usize, h: usize, seed: u64) -> Vec<f32> {
    let mut data = vec![0.05f32; w * h];
    let cols = (w.saturating_sub(60)) / 35 + 1;
    let rows = (h.saturating_sub(60)) / 35 + 1;
    assert!(
        cols * rows >= 8,
        "{w} x {h} holds {} donuts, 8 needed",
        cols * rows
    );
    for i in 0..cols * rows {
        add_donut_gain(&mut data, w, 30 + 35 * (i % cols), 30 + 35 * (i / cols), 1.0);
    }
    add_noise(&mut data, 0.002, seed);
    data
}

/// A deterministic 2000-point sunflower lattice over the annulus `r_in ≤ r ≤ r_out` about `(cx, cy)`: point `k`
/// at the radius enclosing the area fraction `(k + ½)/2000` of the annulus, turned by the golden angle per point,
/// so the points sample the annulus uniformly by area.
pub(crate) fn ring_samples(cx: f64, cy: f64, r_in: f64, r_out: f64) -> Vec<(f64, f64)> {
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

/// [`add_donut`] with its amplitude scaled by `gain`.
pub(crate) fn add_donut_gain(data: &mut [f32], w: usize, cx: usize, cy: usize, gain: f64) {
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
