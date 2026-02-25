use athenaeum_core::cache::MemoryImageCache;
use athenaeum_core::db::{self, Database};
use athenaeum_core::logging;
use athenaeum_core::services::ServiceContext;
use athenaeum_core::settings::{self, SettingsManager};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
}

#[tokio::main]
async fn main() {
    logging::init();

    let config = Config::from_env();

    println!("Athenaeum Web Server v{}", env!("CARGO_PKG_VERSION"));
    println!("  Database: {}", config.db_path.display());
    println!("  Port:     {}", config.port);
    if let Some(ref dir) = config.static_dir {
        println!("  Static:   {}", dir.display());
    }
    if !config.allowed_paths.is_empty() {
        println!("  Allowed:  {:?}", config.allowed_paths);
    }
    if let Some(ref dir) = config.export_dir {
        println!("  Export:   {}", dir.display());
    }

    // Ensure DB parent directory exists
    if let Some(parent) = config.db_path.parent() {
        std::fs::create_dir_all(parent).expect("Failed to create database directory");
    }

    // Initialize database
    let db = Database::new(config.db_path.clone()).expect("Failed to initialize database");
    println!("Database initialized: {}", config.db_path.display());

    // Build thread pool
    let max_threads = std::thread::available_parallelism()
        .map(|n| n.get().min(16))
        .unwrap_or(4);
    let image_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_threads)
        .build()
        .expect("Failed to create image processing thread pool");

    // Read memory cache settings from DB
    let cache_size: usize = db::get_setting(&db.conn(), settings::keys::BLINK_MEMORY_CACHE_SIZE)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let retention_minutes: u64 = db::get_setting(&db.conn(), settings::keys::BLINK_MEMORY_RETENTION_MINUTES)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    println!("  Memory cache: {} entries, {} min retention", cache_size, retention_minutes);

    // Build ServiceContext
    let settings_mgr = Arc::new(SettingsManager::new());
    let ctx = Arc::new(ServiceContext {
        db: Mutex::new(Some(db)),
        settings: settings_mgr,
        cache: Arc::new(Mutex::new(None)),
        memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(cache_size, retention_minutes))),
        active_scans: Arc::new(Mutex::new(HashMap::new())),
        active_exports: Arc::new(Mutex::new(HashMap::new())),
        image_pool: Arc::new(image_pool),
    });

    // SSE broadcast channel
    let (event_tx, _) = tokio::sync::broadcast::channel::<events::SseEvent>(256);

    let state = WebAppState {
        ctx,
        event_tx,
        allowed_paths: config.allowed_paths,
        export_dir: config.export_dir,
    };

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

    // Build router
    let app = routes::build_router(state, config.static_dir);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind to address");
    println!("Listening on http://{}", addr);

    logging::log("INFO", &format!("Athenaeum web server started on {}", addr));

    axum::serve(listener, app)
        .await
        .expect("Server error");
}
