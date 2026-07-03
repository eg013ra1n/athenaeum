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
#[tracing::instrument(skip_all, err, level = "debug")]
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
        if let Some(db) = state.ctx.db.get() {
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
    // The key includes the file's mtime (preview_cache_key), so an in-place
    // edit/restore invalidates naturally instead of serving a stale JPEG.
    // The stat doubles as the existence check (fail fast for deleted files —
    // including ones that still have a cached entry).
    let cache_key = match crate::cache::preview_cache_key(&path_buf, &resolution_str) {
        Ok(key) => key,
        Err(e) => {
            return Err(format!("File not found: {} ({})", path_buf.display(), e));
        }
    };

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            tracing::trace!(bytes = cached.data.len(), duration = ?t_start.elapsed(), "memory cache hit (fast path)");
            return Ok(cached.data.clone());
        }
    }

    // Slow path: acquire semaphore for actual processing
    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem.acquire().await.map_err(|e| e.to_string())?;
    let t_process = Instant::now();

    // Double-check cache — another request may have filled it while we waited
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            tracing::trace!(bytes = cached.data.len(), duration = ?t_start.elapsed(), "memory cache hit (after semaphore)");
            return Ok(cached.data.clone());
        }
    }

    // Cache miss — process FITS to JPEG
    let result = tokio::task::block_in_place(|| {
        rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality, &state.ctx.image_pool)
    })
        .map_err(|e| format!("Failed to process FITS image: {}", e))?;

    let jpeg_data = result.image_data;

    // Insert JPEG bytes into memory cache
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        mem_cache.insert(cache_key, CachedImage { data: jpeg_data.clone(), last_accessed: Instant::now() });
    }

    let wait = t_process.duration_since(t_start);
    tracing::trace!(bytes = jpeg_data.len(), duration = ?t_process.elapsed(), waited = ?wait, "memory cache: processed and stored");
    Ok(jpeg_data)
}

