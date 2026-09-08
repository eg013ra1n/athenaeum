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
    /// Evaluates `(Σ ax_k u^i v^j, Σ ay_k u^i v^j)` with power tables
    /// (`order` is at most 4, so this is a dozen multiplies).
    #[inline]
    pub fn eval(&self, u: f64, v: f64) -> (f64, f64) {
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
        Some(Polynomial2D { order, ax, ay })
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
        let pivot =
            (col..n).max_by(|&i, &j| m[i][col].abs().partial_cmp(&m[j][col].abs()).unwrap())?;
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
        if scale <= 0.0 {
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
}
