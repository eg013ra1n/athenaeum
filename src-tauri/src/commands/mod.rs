// Commands module - organized by domain
//
// This module structure replaces the monolithic commands.rs file (2,878 lines)
// with focused, domain-specific modules for better maintainability.

use crate::cache::CacheManager;
use crate::db::Database;
use crate::settings::SettingsManager;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Handle to track an active scan operation
pub struct ScanHandle {
    pub root_id: i64,
    pub cancel_flag: Arc<AtomicBool>,
}

/// App state containing database connection, settings manager, cache manager, and active scans
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub cache: Arc<Mutex<Option<CacheManager>>>,
    pub active_scans: Arc<Mutex<HashMap<i64, ScanHandle>>>,
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
