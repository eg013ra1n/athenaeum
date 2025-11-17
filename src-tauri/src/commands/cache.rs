// Cache management commands

use crate::cache::CacheStats;
use tauri::State;

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
    let cache_arc = state.cache.clone();
    let stats_result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_stats().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    stats_result.map_err(|e| e.to_string())
}

/// Clear all cached images
#[tauri::command]
pub async fn clear_image_cache(state: State<'_, AppState>) -> Result<String, String> {
    println!("🗑️  Clearing image cache...");

    let cache_arc = state.cache.clone();

    // Get cache stats before clearing
    let stats_result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_stats().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    let (total_entries, total_size) = match stats_result {
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
    let result = {
        let cache_guard = cache_arc.lock().unwrap();
        if let Some(cache_mgr) = cache_guard.as_ref() {
            use tokio::runtime::Handle;
            use tokio::task;

            task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.invalidate_all().await
                })
            })
        } else {
            Err(anyhow::anyhow!("Cache manager not available"))
        }
    };

    match result {
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
