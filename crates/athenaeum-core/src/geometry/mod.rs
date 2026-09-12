//! Plane geometry for image registration: linear transform models, the
//! two distortion layers (a SIP-style polynomial and, since M4c, a
//! thin-plate spline), a KD-tree for correspondences and a deterministic
//! RANSAC. Pure math, no I/O, ungated. Conventions: 0-based pixel
//! coordinates, integer = pixel centre; `forward` maps subject →
//! reference, `inverse` maps reference → subject.

pub mod eigen;
pub mod kdtree;
pub mod linear;
pub mod pixel_map;
pub mod polynomial;
pub mod ransac;
pub mod tps;

pub use kdtree::KdTree2;
pub use linear::{fit_affine, Linear, LinearKind, Pair};
// `TpsGrid`/`TpsGrids` are deliberately NOT re-exported (fix round 1,
// minor 10): the displacement grids are an implementation detail of
// `pixel_map`'s spline arm, reached through
// `DistortionModel::{forward,inverse}_displacement`. Nothing outside
// `geometry` names them, and `geometry::pixel_map::TpsGrid` stays
// available for anything that ever needs to.
pub use pixel_map::{DistortionModel, InverseMap, PixelMap};
pub use polynomial::{Distortion, Polynomial2D};
pub use ransac::{ransac_fit, refit_weighted, Quality, RansacConfig, RansacResult, RefitResult};
pub use tps::{select_nodes, ThinPlateSpline, TPS_GRID_PX, TPS_MAX_NODES, TPS_MIN_NODES};
