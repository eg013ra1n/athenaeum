//! The complete subject ↔ reference pixel mapping a registered frame
//! carries: a linear model, its cached inverse, and an optional distortion
//! layer — a polynomial ([`super::polynomial`]) or, since M4c, a
//! thin-plate spline ([`super::tps`]). Serialized as the `transform_json`
//! of `registration_results` (spec §9.1).

use std::sync::OnceLock;

use rayon::prelude::*;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::linear::Linear;
use super::polynomial::Distortion;
use super::tps::{ThinPlateSpline, TPS_GRID_PX};

/// Reference → subject mapping, the only direction a gather-style resampler
/// needs.
pub trait InverseMap: Sync {
    fn inverse(&self, x: f64, y: f64) -> (f64, f64);
}

impl InverseMap for Linear {
    /// A bare `Linear` used as an inverse map is applied as-is: callers hand
    /// in the already-inverted matrix (or the identity).
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        self.apply(x, y)
    }
}

/// A bilinear displacement grid over a [`DistortionModel::Tps`] map's
/// domain, sampled every [`TPS_GRID_PX`] pixels in both directions
/// (ruling R-M4c-5).
///
/// Evaluating the spline itself costs one `ln` per node per pixel — at
/// the 600-node cap that is 600 logarithms for every one of a 26 Mpx
/// frame's pixels, per direction. The grid pays that once per sample
/// point instead (a 1024th of the pixels) and reads a bilinear tap
/// between samples, which on a field as smooth as a registration
/// residual is exact to a small fraction of a milli-pixel.
///
/// Never serialized: `transform_json` stores the splines, and the grid is
/// rebuilt lazily on first use.
#[derive(Clone, Debug)]
pub struct TpsGrid {
    /// Sample origin (the domain's lower corner) in reference pixels.
    x0: f64,
    y0: f64,
    /// Domain's upper corner: a query is clamped into `[x0, x1] × [y0, y1]`
    /// before it is sampled, so a pixel outside the fitted region gets the
    /// nearest edge's displacement instead of a spline extrapolation —
    /// the same guard [`Distortion`]'s own `domain` applies.
    x1: f64,
    y1: f64,
    step: f64,
    nx: usize,
    ny: usize,
    forward: Vec<(f32, f32)>,
    inverse: Vec<(f32, f32)>,
}

impl TpsGrid {
    /// Samples both splines over `domain` at [`TPS_GRID_PX`] spacing,
    /// plus one margin cell each way so the last cell is complete. Rows
    /// are independent, so the build runs over `rayon`; it is a pure
    /// function of its inputs either way.
    pub fn build(
        forward: &ThinPlateSpline,
        inverse: &ThinPlateSpline,
        domain: [f64; 4],
    ) -> TpsGrid {
        let step = TPS_GRID_PX as f64;
        let [x0, y0, x1, y1] = domain;
        let nx = ((x1 - x0) / step).ceil().max(0.0) as usize + 2;
        let ny = ((y1 - y0) / step).ceil().max(0.0) as usize + 2;
        let rows: Vec<(Vec<(f32, f32)>, Vec<(f32, f32)>)> = (0..ny)
            .into_par_iter()
            .map(|j| {
                let y = y0 + j as f64 * step;
                let mut f = Vec::with_capacity(nx);
                let mut i = Vec::with_capacity(nx);
                for col in 0..nx {
                    let x = x0 + col as f64 * step;
                    let (fx, fy) = forward.displacement(x, y);
                    let (ix, iy) = inverse.displacement(x, y);
                    f.push((fx as f32, fy as f32));
                    i.push((ix as f32, iy as f32));
                }
                (f, i)
            })
            .collect();
        let mut fwd = Vec::with_capacity(nx * ny);
        let mut inv = Vec::with_capacity(nx * ny);
        for (f, i) in rows {
            fwd.extend_from_slice(&f);
            inv.extend_from_slice(&i);
        }
        TpsGrid {
            x0,
            y0,
            x1,
            y1,
            step,
            nx,
            ny,
            forward: fwd,
            inverse: inv,
        }
    }

