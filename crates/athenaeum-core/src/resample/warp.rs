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
        let yl = yc.clamp(self.y_offset, self.y_offset + self.height - 1) - self.y_offset;
        self.data[yl * self.width + xc]
    }
}

/// One interpolated sample at source coordinates `(x, y)`; NaN outside
/// `[0, w−1] × [0, h−1]`.
pub fn sample_at(src: &Plane, x: f64, y: f64, interp: Interpolation, clamping: f32) -> f32 {
    if !(x.is_finite() && y.is_finite()) {
        return f32::NAN;
    }
    if x < 0.0 || y < 0.0 || x > (src.width - 1) as f64 || y > (src.full_height - 1) as f64 {
        return f32::NAN;
    }
    let tx = taps_for(interp, x as f32);
    let ty = taps_for(interp, y as f32);
    match interp {
        Interpolation::BicubicSpline => {
            // Row-wise Keys with the 1-D clamp, then the column pass.
            let mut col = [0f32; 4];
            for (j, c) in col.iter_mut().enumerate() {
                let yy = ty.first + j as isize;
                let p = [
                    src.at(tx.first, yy),
                    src.at(tx.first + 1, yy),
                    src.at(tx.first + 2, yy),
                    src.at(tx.first + 3, yy),
                ];
                let w = [tx.w[0], tx.w[1], tx.w[2], tx.w[3]];
                *c = clamp_keys_1d(&w, &p, clamping);
            }
            let w = [ty.w[0], ty.w[1], ty.w[2], ty.w[3]];
            clamp_keys_1d(&w, &col, clamping)
        }
        Interpolation::Lanczos3 | Interpolation::Lanczos4 => {
            // Separable: clamp each row's 1-D sum, then clamp the column
            // combination of the clamped rows — the 1-D deringing rule applied
            // once per axis (a 2-D split of every product weight by sign
            // over-counts negative lobes on smooth flanks and inflates flux).
            let (mut cpos, mut cposw, mut cneg, mut cnegw) = (0f32, 0f32, 0f32, 0f32);
            for j in 0..ty.n {
                let yy = ty.first + j as isize;
                let (mut pos, mut posw, mut neg, mut negw) = (0f32, 0f32, 0f32, 0f32);
                for i in 0..tx.n {
                    let w = tx.w[i];
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
                let wy = ty.w[j];
                if wy >= 0.0 {
                    cpos += wy * row;
                    cposw += wy;
                } else {
                    cneg += -wy * row;
                    cnegw += -wy;
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
                    row += tx.w[i] * src.at(tx.first + i as isize, yy);
                }
                acc += ty.w[j] * row;
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
    out.par_chunks_mut(out_width)
        .enumerate()
        .for_each(|(r, row)| {
            let y = (y0 + r) as f64;
            for (x, o) in row.iter_mut().enumerate() {
                let (sx, sy) = map.inverse(x as f64, y);
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
            for &(sx, sy, _) in &STARS {
                let (cx, cy) = centroid(&out, W, sx, sy, 7, BG);
                assert!(
                    (cx - sx).abs() < centroid_tol && (cy - sy).abs() < centroid_tol,
                    "{k:?}: centroid ({cx},{cy}) vs ({sx},{sy})"
                );
                let f_out = flux(&out, W, sx, sy, 7, BG);
                let f_ref = flux(&reference, W, sx, sy, 7, BG);
                assert!(
                    ((f_out - f_ref) / f_ref).abs() < 0.005,
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
}
