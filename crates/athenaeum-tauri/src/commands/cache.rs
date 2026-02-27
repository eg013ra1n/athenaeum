// Cache management commands

use tauri::State;

use super::AppState;

/// Clear the in-memory image cache.
#[tauri::command]
pub async fn clear_image_cache(state: State<'_, AppState>) -> Result<String, String> {
    println!("🗑️  Clearing memory image cache...");
    let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
    mem_cache.clear();
    let msg = "Memory image cache cleared".to_string();
    println!("✅ {}", msg);
    Ok(msg)
}
