//! Service layer — shared business logic callable from both Tauri and Axum.
//!
//! The `ServiceContext` holds all shared state. Each backend creates one at
//! startup and passes it (or references to it) into service functions.

pub mod compute_queue;
pub mod operation_queue;

use crate::cache::MemoryImageCache;
use crate::db::Database;
// DsoCatalog lives in the render+solver-gated plate_solve module; the
// `dso_catalog` cache field below is gated to match.
#[cfg(all(feature = "render", feature = "solver"))]
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::settings::SettingsManager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, OnceLock};
// RwLock is used only by the solver-gated cache fields below.
#[cfg(feature = "solver")]
use std::sync::RwLock;

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

/// Handle to track an active frame-set registration operation.
pub struct RegistrationHandle {
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to track an active archive operation (ZIP archive feature).
/// Only one archive operation can run at a time, but the map allows
/// querying state by operation_id.
pub struct ArchiveHandle {
    pub operation_id: i64,
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to track an active master-build operation (Task 12). Keyed by the
/// SOURCE calibration_set id (not the resulting master set id — that doesn't
/// exist yet while the build is running).
pub struct MasterBuildHandle {
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to track an active light-calibration batch (B5 Task 5). Keyed by the
/// frame-set id being calibrated — only one light-cal run per frame set at a
/// time (mirrors `MasterBuildHandle`'s duplicate-run guard). `job_id` is the
/// compute-queue ticket, stored by the worker thread once
/// `ComputeQueue::acquire` returns so `cancel_light_calibration` can promptly
/// drop a still-queued job; `0` until acquired (setting `cancel_flag` alone is
/// always sufficient — the queue's `acquire` loop polls it — so the job id is
/// a best-effort promptness aid, not a correctness requirement).
pub struct LightCalHandle {
    pub cancel_flag: Arc<AtomicBool>,
    pub job_id: Arc<AtomicI64>,
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
    /// Active registration operations, keyed by `frames_set_id`.
    pub active_registrations: Arc<Mutex<HashMap<i64, RegistrationHandle>>>,
    /// Active archive operations (ZIP archive feature). Capped at one at a
    /// time by command-layer enforcement; HashMap form keeps the same shape
    /// as the other active-handle maps for consistency.
    pub active_archives: Arc<Mutex<HashMap<i64, ArchiveHandle>>>,
    /// Active master-build operations (Task 12), keyed by SOURCE calibration
    /// set id. Only one build per source set at a time.
    pub active_master_builds: Arc<Mutex<HashMap<i64, MasterBuildHandle>>>,
    /// Active light-calibration batches (B5 Task 5), keyed by frame-set id.
    /// Only one light-cal run per frame set at a time.
    pub active_light_cal: Arc<Mutex<HashMap<i64, LightCalHandle>>>,
    /// Lazy-loaded deep-sky object catalog, used to auto-label plate-solve
    /// results (e.g. "M 42", "NGC 7000"). Parsed on first use, then cached.
    /// Gated to match `DsoCatalog`'s home in the render+solver plate_solve module.
    #[cfg(all(feature = "render", feature = "solver"))]
    pub dso_catalog: Arc<RwLock<Option<Arc<DsoCatalog>>>>,
    /// Lazy-opened solvemyastro star-cache (`stars.smac`). Loaded on first
    /// solve attempt and shared read-only across all worker threads.
    /// `None` until the cache file is located (opens from the `smac_gaia`
    /// subdir of the app-data catalogs dir). If the file is absent the solve
    /// command returns an actionable error.
    #[cfg(feature = "solver")]
    pub star_cache: Arc<RwLock<Option<Arc<solvemyastro::StarCache>>>>,
    /// Optional bright sub-catalog (G<16 hybrid floor + density top-up;
    /// built via `solvemyastro build-bright-cache`). When present, the
    /// plate-solve hot path uses it for fast quad matching with
    /// auto-fallback to `star_cache`. `None` if no bright cache is
    /// available — production runs on the deep cache alone.
    #[cfg(feature = "solver")]
    pub bright_cache: Arc<RwLock<Option<Arc<solvemyastro::StarCache>>>>,
    pub image_pool: Arc<rayon::ThreadPool>,
    /// Single serialized worker queue shared by ZIP archive + file ops.
    /// Created at startup; lives for the process lifetime.
    pub operation_queue: operation_queue::OperationQueue,
    /// Global FIFO admission queue for heavy CPU jobs (analysis, master
    /// builds). See compute_queue module docs.
    pub compute_queue: compute_queue::ComputeQueue,
}
