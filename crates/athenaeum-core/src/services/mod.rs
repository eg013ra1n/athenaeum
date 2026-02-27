//! Service layer — shared business logic callable from both Tauri and Axum.
//!
//! The `ServiceContext` holds all shared state. Each backend creates one at
//! startup and passes it (or references to it) into service functions.

use crate::cache::MemoryImageCache;
use crate::db::Database;
use crate::settings::SettingsManager;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Handle to track an active scan operation.
pub struct ScanHandle {
    #[allow(dead_code)]
    pub root_id: i64,
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to track an active export operation.
pub struct ExportHandle {
    pub cancel_flag: Arc<AtomicBool>,
}

/// Shared application state accessible from any backend (Tauri, Axum, CLI).
pub struct ServiceContext {
    pub db: Mutex<Option<Database>>,
    pub settings: Arc<SettingsManager>,
    pub memory_cache: Arc<Mutex<MemoryImageCache>>,
    pub active_scans: Arc<Mutex<HashMap<i64, ScanHandle>>>,
    pub active_exports: Arc<Mutex<HashMap<i64, ExportHandle>>>,
    pub image_pool: Arc<rayon::ThreadPool>,
}
