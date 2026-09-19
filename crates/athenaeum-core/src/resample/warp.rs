//! Inverse-mapped gather: for every output pixel, map its centre back into
//! the source through the frame's inverse map and evaluate the separable
//! kernel there. Outside the source's sampled domain the output is NaN —
//! the integration engine already treats a non-finite sample as "missing"
//! with per-frame accounting (spec §3.4).

use rayon::prelude::*;

use super::kernels::{clamp_keys_1d, combine_lanczos_clamped, taps_for, Interpolation};
use crate::geometry::InverseMap;

/// A window of rows of a larger plane. `data` holds rows
/// `[y_offset, y_offset + height)` of an image `width × full_height`.
pub struct Plane<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
    pub y_offset: usize,
    pub full_height: usize,
}

impl<'a> Plane<'a> {
    pub fn full(data: &'a [f32], width: usize, height: usize) -> Plane<'a> {
        Plane {
            data,
            width,
            height,
            y_offset: 0,
            full_height: height,
        }
    }

    /// Sample with clamp-to-edge on both the frame and the window.
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        let xc = x.clamp(0, self.width as isize - 1) as usize;
        let yc = y.clamp(0, self.full_height as isize - 1) as usize;
        debug_assert!(
            yc >= self.y_offset && yc < self.y_offset + self.height,
            "tap row {yc} outside the source window [{}, {}) — the window was computed \
             with a smaller kernel radius than the one used to warp",
            self.y_offset,
            self.y_offset + self.height
        );
        let yl = yc.clamp(self.y_offset, self.y_offset + self.height - 1) - self.y_offset;
        self.data[yl * self.width + xc]
    }
}

/// True when every tap of an `n × n` footprint starting at
/// `(x_first, y_first)` (both already the generic path's `Taps::first`)
/// lies inside the plane WITHOUT triggering either of `Plane::at`'s two
/// clamps — the x-clamp to `[0, width)` and the y-clamp to the window
/// `[y_offset, y_offset + height)` (the window is always a subset of
/// `[0, full_height)`, so satisfying the window bound implies satisfying
/// the frame bound too; this is the one and only test to make). Only a
/// pixel this returns `true` for takes the interior fast path — everything
/// else, including any case this under-approximates, falls back to the
/// clamped generic path, so misclassifying a pixel as "border" can never
/// produce a wrong answer, only a slower one.
#[inline]
fn is_interior(src: &Plane, x_first: isize, y_first: isize, n: usize) -> bool {
    let n = n as isize;
    x_first >= 0
        && x_first + n <= src.width as isize
        && y_first >= src.y_offset as isize
        && y_first + n <= (src.y_offset + src.height) as isize
}

/// The `n` taps of one row starting at local column `x0` of the row at
/// local index `y_local`, as one hoisted slice-to-array conversion instead
/// of `n` separate clamped `Plane::at` calls — the caller has already
/// proven the range is in bounds via [`is_interior`].
#[inline(always)]
fn interior_row<const N: usize>(src: &Plane, y_local: usize, x0: usize) -> [f32; N] {
    let start = y_local * src.width + x0;
    src.data[start..start + N]
        .try_into()
        .expect("range proven in bounds by is_interior")
}

/// Interior BicubicSpline: the Keys-clamp path (math reference §5.4) with
/// four hoisted row slices instead of sixteen clamped `Plane::at` calls.
/// Same row-major accumulation, same `clamp_keys_1d` calls, as the generic
/// path — this only removes the per-tap bounds/clamp guards.
#[inline]
fn sample_interior_keys_clamp(
    src: &Plane,
    x0: usize,
    y0: usize,
    wx: &[f32; 4],
    wy: &[f32; 4],
    clamping: f32,
) -> f32 {
    let mut col = [0f32; 4];
    for (j, c) in col.iter_mut().enumerate() {
        let p: [f32; 4] = interior_row(src, y0 + j, x0);
        *c = clamp_keys_1d(wx, &p, clamping);
    }
    clamp_keys_1d(wy, &col, clamping)
}

