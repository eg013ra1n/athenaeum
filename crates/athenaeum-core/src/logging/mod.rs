pub mod config;
mod panic_hook;
pub use config::LoggingConfig;

use std::path::PathBuf;
use std::sync::OnceLock;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    reload,
    util::SubscriberInitExt,
    EnvFilter, Layer, Registry,
};

#[derive(Clone, Copy)]
pub enum Process {
    Desktop,
    Web,
}

impl Process {
    fn prefix(self) -> &'static str {
        match self {
            Process::Desktop => "athenaeum-desktop",
            Process::Web => "athenaeum-web",
        }
    }
}

pub struct LoggingHandle {
    reload: reload::Handle<EnvFilter, Registry>,
    env_override: bool,
    // keep the appender guard alive for the process lifetime
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Resolve the app data directory from environment variables (no Tauri needed).
/// Mirrors Tauri's default app_data_dir resolution per platform.
/// Moved verbatim from the old (pre-tracing) `logging.rs::resolve_app_data_dir`.
fn resolve_app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("com.vsharifov.athenaeum"));
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(|d| PathBuf::from(d).join("Library/Application Support/com.vsharifov.athenaeum"));
    }
    #[cfg(target_os = "linux")]
    {
        return std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("com.vsharifov.athenaeum"));
    }
    #[allow(unreachable_code)]
    None
}

/// Resolve the directory rolling log files are written into.
///
/// Precedence: `ATHENAEUM_LOG_DIR` (test/Docker injection hook, used directly)
/// > `ATHENAEUM_DB_PATH`'s parent joined with `logs/` (web/Docker convention,
/// keeps logs next to the DB volume) > app-data dir joined with `logs/`
/// (desktop default).
fn resolve_log_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ATHENAEUM_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Ok(db_path) = std::env::var("ATHENAEUM_DB_PATH") {
        let parent = PathBuf::from(&db_path)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return Some(parent.join("logs"));
    }
    resolve_app_data_dir().map(|d| d.join("logs"))
}

pub fn init(process: Process) -> Option<LoggingHandle> {
    let dir = resolve_log_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    // Legacy cleanup: best-effort delete of the old (pre-tracing) single-file logs,
    // which lived directly in the app-data dir (the parent of the new `logs/` dir).
    for old in ["athenaeum.log", "athenaeum.log.1"] {
        let _ = std::fs::remove_file(dir.parent().unwrap_or(&dir).join(old));
    }

    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(process.prefix())
        .filename_suffix("jsonl")
        .max_log_files(14)
        .build(&dir)
        .ok()?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_override = std::env::var("ATHENAEUM_LOG").is_ok();
    let initial = if env_override {
        EnvFilter::try_from_env("ATHENAEUM_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
    } else {
        EnvFilter::new(LoggingConfig::default().to_directives())
    };
    let (filter, handle) = reload::Layer::new(initial);

    let file_layer = fmt::layer()
        .json()
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking)
        .boxed();
    let console_layer = match process {
        // human-pretty on stderr for desktop terminal launches
        Process::Desktop => fmt::layer()
            .with_span_events(FmtSpan::CLOSE)
            .with_writer(std::io::stderr)
            .boxed(),
        // container convention: JSON to stdout
        Process::Web => fmt::layer()
            .json()
            .with_span_events(FmtSpan::CLOSE)
            .with_writer(std::io::stdout)
            .boxed(),
    };

    // The reloadable filter is attached first, directly onto the bare
    // `Registry`, so it gates every downstream layer globally (file +
    // console) rather than filtering just one of them.
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .ok()?;

    let _ = LOG_DIR.set(dir);
    panic_hook::install(); // preserved crash.log behavior + emits tracing::error!
    Some(LoggingHandle {
        reload: handle,
        env_override,
        _guard: guard,
    })
}

impl LoggingHandle {
    pub fn env_override_active(&self) -> bool {
        self.env_override
    }
    /// Live-apply a config. No-op (with one warn) while ATHENAEUM_LOG is set.
    pub fn apply_config(&self, cfg: &LoggingConfig) {
        if self.env_override {
            tracing::warn!("logging config change ignored: ATHENAEUM_LOG override active");
            return;
        }
        match cfg.to_directives().parse::<EnvFilter>() {
            Ok(f) => {
                let _ = self.reload.reload(f);
                tracing::info!(directives = %cfg.to_directives(), "logging filter applied");
            }
            Err(error) => {
                tracing::warn!(%error, "invalid logging directives; keeping previous filter")
            }
        }
    }
}

pub fn get_path() -> Option<PathBuf> {
    LOG_DIR.get().cloned() // directory; current file is <prefix>.<date>.jsonl inside it
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn jsonl_line_parses_with_expected_fields() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // Test-only injection hook: resolve_log_dir() checks this first.
        std::env::set_var("ATHENAEUM_LOG_DIR", tmp.path());
        std::env::remove_var("ATHENAEUM_LOG");

        // `init()` installs a process-global panic hook (`std::panic::set_hook`).
        // Rust test binaries run every test in one process, so leaving the new
        // hook in place would make every *other* test's panics pay its extra
        // cost (backtrace capture + tracing/JSON formatting + crash.log I/O)
        // for the rest of the run — measured to be enough overhead to make an
        // unrelated timing-sensitive test (operation_queue's panic-recovery
        // test) flake under full-suite concurrency. Save/restore it so this
        // test's global-state mutation doesn't leak into its neighbors. (The
        // tracing subscriber itself has no analogous "uninstall" — that part
        // is comparatively cheap and isn't what caused the flake.)
        let previous_hook = std::panic::take_hook();
        let handle = init(Process::Web);
        // init() may return None if a global subscriber was already installed
        // by an earlier test in this binary (tracing's subscriber is
        // process-global and try_init() only succeeds once). Skip verifying
        // subscriber-dependent behavior in that case rather than failing the
        // whole suite on test ordering.
        if handle.is_none() {
            std::panic::set_hook(previous_hook);
            std::env::remove_var("ATHENAEUM_LOG_DIR");
            return;
        }

        tracing::info!(count = 3, "test event");

        // Drop the handle (and its WorkerGuard) to flush the non-blocking writer.
        drop(handle);
        std::panic::set_hook(previous_hook);
        std::env::remove_var("ATHENAEUM_LOG_DIR");

        let mut found = false;
        for entry in std::fs::read_dir(tmp.path()).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mut contents = String::new();
            std::fs::File::open(&path)
                .expect("open log file")
                .read_to_string(&mut contents)
                .expect("read log file");
            for line in contents.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: serde_json::Value =
                    serde_json::from_str(line).expect("log line is valid JSON");
                let fields = &value["fields"];
                if fields.get("count").and_then(|v| v.as_i64()) == Some(3) {
                    assert_eq!(value["level"].as_str(), Some("INFO"));
                    found = true;
                }
            }
        }
        assert!(
            found,
            "expected a JSONL line with fields.count == 3 and level == INFO"
        );
    }
}
