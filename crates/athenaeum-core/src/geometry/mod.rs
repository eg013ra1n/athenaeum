//! Plane geometry for image registration: linear transform models, the
//! polynomial distortion layer, a KD-tree for correspondences and a
//! deterministic RANSAC. Pure math, no I/O, ungated. Conventions: 0-based
//! pixel coordinates, integer = pixel centre; `forward` maps subject →
//! reference, `inverse` maps reference → subject.

pub mod eigen;
pub mod linear;

pub use linear::{Linear, LinearKind, Pair};