/// Interior Lanczos3/Lanczos4: the positive/negative-lobe deringing path
/// (math reference §5.4), `N` (6 or 8) a compile-time trip count so both
/// loops are fully unrolled and every row is one hoisted slice instead of
/// `N` clamped `Plane::at` calls. Same row-major accumulation, same sign
/// split, same `combine_lanczos_clamped` calls, as the generic path.
#[inline]
fn sample_interior_lanczos_clamp<const N: usize>(
    src: &Plane,
    x0: usize,
    y0: usize,
    wx: &[f32; N],
    wy: &[f32; N],
    clamping: f32,
) -> f32 {
    let (mut cpos, mut cposw, mut cneg, mut cnegw) = (0f32, 0f32, 0f32, 0f32);
    for j in 0..N {
        let p: [f32; N] = interior_row(src, y0 + j, x0);
        let (mut pos, mut posw, mut neg, mut negw) = (0f32, 0f32, 0f32, 0f32);
        for i in 0..N {
            let w = wx[i];
            let v = p[i];
            if w >= 0.0 {
                pos += w * v;
                posw += w;
            } else {
                neg += -w * v;
                negw += -w;
            }
        }
        let row = combine_lanczos_clamped(pos, posw, neg, negw, clamping);
        let wy_j = wy[j];
        if wy_j >= 0.0 {
            cpos += wy_j * row;
            cposw += wy_j;
        } else {
            cneg += -wy_j * row;
            cnegw += -wy_j;
        }
    }
    combine_lanczos_clamped(cpos, cposw, cneg, cnegw, clamping)
}

/// Interior Nearest/Bilinear/BicubicBSpline/MitchellNetravali: the plain
/// weighted-sum path, `N` (1, 2 or 4) a compile-time trip count. Same
/// row-major accumulation as the generic path.
#[inline]
fn sample_interior_plain<const N: usize>(
    src: &Plane,
    x0: usize,
    y0: usize,
    wx: &[f32; N],
    wy: &[f32; N],
) -> f32 {
    let mut acc = 0f32;
    for j in 0..N {
        let p: [f32; N] = interior_row(src, y0 + j, x0);
        let mut row = 0f32;
        for i in 0..N {
            row += wx[i] * p[i];
        }
        acc += wy[j] * row;
    }
    acc
}

/// Dispatches an interior pixel to the const-generic specialisation for its
/// kernel's tap count. `x0`/`y0` are LOCAL indices into `src.data` (`y0`
/// already has `src.y_offset` subtracted).
#[inline]
fn sample_interior(
    src: &Plane,
    interp: Interpolation,
    x0: usize,
    y0: usize,
    wx: &[f32; 8],
    wy: &[f32; 8],
    clamping: f32,
) -> f32 {
    match interp {
        Interpolation::BicubicSpline => {
            let wx4: [f32; 4] = wx[..4].try_into().unwrap();
            let wy4: [f32; 4] = wy[..4].try_into().unwrap();
            sample_interior_keys_clamp(src, x0, y0, &wx4, &wy4, clamping)
        }
        Interpolation::Lanczos3 => {
            let wx6: [f32; 6] = wx[..6].try_into().unwrap();
            let wy6: [f32; 6] = wy[..6].try_into().unwrap();
            sample_interior_lanczos_clamp::<6>(src, x0, y0, &wx6, &wy6, clamping)
        }
        Interpolation::Lanczos4 => {
            sample_interior_lanczos_clamp::<8>(src, x0, y0, wx, wy, clamping)
        }
        Interpolation::Nearest => {
            let wx1: [f32; 1] = [wx[0]];
            let wy1: [f32; 1] = [wy[0]];
            sample_interior_plain::<1>(src, x0, y0, &wx1, &wy1)
        }
        Interpolation::Bilinear => {
            let wx2: [f32; 2] = wx[..2].try_into().unwrap();
            let wy2: [f32; 2] = wy[..2].try_into().unwrap();
            sample_interior_plain::<2>(src, x0, y0, &wx2, &wy2)
        }
        Interpolation::BicubicBSpline | Interpolation::MitchellNetravali => {
            let wx4: [f32; 4] = wx[..4].try_into().unwrap();
            let wy4: [f32; 4] = wy[..4].try_into().unwrap();
            sample_interior_plain::<4>(src, x0, y0, &wx4, &wy4)
        }
    }
}

