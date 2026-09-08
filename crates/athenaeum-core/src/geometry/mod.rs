//! Plane geometry for image registration: linear transform models, the
//! polynomial distortion layer, a KD-tree for correspondences and a
//! deterministic RANSAC. Pure math, no I/O, ungated. Conventions: 0-based
//! pixel coordinates, integer = pixel centre; `forward` maps subject →
//! reference, `inverse` maps reference → subject.

pub mod eigen;
pub mod kdtree;
pub mod linear;
pub mod pixel_map;
pub mod polynomial;
pub mod ransac;

pub use kdtree::KdTree2;
pub use linear::{Linear, LinearKind, Pair};
pub use pixel_map::{InverseMap, PixelMap};
pub use polynomial::{Distortion, Polynomial2D};
pub use ransac::{ransac_fit, refit_weighted, Quality, RansacConfig, RansacResult, RefitResult};
