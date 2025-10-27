// Export module
// Handles file export with path templating

use crate::models::Frame;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Resolve export template tokens to generate output path
pub fn resolve_template(_template: &str, _frame: &Frame) -> Result<PathBuf> {
    // TODO: Implement token resolution:
    // Core tokens:
    // - {OBJECT} - object name
    // - {DATE-OBS:%Y-%m-%d} - date with strftime formatting
    // - {TELESCOP} - telescope name
    // - {INSTRUME} - instrument name
    // - {EXPTIME} - exposure time
    // - {FILTER} - filter name
    // - {IMAGETYP} - image type
    // - {FRAME_FOLDER} - derived from IMAGETYP (Lights or Calibration/*)
    // - {SEQ:%03d} - sequence number for collision resolution
    //
    // Transformations:
    // - {OBJECT|Unknown} - fallback for missing values
    // - {INSTRUME:slug} - slugify (lowercase, replace spaces with -)
    // - Case transforms

    unimplemented!("Template resolution not yet implemented")
}

/// Preview export paths before copying
pub fn preview_export(
    _frame_ids: &[i64],
    _template: &str,
    _output_root: &Path,
) -> Result<Vec<ExportPreview>> {
    // TODO: Generate preview of all export paths
    // Detect collisions and apply Skip/Overwrite/Rename policies

    unimplemented!("Export preview not yet implemented")
}

/// Execute export operation
pub fn execute_export(
    _exports: &[ExportPreview],
    _output_root: &Path,
) -> Result<ExportResult> {
    // TODO: Copy files to generated paths
    // Handle collisions according to policy
    // Return completion report

    unimplemented!("Export execution not yet implemented")
}

#[derive(Debug, Clone)]
pub struct ExportPreview {
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub conflict: Option<ConflictResolution>,
}

#[derive(Debug, Clone)]
pub enum ConflictResolution {
    Skip,
    Overwrite,
    Rename(String), // new filename
}

pub struct ExportResult {
    pub files_exported: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}
