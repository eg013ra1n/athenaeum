//! Service layer — shared business logic callable from both Tauri and Axum.
//!
//! The `ServiceContext` holds all shared state. Each backend creates one at
//! startup and passes it (or references to it) into service functions.

use crate::cache::MemoryImageCache;
use crate::db::Database;
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::quad_index::QuadIndex;
use crate::settings::SettingsManager;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

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

/// Handle to track an active analysis operation.
pub struct AnalysisHandle {
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to track an active plate solve batch operation.
pub struct PlateSolveHandle {
    pub cancel_flag: Arc<AtomicBool>,
}

/// Shared application state accessible from any backend (Tauri, Axum, CLI).
pub struct ServiceContext {
    pub db: OnceLock<Database>,
    pub settings: Arc<SettingsManager>,
    pub memory_cache: Arc<Mutex<MemoryImageCache>>,
    pub active_scans: Arc<Mutex<HashMap<i64, ScanHandle>>>,
    pub active_exports: Arc<Mutex<HashMap<i64, ExportHandle>>>,
    pub active_analyses: Arc<Mutex<HashMap<i64, AnalysisHandle>>>,
    pub active_plate_solves: Arc<Mutex<HashMap<i64, PlateSolveHandle>>>,
    /// Lazy-loaded pre-built all-sky quad index for plate solving.
    /// None until the user builds the index or the app detects an existing one.
    pub quad_index: Arc<RwLock<Option<Arc<QuadIndex>>>>,
    /// Lazy-loaded deep-sky object catalog, used to auto-label plate-solve
    /// results (e.g. "M 42", "NGC 7000"). Parsed on first use, then cached.
    pub dso_catalog: Arc<RwLock<Option<Arc<DsoCatalog>>>>,
    pub image_pool: Arc<rayon::ThreadPool>,
}
