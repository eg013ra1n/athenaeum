//! Polynomial distortion on top of a linear model — the plate solver's SIP
//! convention: only terms of total degree 2..=order, fitted independently
//! forward and inverse by weighted least squares, on coordinates normalized
//! to the reference centre and half the longer side so every coefficient is
//! O(1) and the normal equations stay conditioned. Math reference §5.3.

use serde::{Deserialize, Serialize};

use super::linear::{Linear, Pair};
// Brought in only for the integration test below (`super::*` glob import);
// not part of this module's own public surface.
#[cfg(test)]
use super::pixel_map::PixelMap;

/// Exponent pairs `(i, j)` for `u^i v^j`, `2 ≤ i+j ≤ order`, in a fixed
/// order: by total degree, then by descending `i`.
pub fn term_exponents(order: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    for deg in 2..=order {
        for i in (0..=deg).rev() {
            out.push((i, deg - i));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polynomial2D {
    pub order: u8,
    pub ax: Vec<f64>,
    pub ay: Vec<f64>,
}

impl Polynomial2D {
    /// True when `order` is 2..=4, both coefficient vectors have exactly one
    /// entry per term of that order, and every coefficient is finite — what
    /// `fit` always produces and what `eval` assumes.
    pub fn is_well_formed(&self) -> bool {
        (2..=4).contains(&self.order)
            && self.ax.len() == term_exponents(self.order).len()
            && self.ay.len() == self.ax.len()
            && self.ax.iter().chain(self.ay.iter()).all(|c| c.is_finite())
    }

    /// Evaluates `(Σ ax_k u^i v^j, Σ ay_k u^i v^j)` with power tables
    /// (`order` is at most 4, so this is a dozen multiplies).
    #[inline]
    pub fn eval(&self, u: f64, v: f64) -> (f64, f64) {
        debug_assert!(
            self.is_well_formed(),
            "Polynomial2D::eval on a malformed polynomial"
        );
        let mut pu = [1.0f64; 5];
        let mut pv = [1.0f64; 5];
        for k in 1..=self.order as usize {
            pu[k] = pu[k - 1] * u;
            pv[k] = pv[k - 1] * v;
        }
        let (mut du, mut dv) = (0.0, 0.0);
        let mut k = 0;
        for deg in 2..=self.order as usize {
            for i in (0..=deg).rev() {
                let t = pu[i] * pv[deg - i];
                du += self.ax[k] * t;
                dv += self.ay[k] * t;
                k += 1;
            }
        }
        (du, dv)
    }

    /// Weighted least squares over `samples = ((u, v), (du, dv))`. Needs at
    /// least `terms + 2` samples. Solved through the normal equations with
    /// Gauss–Jordan elimination and partial pivoting (≤ 12 unknowns).
    pub fn fit(
        order: u8,
        samples: &[((f64, f64), (f64, f64))],
        weights: Option<&[f64]>,
    ) -> Option<Polynomial2D> {
        if !(2..=4).contains(&order) {
            return None;
        }
        let terms = term_exponents(order);
        let n = terms.len();
        if samples.len() < n + 2 {
            return None;
        }
        let mut ata = vec![vec![0f64; n]; n];
        let mut atx = vec![0f64; n];
        let mut aty = vec![0f64; n];
        let mut row = vec![0f64; n];
        for (s, ((u, v), (du, dv))) in samples.iter().enumerate() {
            let w = weights.map(|w| w[s]).unwrap_or(1.0);
            for (k, (i, j)) in terms.iter().enumerate() {
                row[k] = u.powi(*i as i32) * v.powi(*j as i32);
            }
            for a in 0..n {
                for b in 0..n {
                    ata[a][b] += w * row[a] * row[b];
                }
                atx[a] += w * row[a] * du;
                aty[a] += w * row[a] * dv;
            }
        }
        let ax = solve_dense(&ata, &atx)?;
        let ay = solve_dense(&ata, &aty)?;
        let p = Polynomial2D { order, ax, ay };
        if !p.is_well_formed() {
            return None;
        }
        Some(p)
    }

    /// Joint weighted least squares of an affine correction
    /// `(a0 + a1·u + a2·v, b0 + b1·u + b2·v)` and the degree-`2..=order`
    /// terms over the same samples. A residual field whose linear part the
    /// linear model absorbed only in projection cannot be recovered by
    /// alternating the two fits (a linear and a cubic term are ~0.9
    /// correlated on a finite field, so alternation converges at ~0.85 per
    /// round); one joint solve is exact. Returns the affine coefficients
    /// `[a0, a1, a2, b0, b1, b2]` (pixel displacements per normalized unit)
    /// and the polynomial. Needs at least `terms + 5` samples.
    pub fn fit_with_affine(
        order: u8,
        samples: &[((f64, f64), (f64, f64))],
        weights: Option<&[f64]>,
    ) -> Option<([f64; 6], Polynomial2D)> {
        if !(2..=4).contains(&order) {
            return None;
        }
        let terms = term_exponents(order);
        let n = 3 + terms.len();
        if samples.len() < n + 2 {
            return None;
        }
        let mut ata = vec![vec![0f64; n]; n];
        let mut atx = vec![0f64; n];
        let mut aty = vec![0f64; n];
        let mut row = vec![0f64; n];
        for (s, ((u, v), (du, dv))) in samples.iter().enumerate() {
            let w = weights.map(|w| w[s]).unwrap_or(1.0);
            row[0] = 1.0;
            row[1] = *u;
            row[2] = *v;
            for (k, (i, j)) in terms.iter().enumerate() {
                row[3 + k] = u.powi(*i as i32) * v.powi(*j as i32);
            }
            for a in 0..n {
                for b in 0..n {
                    ata[a][b] += w * row[a] * row[b];
                }
                atx[a] += w * row[a] * du;
                aty[a] += w * row[a] * dv;
            }
        }
        let cx = solve_dense(&ata, &atx)?;
        let cy = solve_dense(&ata, &aty)?;
        let affine = [cx[0], cx[1], cx[2], cy[0], cy[1], cy[2]];
        if affine.iter().any(|c| !c.is_finite()) {
            return None;
        }
        let p = Polynomial2D {
            order,
            ax: cx[3..].to_vec(),
            ay: cy[3..].to_vec(),
        };
        if !p.is_well_formed() {
            return None;
        }
        Some((affine, p))
    }
}

/// Gauss–Jordan with partial pivoting; `None` on a singular system.
fn solve_dense(a: &[Vec<f64>], b: &[f64]) -> Option<Vec<f64>> {
    let n = b.len();
    let mut m: Vec<Vec<f64>> = a
        .iter()
        .zip(b.iter())
        .map(|(r, &bi)| {
            let mut row = r.clone();
            row.push(bi);
            row
        })
        .collect();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[pivot][col].abs() < 1e-14 {
            return None;
        }
        m.swap(col, pivot);
        let p = m[col][col];
        for v in m[col].iter_mut() {
            *v /= p;
        }
        for r in 0..n {
            if r != col {
                let f = m[r][col];
                if f != 0.0 {
                    for c in col..=n {
                        let sub = f * m[col][c];
                        m[r][c] -= sub;
                    }
                }
            }
        }
    }
    Some(m.iter().map(|row| row[n]).collect())
}

fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// Forward and inverse polynomial corrections around a linear model.
///
/// Forward: `ref = p + F(norm(p))`, `p = L(sub)`.
/// Inverse: `sub = L⁻¹(ref + I(norm(ref)))`.
/// `norm(x, y) = ((x − cx)/scale, (y − cy)/scale)`; the polynomials return
/// displacements in pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Distortion {
    pub order: u8,
    pub center: (f64, f64),
    pub scale: f64,
    pub forward: Polynomial2D,
    pub inverse: Polynomial2D,
}

impl Distortion {
    pub fn is_well_formed(&self) -> bool {
        self.scale.is_finite()
            && self.scale > 0.0
            && self.center.0.is_finite()
            && self.center.1.is_finite()
            && self.forward.order == self.order
            && self.inverse.order == self.order
            && self.forward.is_well_formed()
            && self.inverse.is_well_formed()
    }

    #[inline]
    pub fn norm(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.center.0) / self.scale,
            (y - self.center.1) / self.scale,
        )
    }

    /// Fits both directions on the residuals of `linear` over `pairs`.
    pub fn fit(
        order: u8,
        linear: &Linear,
        pairs: &[Pair],
        weights: Option<&[f64]>,
        center: (f64, f64),
        scale: f64,
    ) -> Option<Distortion> {
        if !(scale > 0.0) {
            return None;
        }
        linear.inverse()?;
        let norm = |x: f64, y: f64| ((x - center.0) / scale, (y - center.1) / scale);
        let fwd_samples: Vec<_> = pairs
            .iter()
            .map(|((x, y), (u, v))| {
                let (px, py) = linear.apply(*x, *y);
                (norm(px, py), (u - px, v - py))
            })
            .collect();
        // Inverse: we need I(ref) such that L⁻¹(ref + I) = sub, i.e.
        // ref + I = L(sub)  ⇒  I = L(sub) − ref.
        let inv_samples: Vec<_> = pairs
            .iter()
            .map(|((x, y), (u, v))| {
                let (px, py) = linear.apply(*x, *y);
                (norm(*u, *v), (px - u, py - v))
            })
            .collect();
        let forward = Polynomial2D::fit(order, &fwd_samples, weights)?;
        let inverse = Polynomial2D::fit(order, &inv_samples, weights)?;
        Some(Distortion {
            order,
            center,
            scale,
            forward,
            inverse,
        })
    }

    /// [`Distortion::fit`] with the forward residual's affine part folded
    /// into the linear model: the affine correction `p′ = p + a0 + A·norm(p)`
    /// is `p′ = (I + A/scale)·p + (a0 − A·c/scale)`, so `L′ = M·L`; both
    /// polynomial directions are then fitted around `L′`. The kind label is
    /// kept although a similarity may become a general affine.
    pub fn fit_joint(
        order: u8,
        linear: &Linear,
        pairs: &[Pair],
        weights: Option<&[f64]>,
        center: (f64, f64),
        scale: f64,
    ) -> Option<(Linear, Distortion)> {
        if !(scale > 0.0) {
            return None;
        }
        linear.inverse()?;
        let norm = |x: f64, y: f64| ((x - center.0) / scale, (y - center.1) / scale);
        let fwd_samples: Vec<_> = pairs
            .iter()
            .map(|((x, y), (u, v))| {
                let (px, py) = linear.apply(*x, *y);
                (norm(px, py), (u - px, v - py))
            })
            .collect();
        let (a, _) = Polynomial2D::fit_with_affine(order, &fwd_samples, weights)?;
        let (cx, cy) = center;
        let m = [
            [
                1.0 + a[1] / scale,
                a[2] / scale,
                a[0] - (a[1] * cx + a[2] * cy) / scale,
            ],
            [
                a[4] / scale,
                1.0 + a[5] / scale,
                a[3] - (a[4] * cx + a[5] * cy) / scale,
            ],
            [0.0, 0.0, 1.0],
        ];
        let refined = Linear {
            kind: linear.kind,
            m: mat_mul(&m, &linear.m),
        };
        refined.inverse()?;
        let distortion = Distortion::fit(order, &refined, pairs, weights, center, scale)?;
        Some((refined, distortion))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind, Pair};

    fn grid(w: f64, h: f64, step: f64) -> Vec<(f64, f64)> {
        let mut pts = Vec::new();
        let mut y = step / 2.0;
        while y < h {
            let mut x = step / 2.0;
            while x < w {
                pts.push((x, y));
                x += step;
            }
            y += step;
        }
        pts
    }

    /// Radial barrel distortion about the frame centre: r' = r (1 + k r²).
    fn barrel(x: f64, y: f64, cx: f64, cy: f64, k: f64) -> (f64, f64) {
        let (dx, dy) = (x - cx, y - cy);
        let r2 = dx * dx + dy * dy;
        (cx + dx * (1.0 + k * r2), cy + dy * (1.0 + k * r2))
    }

    #[test]
    fn term_order_is_stable_and_complete() {
        assert_eq!(term_exponents(2), vec![(2, 0), (1, 1), (0, 2)]);
        assert_eq!(term_exponents(3).len(), 7);
        assert_eq!(term_exponents(4).len(), 12);
    }

    #[test]
    fn polynomial_fit_reproduces_a_known_cubic() {
        // du = 0.5 u² − 0.25 u v + 0.1 v³ ; dv = −0.3 v² + 0.2 u² v
        let mut samples = Vec::new();
        for (u, v) in grid(2.0, 2.0, 0.1) {
            let (u, v) = (u - 1.0, v - 1.0);
            let du = 0.5 * u * u - 0.25 * u * v + 0.1 * v * v * v;
            let dv = -0.3 * v * v + 0.2 * u * u * v;
            samples.push(((u, v), (du, dv)));
        }
        let p = Polynomial2D::fit(3, &samples, None).unwrap();
        for ((u, v), (du, dv)) in &samples {
            let (eu, ev) = p.eval(*u, *v);
            assert!((eu - du).abs() < 1e-9 && (ev - dv).abs() < 1e-9);
        }
    }

    #[test]
    fn distortion_fit_recovers_barrel_within_a_hundredth_pixel() {
        let (w, h) = (6000.0, 4000.0);
        let (cx, cy) = (w / 2.0, h / 2.0);
        // The inverse of a cubic is not a cubic: an order-3 inverse neglects the
        // 3k²r⁵ term, which at the corner (r ≈ 3606) is 0.005 px for this k.
        let k = 5e-11; // ≈ 2.4 px of barrel at the corner
        let linear = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 12.5], [0.0, 1.0, -7.25], [0.0, 0.0, 1.0]],
        };
        let pairs: Vec<Pair> = grid(w, h, 250.0)
            .into_iter()
            .map(|(x, y)| {
                let (lx, ly) = linear.apply(x, y);
                ((x, y), barrel(lx, ly, cx, cy, k))
            })
            .collect();
        let d = Distortion::fit(3, &linear, &pairs, None, (cx, cy), w.max(h) / 2.0).unwrap();
        let map = PixelMap::with_distortion(linear, d).unwrap();
        let mut worst_fwd = 0.0f64;
        let mut worst_inv = 0.0f64;
        for ((x, y), (u, v)) in &pairs {
            let (fx, fy) = map.forward(*x, *y);
            worst_fwd = worst_fwd.max(((fx - u).powi(2) + (fy - v).powi(2)).sqrt());
            let (bx, by) = map.inverse(*u, *v);
            worst_inv = worst_inv.max(((bx - x).powi(2) + (by - y).powi(2)).sqrt());
        }
        assert!(worst_fwd < 0.01, "forward worst {worst_fwd}");
        assert!(worst_inv < 0.01, "inverse worst {worst_inv}");
    }

    #[test]
    fn too_few_samples_returns_none() {
        let samples = vec![((0.0, 0.0), (0.0, 0.0)); 3];
        assert!(Polynomial2D::fit(3, &samples, None).is_none());
    }

    #[test]
    fn nan_input_yields_none_not_a_panic() {
        let mut samples: Vec<((f64, f64), (f64, f64))> = grid(2.0, 2.0, 0.25)
            .into_iter()
            .map(|(u, v)| ((u - 1.0, v - 1.0), (0.1 * u, -0.1 * v)))
            .collect();
        samples[3].1 .0 = f64::NAN;
        assert!(Polynomial2D::fit(3, &samples, None).is_none());
        let linear = Linear::identity();
        let pairs: Vec<Pair> = vec![((0.0, 0.0), (0.0, 0.0)); 20];
        assert!(Distortion::fit(2, &linear, &pairs, None, (0.0, 0.0), f64::NAN).is_none());
    }

    #[test]
    fn fit_with_affine_recovers_linear_and_cubic_terms_jointly() {
        let pts = grid(1000.0, 800.0, 40.0);
        let samples: Vec<((f64, f64), (f64, f64))> = pts
            .iter()
            .map(|&(x, y)| {
                let (u, v) = ((x - 500.0) / 500.0, (y - 400.0) / 500.0);
                (
                    (u, v),
                    (
                        0.1 + 0.2 * u - 0.3 * v + 0.5 * u * u * u,
                        -0.05 + 0.1 * v + 0.25 * u * v * v,
                    ),
                )
            })
            .collect();
        let (a, p) = Polynomial2D::fit_with_affine(3, &samples, None).unwrap();
        for (got, want) in a.iter().zip([0.1, 0.2, -0.3, -0.05, 0.0, 0.1]) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        let terms = term_exponents(3);
        let k_u3 = terms.iter().position(|t| *t == (3, 0)).unwrap();
        let k_uv2 = terms.iter().position(|t| *t == (1, 2)).unwrap();
        assert!((p.ax[k_u3] - 0.5).abs() < 1e-9 && (p.ay[k_uv2] - 0.25).abs() < 1e-9);
        assert!(p
            .ax
            .iter()
            .enumerate()
            .all(|(k, c)| k == k_u3 || c.abs() < 1e-9));
    }

    #[test]
    fn fit_joint_absorbs_a_barrel_the_projected_linear_model_left_behind() {
        let (cx, cy, k) = (500.0, 400.0, 8e-9);
        let truth = Linear::from_flat(
            LinearKind::Similarity,
            [
                0.9998477, -0.0174524, 4.0, 0.0174524, 0.9998477, -3.0, 0.0, 0.0, 1.0,
            ],
        );
        let pairs: Vec<Pair> = grid(1000.0, 800.0, 25.0)
            .into_iter()
            .map(|s| {
                let (x, y) = truth.apply(s.0, s.1);
                (s, barrel(x, y, cx, cy, k))
            })
            .collect();
        // The least-squares linear model absorbs the barrel's linear projection.
        let projected =
            crate::geometry::linear::fit_linear(LinearKind::Homography, &pairs, None).unwrap();
        let (refined, d) =
            Distortion::fit_joint(3, &projected, &pairs, None, (cx, cy), 500.0).unwrap();
        let map = PixelMap::with_distortion(refined, d).unwrap();
        let mut worst = 0.0f64;
        for &(s, r) in &pairs {
            let (fx, fy) = map.forward(s.0, s.1);
            worst = worst.max(((fx - r.0).powi(2) + (fy - r.1).powi(2)).sqrt());
            let (bx, by) = map.inverse(r.0, r.1);
            // The inverse polynomial has no linear terms and the inverse of a
            // cubic field is not a cubic: a few milli-pixels on a 1 px barrel
            // is its second-order residual, far below anything the resampler
            // can resolve.
            assert!(
                (bx - s.0).abs() < 1e-2 && (by - s.1).abs() < 1e-2,
                "inverse {bx} {by} vs {s:?}"
            );
        }
        assert!(worst < 1e-3, "forward residual {worst}");
        let (plain, _) = (
            Distortion::fit(3, &projected, &pairs, None, (cx, cy), 500.0).unwrap(),
            0,
        );
        let plain_map = PixelMap::with_distortion(projected, plain).unwrap();
        let plain_worst = pairs
            .iter()
            .map(|&(s, r)| {
                let (fx, fy) = plain_map.forward(s.0, s.1);
                ((fx - r.0).powi(2) + (fy - r.1).powi(2)).sqrt()
            })
            .fold(0.0f64, f64::max);
        assert!(
            plain_worst > 0.1,
            "the un-balanced fit must leave the linear residual: {plain_worst}"
        );
    }
}
