//! Image resampling for registration: separable interpolation kernels with
//! deringing clamps, an inverse-mapped gather warp, and the source-row
//! window a band of output rows needs. Pure math, no I/O, ungated.

pub mod kernels;

pub use kernels::{Interpolation, Taps};
