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
    execute_generation, resolve_generation, resolve_generation_cached, DivisorCache, GeneratedLight,
    GenerationSpec,
};
// The generator's run options and its output-naming rule live in `models`, not
// behind the `render` gate: the ungated mode transform names those files and
// records the debayer decision (see the note beside them there).
pub use models::{calibrated_output_filename, CalibratedLightOptions};
pub use data_collector::*;
pub use file_organizer::*;
pub use project_collector::{collect_project_export_data, ProjectExportData};
