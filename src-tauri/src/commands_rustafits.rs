// Rustafits-based commands for FITS image processing

use crate::cache::{CachedImage, StretchMode, StretchParams};
use crate::commands::AppState;
use crate::rustafits_processor::{self, Resolution};
use std::path::PathBuf;
use tauri::State;

/// Read FITS image and return JPEG bytes.
///
/// Both cache modes now return JPEG:
/// - "file" mode: disk-based JPEG cache (persistent across sessions)
/// - "memory" mode: in-memory JPEG cache (RAM-only, no disk I/O)
///
/// Both modes use a fast path: cache hits bypass the semaphore entirely (<1ms).
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
    let (resolution_str, cache_mode, quality, file_info) = {
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

            // Read quality setting and file info (needed for both modes now)
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
            let fi = if mode != "memory" {
                match crate::db::get_file_by_path(&conn, &path) {
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
                }
            } else {
                None
            };

            (res_str, mode, q, fi)
        } else {
            (
                resolution.as_deref().unwrap_or("preview").to_string(),
                "file".to_string(),
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

    // ── Step 2: Memory mode — JPEG stored in RAM ──
    if cache_mode == "memory" {
        let cache_key = format!("{}:{}", path, resolution_str);

        // Fast path: cache hit returns instantly, no semaphore needed
        {
            let mut mem_cache = state.memory_cache.lock().unwrap();
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
            let mut mem_cache = state.memory_cache.lock().unwrap();
            if let Some(cached) = mem_cache.get(&cache_key) {
                println!("⚡ Memory cache hit (after semaphore, {} bytes) in {:?}", cached.data.len(), t_start.elapsed());
                return Ok(cached.data.clone());
            }
        }

        // Cache miss — process FITS to JPEG (same pipeline as file mode)
        println!("⏳ Memory cache miss, processing...");
        let result = rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality, &state.image_pool)
            .map_err(|e| {
                let error_msg = format!("Failed to process FITS image: {}", e);
                eprintln!("ERROR: {}", error_msg);
                error_msg
            })?;

        let jpeg_data = result.image_data;

        // Insert JPEG bytes into memory cache
        {
            let mut mem_cache = state.memory_cache.lock().unwrap();
            mem_cache.insert(cache_key, CachedImage { data: jpeg_data.clone() });
        }

        println!("✅ Processed and cached ({} bytes) in {:?}", jpeg_data.len(), t_start.elapsed());
        return Ok(jpeg_data);
    }

    // ── Step 3: File mode — disk-based JPEG cache ──
    // Fast path: check disk cache BEFORE acquiring semaphore
    let cache_mgr = state.cache.lock().unwrap().clone();

    if let Some(ref cache_mgr) = cache_mgr {
        if let Some(ref file) = file_info {
            let stretch_params = StretchParams {
                mode: StretchMode::Auto,
                black_point: 0,
                white_point: 0,
                midtones: 0.35,
                resolution: resolution_str.clone(),
            };

            // Try cache lookup without semaphore
            use tokio::runtime::Handle;
            use tokio::task;

            let cached_result = task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_cached(file, &stretch_params).await
                })
            });

            match cached_result {
                Ok(Some(cached_data)) => {
                    println!("✅ File cache hit: {} bytes in {:?}", cached_data.len(), t_start.elapsed());
                    return Ok(cached_data);
                }
                Ok(None) => {
                    // Cache miss — fall through to semaphore + processing
                }
                Err(e) => {
                    eprintln!("Cache lookup error: {}", e);
                    // Fall through to semaphore + processing
                }
            }
        }
    }

    // Cache miss: acquire semaphore for processing
    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem.acquire().await.map_err(|e| e.to_string())?;

    // Check if file exists
    if !path_buf.exists() {
        let error_msg = format!("File not found: {}", path_buf.display());
        eprintln!("ERROR: {}", error_msg);
        return Err(error_msg);
    }

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

            // Double-check cache (another concurrent request may have created it)
            let cached_result = task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr.get_cached(&file, &stretch_params).await
                })
            });

            if let Ok(Some(cached_data)) = cached_result {
                println!("✅ File cache hit (after semaphore): {} bytes in {:?}", cached_data.len(), t_start.elapsed());
                return Ok(cached_data);
            }

            // Process and cache
            let create_result = task::block_in_place(|| {
                Handle::current().block_on(async {
                    cache_mgr
                        .create_cache_entry(&file, &path_buf, &stretch_params, quality)
                        .await
                })
            });

            match create_result {
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
