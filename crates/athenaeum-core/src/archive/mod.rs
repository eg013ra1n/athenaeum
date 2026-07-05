//! Archive feature — moves a frame set's lights (and chosen calibrations)
//! into one zip per frame type inside a user-chosen archive root.
//!
//! See `docs/superpowers/specs/2026-04-29-archive-feature-design.md`.

pub mod models;
pub mod db;
pub mod staging;
pub mod zip_writer;
pub mod zip_reader;
pub mod shared_calibration;
pub mod path_layout;
pub mod planner;
pub mod executor;
pub mod rollback;
pub mod resume;
pub mod restore;
pub mod root;

pub use models::*;
pub use root::{migrate_legacy_archive_root, resolve_archive_root};
