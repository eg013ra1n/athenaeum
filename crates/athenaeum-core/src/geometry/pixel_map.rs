//! The complete subject ↔ reference pixel mapping a registered frame
//! carries: a linear model, its cached inverse, and an optional polynomial
//! distortion. Serialized as the `transform_json` of `registration_results`
//! (spec §9.1).

use serde::{Deserialize, Serialize};

use super::linear::Linear;
use super::polynomial::Distortion;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelMap {
    pub linear: Linear,
    pub linear_inv: Linear,
    pub distortion: Option<Distortion>,
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
                let (u, v) = d.norm(px, py);
                let (dx, dy) = d.forward.eval(u, v);
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
                let (u, v) = d.norm(x, y);
                let (dx, dy) = d.inverse.eval(u, v);
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
        serde_json::from_str(s)
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
}
