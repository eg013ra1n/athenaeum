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

#[allow(unused_imports)]
pub use cli_runner::{
    collect_calibrated_frames, find_siril_cli, run_export_pipeline, run_siril_script,
    CollectedFrames, CollectedFrame, FrameGroup, DEFAULT_SIRIL_CLI,
};
#[allow(unused_imports)]
pub use script_generator::generate_scripts_v3;