    #[inline]
    fn sample(&self, grid: &[(f32, f32)], x: f64, y: f64) -> (f64, f64) {
        let fx = (x.clamp(self.x0, self.x1) - self.x0) / self.step;
        let fy = (y.clamp(self.y0, self.y1) - self.y0) / self.step;
        let i = (fx.floor().max(0.0) as usize).min(self.nx - 2);
        let j = (fy.floor().max(0.0) as usize).min(self.ny - 2);
        let tx = fx - i as f64;
        let ty = fy - j as f64;
        let row = j * self.nx + i;
        let (a, b) = (grid[row], grid[row + 1]);
        let (c, d) = (grid[row + self.nx], grid[row + self.nx + 1]);
        let lerp = |p: f64, q: f64, t: f64| p + (q - p) * t;
        let dx_top = lerp(a.0 as f64, b.0 as f64, tx);
        let dx_bot = lerp(c.0 as f64, d.0 as f64, tx);
        let dy_top = lerp(a.1 as f64, b.1 as f64, tx);
        let dy_bot = lerp(c.1 as f64, d.1 as f64, tx);
        (lerp(dx_top, dx_bot, ty), lerp(dy_top, dy_bot, ty))
    }

    /// Forward (subject → reference) displacement at a reference-space
    /// point.
    #[inline]
    pub fn forward_at(&self, x: f64, y: f64) -> (f64, f64) {
        self.sample(&self.forward, x, y)
    }

    /// Inverse (reference → subject) displacement at a reference pixel.
    #[inline]
    pub fn inverse_at(&self, x: f64, y: f64) -> (f64, f64) {
        self.sample(&self.inverse, x, y)
    }
}

/// The distortion layer sitting on top of a [`PixelMap`]'s linear model
/// (M4c, ruling R-M4c-6): the SIP-style polynomial M1 shipped, or a
/// thin-plate spline.
///
/// Tagged explicitly on the wire (`{"kind": "polynomial", …}` /
/// `{"kind": "tps", …}`). A stored `transform_json` written before M4c
/// carries no tag at all, and every one of those is a polynomial — see
/// the hand-written [`Deserialize`] below, which is the whole reason this
/// enum is not a plain derive.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DistortionModel {
    Polynomial(Distortion),
    Tps {
        forward: ThinPlateSpline,
        inverse: ThinPlateSpline,
        /// Reference-pixel box `[x0, y0, x1, y1]` the splines were fitted
        /// over (the nodes' bounding box, each side inflated by
        /// [`super::polynomial::DOMAIN_MARGIN`]). Evaluation clamps into
        /// it and the grid covers exactly it.
        domain: [f64; 4],
        /// Built on first use, never serialized, never part of equality.
        #[serde(skip)]
        grid: OnceLock<TpsGrid>,
    },
}

/// The cached grid is a pure function of the two splines and the domain,
/// so two models that agree on those three ARE equal — a map that has
/// already been evaluated must compare equal to a freshly deserialized
/// one.
impl PartialEq for DistortionModel {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DistortionModel::Polynomial(a), DistortionModel::Polynomial(b)) => a == b,
            (
                DistortionModel::Tps {
                    forward: f1,
                    inverse: i1,
                    domain: d1,
                    ..
                },
                DistortionModel::Tps {
                    forward: f2,
                    inverse: i2,
                    domain: d2,
                    ..
                },
            ) => f1 == f2 && i1 == i2 && d1 == d2,
            _ => false,
        }
    }
}

/// The `Tps` variant's wire shape, minus the tag and the cache.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TpsWire {
    forward: ThinPlateSpline,
    inverse: ThinPlateSpline,
    domain: [f64; 4],
}

