//! Export module for organizing frames into PixInsight WBPP folder structure

// Calibrated-light generation (resolve → calibrate → cosmetic → debayer →
// write). Render-gated: it streams pixels through `integration` and hands the
// mosaic to the rustafits debayer, both of which live behind `render`.
#[cfg(feature = "render")]
pub mod calibrated_generator;
pub mod data_collector;
pub mod file_organizer;
pub mod frame_set_queries;
pub mod models;
pub mod project_collector;

#[cfg(feature = "render")]
pub use calibrated_generator::{
    calibrated_output_filename, execute_generation, resolve_generation, CalibratedLightOptions,
    GeneratedLight, GenerationSpec,
};
pub use data_collector::*;
pub use file_organizer::*;
pub use project_collector::{collect_project_export_data, ProjectExportData};
