//! Linear registration models as 3×3 homogeneous matrices. A similarity
//! or affine simply has a `[0, 0, 1]` last row. Fitters are least squares
//! with optional per-pair weights; the homography uses the normalized
//! direct linear transform (Hartley 1997) — points shifted to their centroid
//! and scaled to mean distance √2 before the 2n×9 system is solved through
//! its Gram matrix's smallest eigenvector (`eigen.rs`).

use serde::{Deserialize, Serialize};

use super::eigen::smallest_eigenvector_sym;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinearKind {
    Similarity,
    Affine,
    Homography,
}

/// `(subject, reference)` correspondence.
pub type Pair = ((f64, f64), (f64, f64));

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Linear {
    pub kind: LinearKind,
    pub m: [[f64; 3]; 3],
}

impl Linear {
    pub fn identity() -> Linear {
        Linear {
            kind: LinearKind::Similarity,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    pub fn from_flat(kind: LinearKind, f: [f64; 9]) -> Linear {
        Linear {
            kind,
            m: [[f[0], f[1], f[2]], [f[3], f[4], f[5]], [f[6], f[7], f[8]]],
        }
    }

    pub fn to_flat(&self) -> [f64; 9] {
        let m = &self.m;
        [
            m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2],
        ]
    }

    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        let u = (m[0][0] * x + m[0][1] * y + m[0][2]) / w;
        let v = (m[1][0] * x + m[1][1] * y + m[1][2]) / w;
        (u, v)
    }

    /// Matrix inverse by adjugate; `None` when singular.
    pub fn inverse(&self) -> Option<Linear> {
        let m = &self.m;
        let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
        let c01 = -(m[1][0] * m[2][2] - m[1][2] * m[2][0]);
        let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
        let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
        if det.abs() < 1e-300 || !det.is_finite() {
            return None;
        }
        let c10 = -(m[0][1] * m[2][2] - m[0][2] * m[2][1]);
        let c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0];
        let c12 = -(m[0][0] * m[2][1] - m[0][1] * m[2][0]);
        let c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1];
        let c21 = -(m[0][0] * m[1][2] - m[0][2] * m[1][0]);
        let c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        let mut inv = [[c00, c10, c20], [c01, c11, c21], [c02, c12, c22]];
        for row in inv.iter_mut() {
            for v in row.iter_mut() {
                *v /= det;
            }
        }
        // Keep the affine rows exactly [0, 0, 1] when the input had them.
        if self.kind != LinearKind::Homography {
            let s = inv[2][2];
            for row in inv.iter_mut() {
                for v in row.iter_mut() {
                    *v /= s;
                }
            }
            inv[2] = [0.0, 0.0, 1.0];
        }
        Some(Linear {
            kind: self.kind,
            m: inv,
        })
    }

    /// Determinant of the upper-left 2×2 (the local linear part at the origin).
    pub fn det2(&self) -> f64 {
        self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]
    }

    pub fn scale(&self) -> f64 {
        self.det2().abs().sqrt()
    }

    pub fn rotation_deg(&self) -> f64 {
        self.m[1][0].atan2(self.m[0][0]).to_degrees()
    }

    pub fn translation(&self) -> (f64, f64) {
        (self.m[0][2], self.m[1][2])
    }

    /// A mirror image (meridian flip seen through the optics) has a
    /// negative determinant; a 180° rotation does not.
    pub fn is_flipped(&self) -> bool {
        self.det2() < 0.0
    }
}

fn weight_at(weights: Option<&[f64]>, i: usize) -> f64 {
    weights.map(|w| w[i]).unwrap_or(1.0)
}

