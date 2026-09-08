//! Image resampling for registration: separable interpolation kernels with
//! deringing clamps, an inverse-mapped gather warp, and the source-row
//! window a band of output rows needs. Pure math, no I/O, ungated.

pub mod kernels;
pub mod warp;
pub mod window;

pub use kernels::{Interpolation, Taps};
pub use warp::{sample_at, warp_rows, Plane};
pub use window::{source_window, SourceWindow};
