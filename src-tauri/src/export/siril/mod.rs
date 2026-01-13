//! Siril integration module
//!
//! Handles Siril script generation and CLI execution for
//! calibration, registration, and stacking.
//!
//! Phase 4: Added new script generation functions that use
//! ExportGroup structure with subgroups and MasterCreationPlan.
//!
//! Phase 5: Added pipeline orchestration for automated export execution.

pub mod cli_runner;
pub mod script_generator;
pub mod templates;

#[allow(unused_imports)]
pub use cli_runner::{
    collect_calibrated_frames, find_siril_cli, run_export_pipeline, run_siril_script,
    CollectedFrames, CollectedFrame, FrameGroup, DEFAULT_SIRIL_CLI,
};
#[allow(unused_imports)]
pub use script_generator::{
    generate_combined_script, generate_scripts, generate_scripts_v2, generate_scripts_v3,
};
// Note: templates are used internally by script_generator
