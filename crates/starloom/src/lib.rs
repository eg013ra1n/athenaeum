//! Starloom: the astrophotography stacking kernel.
//!
//! The crate is built in three layers, each a cargo feature:
//! - base (no feature): geometry, resampling, the FITS/XISF writer core and small platform helpers;
//! - `engine`: integration, light calibration and cosmetic correction (needs `rustafits`);
//! - `stack`: registration, measurement, rejection, drizzle and weighting (needs `solvemyastro`).
//!
//! Starloom never builds a rayon pool; it runs on the caller's pool.

// Base layer.
pub mod calibration_library;
pub mod fits_writer;
pub mod fs_util;
pub mod geometry;
pub mod orientation;
pub mod resample;
pub mod storage_class;
pub mod sys;

// `engine` layer.
#[cfg(feature = "engine")]
pub mod integration;
#[cfg(feature = "engine")]
pub use astroimage::star_measure;

// `stack` layer.
#[cfg(feature = "stack")]
pub mod stacking;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
