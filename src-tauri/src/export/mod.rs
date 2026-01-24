//! Export module for organizing frames and generating processing scripts
//!
//! This module handles:
//! - Collecting frames and their calibrations for a frame set
//! - Organizing files into folder structures for different targets (Siril, WBPP)
//! - Generating Siril processing scripts
//! - Direct execution of Siril CLI with progress tracking
//! - Pipeline execution with checkpoint/resume capability

pub mod data_collector;
pub mod file_organizer;
pub mod folder_structures;
pub mod models;
pub mod pipeline;
pub mod siril;

pub use data_collector::*;
pub use file_organizer::*;
#[allow(unused_imports)]
pub use models::*;
pub use pipeline::*;
