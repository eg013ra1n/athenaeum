// Commands module - organized by domain
//
// This module structure replaces the monolithic commands.rs file (2,878 lines)
// with focused, domain-specific modules for better maintainability.

use crate::cache::{CacheManager, MemoryImageCache};
use crate::db::Database;
use crate::settings::SettingsManager;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Handle to track an active scan operation
pub struct ScanHandle {
    #[allow(dead_code)] // Kept for debugging - HashMap key already provides root_id
    pub root_id: i64,
    pub cancel_flag: Arc<AtomicBool>,
}

/// App state containing database connection, settings manager, cache manager, and active scans
pub struct AppState {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub cache: Arc<Mutex<Option<Arc<CacheManager>>>>,
    pub memory_cache: Arc<Mutex<MemoryImageCache>>,
    pub active_scans: Arc<Mutex<HashMap<i64, ScanHandle>>>,
    pub image_pool: Arc<rayon::ThreadPool>,
    /// Serializes image processing so only one conversion uses the rayon pool at a time
    pub image_semaphore: Arc<tokio::sync::Semaphore>,
}

pub mod core;
pub mod scan_roots;
pub mod files;
pub mod settings;
pub mod frame_sets;
pub mod calibration;
pub mod duplicates;
pub mod missing_files;
pub mod cache;
pub mod spatial;
pub mod calendar;
pub mod export;
pub mod utils;

// Re-export all commands for convenient access
pub use core::*;
pub use scan_roots::*;
pub use files::*;
pub use settings::*;
pub use frame_sets::*;
pub use calibration::*;
pub use duplicates::*;
pub use missing_files::*;
pub use cache::*;
pub use spatial::*;
pub use calendar::*;
pub use export::*;
