//! Service layer — shared business logic callable from both Tauri and Axum.
//!
//! The `ServiceContext` holds all shared state. Each backend creates one at
//! startup and passes it (or references to it) into service functions.

pub mod operation_queue;

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

/// Handle to track an active archive operation (ZIP archive feature).
/// Only one archive operation can run at a time, but the map allows
/// querying state by operation_id.
pub struct ArchiveHandle {
    pub operation_id: i64,
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
    /// Active archive operations (ZIP archive feature). Capped at one at a
    /// time by command-layer enforcement; HashMap form keeps the same shape
    /// as the other active-handle maps for consistency.
    pub active_archives: Arc<Mutex<HashMap<i64, ArchiveHandle>>>,
    /// Lazy-loaded pre-built all-sky quad index for plate solving.
    /// None until the user builds the index or the app detects an existing one.
    /// Phase 4 will remove this once the solvemyastro adapter is the only path.
    pub quad_index: Arc<RwLock<Option<Arc<QuadIndex>>>>,
    /// Lazy-loaded deep-sky object catalog, used to auto-label plate-solve
    /// results (e.g. "M 42", "NGC 7000"). Parsed on first use, then cached.
    pub dso_catalog: Arc<RwLock<Option<Arc<DsoCatalog>>>>,
    /// Lazy-opened solvemyastro star-cache (`stars.smac`). Loaded on first
    /// solve attempt and shared read-only across all worker threads.
    /// `None` until the cache file is located (opens from the `smac_gaia`
    /// subdir of the app-data catalogs dir). If the file is absent the solve
    /// command returns an actionable error.
    pub star_cache: Arc<RwLock<Option<Arc<solvemyastro::StarCache>>>>,
    /// Optional bright sub-catalog (G<16 hybrid floor + density top-up;
    /// built via `solvemyastro build-bright-cache`). When present, the
    /// plate-solve hot path uses it for fast quad matching with
    /// auto-fallback to `star_cache`. `None` if no bright cache is
    /// available — production runs on the deep cache alone.
    pub bright_cache: Arc<RwLock<Option<Arc<solvemyastro::StarCache>>>>,
    pub image_pool: Arc<rayon::ThreadPool>,
    /// Single serialized worker queue shared by ZIP archive + file ops.
    /// Created at startup; lives for the process lifetime.
    pub operation_queue: operation_queue::OperationQueue,
}
