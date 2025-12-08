// Commands module - organized by domain
//
// This module structure replaces the monolithic commands.rs file (2,878 lines)
// with focused, domain-specific modules for better maintainability.

use crate::cache::CacheManager;
use crate::db::Database;
use crate::settings::SettingsManager;
use std::sync::{Arc, Mutex};

/// App state containing database connection, settings manager, and cache manager
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub cache: Arc<Mutex<Option<CacheManager>>>,
}

pub mod core;
pub mod scan_roots;
pub mod files;
pub mod settings;
pub mod frame_sets;
pub mod calibration;
pub mod duplicates;
pub mod cache;
pub mod spatial;
pub mod utils;

// Re-export all commands for convenient access
pub use core::*;
pub use scan_roots::*;
pub use files::*;
pub use settings::*;
pub use frame_sets::*;
pub use calibration::*;
pub use duplicates::*;
pub use cache::*;
pub use spatial::*;
