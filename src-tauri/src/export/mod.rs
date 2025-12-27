//! Export module for organizing frames and generating processing scripts
//!
//! This module handles:
//! - Collecting frames and their calibrations for a frame set
//! - Organizing files into folder structures
//! - Generating Siril processing scripts
//! - Direct execution of Siril CLI with progress tracking

pub mod data_collector;
pub mod file_organizer;
pub mod models;
pub mod siril;

pub use data_collector::*;
pub use file_organizer::*;
pub use models::*;