/// The unspecialised, clamped path — identical to the pre-Task-6 `sample_at`
/// match arms, just reading `first`/`n`/weights out of separate arguments
/// instead of a `Taps` value that carried its own `w`. Used for any pixel
/// [`is_interior`] does not clear, i.e. every border/window-edge pixel.
#[inline]
#[allow(clippy::too_many_arguments)]
fn sample_border(
    src: &Plane,
    interp: Interpolation,
    tx_first: isize,
    tx_n: usize,
    wx: &[f32; 8],
    ty_first: isize,
    ty_n: usize,
    wy: &[f32; 8],
    clamping: f32,
) -> f32 {
    match interp {
        Interpolation::BicubicSpline => {
            // Row-wise Keys with the 1-D clamp, then the column pass.
            let mut col = [0f32; 4];
            for (j, c) in col.iter_mut().enumerate() {
                let yy = ty_first + j as isize;
                let p = [
                    src.at(tx_first, yy),
                    src.at(tx_first + 1, yy),
                    src.at(tx_first + 2, yy),
                    src.at(tx_first + 3, yy),
                ];
                let w = [wx[0], wx[1], wx[2], wx[3]];
                *c = clamp_keys_1d(&w, &p, clamping);
            }
            let w = [wy[0], wy[1], wy[2], wy[3]];
            clamp_keys_1d(&w, &col, clamping)
        }
        Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
            // Separable: clamp each row's 1-D sum, then clamp the column
            // combination of the clamped rows — the 1-D deringing rule applied
            // once per axis (a 2-D split of every product weight by sign
            // over-counts negative lobes on smooth flanks and inflates flux).
            let (mut cpos, mut cposw, mut cneg, mut cnegw) = (0f32, 0f32, 0f32, 0f32);
            for j in 0..ty_n {
                let yy = ty_first + j as isize;
                let (mut pos, mut posw, mut neg, mut negw) = (0f32, 0f32, 0f32, 0f32);
                for i in 0..tx_n {
                    let w = wx[i];
                    let v = src.at(tx_first + i as isize, yy);
                    if w >= 0.0 {
                        pos += w * v;
                        posw += w;
                    } else {
                        neg += -w * v;
                        negw += -w;
                    }
                }
                let row = combine_lanczos_clamped(pos, posw, neg, negw, clamping);
                let wy_j = wy[j];
                if wy_j >= 0.0 {
                    cpos += wy_j * row;
                    cposw += wy_j;
                } else {
                    cneg += -wy_j * row;
                    cnegw += -wy_j;
                }
            }
            combine_lanczos_clamped(cpos, cposw, cneg, cnegw, clamping)
        }
        _ => {
            let mut acc = 0f32;
            for j in 0..ty_n {
                let yy = ty_first + j as isize;
                let mut row = 0f32;
                for i in 0..tx_n {
                    row += wx[i] * src.at(tx_first + i as isize, yy);
                }
                acc += wy[j] * row;
            }
            acc
        }
    }
}

/// One interpolated sample at source coordinates `(x, y)`; NaN outside
/// `[0, w−1] × [0, h−1]`. Picks the interior fast path
/// ([`sample_interior`]) when every tap the generic path would read is in
/// bounds without clamping, else falls back to [`sample_border`] — the two
/// share the same weight formula, tap order and renormalisation branch
/// (only [`taps_for`] computes weights; Task 6 (W1) does not touch that
/// arithmetic), so the choice of path can only change how fast the answer
/// arrives, never what it is.
pub fn sample_at(src: &Plane, x: f64, y: f64, interp: Interpolation, clamping: f32) -> f32 {
    if !(x.is_finite() && y.is_finite()) {
        return f32::NAN;
    }
    if x < 0.0 || y < 0.0 || x > (src.width - 1) as f64 || y > (src.full_height - 1) as f64 {
        return f32::NAN;
    }
    let mut wx = [0f32; 8];
    let mut wy = [0f32; 8];
    let tx = taps_for(interp, x as f32, &mut wx);
    let ty = taps_for(interp, y as f32, &mut wy);
    let n = tx.n; // ty.n is always the same tap count: same `interp` both axes.
    if is_interior(src, tx.first, ty.first, n) {
        let x0 = tx.first as usize;
        let y0 = (ty.first - src.y_offset as isize) as usize;
        return sample_interior(src, interp, x0, y0, &wx, &wy, clamping);
    }
    sample_border(
        src, interp, tx.first, tx.n, &wx, ty.first, ty.n, &wy, clamping,
    )
}

