//! Starloom: the astrophotography stacking kernel.
//!
//! It turns light frames into a stacked master: it aligns frames (geometry and resampling), calibrates
//! lights against master darks, biases and flats, measures stars, registers, rejects outlier pixels, integrates,
//! drizzles and weights, and writes FITS and XISF files. The crate is built in three layers, each a cargo feature:
//! - base (no feature): geometry, resampling, the FITS/XISF writer core and small platform helpers;
//! - `engine`: integration, light calibration and cosmetic correction (needs `rustafits`);
//! - `stack`: registration, measurement, rejection, drizzle and weighting (needs `solvemyastro`).
//!
//! # Threads
//!
//! Starloom never builds a rayon pool outside its tests (contract SL-9); it runs on the caller's pool. A function that
//! takes a `pool` runs there; one that runs bare parallel iterators runs on the caller's current rayon pool, so the
//! caller installs it first.
//!
//! # Units
//!
//! - **DN**: the raw sensor value, 0 to 65535 for a 16-bit camera. Calibration frames and master calibration frames
//!   (bias, dark, flat) are in DN.
//! - **normalized**: the [0, 1] scale of a calibrated or stacked plane, DN divided by the scale divisor the caller
//!   chose (65535 for 16-bit data, 1 for float data already in physical values).
//! - **pixels**: image coordinates, 0-based, with integer values at pixel centres; a registration maps subject-frame
//!   pixels to reference-frame pixels.
//! - **e-**: electrons. A gain is in e- per normalized unit.
//! - **seconds** and **bytes**: durations and memory sizes, unless a name says milliseconds or MB.
//!
//! # Versions and goldens
//!
//! Every stage's output is tied to a public version constant: a change to a stage's output bits bumps that stage's
//! version. The README's "Versions" table lists the constants and what each enters (a hash, a file header or nothing),
//! and `tests/stage_goldens.rs` pins every stage's output on fixed inputs so a change that forgets the bump fails.

#![deny(missing_docs)]

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
