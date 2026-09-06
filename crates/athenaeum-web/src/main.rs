use athenaeum_core::cache::MemoryImageCache;
use athenaeum_core::db::{self, Database};
use athenaeum_core::logging;
use athenaeum_core::services::ServiceContext;
use athenaeum_core::settings::{self, SettingsManager};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

mod events;
mod routes;

/// Configuration parsed from environment variables.
struct Config {
    db_path: PathBuf,
    port: u16,
    static_dir: Option<PathBuf>,
    allowed_paths: Vec<PathBuf>,
    export_dir: Option<PathBuf>,
    /// Opt-in shared secret for `/api/*` (see `routes::auth`). `None` means
    /// auth is disabled — every request passes through unchanged, matching
    /// today's fully-open behavior.
    api_key: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            db_path: std::env::var("ATHENAEUM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("athenaeum.db")),
            port: std::env::var("ATHENAEUM_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3000),
            static_dir: std::env::var("ATHENAEUM_STATIC_DIR").ok().map(PathBuf::from),
            allowed_paths: std::env::var("ATHENAEUM_ALLOWED_PATHS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            export_dir: std::env::var("ATHENAEUM_EXPORT_DIR").ok().map(PathBuf::from),
            // Trim, then treat empty/whitespace-only as unset — a stray
            // `ATHENAEUM_API_KEY=` in a compose file must not lock the app
            // into an unusable state where an empty string "matches".
            api_key: std::env::var("ATHENAEUM_API_KEY")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }
}

/// Web-specific app state wrapping the shared ServiceContext.
#[derive(Clone)]
pub struct WebAppState {
    pub ctx: Arc<ServiceContext>,
    pub event_tx: tokio::sync::broadcast::Sender<events::SseEvent>,
    pub allowed_paths: Vec<PathBuf>,
    pub export_dir: Option<PathBuf>,
    /// Opt-in shared secret for `/api/*` auth; see `routes::auth`. `None` =
    /// auth disabled (today's open behavior).
    pub api_key: Option<String>,
    /// Limits concurrent image conversions; wrapped in RwLock so the semaphore
    /// can be swapped at runtime when the user changes blink.threads.
    pub image_semaphore: Arc<RwLock<Arc<tokio::sync::Semaphore>>>,
    /// CPU-based upper bound for blink threads (min(vCPUs, 16)).
    pub max_blink_threads: usize,
    /// Handle to the background folder-monitoring service. Routes use this
    /// to `kick()` the loop awake when monitor-relevant settings change.
    pub monitor: athenaeum_core::monitor::MonitorService,
    /// Personal-sync receive-side runtime (Stage I, task A7). Lazily starts the
    /// iroh transport + receiver behind the dev pairing flag.
    pub sync: Arc<athenaeum_core::sync::SyncRuntime>,
    /// Personal-sync send-side runtime (task M2). Lazily builds the sender engine
    /// on the first enqueue (manual send / auto mode).
    pub sync_sender: Arc<athenaeum_core::sync::SyncSenderRuntime>,
    /// Stage-II collab send-side runtime (Task 11): the DEDICATED collab sender
    /// map (distinct from `sync_sender` — collab serves ride a `blobs_collab`
    /// store, audit m7). Shared by the request-to-serve handler and the publish path.
    pub collab_sender: Arc<athenaeum_core::sync::SyncSenderRuntime>,
}

#[tokio::main]
async fn main() {
    // Retains the LoggingHandle (and its WorkerGuard) for the process
    // lifetime via a global OnceLock; reachable later via
    // `logging::global_handle()` for settings-driven `apply_config`.
    logging::init_global(logging::Process::Web);

    let config = Config::from_env();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "athenaeum web server starting");
    tracing::info!(db_path = %config.db_path.display(), port = config.port, "server config");
    if let Some(ref dir) = config.static_dir {
        tracing::info!(static_dir = %dir.display(), "server config");
    }
    if !config.allowed_paths.is_empty() {
        tracing::info!(allowed_paths = ?config.allowed_paths, "server config");
    }
    if let Some(ref dir) = config.export_dir {
        tracing::info!(export_dir = %dir.display(), "server config");
    }
    // Never log the key itself — only whether auth is on.
    tracing::info!(auth_enabled = config.api_key.is_some(), "server config");

    // Ensure DB parent directory exists
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).expect("Failed to create database directory");
    }

    // Initialize database
    let db = Database::new(config.db_path.clone()).expect("Failed to initialize database");
    tracing::info!(db_path = %config.db_path.display(), "database initialized");

    // Apply persisted logging config now that the DB is available. A parse
    // failure falls back to the default (info) filter rather than failing
    // startup; a missing setting also falls back to the default silently
    // (that's the expected first-run state) — but a DB read error is never
    // swallowed silently, unlike the parse/missing cases.
    {
        let cfg = match db::get_setting(&db.conn(), logging::config::SETTINGS_KEY) {
            Ok(Some(raw)) => serde_json::from_str::<logging::LoggingConfig>(&raw)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "invalid stored logging config; using default");
                    logging::LoggingConfig::default()
                }),
            Ok(None) => logging::LoggingConfig::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read stored logging config; using default");
                logging::LoggingConfig::default()
            }
        };
        if let Some(handle) = logging::global_handle() {
            handle.apply_config(&cfg);
        }
    }

    // Build thread pool
    let max_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(16))
        .unwrap_or(4);
    let image_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_threads)
        .build()
        .expect("Failed to create image processing thread pool");

    // Read blink threads setting from DB
    let blink_threads_raw: usize = db::get_setting(&db.conn(), settings::keys::BLINK_THREADS)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0); // 0 = auto (half of available cores)
    let blink_threads = if blink_threads_raw == 0 { (max_threads / 2).max(2) } else { blink_threads_raw.clamp(1, max_threads) };
    let image_semaphore = Arc::new(RwLock::new(Arc::new(
        tokio::sync::Semaphore::new(blink_threads),
    )));
    tracing::info!(permits = blink_threads, "blink semaphore initialized");

    // Read memory cache settings from DB
    let cache_size: usize = db::get_setting(&db.conn(), settings::keys::BLINK_MEMORY_CACHE_SIZE)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let cache_max_mb = settings::resolve_blink_memory_cache_max_mb(
        db::get_setting(&db.conn(), settings::keys::BLINK_MEMORY_CACHE_MAX_MB)
            .ok()
            .flatten()
            .as_deref(),
    );
    let retention_minutes: u64 = db::get_setting(&db.conn(), settings::keys::BLINK_MEMORY_RETENTION_MINUTES)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    tracing::info!(cache_size, cache_max_mb, retention_minutes, "memory cache initialized");

    // Build ServiceContext
    let settings_mgr = Arc::new(SettingsManager::new());
    let db_cell = OnceLock::new();
    let _ = db_cell.set(db);
    let ctx = Arc::new(ServiceContext {
        db: db_cell,
        settings: settings_mgr,
        memory_cache: Arc::new(Mutex::new(
            MemoryImageCache::new(cache_size, retention_minutes)
                .with_max_bytes(cache_max_mb * 1024 * 1024),
        )),
        active_scans: Arc::new(Mutex::new(HashMap::new())),
        active_exports: Arc::new(Mutex::new(HashMap::new())),
        active_analyses: Arc::new(Mutex::new(HashMap::new())),
        active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
        active_registrations: Arc::new(Mutex::new(HashMap::new())),
        active_archives: Arc::new(Mutex::new(HashMap::new())),
        active_master_builds: Arc::new(Mutex::new(HashMap::new())),
        dso_catalog: Arc::new(std::sync::RwLock::new(None)),
        star_cache: Arc::new(std::sync::RwLock::new(None)),
        bright_cache: Arc::new(std::sync::RwLock::new(None)),
        image_pool: Arc::new(image_pool),
        operation_queue: athenaeum_core::services::operation_queue::OperationQueue::start(),
        compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
        iroh_node: Arc::new(tokio::sync::Mutex::new(None)),
    });

    // SSE broadcast channel
    let (event_tx, _) = tokio::sync::broadcast::channel::<events::SseEvent>(1024);

    // Wire the compute-queue transport notifier now that the SSE channel
    // exists.
    {
        let sse_emitter = events::SseProgressEmitter::new(event_tx.clone());
        ctx.compute_queue.set_notifier(Box::new(move |entries| {
            athenaeum_core::events::emit_event(
                &sse_emitter,
                "compute-queue-changed",
                &serde_json::json!({ "entries": entries }),
            );
        }));
    }

    // Apply persisted compute.max_concurrent — unlike desktop, the web
    // server's DB is already initialized synchronously above (before `ctx`
    // was built), so this reliably takes effect on every startup.
    if let Some(db) = ctx.db.get() {
        let conn = db.conn();
        match ctx.settings.get_compute_max_concurrent(&conn) {
            Ok(n) => {
                ctx.compute_queue.set_max_concurrent(n);
                tracing::info!(max_concurrent = n, "compute queue concurrency set from DB");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read compute.max_concurrent; leaving default in place");
            }
        }
    }

    let state = WebAppState {
        ctx,
        event_tx,
        allowed_paths: config.allowed_paths,
        export_dir: config.export_dir,
        api_key: config.api_key,
        image_semaphore,
        max_blink_threads: max_threads,
        monitor: athenaeum_core::monitor::MonitorService::new(),
        sync: Arc::new(athenaeum_core::sync::SyncRuntime::new()),
        sync_sender: Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        collab_sender: Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
    };

    // Personal sync (Stage I, task A7): start the receiver + iroh transport at
    // boot when the dev flag is on. The web backend's DB is already initialised
    // here (unlike desktop, where the receiver starts lazily on the first
    // `get_sync_pairing_ticket`). Best-effort in a spawned task — a transport
    // build failure is logged, never fatal to the server.
    {
        let ctx_for_sync = Arc::clone(&state.ctx);
        let sync_for_sync = Arc::clone(&state.sync);
        let sync_sender_for_sync = Arc::clone(&state.sync_sender);
        let collab_for_sync = Arc::clone(&state.collab_sender);
        let emitter = Arc::new(events::SseProgressEmitter::new(state.event_tx.clone()));
        tokio::spawn(async move {
            match athenaeum_core::api::sync::autostart_if_enabled(ctx_for_sync, sync_for_sync, sync_sender_for_sync, collab_for_sync, emitter).await {
                Ok(true) => tracing::info!("personal sync receiver autostarted (dev pairing enabled)"),
                Ok(false) => tracing::debug!("personal sync disabled; receiver not started"),
                Err(e) => tracing::error!(error = %e, "personal sync autostart failed"),
            }
        });
    }

    // Content index (transfer dedup's `files.content_hash`). Gated on sync being
    // configured and admitted through the compute queue, so it is visible in the
    // sidebar and cancellable — the predecessor ran silently on every launch and
    // read ~1.5 MB per catalogued file with nothing in the UI to explain it.
    // Mirrors the desktop wiring in `commands/core.rs`.
    {
        let ctx = Arc::clone(&state.ctx);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
            Arc::new(events::SseProgressEmitter::new(state.event_tx.clone()));
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }

    // Second re-arm trigger: the background monitor scans on its own timer and
    // calls the scanner directly, never crossing the route boundary where the
    // interactive re-arm sits. `ScanCompletionHook` is the seam core exposes for
    // exactly this, and it fires only when a cycle actually ingested new files.
    // Mirrors the desktop wiring in `commands/core.rs`.
    state.monitor.set_scan_completion_hook(Arc::new(
        routes::content_index::ContentIndexRearmHook::new(
            Arc::clone(&state.ctx),
            state.event_tx.clone(),
        ),
    ));

    // Auto-reconcile abandoned cross-volume moves — enqueue this as the
    // FIRST job on the operation queue so it serializes ahead of any file op
    // a client triggers. Unlike the desktop app, the DB is already
    // initialized by this point (set above before the queue was created),
    // so this reliably runs; the pre-enqueue trigger in
    // `enqueue_move_operation` covers requests that race the startup job.
    // See `athenaeum_core::file_op::reconcile`.
    {
        use athenaeum_core::services::operation_queue::{OperationKind, QueuedJob};
        let ctx_for_reconcile = Arc::clone(&state.ctx);
        let event_tx_for_reconcile = state.event_tx.clone();
        state.ctx.operation_queue.enqueue(QueuedJob {
            kind: OperationKind::FileOpReconcile,
            operation_id: 0,
            run: Box::new(move || {
                let Some(db) = ctx_for_reconcile.db.get() else {
                    tracing::warn!("file_op reconcile (startup): database not yet initialized, skipping");
                    return;
                };
                let conn = db.conn();
                match athenaeum_core::file_op::reconcile::reconcile_abandoned_commit_moves(&conn) {
                    Ok(summary) if summary.healed > 0 || !summary.skipped.is_empty() => {
                        let emitter = events::SseProgressEmitter::new(event_tx_for_reconcile);
                        athenaeum_core::events::emit_event(
                            &emitter,
                            "file-op-reconciled",
                            &athenaeum_core::file_op::models::FileOpReconciled {
                                healed: summary.healed,
                                skipped: summary.skipped.len(),
                                operation_ids: summary.touched_operation_ids(),
                            },
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = ?e, "file_op reconcile (startup) failed"),
                }
            }),
        });
    }

    // Spawn background sweeper for stale memory-cache entries (every 60s)
    {
        let memory_cache = Arc::clone(&state.ctx.memory_cache);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let mut cache = memory_cache.lock().unwrap();
                cache.evict_stale();
            }
        });
    }

    // Spawn background folder-monitoring service (no startup delay — the
    // server has typically been running continuously in web mode). The
    // handle lives in WebAppState so routes can `kick()` it; we run a clone.
    {
        let ctx_clone = Arc::clone(&state.ctx);
        let emitter = Arc::new(events::SseProgressEmitter::new(state.event_tx.clone()));
        let monitor = state.monitor.clone();
        tokio::spawn(async move {
            monitor
                .run_loop(ctx_clone, emitter, Duration::ZERO)
                .await;
        });
    }

    // Build router. Keep a handle to the shared ServiceContext so the ONE iroh
    // node (if the sync autostart bound it) can be torn down after serve returns.
    let ctx_for_shutdown = Arc::clone(&state.ctx);
    let app = routes::build_router(state, config.static_dir);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind to address");

    tracing::info!(%addr, "Athenaeum web server started");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("Server error");

    // Graceful shutdown (I1): after serve returns, tear down the shared iroh node
    // if the sync autostart bound one. Bounded (5s) + best-effort so a stuck
    // relay/close never hangs process exit; `SharedIrohNode::shutdown` is itself
    // idempotent and logs its own completion.
    tracing::info!("web server stopped; shutting down shared iroh node");
    // Take the node out (dropping the guard) BEFORE awaiting shutdown, so the
    // MutexGuard temporary never lives across the await.
    let node_to_shutdown = ctx_for_shutdown.iroh_node.lock().await.take();
    if let Some(node) = node_to_shutdown {
        if tokio::time::timeout(Duration::from_secs(5), node.shutdown())
            .await
            .is_err()
        {
            tracing::warn!("shared iroh node shutdown timed out");
        }
    } else {
        tracing::debug!("no shared iroh node was bound; nothing to shut down");
    }
}

/// Resolve when the process receives Ctrl-C or (on unix) SIGTERM — the trigger
/// for `axum::serve`'s graceful shutdown so the shared iroh node can be closed
/// cleanly on exit (I1). SIGTERM matters for `docker stop`, which sends it.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining server");
}
