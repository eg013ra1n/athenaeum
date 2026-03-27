// Rustafits-based commands for FITS image processing

use crate::cache::CachedImage;
use crate::commands::AppState;
use crate::rustafits_processor::{self, AnnotationMetrics, AnnotationSettings, Resolution};
use athenaeum_core::analysis::config as analysis_config;
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

/// Response for annotated image requests — JPEG bytes + analysis metrics
#[derive(Serialize)]
pub struct AnnotatedImageResponse {
    pub image_data: Vec<u8>,
    pub metrics: Option<AnnotationMetrics>,
}

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
    let t_process = Instant::now();

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

    let wait = t_process.duration_since(t_start);
    println!("✅ Memory cache: {} bytes in {:?} (waited {:?})", jpeg_data.len(), t_process.elapsed(), wait);
    Ok(jpeg_data)
}

/// Read FITS image with star annotations burned in + analysis metrics.
///
/// JPEG bytes are cached in memory_cache (key suffix ":annotated").
/// Metrics are cached in annotation_metrics map.
#[tauri::command]
pub async fn read_fits_image_annotated(
    path: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<AnnotatedImageResponse, String> {
    use std::time::Instant;

    let t_start = Instant::now();
    let path_buf = PathBuf::from(&path);

    // ── Step 1: Read settings from DB ──
    let (resolution_str, quality, ann_settings, analysis_cfg) = {
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

            // Load annotation display settings
            let ann: AnnotationSettings = match crate::db::get_setting(&conn, "blink.annotation_config") {
                Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
                _ => AnnotationSettings::default(),
            };

            // Load analysis config (same settings used by batch analysis)
            let acfg = analysis_config::load_config(&conn);

            (res_str, q, ann, acfg)
        } else {
            (
                resolution.as_deref().unwrap_or("preview").to_string(),
                None,
                AnnotationSettings::default(),
                analysis_config::AnalysisConfig::default(),
            )
        }
    };

    let res = match resolution_str.as_str() {
        "thumbnail" => Resolution::Thumbnail,
        "preview" => Resolution::Preview,
        "full" => Resolution::Full,
        _ => Resolution::Preview,
    };

    // ── Step 2: Memory cache lookup (fast path) ──
    // Include annotation + analysis config hashes for proper cache invalidation
    let ann_hash = {
        let json = serde_json::to_string(&ann_settings).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&json, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    };
    let analysis_hash = analysis_cfg.config_hash();
    let cache_key = format!("{}:{}:annotated:{:x}:{}", path, resolution_str, ann_hash, &analysis_hash[..8]);

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            // Look up cached metrics
            let metrics = state.ctx.annotation_metrics.lock().unwrap().get(&cache_key).cloned();
            println!("⚡ Annotated cache hit ({} bytes) in {:?}", cached.data.len(), t_start.elapsed());
            return Ok(AnnotatedImageResponse {
                image_data: cached.data.clone(),
                metrics,
            });
        }
    }

    if !path_buf.exists() {
        let error_msg = format!("File not found: {}", path_buf.display());
        eprintln!("ERROR: {}", error_msg);
        return Err(error_msg);
    }

    // Slow path: acquire semaphore
    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem.acquire().await.map_err(|e| e.to_string())?;
    let t_process = Instant::now();

    // Double-check cache
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            let metrics = state.ctx.annotation_metrics.lock().unwrap().get(&cache_key).cloned();
            println!("⚡ Annotated cache hit (after semaphore, {} bytes) in {:?}", cached.data.len(), t_start.elapsed());
            return Ok(AnnotatedImageResponse {
                image_data: cached.data.clone(),
                metrics,
            });
        }
    }

    // Cache miss — process with annotations
    let result = tokio::task::block_in_place(|| {
        rustafits_processor::process_fits_to_jpeg_annotated(&path_buf, res, quality, &state.ctx.image_pool, Some(&ann_settings), &analysis_cfg)
    })
    .map_err(|e| {
        let error_msg = format!("Failed to process annotated FITS image: {}", e);
        eprintln!("ERROR: {}", error_msg);
        error_msg
    })?;

    let jpeg_data = result.image_data;
    let metrics = result.metrics.clone();

    // Cache JPEG bytes
    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        mem_cache.insert(cache_key.clone(), CachedImage { data: jpeg_data.clone(), last_accessed: Instant::now() });
    }

    // Cache metrics separately
    if let Some(ref m) = metrics {
        state.ctx.annotation_metrics.lock().unwrap().insert(cache_key, m.clone());
    }

    let wait = t_process.duration_since(t_start);
    println!("✅ Annotated cache: {} bytes in {:?} (waited {:?})", jpeg_data.len(), t_process.elapsed(), wait);
    Ok(AnnotatedImageResponse {
        image_data: jpeg_data,
        metrics,
    })
}
