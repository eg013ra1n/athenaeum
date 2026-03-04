// Image rendering route handlers.
//
// Renders a FITS/XISF file to JPEG using the rustafits processing pipeline.
// The memory image cache (MemoryImageCache) is used for fast repeated requests.

use athenaeum_core::cache::CachedImage;
use athenaeum_core::db;
use athenaeum_core::rustafits_processor::{self, AnnotationMetrics, Resolution};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::path::PathBuf;
use std::sync::Arc;

use crate::WebAppState;

// ── Request structs ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFramePreviewArgs {
    #[serde(alias = "fileId")]
    pub frame_id: i64,
    pub resolution: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct AnnotatedImageArgs {
    pub path: String,
    pub resolution: Option<String>,
}

/// Response for annotated image requests — JPEG bytes + analysis metrics
#[derive(serde::Serialize)]
pub struct AnnotatedImageResponse {
    pub image_data: Vec<u8>,
    pub metrics: Option<AnnotationMetrics>,
}

// ── Error helper ──────────────────────────────────────────────────────────────

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("images error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `POST /api/get_frame_preview`
///
/// Renders a FITS or XISF file identified by `fileId` to a JPEG image.
/// Optional `resolution` controls quality: "thumbnail", "preview" (default), or "full".
///
/// Returns raw JPEG bytes with `Content-Type: image/jpeg`.
pub async fn get_frame_preview(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramePreviewArgs>,
) -> Result<Response, (StatusCode, String)> {
    // ── 1. Read path and settings from DB, then drop the lock ────────────────

    let (file_path, resolution_str, quality) = {
        let db = state.ctx.db.get()
            .ok_or_else(|| db_err("Database not initialized"))?;
        let conn = db.conn();

        // Resolve the file path for this file_id.
        let path: String = conn
            .query_row(
                "SELECT path FROM files WHERE id = ?1",
                rusqlite::params![args.frame_id],
                |row| row.get(0),
            )
            .map_err(|e| db_err(format!("File id {} not found: {}", args.frame_id, e)))?;

        // Determine resolution string: prefer explicit arg, fall back to DB setting.
        let resolution_str = match args.resolution.as_deref() {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => db::get_setting(&conn, "blink.resolution")
                .map_err(|e| db_err(e))?
                .unwrap_or_else(|| "preview".to_string()),
        };

        let res = Resolution::from_string(&resolution_str);

        // Read per-resolution quality setting from DB.
        let quality: Option<u8> = db::get_setting(&conn, res.quality_setting_key())
            .map_err(|e| db_err(e))?
            .and_then(|v| v.parse::<u8>().ok());

        (path, resolution_str, quality)
        // DB lock dropped here.
    };

    // ── 2. Check memory cache ─────────────────────────────────────────────────

    let cache_key = format!("{}:{}", file_path, resolution_str);

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            return Ok((
                [(header::CONTENT_TYPE, "image/jpeg")],
                cached.data.clone(),
            )
                .into_response());
        }
    }

    // ── 3. Process FITS/XISF to JPEG on a blocking thread ────────────────────
    //
    // process_fits_to_jpeg is CPU-intensive synchronous work; we move it off
    // the async executor using spawn_blocking to avoid blocking tokio threads.
    // Acquire a semaphore permit first to limit concurrent image processing.

    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem
        .acquire_owned()
        .await
        .map_err(|e| db_err(format!("Semaphore closed: {}", e)))?;

    let path_buf = PathBuf::from(&file_path);
    let res = Resolution::from_string(&resolution_str);
    let pool: Arc<rayon::ThreadPool> = Arc::clone(&state.ctx.image_pool);

    let jpeg_data = tokio::task::spawn_blocking(move || {
        let _permit = _permit; // hold permit until processing completes
        rustafits_processor::process_fits_to_jpeg(&path_buf, res, quality, &pool)
    })
    .await
    .map_err(|e| db_err(format!("Task join error: {}", e)))?
    .map_err(|e| db_err(format!("Image processing failed: {}", e)))?
    .image_data;

    // ── 4. Store result in memory cache ───────────────────────────────────────

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        mem_cache.insert(cache_key, CachedImage { data: jpeg_data.clone(), last_accessed: std::time::Instant::now() });
    }

    // ── 5. Return JPEG bytes ──────────────────────────────────────────────────

    Ok(([(header::CONTENT_TYPE, "image/jpeg")], jpeg_data).into_response())
}

/// `POST /api/read_fits_image_annotated`
///
/// Renders a FITS/XISF file with star annotations burned in.
/// Returns JSON `{ image_data: [...], metrics: {...} }`.
pub async fn read_fits_image_annotated(
    State(state): State<WebAppState>,
    Json(args): Json<AnnotatedImageArgs>,
) -> Result<Json<AnnotatedImageResponse>, (StatusCode, String)> {
    let path = args.path;

    // ── 1. Read settings from DB ─────────────────────────────────────────────

    let (resolution_str, quality) = {
        let db = state.ctx.db.get()
            .ok_or_else(|| db_err("Database not initialized"))?;
        let conn = db.conn();

        let res_str = match args.resolution.as_deref() {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => db::get_setting(&conn, "blink.resolution")
                .map_err(|e| db_err(e))?
                .unwrap_or_else(|| "preview".to_string()),
        };

        let res = Resolution::from_string(&res_str);
        let q: Option<u8> = db::get_setting(&conn, res.quality_setting_key())
            .map_err(|e| db_err(e))?
            .and_then(|v| v.parse::<u8>().ok());

        (res_str, q)
    };

    // ── 2. Check memory cache ────────────────────────────────────────────────

    let cache_key = format!("{}:{}:annotated", path, resolution_str);

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            let metrics = state
                .ctx
                .annotation_metrics
                .lock()
                .unwrap()
                .get(&cache_key)
                .cloned();
            return Ok(Json(AnnotatedImageResponse {
                image_data: cached.data.clone(),
                metrics,
            }));
        }
    }

    // ── 3. Process with annotations on a blocking thread ─────────────────────

    let path_buf = PathBuf::from(&path);
    if !path_buf.exists() {
        return Err(db_err(format!("File not found: {}", path)));
    }

    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem
        .acquire_owned()
        .await
        .map_err(|e| db_err(format!("Semaphore closed: {}", e)))?;

    let res = Resolution::from_string(&resolution_str);
    let pool: Arc<rayon::ThreadPool> = Arc::clone(&state.ctx.image_pool);

    let result = tokio::task::spawn_blocking(move || {
        let _permit = _permit;
        rustafits_processor::process_fits_to_jpeg_annotated(&path_buf, res, quality, &pool)
    })
    .await
    .map_err(|e| db_err(format!("Task join error: {}", e)))?
    .map_err(|e| db_err(format!("Annotated image processing failed: {}", e)))?;

    let jpeg_data = result.image_data;
    let metrics = result.metrics.clone();

    // ── 4. Cache JPEG bytes and metrics ──────────────────────────────────────

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        mem_cache.insert(
            cache_key.clone(),
            CachedImage {
                data: jpeg_data.clone(),
                last_accessed: std::time::Instant::now(),
            },
        );
    }

    if let Some(ref m) = metrics {
        state
            .ctx
            .annotation_metrics
            .lock()
            .unwrap()
            .insert(cache_key, m.clone());
    }

    Ok(Json(AnnotatedImageResponse {
        image_data: jpeg_data,
        metrics,
    }))
}