impl<'de> Deserialize<'de> for DistortionModel {
    /// Reads the `kind` tag, defaulting to `"polynomial"` when it is
    /// absent: that is every `transform_json` M1–M4b ever wrote, and a
    /// derived internally-tagged enum would reject all of them.
    fn deserialize<D>(deserializer: D) -> Result<DistortionModel, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("polynomial")
            .to_string();
        match kind.as_str() {
            "polynomial" => Distortion::deserialize(&value)
                .map(DistortionModel::Polynomial)
                .map_err(D::Error::custom),
            "tps" => {
                let w = TpsWire::deserialize(&value).map_err(D::Error::custom)?;
                Ok(DistortionModel::tps(w.forward, w.inverse, w.domain))
            }
            other => Err(D::Error::custom(format!(
                "unknown distortion kind {other:?}: expected \"polynomial\" or \"tps\""
            ))),
        }
    }
}

impl DistortionModel {
    /// A thin-plate-spline layer with an empty grid cache.
    pub fn tps(
        forward: ThinPlateSpline,
        inverse: ThinPlateSpline,
        domain: [f64; 4],
    ) -> DistortionModel {
        DistortionModel::Tps {
            forward,
            inverse,
            domain,
            grid: OnceLock::new(),
        }
    }

    /// What [`PixelMap::from_json`] demands of a stored layer before it
    /// trusts it — see each arm's own `is_well_formed`.
    pub fn is_well_formed(&self) -> bool {
        match self {
            DistortionModel::Polynomial(d) => d.is_well_formed(),
            DistortionModel::Tps {
                forward,
                inverse,
                domain,
                ..
            } => {
                forward.is_well_formed()
                    && inverse.is_well_formed()
                    && domain.iter().all(|v| v.is_finite())
                    && domain[0] <= domain[2]
                    && domain[1] <= domain[3]
            }
        }
    }

    /// `"polynomial<order>"` / `"tps"` — the suffix
    /// `registration_results.model` carries.
    pub fn label(&self) -> String {
        match self {
            DistortionModel::Polynomial(d) => format!("polynomial{}", d.order),
            DistortionModel::Tps { .. } => "tps".to_string(),
        }
    }

    /// The grid, built on first use.
    #[inline]
    fn grid(&self) -> Option<&TpsGrid> {
        match self {
            DistortionModel::Polynomial(_) => None,
            DistortionModel::Tps {
                forward,
                inverse,
                domain,
                grid,
            } => Some(grid.get_or_init(|| TpsGrid::build(forward, inverse, *domain))),
        }
    }

    /// Forward displacement (pixels) at reference-space point `(x, y)` —
    /// the linear model's output.
    #[inline]
    pub fn forward_displacement(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.forward_displacement(x, y),
            DistortionModel::Tps { .. } => self
                .grid()
                .map(|g| g.forward_at(x, y))
                .unwrap_or((0.0, 0.0)),
        }
    }

    /// Inverse displacement (pixels) at reference pixel `(x, y)`.
    #[inline]
    pub fn inverse_displacement(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.inverse_displacement(x, y),
            DistortionModel::Tps { .. } => self
                .grid()
                .map(|g| g.inverse_at(x, y))
                .unwrap_or((0.0, 0.0)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelMap {
    pub linear: Linear,
    pub linear_inv: Linear,
    pub distortion: Option<DistortionModel>,
}

impl PixelMap {
    pub fn linear(linear: Linear) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap {
            linear,
            linear_inv,
            distortion: None,
        })
    }

    pub fn with_distortion(linear: Linear, distortion: Distortion) -> Option<PixelMap> {
        PixelMap::with_distortion_model(linear, DistortionModel::Polynomial(distortion))
    }

    pub fn with_distortion_model(linear: Linear, distortion: DistortionModel) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap {
            linear,
            linear_inv,
            distortion: Some(distortion),
        })
    }

    /// Subject pixel → reference pixel.
    #[inline]
    pub fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.linear.apply(x, y);
        match &self.distortion {
            None => (px, py),
            Some(d) => {
                let (dx, dy) = d.forward_displacement(px, py);
                (px + dx, py + dy)
            }
        }
    }

    /// Reference pixel → subject pixel.
    #[inline]
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match &self.distortion {
            None => (x, y),
            Some(d) => {
                let (dx, dy) = d.inverse_displacement(x, y);
                (x + dx, y + dy)
            }
        };
        self.linear_inv.apply(rx, ry)
    }

    pub fn is_flipped(&self) -> bool {
        self.linear.is_flipped()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PixelMap is always serializable")
    }

    pub fn from_json(s: &str) -> Result<PixelMap, serde_json::Error> {
        let map: PixelMap = serde_json::from_str(s)?;
        if let Some(d) = &map.distortion {
            if !d.is_well_formed() {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "malformed distortion: a polynomial's order must be 2..=4 with one finite coefficient per term, a positive finite scale and an ordered finite domain; a spline needs at least one node, matching weight vectors, a positive finite scale, a non-negative smoothing and an ordered finite domain",
                ));
            }
        }
        let linear_inv = map.linear.inverse().ok_or_else(|| {
            <serde_json::Error as serde::de::Error>::custom("singular linear transform")
        })?;
        Ok(PixelMap {
            linear: map.linear,
            linear_inv,
            distortion: map.distortion,
        })
    }
}

