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
use std::sync::atomic::AtomicBool;
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
    /// The ONE process-wide iroh node (C1 fix, Д2). Bound lazily on the first
    /// sync/collab need via [`crate::api::sync::ensure_iroh_node`] and shared by
    /// every role (receiver / personal sender / collab sender) as role handles
    /// off a single endpoint + store. `None` until first bind; the host tears it
    /// down at shutdown (`SharedIrohNode::shutdown`). A `tokio::sync::Mutex`
    /// (held across the async first-bind) serializes concurrent first callers;
    /// the `Option` allows a re-bind after shutdown.
    pub iroh_node:
        Arc<tokio::sync::Mutex<Option<Arc<crate::sharing::iroh::node::SharedIrohNode>>>>,
}

impl ServiceContext {
    /// Test-support constructor: a minimal, fully real `ServiceContext` backed by
    /// a fresh SQLite catalog at `db_path` (schema initialised by
    /// [`Database::new`]), every active-operation map empty, single-threaded
    /// pools, and default (empty) settings. It mirrors what the desktop/web hosts
    /// build at startup, minus the host-specific wiring (no `AppHandle`, no SSE
    /// channel, no persisted-settings load).
    ///
    /// This is the ONE test-support surface exposed for the two-instance sync
    /// E2E harness (`tests/sync_e2e.rs`, task M5): an out-of-crate integration
    /// test cannot reach the private `#[cfg(test)]` `test_ctx()` helpers, and the
    /// alternative — a hand-written struct literal in the test — would have to
    /// name the feature-gated solver/render cache fields and break on every field
    /// addition. Kept `#[doc(hidden)]` because it is not part of the app's public
    /// API and must never be reached from production host code (both hosts build
    /// their own `ServiceContext` inline).
    #[doc(hidden)]
    pub fn new_for_tests(db_path: std::path::PathBuf) -> Self {
        let database = Database::new(db_path).expect("open test catalog db");
        let db = OnceLock::new();
        let _ = db.set(database);
        ServiceContext {
            db,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .expect("build test image pool"),
            ),
            operation_queue: operation_queue::OperationQueue::start(),
            compute_queue: compute_queue::ComputeQueue::new(),
            iroh_node: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}
