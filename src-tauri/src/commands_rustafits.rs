// Rustafits-based commands for FITS image processing

use crate::cache::{StretchMode, StretchParams};
use crate::commands::AppState;
use crate::rustafits_processor::{self, Resolution};
use std::path::PathBuf;
use tauri::State;

/// Read FITS image and return processed bytes.
///
/// When `blink.cache_mode` = "file" (default): returns JPEG bytes via disk cache.
/// When `blink.cache_mode` = "memory": returns packed binary (9-byte header + RGBA data)
/// via in-memory LRU cache.
///
/// Memory mode uses a fast path: cache hits bypass the semaphore entirely (<1ms).
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

    // ── Step 1: Read settings from DB (separate mutex, held <100μs) ──
    let (resolution_str, cache_mode, memory_downscale, quality, file_info) = {
        let state_lock = state.db.lock().unwrap();
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
            let mode = match crate::db::get_setting(&conn, "blink.cache_mode") {
                Ok(Some(value)) => value,
                _ => "file".to_string(),
            };
            let downscale = match crate::db::get_setting(&conn, "blink.memory_downscale") {
                Ok(Some(value)) => value.parse::<usize>().unwrap_or(1),
                _ => 1,
            };

            // Pre-read quality setting and file info for file mode
            let (q, fi) = if mode != "memory" {
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
                let fi = match crate::db::get_file_by_path(&conn, &path) {
                    Ok(file) => {
                        Some(file)
                    }
                    Err(_e) => {
                        let metadata = std::fs::metadata(&path_buf).ok();
                        Some(crate::models::File {
                            id: Some(-1),
                            path: path.clone(),
                            filename: path_buf
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            size: metadata.as_ref().map(|m| m.len() as i64).unwrap_or(0),
                            modified_at: metadata
                                .as_ref()
                                .and_then(|m| m.modified().ok())
                                .map(|t| chrono::DateTime::<chrono::Utc>::from(t))
                                .unwrap_or_else(|| chrono::Utc::now()),
                            format: if path_buf.extension().map_or(false, |e| e.eq_ignore_ascii_case("xisf")) {
                                crate::models::FileFormat::XISF
                            } else {
                                crate::models::FileFormat::FITS
                            },
                            created_at: chrono::Utc::now(),
                            metadata_hash: None,
                            content_hash: None,
                        })
                    }
                };
                (q, fi)
            } else {
                (None, None)
            };

            (res_str, mode, downscale, q, fi)
        } else {
            (
                resolution.as_deref().unwrap_or("preview").to_string(),
                "file".to_string(),
                1usize,
                None,
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

    // ── Step 2: Memory mode fast path — check cache WITHOUT semaphore ──
    if cache_mode == "memory" {
        let cache_key = format!("{}:{}:{}", path, resolution_str, memory_downscale);

        // Fast path: cache hit returns instantly, no semaphore needed
        {
            let mut mem_cache = state.memory_cache.lock().unwrap();
            if let Some(cached) = mem_cache.get(&cache_key) {
                let packed = pack_raw_image(cached.width, cached.height, cached.is_color, &cached.data);
                println!("⚡ Memory cache hit (fast path, {} bytes) in {:?}", packed.len(), t_start.elapsed());
                return Ok(packed);
            }
        }

        // Check if file exists before acquiring semaphore
        if !path_buf.exists() {
            let error_msg = format!("File not found: {}", path_buf.display());
            eprintln!("ERROR: {}", error_msg);
            return Err(error_msg);
        }

        // Slow path: acquire semaphore for actual processing
        let _permit = state.image_semaphore.acquire().await.map_err(|e| e.to_string())?;

        // Double-check cache — another request may have filled it while we waited
        {
            let mut mem_cache = state.memory_cache.lock().unwrap();
            if let Some(cached) = mem_cache.get(&cache_key) {
                let packed = pack_raw_image(cached.width, cached.height, cached.is_color, &cached.data);
                println!("⚡ Memory cache hit (after semaphore, {} bytes) in {:?}", packed.len(), t_start.elapsed());
                return Ok(packed);
            }
        }

        // Cache miss — process image
        println!("⏳ Memory cache miss, processing...");
        let raw = rustafits_processor::process_fits_to_raw(&path_buf, res, memory_downscale, &state.image_pool)
            .map_err(|e| {
                let error_msg = format!("Failed to process FITS image: {}", e);
                eprintln!("ERROR: {}", error_msg);
                error_msg
            })?;

        let packed = pack_raw_image(raw.width, raw.height, raw.is_color, &raw.data);

        // Insert into memory cache
        {
            let mut mem_cache = state.memory_cache.lock().unwrap();
            mem_cache.insert(cache_key, raw);
        }

        println!("✅ Processed and cached ({} bytes) in {:?}", packed.len(), t_start.elapsed());
        return Ok(packed);
    }

    // ── File mode: disk-based JPEG cache ──
    // Acquire semaphore for file mode (processing-heavy)
    let _permit = state.image_semaphore.acquire().await.map_err(|e| e.to_string())?;

    // Check if file exists
    if !path_buf.exists() {
        let error_msg = format!("File not found: {}", path_buf.display());
        eprintln!("ERROR: {}", error_msg);
        return Err(error_msg);
    }

    // Clone the Arc<CacheManager> so we can drop the outer lock and process concurrently
    let cache_mgr = state.cache.lock().unwrap().clone();

    if let Some(cache_mgr) = cache_mgr {
        if let Some(file) = file_info {
            let stretch_params = StretchParams {
                mode: StretchMode::Auto,
                black_point: 0,
                white_point: 0,
                midtones: 0.35,
                resolution: resolution_str.clone(),
            };

            use tokio::runtime::Handle;
            use tokio::task;

            let cached_data_result = task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr
                        .get_or_create(&file, &path_buf, &stretch_params, quality)
                        .await
                })
            });

            match cached_data_result {
                Ok(cached_data) => {
                    println!("✅ File cache: {} bytes in {:?}", cached_data.len(), t_start.elapsed());
                    return Ok(cached_data);
                }
                Err(e) => {
                    eprintln!("Cache error: {}", e);
                }
            }
        }
    }

    // Fallback: Direct processing without cache
    println!("⚠️  Processing without cache...");

    let result = rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality, &state.image_pool)
        .map_err(|e| {
            let error_msg = format!("Failed to process FITS image: {}", e);
            eprintln!("ERROR: {}", error_msg);
            error_msg
        })?;

    println!("✅ Processing complete in {:?}", t_start.elapsed());
    Ok(result.image_data)
}

/// Pack raw image into binary format for IPC:
/// [width: 4 bytes LE u32][height: 4 bytes LE u32][is_color: 1 byte][RGBA data...]
/// Converts RGB→RGBA inline so the frontend can create ImageData zero-copy.
fn pack_raw_image(width: usize, height: usize, is_color: bool, rgb_data: &[u8]) -> Vec<u8> {
    let pixel_count = width * height;
    let mut packed = Vec::with_capacity(9 + pixel_count * 4);
    packed.extend_from_slice(&(width as u32).to_le_bytes());
    packed.extend_from_slice(&(height as u32).to_le_bytes());
    packed.push(if is_color { 1 } else { 0 });
    for chunk in rgb_data.chunks_exact(3) {
        packed.extend_from_slice(chunk);
        packed.push(255);
    }
    packed
}