impl InverseMap for PixelMap {
    #[inline]
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        PixelMap::inverse(self, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind};
    use crate::geometry::polynomial::{Distortion, Polynomial2D, DOMAIN_MARGIN};
    use crate::geometry::ransac::SplitMix64;

    const TW: f64 = 1000.0;
    const TH: f64 = 800.0;

    /// A smooth field no polynomial of order 2..=4 can follow — the one
    /// the registration pin in `align.rs` uses too.
    fn wobble(x: f64, y: f64) -> (f64, f64) {
        (
            1.5 * (x / 300.0).sin() * (y / 250.0).cos(),
            1.5 * (x / 300.0).cos() * (y / 250.0).sin(),
        )
    }

    /// A TPS layer over `n` nodes scattered across the reference frame:
    /// forward = the field, inverse = its negation (an exact-enough
    /// inverse for a field this small, which is all these pins need).
    fn tps_model(seed: u64, n: usize) -> DistortionModel {
        let mut rng = SplitMix64(seed);
        let nodes: Vec<(f64, f64)> = (0..n)
            .map(|_| (rng.next_f64() * TW, rng.next_f64() * TH))
            .collect();
        let mut fx = Vec::new();
        let mut fy = Vec::new();
        for &(x, y) in &nodes {
            let (a, b) = wobble(x, y);
            fx.push(a);
            fy.push(b);
        }
        let ix: Vec<f64> = fx.iter().map(|v| -v).collect();
        let iy: Vec<f64> = fy.iter().map(|v| -v).collect();
        let forward = ThinPlateSpline::fit(&nodes, &fx, &fy, 0.0).expect("forward spline");
        let inverse = ThinPlateSpline::fit(&nodes, &ix, &iy, 0.0).expect("inverse spline");
        let b = forward.node_bounds();
        let (mx, my) = ((b[2] - b[0]) * DOMAIN_MARGIN, (b[3] - b[1]) * DOMAIN_MARGIN);
        DistortionModel::tps(
            forward,
            inverse,
            [b[0] - mx, b[1] - my, b[2] + mx, b[3] + my],
        )
    }