/// Weighted similarity (rotation + isotropic scale + translation), closed
/// form via weighted centroids. Proper rotations only — a mirrored field
/// needs `Affine` or `Homography`.
pub fn fit_similarity(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 2 {
        return None;
    }
    let mut sw = 0.0;
    let (mut sx, mut sy, mut rx, mut ry) = (0.0, 0.0, 0.0, 0.0);
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        sw += w;
        sx += w * x;
        sy += w * y;
        rx += w * u;
        ry += w * v;
    }
    if sw <= 0.0 {
        return None;
    }
    let (cx, cy, cu, cv) = (sx / sw, sy / sw, rx / sw, ry / sw);
    // Complex regression: (u + iv) = (a + ib)(x + iy) about the centroids.
    let (mut num_re, mut num_im, mut den) = (0.0, 0.0, 0.0);
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        let (px, py, qu, qv) = (x - cx, y - cy, u - cu, v - cv);
        num_re += w * (px * qu + py * qv);
        num_im += w * (px * qv - py * qu);
        den += w * (px * px + py * py);
    }
    if den < 1e-12 {
        return None;
    }
    let a = num_re / den;
    let b = num_im / den;
    if a * a + b * b < 1e-24 {
        return None;
    }
    let tx = cu - (a * cx - b * cy);
    let ty = cv - (b * cx + a * cy);
    Some(Linear {
        kind: LinearKind::Similarity,
        m: [[a, -b, tx], [b, a, ty], [0.0, 0.0, 1.0]],
    })
}

/// Solves the symmetric 3×3 system `a·x = b` by Cramer's rule.
fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(a);
    if d.abs() < 1e-18 || !d.is_finite() {
        return None;
    }
    let mut out = [0f64; 3];
    for k in 0..3 {
        let mut ak = a;
        for row in 0..3 {
            ak[row][k] = b[row];
        }
        out[k] = det(ak) / d;
    }
    Some(out)
}

/// Weighted least-squares affine: two independent 3-parameter regressions
/// on centred coordinates (centring keeps the normal equations conditioned
/// for 6000-pixel frames).
pub fn fit_affine(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 3 {
        return None;
    }
    let mut sw = 0.0;
    let (mut cx, mut cy) = (0.0, 0.0);
    for (i, ((x, y), _)) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        sw += w;
        cx += w * x;
        cy += w * y;
    }
    if sw <= 0.0 {
        return None;
    }
    cx /= sw;
    cy /= sw;
    let mut ata = [[0f64; 3]; 3];
    let mut atu = [0f64; 3];
    let mut atv = [0f64; 3];
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i);
        let r = [x - cx, y - cy, 1.0];
        for a in 0..3 {
            for b in 0..3 {
                ata[a][b] += w * r[a] * r[b];
            }
            atu[a] += w * r[a] * u;
            atv[a] += w * r[a] * v;
        }
    }
    let pu = solve3(ata, atu)?;
    let pv = solve3(ata, atv)?;
    // Undo the centring: u = pu0 (x - cx) + pu1 (y - cy) + pu2.
    let m = [
        [pu[0], pu[1], pu[2] - pu[0] * cx - pu[1] * cy],
        [pv[0], pv[1], pv[2] - pv[0] * cx - pv[1] * cy],
        [0.0, 0.0, 1.0],
    ];
    let t = Linear {
        kind: LinearKind::Affine,
        m,
    };
    if t.det2().abs() < 1e-12 || !t.det2().is_finite() {
        return None;
    }
    Some(t)
}

struct Norm {
    cx: f64,
    cy: f64,
    s: f64,
}

