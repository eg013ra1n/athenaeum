// Rustafits-based commands for FITS image processing

use crate::cache::CachedImage;
use crate::commands::AppState;
use crate::rustafits_processor::{self, Resolution};
use std::path::PathBuf;
use tauri::State;

/// Read FITS image and return the JPEG as raw bytes.
///
/// Returns [`tauri::ipc::Response`] rather than a bare `Vec<u8>` on purpose. A
/// command returning `Vec<u8>` goes through the blanket `impl<T: Serialize>
/// IpcResponse`, i.e. `serde_json` — a full-resolution one-shot-colour frame is
/// a 17 MB JPEG, which would cross the IPC boundary as a JSON array of
/// seventeen million numbers. `Response` sends `application/octet-stream`
/// instead, which the webview reads as an `ArrayBuffer`.
///
/// Callers inside the Rust side want the bytes, not the wrapper — they use
/// [`read_fits_image_bytes`].
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn read_fits_image_rustafits(
    path: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    read_fits_image_bytes(path, resolution, state)
        .await
        .map(tauri::ipc::Response::new)
}

/// Read FITS image and return JPEG bytes via in-memory cache.
///
/// Cache hits bypass the semaphore entirely (<1ms).
/// The semaphore is only acquired for cache misses (actual image processing).
pub async fn read_fits_image_bytes(
    path: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    render_preview_bytes(PreviewSource::Catalog(path), resolution, state).await
}

/// Where a preview's bytes come from: a cataloged path, or a frame entry of a zip that the catalog holds no
/// row for (read from the zip cache, never written to the catalog).
pub enum PreviewSource {
    Catalog(String),
    ZipEntry { zip_path: String, entry: String },
}

/// Render a frame entry of a zip that the catalog holds no row for (a released entry whose link is gone, a
/// skipped or not yet scanned one) and return the JPEG as raw bytes. Same cache, gates and semaphore as
/// [`read_fits_image_rustafits`]. The zip path is checked by the caller.
#[tauri::command]
#[tracing::instrument(skip_all, err, level = "debug")]
pub async fn preview_zip_entry(
    zip_path: String,
    entry: String,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    athenaeum_core::api::zip::check_zip_entry_readable(&athenaeum_core::api::PathPolicy::AllowAll, &zip_path, &entry)
        .map_err(|e| e.to_string())?;
    render_preview_bytes(PreviewSource::ZipEntry { zip_path, entry }, resolution, state)
        .await
        .map(tauri::ipc::Response::new)
}

async fn render_preview_bytes(
    source: PreviewSource,
    resolution: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    use std::time::Instant;

    let t_start = Instant::now();
    let path = match &source {
        PreviewSource::Catalog(p) => p.clone(),
        PreviewSource::ZipEntry { zip_path, entry } => {
            athenaeum_core::zip_store::scan::virtual_path(std::path::Path::new(zip_path), entry)
        }
    };
    let catalog_path = PathBuf::from(&path);

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
    // A zip-resident frame is keyed by its catalog path plus the zip's and the entry's identity, so a hit
    // needs no extraction and two zips holding the same entry path never share a key.
    let key = match &source {
        PreviewSource::Catalog(p) => athenaeum_core::cache::catalog_preview_cache_key(&state.ctx, p, &resolution_str),
        PreviewSource::ZipEntry { zip_path, entry } => {
            athenaeum_core::cache::zip_entry_preview_cache_key(zip_path, entry, &resolution_str)
        }
    };
    let cache_key = match key {
        Ok(key) => key,
        Err(e) => {
            return Err(format!("File not found: {} ({})", catalog_path.display(), e));
        }
    };

    {
        let mut mem_cache = state.ctx.memory_cache.lock().unwrap();
        if let Some(cached) = mem_cache.get(&cache_key) {
            tracing::trace!(bytes = cached.data.len(), duration = ?t_start.elapsed(), "memory cache hit (fast path)");
            return Ok(cached.data.clone());
        }
    }

    // Cache miss: a zip-resident frame is extracted (or reused from the zip cache) now; the pin keeps the
    // cache file alive until the render below has read it. Any other path comes back untouched.
    let (path_buf, _zip_pin) = tokio::task::block_in_place(|| match &source {
        PreviewSource::Catalog(p) => athenaeum_core::zip_store::materialize::materialize_one(&state.ctx, p),
        PreviewSource::ZipEntry { zip_path, entry } => {
            athenaeum_core::zip_store::materialize::materialize_zip_entry(&state.ctx, zip_path, entry)
                .map(|(local, pin)| (local, Some(pin)))
        }
    })
    .map_err(|e| {
        tracing::warn!(path = %path, error = %e, "preview: frame is not readable");
        format!("Failed to read {}: {}", catalog_path.display(), e)
    })?;

    // Gate BEFORE permit, never the reverse (see `VNG_GATE`'s doc): a
    // full-resolution colour render waits here holding nothing, so the image
    // semaphore keeps serving thumbnails and previews meanwhile. Cache hits
    // above never reach this line, so they never probe a header either. Unlike
    // the web host, the guard does not need to travel into the blocking
    // section here: `block_in_place` below runs the render synchronously on
    // this same task, with no `.await` inside it — once the task starts
    // rendering it cannot be suspended and dropped again until the render
    // returns, so a plain local guard already covers the whole render, the
    // way the web host's moved-in guard has to be arranged to do.
    let _vng_gate = if rustafits_processor::needs_vng_gate(&path_buf, res) {
        Some(rustafits_processor::VNG_GATE.lock().await)
    } else {
        None
    };

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

