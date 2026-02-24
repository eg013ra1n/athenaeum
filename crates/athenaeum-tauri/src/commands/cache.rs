// Cache management commands

use crate::cache::{remove_file_logged, CacheManager, CacheStats};
use std::sync::Arc;
use tauri::{Manager, State};

use super::AppState;

/// Helper function to format bytes into human-readable format
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Get cache statistics (for display in UI)
#[tauri::command]
pub async fn get_cache_stats(state: State<'_, AppState>) -> Result<CacheStats, String> {
    let cache_mgr = state.ctx.cache.lock().unwrap().clone();

    if let Some(cache_mgr) = cache_mgr {
        use tokio::runtime::Handle;
        use tokio::task;

        task::block_in_place(|| {
            Handle::current().block_on(async {
                cache_mgr.get_stats().await
            })
        }).map_err(|e| e.to_string())
    } else {
        Err("Cache manager not available".to_string())
    }
}

/// Clear all cached images
#[tauri::command]
pub async fn clear_image_cache(state: State<'_, AppState>) -> Result<String, String> {
    println!("🗑️  Clearing image cache...");

    let cache_mgr = state.ctx.cache.lock().unwrap().clone();

    let Some(cache_mgr) = cache_mgr else {
        return Err("Cache manager not available".to_string());
    };

    use tokio::runtime::Handle;
    use tokio::task;

    // Get cache stats before clearing
    let (total_entries, total_size) = match task::block_in_place(|| {
        Handle::current().block_on(async { cache_mgr.get_stats().await })
    }) {
        Ok(stats) => {
            println!("📊 Cache stats: {} entries, {}", stats.total_entries, format_bytes(stats.total_size_bytes));
            (stats.total_entries, stats.total_size_bytes)
        }
        Err(e) => {
            println!("⚠️  Could not get cache stats: {}", e);
            (0, 0)
        }
    };

    // Now clear the cache
    match task::block_in_place(|| {
        Handle::current().block_on(async { cache_mgr.invalidate_all().await })
    }) {
        Ok(_) => {
            let msg = if total_size > 0 {
                format!("Cache cleared successfully. Freed {} ({} entries)",
                    format_bytes(total_size), total_entries)
            } else {
                "Cache cleared successfully".to_string()
            };
            println!("✅ {}", msg);
            Ok(msg)
        }
        Err(e) => {
            let error_msg = format!("Failed to clear cache: {}", e);
            eprintln!("❌ ERROR: {}", error_msg);
            Err(error_msg)
        }
    }
}

/// Repair a corrupted cache database by deleting and recreating it
#[tauri::command]
pub async fn repair_cache_database(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    println!("🔧 Repairing cache database...");

    // Get app data directory
    let app_dir = app_handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;

    let cache_db_path = app_dir.join("cache").join("cache.db");
    let cache_dir = app_dir.join("cache").join("previews");

    // Hold the lock for the entire operation to prevent race conditions
    let mut cache_lock = state.ctx.cache.lock().unwrap();

    // Drop the old cache manager (closes the DB connection)
    *cache_lock = None;

    // Delete corrupted database files
    remove_file_logged(&cache_db_path);
    remove_file_logged(&cache_db_path.with_extension("db-wal"));
    remove_file_logged(&cache_db_path.with_extension("db-shm"));

    // Delete all preview JPEGs
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            if entry.path().extension() == Some(std::ffi::OsStr::new("jpg")) {
                remove_file_logged(&entry.path());
            }
        }
    }

    // Recreate cache manager
    match CacheManager::new(&app_dir, state.ctx.settings.clone(), state.ctx.image_pool.clone()) {
        Ok(cache_mgr) => {
            *cache_lock = Some(Arc::new(cache_mgr));
            let msg = "Cache database repaired successfully".to_string();
            println!("✅ {}", msg);
            Ok(msg)
        }
        Err(e) => {
            let error_msg = format!("Failed to recreate cache database: {}", e);
            eprintln!("❌ ERROR: {}", error_msg);
            Err(error_msg)
        }
    }
}

/// Check cache database integrity
#[tauri::command]
pub async fn check_cache_integrity(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let cache_mgr = state.ctx.cache.lock().unwrap().clone();

    let Some(cache_mgr) = cache_mgr else {
        return Err("Cache manager not available".to_string());
    };

    cache_mgr.check_integrity().map_err(|e| e.to_string())
}