fn normalization(points: impl Iterator<Item = (f64, f64)> + Clone) -> Option<Norm> {
    let n = points.clone().count();
    if n == 0 {
        return None;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for (x, y) in points.clone() {
        cx += x;
        cy += y;
    }
    cx /= n as f64;
    cy /= n as f64;
    let mut mean_d = 0.0;
    for (x, y) in points {
        mean_d += ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    }
    mean_d /= n as f64;
    if mean_d < 1e-12 {
        return None;
    }
    Some(Norm {
        cx,
        cy,
        s: std::f64::consts::SQRT_2 / mean_d,
    })
}

/// Weighted normalized DLT homography (Hartley 1997). Returns the matrix
/// scaled so `m[2][2] == 1`.
pub fn fit_homography(pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    if pairs.len() < 4 {
        return None;
    }
    let ns = normalization(pairs.iter().map(|p| p.0))?;
    let nr = normalization(pairs.iter().map(|p| p.1))?;
    let mut ata = [[0f64; 9]; 9];
    for (i, ((x, y), (u, v))) in pairs.iter().enumerate() {
        let w = weight_at(weights, i).max(0.0).sqrt();
        let (x, y) = ((x - ns.cx) * ns.s, (y - ns.cy) * ns.s);
        let (u, v) = ((u - nr.cx) * nr.s, (v - nr.cy) * nr.s);
        let rows = [
            [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v],
            [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, -u],
        ];
        for r in rows {
            for a in 0..9 {
                for b in 0..9 {
                    ata[a][b] += w * w * r[a] * r[b];
                }
            }
        }
    }
    let h = smallest_eigenvector_sym(&ata);
    let hn = [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], h[8]]];
    // Denormalize: H = T_r⁻¹ · Hn · T_s.
    let ts = [
        [ns.s, 0.0, -ns.s * ns.cx],
        [0.0, ns.s, -ns.s * ns.cy],
        [0.0, 0.0, 1.0],
    ];
    let tr_inv = [
        [1.0 / nr.s, 0.0, nr.cx],
        [0.0, 1.0 / nr.s, nr.cy],
        [0.0, 0.0, 1.0],
    ];
    let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| {
        let mut c = [[0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    c[i][j] += a[i][k] * b[k][j];
                }
            }
        }
        c
    };
    let mut m = mul(tr_inv, mul(hn, ts));
    let s = m[2][2];
    if s.abs() < 1e-300 || !s.is_finite() {
        return None;
    }
    for row in m.iter_mut() {
        for v in row.iter_mut() {
            *v /= s;
        }
    }
    let t = Linear {
        kind: LinearKind::Homography,
        m,
    };
    // Reject the degenerate solutions a collinear or duplicated sample yields.
    let res = residuals(&t, pairs);
    if t.det2().abs() < 1e-12 || !t.det2().is_finite() || res.iter().any(|r| !r.is_finite()) {
        return None;
    }
    Some(t)
}

pub fn fit_linear(kind: LinearKind, pairs: &[Pair], weights: Option<&[f64]>) -> Option<Linear> {
    match kind {
        LinearKind::Similarity => fit_similarity(pairs, weights),
        LinearKind::Affine => fit_affine(pairs, weights),
        LinearKind::Homography => fit_homography(pairs, weights),
    }
}

/// Euclidean residual per pair in reference pixels.
pub fn residuals(t: &Linear, pairs: &[Pair]) -> Vec<f64> {
    pairs
        .iter()
        .map(|((x, y), (u, v))| {
            let (px, py) = t.apply(*x, *y);
            ((px - u).powi(2) + (py - v).powi(2)).sqrt()
        })
        .collect()
}

