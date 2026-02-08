use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024; // 5 MB

/// Resolve the app data directory from environment variables (no Tauri needed).
/// Mirrors Tauri's default app_data_dir resolution per platform.
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

/// Initialize logging: rotate if needed, open file, set panic hook.
pub fn init() {
    let Some(dir) = resolve_app_data_dir() else {
        return;
    };
    let _ = fs::create_dir_all(&dir);

    let log_path = dir.join("athenaeum.log");

    // Rotate if over 5 MB
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > MAX_LOG_SIZE {
            let rotated = dir.join("athenaeum.log.1");
            let _ = fs::rename(&log_path, &rotated);
        }
    }

    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = LOG_FILE.set(Mutex::new(file));
        let _ = LOG_PATH.set(log_path);
    }

    // Panic hook — writes to log file before process dies
    let crash_dir = dir;
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {info}\nBacktrace:\n{bt}");
        log("PANIC", &msg);
        // Also write a dedicated crash.log as backup
        let crash_path = crash_dir.join("crash.log");
        let ts = timestamp();
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(crash_path)
            .and_then(|mut f| writeln!(f, "[{ts}] {msg}"));
    }));
}

/// Write a timestamped log line. No-op if init() wasn't called.
pub fn log(level: &str, msg: &str) {
    let Some(file_mutex) = LOG_FILE.get() else {
        return;
    };
    let Ok(mut file) = file_mutex.lock() else {
        return;
    };
    let ts = timestamp();
    let _ = writeln!(file, "[{ts}] [{level}] {msg}");
    let _ = file.flush();
}

/// Get the log file path (for the get_log_path command).
pub fn get_path() -> Option<&'static Path> {
    LOG_PATH.get().map(|p| p.as_path())
}

fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}
