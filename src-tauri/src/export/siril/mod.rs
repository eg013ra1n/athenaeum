//! Siril integration module
//!
//! Handles Siril script generation and CLI execution for
//! calibration, registration, and stacking.
//!
//! Phase 4: Added new script generation functions that use
//! ExportGroup structure with subgroups and MasterCreationPlan.

pub mod cli_runner;
pub mod script_generator;
pub mod templates;

pub use cli_runner::*;
#[allow(unused_imports)]
pub use script_generator::{
    generate_combined_script, generate_scripts, generate_scripts_v2, generate_scripts_v3,
};
// Note: templates are used internally by script_generator