pub fn rms(residuals: &[f64]) -> f64 {
    if residuals.is_empty() {
        return 0.0;
    }
    (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random points spread over a 6000×4000 frame.
    fn points(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut s = seed;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1_000_000) as f64 / 1_000_000.0
        };
        (0..n).map(|_| (next() * 6000.0, next() * 4000.0)).collect()
    }

    fn similarity(rot_deg: f64, scale: f64, tx: f64, ty: f64) -> Linear {
        let (s, c) = rot_deg.to_radians().sin_cos();
        Linear {
            kind: LinearKind::Similarity,
            m: [
                [scale * c, -scale * s, tx],
                [scale * s, scale * c, ty],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    fn pairs_under(t: &Linear, pts: &[(f64, f64)], noise: f64) -> Vec<Pair> {
        let mut k = 0.37f64;
        pts.iter()
            .map(|&(x, y)| {
                let (rx, ry) = t.apply(x, y);
                k = (k * 7.13 + 0.31).fract();
                let nx = (k - 0.5) * 2.0 * noise;
                k = (k * 7.13 + 0.31).fract();
                let ny = (k - 0.5) * 2.0 * noise;
                ((x, y), (rx + nx, ry + ny))
            })
            .collect()
    }

    fn assert_close(a: &Linear, b: &Linear, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (a.m[i][j] - b.m[i][j]).abs() < tol,
                    "m[{i}][{j}]: {} vs {}",
                    a.m[i][j],
                    b.m[i][j]
                );
            }
        }
    }

    #[test]
    fn similarity_fit_recovers_rotation_scale_translation() {
        let truth = similarity(12.0, 1.01, 5.3, -2.1);
        let pairs = pairs_under(&truth, &points(40, 11), 0.01);
        let fit = fit_similarity(&pairs, None).unwrap();
        // translation entries carry centroid_distance × angular_error (≈ 3000 px × 1e-6 rad); the scale/rotation asserts below are the tight gates
        assert_close(&fit, &truth, 1e-2);
        assert!((fit.scale() - 1.01).abs() < 1e-3);
        assert!((fit.rotation_deg() - 12.0).abs() < 0.01);
        assert!(!fit.is_flipped());
    }

    #[test]
    fn affine_fit_recovers_an_anisotropic_transform() {
        let truth = Linear {
            kind: LinearKind::Affine,
            m: [
                [1.002, 0.031, 273.2],
                [-0.028, 0.995, -499.1],
                [0.0, 0.0, 1.0],
            ],
        };
        let pairs = pairs_under(&truth, &points(50, 5), 0.01);
        let fit = fit_affine(&pairs, None).unwrap();
        assert_close(&fit, &truth, 1e-3);
    }

    #[test]
    fn homography_fit_recovers_perspective_terms() {
        let truth = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.992, 0.0387, -23.54],
                [-0.0383, 0.9922, 136.37],
                [8.8e-8, -2.5e-8, 1.0],
            ],
        };
        let pairs = pairs_under(&truth, &points(80, 3), 0.0);
        let fit = fit_homography(&pairs, None).unwrap();
        // Compare after normalizing m[2][2] to 1.
        assert!((fit.m[2][2] - 1.0).abs() < 1e-12);
        for i in 0..3 {
            for j in 0..3 {
                let scale = truth.m[i][j].abs().max(1e-6);
                assert!(
                    (fit.m[i][j] - truth.m[i][j]).abs() / scale < 1e-4,
                    "m[{i}][{j}] {} vs {}",
                    fit.m[i][j],
                    truth.m[i][j]
                );
            }
        }
        let res = residuals(&fit, &pairs);
        assert!(rms(&res) < 1e-6);
    }

    #[test]
    fn inverse_composes_to_identity_and_flags_flips() {
        let flipped = Linear {
            kind: LinearKind::Homography,
            m: [
                [-0.998963, -0.042010, 6270.125],
                [0.041831, -0.999094, 4078.864],
                [0.0, 0.0, 1.0],
            ],
        };
        assert!(!flipped.is_flipped(), "a 180° rotation is not a mirror");
        let mirror = Linear {
            kind: LinearKind::Affine,
            m: [[-1.0, 0.0, 6000.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(mirror.is_flipped());
        for t in [&flipped, &mirror] {
            let inv = t.inverse().unwrap();
            for (x, y) in points(20, 9) {
                let (u, v) = t.apply(x, y);
                let (bx, by) = inv.apply(u, v);
                assert!((bx - x).abs() < 1e-8 && (by - y).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn weighted_fit_follows_the_heavier_points() {
        let truth = similarity(3.0, 1.0, 10.0, 20.0);
        let mut pairs = pairs_under(&truth, &points(30, 21), 0.0);
        // Corrupt two points heavily but give them ~zero weight.
        pairs[0].1 .0 += 500.0;
        pairs[1].1 .1 -= 500.0;
        let mut w = vec![1.0; pairs.len()];
        w[0] = 1e-9;
        w[1] = 1e-9;
        let fit = fit_affine(&pairs, Some(&w)).unwrap();
        assert_close(&fit, &truth, 1e-3);
    }

    #[test]
    fn degenerate_inputs_return_none() {
        assert!(fit_similarity(&[], None).is_none());
        let p = ((1.0, 1.0), (2.0, 2.0));
        assert!(
            fit_affine(&[p, p, p], None).is_none(),
            "collinear/duplicate points"
        );
        assert!(fit_homography(&[p, p, p, p], None).is_none());
    }

    #[test]
    fn flat_roundtrip() {
        let t = similarity(7.0, 1.2, 1.0, 2.0);
        let back = Linear::from_flat(LinearKind::Similarity, t.to_flat());
        assert_eq!(t, back);
    }
}