/// The pre-Task-6 `sample_at`, kept verbatim (module-private, `[f32; 8]`
/// weight buffers substituted for the `Taps::w` field `taps_for` no longer
/// carries — no arithmetic changed) as the bit-identity oracle for
/// [`sample_interior`]/[`sample_border`] above. Do not "fix" this to track
/// a change in the production path: if the two diverge, the pin tests
/// below are telling you something moved.
#[cfg(test)]
fn sample_at_oracle(src: &Plane, x: f64, y: f64, interp: Interpolation, clamping: f32) -> f32 {
    if !(x.is_finite() && y.is_finite()) {
        return f32::NAN;
    }
    if x < 0.0 || y < 0.0 || x > (src.width - 1) as f64 || y > (src.full_height - 1) as f64 {
        return f32::NAN;
    }
    let mut wx = [0f32; 8];
    let mut wy = [0f32; 8];
    let tx = taps_for(interp, x as f32, &mut wx);
    let ty = taps_for(interp, y as f32, &mut wy);
    match interp {
        Interpolation::BicubicSpline => {
            let mut col = [0f32; 4];
            for (j, c) in col.iter_mut().enumerate() {
                let yy = ty.first + j as isize;
                let p = [
                    src.at(tx.first, yy),
                    src.at(tx.first + 1, yy),
                    src.at(tx.first + 2, yy),
                    src.at(tx.first + 3, yy),
                ];
                let w = [wx[0], wx[1], wx[2], wx[3]];
                *c = clamp_keys_1d(&w, &p, clamping);
            }
            let w = [wy[0], wy[1], wy[2], wy[3]];
            clamp_keys_1d(&w, &col, clamping)
        }
        Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
            let (mut cpos, mut cposw, mut cneg, mut cnegw) = (0f32, 0f32, 0f32, 0f32);
            for j in 0..ty.n {
                let yy = ty.first + j as isize;
                let (mut pos, mut posw, mut neg, mut negw) = (0f32, 0f32, 0f32, 0f32);
                for i in 0..tx.n {
                    let w = wx[i];
                    let v = src.at(tx.first + i as isize, yy);
                    if w >= 0.0 {
                        pos += w * v;
                        posw += w;
                    } else {
                        neg += -w * v;
                        negw += -w;
                    }
                }
                let row = combine_lanczos_clamped(pos, posw, neg, negw, clamping);
                let wy_j = wy[j];
                if wy_j >= 0.0 {
                    cpos += wy_j * row;
                    cposw += wy_j;
                } else {
                    cneg += -wy_j * row;
                    cnegw += -wy_j;
                }
            }
            combine_lanczos_clamped(cpos, cposw, cneg, cnegw, clamping)
        }
        _ => {
            let mut acc = 0f32;
            for j in 0..ty.n {
                let yy = ty.first + j as isize;
                let mut row = 0f32;
                for i in 0..tx.n {
                    row += wx[i] * src.at(tx.first + i as isize, yy);
                }
                acc += wy[j] * row;
            }
            acc
        }
    }
}

