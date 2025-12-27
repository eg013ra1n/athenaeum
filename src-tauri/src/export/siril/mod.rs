//! Siril integration module
//!
//! Handles Siril script generation and CLI execution for
//! calibration, registration, and stacking.

pub mod cli_runner;
pub mod script_generator;
pub mod templates;

pub use cli_runner::*;
pub use script_generator::*;
pub use templates::*;
