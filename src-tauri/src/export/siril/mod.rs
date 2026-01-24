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
    collect_calibrated_frames, find_siril_cli, run_export_pipeline, run_export_pipeline_v2,
    run_siril_script, CollectedFrames, CollectedFrame, FrameGroup, DEFAULT_SIRIL_CLI,
    StackingGroup, collect_and_group_registered_frames, collect_registered_frames_from_branches,
    prepare_stacking_group_directory, prepare_global_registration_directories,
    generate_global_registration_script, generate_group_stacking_script, generate_all_stacking_scripts,
    BranchRegisteredFrames, copy_globally_registered_to_stacking_dirs,
};
#[allow(unused_imports)]
pub use script_generator::{generate_scripts_v3, generate_register_branches_script};
