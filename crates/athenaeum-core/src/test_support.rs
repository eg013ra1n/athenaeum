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