    /// Step 2: the 8-px bilinear grid the pixel path reads agrees with the
    /// exact spline. A registration residual field is smooth on the scale
    /// of 8 px, so the bilinear error is `~h²/8 · |f''|` — parts in ten
    /// thousand of a pixel, not hundredths.
    #[test]
    fn the_tps_grid_agrees_with_the_exact_spline() {
        let model = tps_model(21, 200);
        let (forward, inverse) = match &model {
            DistortionModel::Tps {
                forward, inverse, ..
            } => (forward.clone(), inverse.clone()),
            other => panic!("expected a spline: {other:?}"),
        };
        let mut rng = SplitMix64(555);
        let mut worst = 0.0f64;
        for _ in 0..500 {
            let (x, y) = (rng.next_f64() * TW, rng.next_f64() * TH);
            let (gx, gy) = model.forward_displacement(x, y);
            let (ex, ey) = forward.displacement(x, y);
            worst = worst.max((gx - ex).abs()).max((gy - ey).abs());
            let (gx, gy) = model.inverse_displacement(x, y);
            let (ex, ey) = inverse.displacement(x, y);
            worst = worst.max((gx - ex).abs()).max((gy - ey).abs());
        }
        assert!(worst < 0.02, "grid vs exact spline: worst {worst} px");
    }

