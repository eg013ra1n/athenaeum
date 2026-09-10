//! Drizzle stage (M3, spec §7). `geom` is the pure geometry the stage
//! driver (arriving in a later task) calls per source pixel: the
//! reference ↔ output-grid coordinate map, a source pixel's shrunk "drop"
//! corners, forward-mapping those corners subject → reference → output
//! grid through a frame's `PixelMap`, exact convex-quad ∩ unit-pixel
//! clipping (rulings R-M3-1/R-M3-2), and the 16×16 tabulated-kernel
//! micro-drop table for the `circle`/`gaussian` kernels (R-M3-3).

pub mod geom;

pub use crate::stacking::config::DrizzleKernel;
