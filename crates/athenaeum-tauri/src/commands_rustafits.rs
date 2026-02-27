// Rustafits-based commands for FITS image processing

use crate::cache::CachedImage;
use crate::commands::AppState;
use crate::rustafits_processor::{self, Resolution};
use std::path::PathBuf;
use tauri::State;

/// Read FITS image and return JPEG bytes via in-memory cache.
///
/// Cache hits bypass the semaphore entirely (<1ms).
/// The semaphore is only acquired for cache misses (actual image processing).
#[tauri::command]
pub async fn read_fits_image_rustafits(
    path: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    use std::time::Instant;

    let t_start = Instant::now();
    let path_buf = PathBuf::from(&path);

    // ── Step 1: Read settings from DB ──
    let (resolution_str, quality) = {
        let state_lock = state.ctx.db.lock().unwrap();
        if let Some(db) = state_lock.as_ref() {
            let conn = db.conn();
            let res_str = if let Some(res_param) = resolution.as_deref() {
                res_param.to_string()
            } else {
                match crate::db::get_setting(&conn, "blink.resolution") {
                    Ok(Some(value)) => value,
                    _ => "preview".to_string(),
                }
            };

            let res = match res_str.as_str() {
                "thumbnail" => Resolution::Thumbnail,
                "preview" => Resolution::Preview,
                "full" => Resolution::Full,
                _ => Resolution::Preview,
            };
            let quality_key = res.quality_setting_key();
            let q = match crate::db::get_setting(&conn, quality_key) {
                Ok(Some(value)) => value.parse().ok(),
                _ => None,
            };

            (res_str, q)
        } else {
            (
                resolution.as_deref().unwrap_or("preview").to_string(),
                None,
            )
        }
    };

    // Parse resolution parameter
    let res = match resolution_str.as_str() {
        "thumbnail" => Resolution::Thumbnail,
        "preview" => Resolution::Preview,
        "full" => Resolution::Full,
        _ => Resolution::Preview,
    };

    // ── Step 2: Memory cache lookup (fast path) ──
    let cache_key = format!("{}:{}", path, resolution_str);

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            println!("⚡ Memory cache hit (fast path, {} bytes) in {:?}", cached.data.len(), t_start.elapsed());
            return Ok(cached.data.clone());
        }
    }

    // Check if file exists before acquiring semaphore
    if !path_buf.exists() {
        let error_msg = format!("File not found: {}", path_buf.display());
        eprintln!("ERROR: {}", error_msg);
        return Err(error_msg);
    }

    // Slow path: acquire semaphore for actual processing
    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem.acquire().await.map_err(|e| e.to_string())?;

    // Double-check cache — another request may have filled it while we waited
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            println!("⚡ Memory cache hit (after semaphore, {} bytes) in {:?}", cached.data.len(), t_start.elapsed());
            return Ok(cached.data.clone());
        }
    }

    // Cache miss — process FITS to JPEG
    let result = tokio::task::block_in_place(|| {
        rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality, &state.ctx.image_pool)
    })
        .map_err(|e| {
            let error_msg = format!("Failed to process FITS image: {}", e);
            eprintln!("ERROR: {}", error_msg);
            error_msg
        })?;

    let jpeg_data = result.image_data;

    // Insert JPEG bytes into memory cache
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        mem_cache.insert(cache_key, CachedImage { data: jpeg_data.clone(), last_accessed: Instant::now() });
    }

    println!("✅ Memory cache: {} bytes in {:?}", jpeg_data.len(), t_start.elapsed());
    Ok(jpeg_data)
}