    /// Step 2: a `Tps` map's JSON keeps the splines (to `serde_json`'s own
    /// one-ULP float parse — see
    /// `tps::tests::a_spline_round_trips_through_json`) and the grid is
    /// rebuilt on the far side, never stored.
    #[test]
    fn a_tps_map_round_trips_and_rebuilds_its_grid() {
        let linear = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.999, 0.013, -7.5],
                [-0.013, 0.999, 22.25],
                [1.1e-8, -3.0e-8, 1.0],
            ],
        };
        let map = PixelMap::with_distortion_model(linear, tps_model(22, 120)).unwrap();
        // Evaluate first, so the source map carries a BUILT grid: equality
        // and the JSON must both ignore it. Against a bit-identical map
        // that has NOT been evaluated, equality is exact.
        let before = map.forward(511.0, 407.0);
        let fresh = PixelMap::with_distortion_model(linear, tps_model(22, 120)).unwrap();
        assert_eq!(map, fresh, "the cached grid is not part of identity");

        let json = map.to_json();
        assert!(json.contains("\"kind\":\"tps\""), "{json}");
        assert!(!json.contains("grid"), "the grid must never be serialized");
        let back = PixelMap::from_json(&json).unwrap();
        match back.distortion.as_ref().unwrap() {
            DistortionModel::Tps {
                forward, domain, ..
            } => {
                assert_eq!(forward.nodes.len(), 120);
                assert_eq!(domain.len(), 4);
            }
            other => panic!("expected a spline: {other:?}"),
        }
        let after = back.forward(511.0, 407.0);
        assert!(
            (before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9,
            "{before:?} vs {after:?}"
        );
        // Forward then inverse returns the subject pixel: the two splines
        // are independent fits, so this is not an algebraic identity — it
        // is the 0.1 px-class agreement a registered frame relies on.
        let (u, v) = back.forward(311.0, 222.0);
        let (x, y) = back.inverse(u, v);
        assert!(
            (x - 311.0).abs() < 0.05 && (y - 222.0).abs() < 0.05,
            "round trip landed at ({x}, {y})"
        );
    }

    /// Ruling R-M4c-6: the tag defaults to `polynomial`, so every
    /// `transform_json` written before M4c still decodes — here as the
    /// literal fixture string an M1 row carries — and an unknown kind is
    /// refused loudly rather than silently treated as "no distortion".
    #[test]
    fn an_untagged_distortion_is_a_polynomial_and_an_unknown_kind_is_refused() {
        const M1_ROW: &str = "{\"linear\":{\"kind\":\"affine\",\"m\":[[1.0,0.0,3.5],[0.0,1.0,-2.5],[0.0,0.0,1.0]]},\
             \"linearInv\":{\"kind\":\"affine\",\"m\":[[1.0,0.0,-3.5],[0.0,1.0,2.5],[0.0,0.0,1.0]]},\
             \"distortion\":{\"order\":2,\"center\":[500.0,400.0],\"scale\":500.0,\
             \"domain\":[-1.1,-1.1,1.1,1.1],\
             \"forward\":{\"order\":2,\"ax\":[0.1,0.2,0.3],\"ay\":[0.4,0.5,0.6]},\
             \"inverse\":{\"order\":2,\"ax\":[-0.1,-0.2,-0.3],\"ay\":[-0.4,-0.5,-0.6]}}}";
        let map = PixelMap::from_json(M1_ROW).expect("an M1 transform_json still decodes");
        match map.distortion.as_ref().unwrap() {
            DistortionModel::Polynomial(d) => {
                assert_eq!(d.order, 2);
                assert_eq!(d.center, (500.0, 400.0));
            }
            other => panic!("expected a polynomial: {other:?}"),
        }
        assert_eq!(map.distortion.as_ref().unwrap().label(), "polynomial2");
        // The same row with an explicit tag is the same map — these are
        // short decimals, so equality here IS exact.
        let tagged = M1_ROW.replace(
            "\"distortion\":{\"order\":2",
            "\"distortion\":{\"kind\":\"polynomial\",\"order\":2",
        );
        assert_ne!(tagged, M1_ROW, "the splice must have matched");
        assert_eq!(PixelMap::from_json(&tagged).unwrap(), map);
        // An unknown kind is an error, not a shrug.
        let alien = M1_ROW.replace(
            "\"distortion\":{\"order\":2",
            "\"distortion\":{\"kind\":\"wavelet\",\"order\":2",
        );
        let err = PixelMap::from_json(&alien).unwrap_err().to_string();
        assert!(err.contains("unknown distortion kind"), "{err}");
    }

    /// A malformed spline is refused by `from_json` the same way a
    /// malformed polynomial is.
    #[test]
    fn from_json_rejects_a_malformed_spline() {
        let map = PixelMap::with_distortion_model(Linear::identity(), tps_model(23, 40)).unwrap();
        let good = map.to_json();
        // An inverted domain.
        let inverted = good.replace(
            "\"domain\":[",
            "\"domain\":[9e9,9e9,-9e9,-9e9],\"unused\":[",
        );
        assert_ne!(inverted, good, "the splice must have matched");
        assert!(PixelMap::from_json(&inverted).is_err());
        // A weight vector that does not match the node list.
        let wx_at = good.find("\"wx\":[").expect("wx in the json");
        let wx_end = good[wx_at..].find("],").expect("wx closes") + wx_at + 2;
        let short = format!("{}\"wx\":[1.0],{}", &good[..wx_at], &good[wx_end..]);
        assert_ne!(short, good, "the splice must have matched");
        assert!(PixelMap::from_json(&short).is_err(), "{short}");
    }

    #[test]
    fn linear_map_round_trips_and_serializes() {
        let l = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.992, 0.0387, -23.54],
                [-0.0383, 0.9922, 136.37],
                [8.8e-8, -2.5e-8, 1.0],
            ],
        };
        let map = PixelMap::linear(l).unwrap();
        let (u, v) = map.forward(1234.5, 678.25);
        let (x, y) = map.inverse(u, v);
        assert!((x - 1234.5).abs() < 1e-9 && (y - 678.25).abs() < 1e-9);
        let json = map.to_json();
        assert!(json.contains("\"kind\":\"homography\""));
        assert!(json.contains("\"distortion\":null"));
        let back = PixelMap::from_json(&json).unwrap();
        let (u2, v2) = back.forward(1234.5, 678.25);
        assert!((u - u2).abs() < 1e-12 && (v - v2).abs() < 1e-12);
    }

    #[test]
    fn singular_linear_is_rejected() {
        let l = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 2.0, 0.0], [2.0, 4.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(PixelMap::linear(l).is_none());
    }

    #[test]
    fn identity_linear_implements_inverse_map() {
        let id = Linear::identity();
        let m: &dyn InverseMap = &id;
        assert_eq!(m.inverse(3.0, 4.0), (3.0, 4.0));
    }

    #[test]
    fn from_json_rejects_a_malformed_distortion() {
        let l = Linear::identity();
        let good = PixelMap::linear(l).unwrap().to_json();
        // Splice in a distortion whose order is out of range and whose
        // coefficient vectors are too short for it.
        let bad = good.replace(
            "\"distortion\":null",
            "\"distortion\":{\"order\":5,\"center\":[0.0,0.0],\"scale\":1.0,\
             \"forward\":{\"order\":5,\"ax\":[1.0],\"ay\":[1.0]},\
             \"inverse\":{\"order\":5,\"ax\":[1.0],\"ay\":[1.0]}}",
        );
        assert_ne!(good, bad, "the splice must have matched");
        assert!(PixelMap::from_json(&bad).is_err());
        // A well-formed distortion round-trips.
        let d = Distortion {
            order: 2,
            center: (10.0, 20.0),
            scale: 100.0,
            domain: Some([-1.0, -1.0, 1.0, 1.0]),
            forward: Polynomial2D {
                order: 2,
                ax: vec![0.1, 0.2, 0.3],
                ay: vec![0.4, 0.5, 0.6],
            },
            inverse: Polynomial2D {
                order: 2,
                ax: vec![-0.1, -0.2, -0.3],
                ay: vec![-0.4, -0.5, -0.6],
            },
        };
        assert!(d.is_well_formed());
        let map = PixelMap::with_distortion(l, d).unwrap();
        let back = PixelMap::from_json(&map.to_json()).unwrap();
        assert_eq!(map, back);
    }

    #[test]
    fn from_json_recomputes_a_corrupted_stored_inverse() {
        let linear = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 12.5], [0.0, 1.0, -7.25], [0.0, 0.0, 1.0]],
        };
        let map = PixelMap::linear(linear).unwrap();
        let good = map.to_json();
        let stored_inv = format!(
            "\"linearInv\":{}",
            serde_json::to_string(&map.linear_inv).unwrap()
        );
        assert!(
            good.contains(&stored_inv),
            "expected to find the stored inverse in the JSON"
        );
        // Splice in a wrong-but-well-formed inverse (a similarity that scales
        // by 2) — a stale/hand-edited inverse must never survive `from_json`.
        let corrupted = good.replace(
            &stored_inv,
            "\"linearInv\":{\"kind\":\"similarity\",\"m\":[[2.0,0.0,0.0],[0.0,2.0,0.0],[0.0,0.0,1.0]]}",
        );
        assert_ne!(good, corrupted, "the splice must have matched");
        let back = PixelMap::from_json(&corrupted).unwrap();
        assert_eq!(back.linear_inv, linear.inverse().unwrap());
    }

    #[test]
    fn distortion_json_without_a_domain_is_unbounded_and_an_inverted_domain_is_rejected() {
        let l = Linear::identity();
        let good = PixelMap::linear(l).unwrap().to_json();
        // A transform_json written before the domain field existed.
        let old = good.replace(
            "\"distortion\":null",
            "\"distortion\":{\"order\":2,\"center\":[0.0,0.0],\"scale\":1.0,\
             \"forward\":{\"order\":2,\"ax\":[0.0,0.0,0.0],\"ay\":[0.0,0.0,0.0]},\
             \"inverse\":{\"order\":2,\"ax\":[0.0,0.0,0.0],\"ay\":[0.0,0.0,0.0]}}",
        );
        assert_ne!(good, old, "the splice must have matched");
        let map = PixelMap::from_json(&old).unwrap();
        match map.distortion.as_ref().unwrap() {
            DistortionModel::Polynomial(d) => assert_eq!(d.domain, None),
            other => panic!("an untagged distortion must decode as a polynomial: {other:?}"),
        }
        assert_eq!(map.inverse(3.0, 4.0), (3.0, 4.0));
        let inverted = old.replace(
            "\"scale\":1.0,",
            "\"scale\":1.0,\"domain\":[1.0,0.0,-1.0,0.0],",
        );
        assert_ne!(old, inverted, "the splice must have matched");
        assert!(PixelMap::from_json(&inverted).is_err());
    }
}