/// Fills `out` (`rows × out_width`) with output rows `[y0, y0 + rows)`.
#[allow(clippy::too_many_arguments)]
pub fn warp_rows(
    src: &Plane,
    map: &dyn InverseMap,
    out_width: usize,
    y0: usize,
    rows: usize,
    interp: Interpolation,
    clamping: f32,
    out: &mut [f32],
) {
    assert_eq!(
        out.len(),
        rows * out_width,
        "warp_rows: out must be rows * out_width"
    );
    // Ruling R-T4-6b: ONE evaluator for the whole band. For a spline map
    // this captures the inverse displacement grid's handle here instead
    // of taking the cache lock per pixel — and holds it, so a concurrent
    // `release_grids` cannot free the grid these rows are reading. Every
    // other map's `inverse_burst` is the default, i.e. exactly the
    // `&dyn InverseMap` call this replaced.
    let at = map.inverse_burst();
    out.par_chunks_mut(out_width)
        .enumerate()
        .for_each(|(r, row)| {
            let y = (y0 + r) as f64;
            for (x, o) in row.iter_mut().enumerate() {
                let (sx, sy) = at(x as f64, y);
                *o = sample_at(src, sx, sy, interp, clamping);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Linear, LinearKind, PixelMap};
    use crate::resample::{source_window, Interpolation, SourceWindow};
    use crate::test_support::{centroid, flux, gaussian_field};

    const W: usize = 240;
    const H: usize = 180;
    const BG: f32 = 100.0;
    const STARS: [(f64, f64, f64); 3] = [
        (60.3, 50.7, 4000.0),
        (150.0, 90.0, 2500.0),
        (200.6, 140.2, 6000.0),
    ];

    fn field() -> Vec<f32> {
        gaussian_field(W, H, &STARS, 1.8, BG)
    }

    /// Reference → subject map for a subject that is the reference shifted
    /// by (dx, dy): subject(x, y) = reference(x − dx, y − dy)  ⇒  inverse
    /// maps reference (x, y) to subject (x − dx, y − dy)... the subject
    /// frame holds the star at (sx + dx, sy + dy), so to recover the
    /// reference the warp samples the subject at (x + dx, y + dy).
    fn shift_map(dx: f64, dy: f64) -> PixelMap {
        // forward: subject → reference = subtract the shift.
        let fwd = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]],
        };
        PixelMap::linear(fwd).unwrap()
    }

    fn warp_full(src: &[f32], map: &PixelMap, interp: Interpolation) -> Vec<f32> {
        let plane = Plane::full(src, W, H);
        let mut out = vec![0f32; W * H];
        warp_rows(&plane, map, W, 0, H, interp, 0.3, &mut out);
        out
    }

    #[test]
    fn identity_reproduces_interpolating_kernels_exactly() {
        let src = field();
        let id = PixelMap::linear(Linear::identity()).unwrap();
        for k in [
            Interpolation::Nearest,
            Interpolation::Bilinear,
            Interpolation::BicubicSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
        ] {
            let out = warp_full(&src, &id, k);
            for (a, b) in out.iter().zip(src.iter()) {
                assert!((a - b).abs() < 1e-3, "{k:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn constant_field_is_invariant_under_every_kernel() {
        let src = vec![BG; W * H];
        let map = shift_map(0.37, -0.61);
        for k in [
            Interpolation::BicubicBSpline,
            Interpolation::MitchellNetravali,
            Interpolation::Lanczos3,
            Interpolation::BicubicSpline,
            Interpolation::Bilinear,
        ] {
            let out = warp_full(&src, &map, k);
            for (y, row) in out.chunks(W).enumerate() {
                for (x, v) in row.iter().enumerate() {
                    if v.is_finite() {
                        assert!((v - BG).abs() < 1e-3, "{k:?} at ({x},{y}): {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn subpixel_shift_recovers_centroids_and_flux() {
        // Subject holds the stars shifted by (+0.3, −0.6).
        let (dx, dy) = (0.3, -0.6);
        let shifted: Vec<(f64, f64, f64)> =
            STARS.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
        let subject = gaussian_field(W, H, &shifted, 1.8, BG);
        let reference = field();
        let map = shift_map(dx, dy);
        for k in [
            Interpolation::Bilinear,
            Interpolation::BicubicSpline,
            Interpolation::BicubicBSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
            Interpolation::MitchellNetravali,
        ] {
            let out = warp_full(&subject, &map, k);
            // Windowed-sinc kernels do not reproduce linear functions exactly,
            // so they carry a phase-dependent first-moment error of ~0.02 px
            // (uniform over the frame); the cubic kernels have linear precision.
            let centroid_tol = match k {
                Interpolation::Lanczos3 | Interpolation::Lanczos4 => 0.05,
                _ => 0.02,
            };
            // Lanczos-4 with the 0.3 clamp keeps a measured +0.1..0.2 % one-signed
            // flux inflation on stars (the clamp attenuates its negative lobes on
            // smooth flanks); every other kernel is at 1e-4. Bounds sized so a
            // regression is caught, not hidden.
            let flux_tol = match k {
                Interpolation::Lanczos4 => 0.005,
                _ => 0.001,
            };
            for &(sx, sy, _) in &STARS {
                let (cx, cy) = centroid(&out, W, sx, sy, 7, BG);
                assert!(
                    (cx - sx).abs() < centroid_tol && (cy - sy).abs() < centroid_tol,
                    "{k:?}: centroid ({cx},{cy}) vs ({sx},{sy})"
                );
                let f_out = flux(&out, W, sx, sy, 7, BG);
                let f_ref = flux(&reference, W, sx, sy, 7, BG);
                assert!(
                    ((f_out - f_ref) / f_ref).abs() < flux_tol,
                    "{k:?}: flux {f_out} vs {f_ref}"
                );
            }
        }
    }

    #[test]
    fn rotation_about_the_centre_recovers_centroids() {
        let (cx, cy) = ((W as f64 - 1.0) / 2.0, (H as f64 - 1.0) / 2.0);
        let th = 30f64.to_radians();
        let (s, c) = th.sin_cos();
        // forward (subject → reference): rotate by +30° about the centre.
        let fwd = Linear {
            kind: LinearKind::Similarity,
            m: [
                [c, -s, cx - c * cx + s * cy],
                [s, c, cy - s * cx - c * cy],
                [0.0, 0.0, 1.0],
            ],
        };
        let map = PixelMap::linear(fwd).unwrap();
        // Subject star positions = inverse of the reference positions.
        let subj_stars: Vec<(f64, f64, f64)> = STARS
            .iter()
            .map(|&(x, y, a)| {
                let (u, v) = map.inverse(x, y);
                (u, v, a)
            })
            .collect();
        let subject = gaussian_field(W, H, &subj_stars, 1.8, BG);
        let out = warp_full(&subject, &map, Interpolation::Lanczos3);
        for &(sx, sy, _) in &STARS {
            let (ox, oy) = centroid(&out, W, sx, sy, 7, BG);
            assert!(
                (ox - sx).abs() < 0.02 && (oy - sy).abs() < 0.02,
                "({ox},{oy}) vs ({sx},{sy})"
            );
        }
    }

    #[test]
    fn pixels_mapping_outside_the_source_are_nan() {
        let src = field();
        let map = shift_map(100.0, 0.0); // reference x maps to subject x + 100
        let out = warp_full(&src, &map, Interpolation::Bilinear);
        for y in 0..H {
            for x in 0..W {
                let v = out[y * W + x];
                if x + 100 > W - 1 {
                    assert!(v.is_nan(), "({x},{y}) should be outside");
                } else {
                    assert!(v.is_finite(), "({x},{y}) should be inside");
                }
            }
        }
    }

    #[test]
    fn windowed_plane_matches_the_full_plane() {
        let src = field();
        let map = shift_map(0.25, 10.4);
        let full = warp_full(&src, &map, Interpolation::BicubicBSpline);
        // Output rows 40..70 need source rows ≈ 50..81 (+margins).
        let (y0, rows) = (40usize, 30usize);
        let win = match source_window(&map, W, y0, rows, W, H, 2, 0.6) {
            SourceWindow::Rows { y0, y1 } => (y0, y1),
            SourceWindow::Whole => (0, H),
        };
        let plane = Plane {
            data: &src[win.0 * W..win.1 * W],
            width: W,
            height: win.1 - win.0,
            y_offset: win.0,
            full_height: H,
        };
        let mut out = vec![0f32; rows * W];
        warp_rows(
            &plane,
            &map,
            W,
            y0,
            rows,
            Interpolation::BicubicBSpline,
            0.3,
            &mut out,
        );
        for (i, v) in out.iter().enumerate() {
            let f = full[y0 * W + i];
            assert!(
                (v.is_nan() && f.is_nan()) || (v - f).abs() < 1e-5,
                "row {} col {}: {v} vs {f}",
                y0 + i / W,
                i % W
            );
        }
    }

    /// Task 6 (W1) pin: a deterministic fixture (Gaussian stars + a linear
    /// gradient + a NaN patch, so both finite and non-finite taps exercise
    /// the interior path) warped under Similarity/Affine/Homography maps —
    /// rotation, scale ≠ 1, fractional translation — at every kernel the
    /// interior fast path specialises, must be bit-for-bit (`to_bits`)
    /// identical to `sample_at_oracle`, the pre-change implementation, for
    /// EVERY output pixel. This is the identity the whole task rests on:
    /// `sample_interior` and `sample_border` are independently written from
    /// `sample_at_oracle`, so an arithmetic slip anywhere (tap order,
    /// accumulator, the clamp/renormalisation branch) shows up here.
    fn pin_fixture(w: usize, h: usize) -> Vec<f32> {
        let stars: [(f64, f64, f64); 5] = [
            (30.3, 22.7, 3000.0),
            (w as f64 * 0.5 + 0.4, h as f64 * 0.5 - 0.25, 5000.0),
            (w as f64 - 20.1, h as f64 - 18.9, 2000.0),
            (4.2, h as f64 - 6.8, 1500.0),
            (w as f64 - 5.6, 3.3, 1800.0),
        ];
        let mut data = gaussian_field(w, h, &stars, 2.1, 100.0);
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] += (x as f32) * 0.05 + (y as f32) * 0.08;
            }
        }
        // A deterministic NaN patch away from the stars above, well inside
        // the frame so its neighbourhood exercises interior taps that must
        // propagate NaN identically to the oracle.
        let (nx0, ny0, nx1, ny1) = (w / 3, h / 3, w / 3 + 14, h / 3 + 10);
        for y in ny0..ny1 {
            for x in nx0..nx1 {
                data[y * w + x] = f32::NAN;
            }
        }
        data
    }

    /// Every output pixel of a full [`warp_rows`] (the production path,
    /// interior fast path included) matches [`sample_at_oracle`] pixel for
    /// pixel, `to_bits` exact.
    fn assert_warp_matches_oracle(
        src: &[f32],
        w: usize,
        h: usize,
        map: &PixelMap,
        interp: Interpolation,
        clamping: f32,
        label: &str,
    ) {
        let plane = Plane::full(src, w, h);
        let mut prod = vec![0f32; w * h];
        warp_rows(&plane, map, w, 0, h, interp, clamping, &mut prod);
        let at = map.inverse_burst();
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = at(x as f64, y as f64);
                let want = sample_at_oracle(&plane, sx, sy, interp, clamping);
                let got = prod[y * w + x];
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{label} {interp:?} at ({x},{y}) → src ({sx},{sy}): \
                     production {got} (bits {:x}) vs oracle {want} (bits {:x})",
                    got.to_bits(),
                    want.to_bits(),
                );
            }
        }
    }

    #[test]
    fn interior_fast_path_matches_the_oracle_bit_for_bit() {
        const FW: usize = 320;
        const FH: usize = 240;
        let src = pin_fixture(FW, FH);

        // Similarity: rotation + scale ≠ 1 + fractional translation.
        let th = 11f64.to_radians();
        let (s, c) = th.sin_cos();
        let scale = 0.947;
        let similarity = Linear {
            kind: LinearKind::Similarity,
            m: [
                [scale * c, -scale * s, 9.35],
                [scale * s, scale * c, -6.7],
                [0.0, 0.0, 1.0],
            ],
        };

        // Affine: independent x/y scale + shear + fractional translation.
        let affine = Linear {
            kind: LinearKind::Affine,
            m: [
                [1.031, 0.021, 4.65],
                [-0.014, 0.985, -11.2],
                [0.0, 0.0, 1.0],
            ],
        };

        // Homography: rotation/scale/translation as above plus small
        // genuine projective terms (the third row is not `[0, 0, 1]`).
        let homography = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.994, 0.026, 18.35],
                [-0.031, 1.006, -4.65],
                [1.8e-6, -9.0e-7, 1.0],
            ],
        };

        let canonical: [(&str, Linear); 3] = [
            ("similarity", similarity),
            ("affine", affine),
            ("homography", homography),
        ];
        let kernels = [
            Interpolation::BicubicBSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
            Interpolation::Bilinear,
        ];
        for (label, linear) in canonical {
            let map = PixelMap::linear(linear).unwrap();
            for k in kernels {
                assert_warp_matches_oracle(&src, FW, FH, &map, k, 0.3, label);
            }
        }

        // A map that pushes part of the output outside the source, so both
        // the interior path (deep inside) and the NaN/border path (the
        // pushed-off edge) run in the same pass.
        let off_edge = Linear {
            kind: LinearKind::Similarity,
            m: [[1.0, 0.0, 137.4], [0.0, 1.0, -0.6], [0.0, 0.0, 1.0]],
        };
        let off_edge_map = PixelMap::linear(off_edge).unwrap();
        for k in kernels {
            assert_warp_matches_oracle(&src, FW, FH, &off_edge_map, k, 0.3, "off_edge");
        }

        // A map with the source exactly at the boundary condition: an
        // integer-pixel shift, so several tap footprints sit exactly on the
        // interior/border seam (`x_first + n == width` etc.) rather than
        // strictly inside or strictly outside it.
        let boundary = Linear {
            kind: LinearKind::Similarity,
            m: [[1.0, 0.0, 1.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0]],
        };
        let boundary_map = PixelMap::linear(boundary).unwrap();
        for k in kernels {
            assert_warp_matches_oracle(&src, FW, FH, &boundary_map, k, 0.3, "boundary");
        }
    }

    /// The same bit-identity check over a [`Plane`] window (not the full
    /// plane) — the interior predicate is keyed on `y_offset`/`height`, not
    /// `full_height`, so this exercises that path independently.
    #[test]
    fn interior_fast_path_matches_the_oracle_on_a_windowed_plane() {
        const FW: usize = 320;
        const FH: usize = 240;
        let src = pin_fixture(FW, FH);
        let shift = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, -0.25], [0.0, 1.0, -10.4], [0.0, 0.0, 1.0]],
        };
        let map = PixelMap::linear(shift).unwrap();
        let (y0, rows) = (40usize, 30usize);
        let win = match source_window(&map, FW, y0, rows, FW, FH, 4, 0.6) {
            SourceWindow::Rows { y0, y1 } => (y0, y1),
            SourceWindow::Whole => (0, FH),
        };
        let windowed = Plane {
            data: &src[win.0 * FW..win.1 * FW],
            width: FW,
            height: win.1 - win.0,
            y_offset: win.0,
            full_height: FH,
        };
        for k in [
            Interpolation::BicubicBSpline,
            Interpolation::Lanczos3,
            Interpolation::Lanczos4,
            Interpolation::Bilinear,
        ] {
            let mut out = vec![0f32; rows * FW];
            warp_rows(&windowed, &map, FW, y0, rows, k, 0.3, &mut out);
            let at = map.inverse_burst();
            for r in 0..rows {
                for x in 0..FW {
                    let y = y0 + r;
                    let (sx, sy) = at(x as f64, y as f64);
                    let want = sample_at_oracle(&windowed, sx, sy, k, 0.3);
                    let got = out[r * FW + x];
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "{k:?} at ({x},{y}) → src ({sx},{sy}): {got} vs {want}"
                    );
                }
            }
        }
    }
}
