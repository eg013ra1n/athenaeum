// Image rendering route handlers.
//
// Renders a FITS/XISF file to JPEG using the rustafits processing pipeline.
// The memory image cache (MemoryImageCache) is used for fast repeated requests.

use athenaeum_core::cache::CachedImage;
use athenaeum_core::db;
use athenaeum_core::rustafits_processor::{self, Resolution};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
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

// ── Error helper ──────────────────────────────────────────────────────────────

// Deleted: the raw stderr print here duplicated the `#[tracing::instrument(err(Debug))]`
// attribute on `get_frame_preview` below, which already logs every returned
// Err at the command boundary — see the T7 sweep report.
fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `POST /api/get_frame_preview`
///
/// Renders a FITS or XISF file identified by `fileId` to a JPEG image.
/// Optional `resolution` controls quality: "thumbnail", "preview" (default), or "full".
///
/// Returns raw JPEG bytes with `Content-Type: image/jpeg`.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_frame_preview(
    State(state): State<WebAppState>,
    Json(args): Json<GetFramePreviewArgs>,
) -> Result<Response, (StatusCode, String)> {
    let file_path: String = {
        let db = state.ctx.db.get()
            .ok_or_else(|| db_err("Database not initialized"))?;
        let conn = db.conn();

        // Resolve the file path for this file_id.
        conn.query_row(
            "SELECT path FROM files WHERE id = ?1",
            rusqlite::params![args.frame_id],
            |row| row.get(0),
        )
        .map_err(|e| db_err(format!("File id {} not found: {}", args.frame_id, e)))?
        // DB lock dropped here.
    };
    render_path_jpeg(&state, file_path, args.resolution).await
}

/// Render the FITS/XISF file at `file_path` to JPEG through the shared cache,
/// gate, semaphore and pool. The caller has already resolved the path from the
/// DB (never from the client). Also used by `get_collab_frame_image`.
pub(crate) async fn render_path_jpeg(
    state: &WebAppState,
    file_path: String,
    resolution: Option<String>,
) -> Result<Response, (StatusCode, String)> {
    render_source_jpeg(state, PreviewSource::Catalog(file_path), resolution).await
}

/// Where a preview's bytes come from: a cataloged path, or a frame entry of a zip that the catalog holds no
/// row for (read from the zip cache, never written to the catalog).
pub(crate) enum PreviewSource {
    Catalog(String),
    ZipEntry { zip_path: String, entry: String },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewZipEntryArgs {
    pub zip_path: String,
    pub entry: String,
    pub resolution: Option<String>,
}

/// `POST /api/preview_zip_entry`
///
/// Renders a frame entry of a zip that the catalog holds no row for (a released entry whose link is gone, a
/// skipped or not yet scanned one) to a JPEG through the same cache, gate, semaphore and pool as
/// `get_frame_preview`. The zip path is checked against the allowed roots; nothing is written to the catalog.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn preview_zip_entry(
    State(state): State<WebAppState>,
    Json(args): Json<PreviewZipEntryArgs>,
) -> Result<Response, (StatusCode, String)> {
    let policy = crate::routes::scan_roots::allowed_roots_policy(&state.allowed_paths);
    athenaeum_core::api::zip::check_zip_entry_readable(&policy, &args.zip_path, &args.entry)
        .map_err(crate::routes::api_err)?;
    render_source_jpeg(&state, PreviewSource::ZipEntry { zip_path: args.zip_path, entry: args.entry }, args.resolution).await
}

async fn render_source_jpeg(
    state: &WebAppState,
    source: PreviewSource,
    resolution: Option<String>,
) -> Result<Response, (StatusCode, String)> {
    let file_path = match &source {
        PreviewSource::Catalog(p) => p.clone(),
        PreviewSource::ZipEntry { zip_path, entry } => {
            athenaeum_core::zip_store::scan::virtual_path(std::path::Path::new(zip_path), entry)
        }
    };
    // ── 1. Read resolution and quality settings, then drop the lock ──────────
    let (resolution_str, quality) = {
        let db = state.ctx.db.get()
            .ok_or_else(|| db_err("Database not initialized"))?;
        let conn = db.conn();

        // Determine resolution string: prefer explicit arg, fall back to DB setting.
        let resolution_str = match resolution.as_deref() {
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

        (resolution_str, quality)
        // DB lock dropped here.
    };

    // ── 2. Check memory cache ─────────────────────────────────────────────────
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
    let cache_key = key.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            format!("File not found: {} ({})", file_path, e),
        )
    })?;

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

    // Gate BEFORE permit, never the reverse (see `VNG_GATE`'s doc in core):
    // a full-resolution colour render waits here holding nothing, so the
    // image semaphore keeps serving thumbnails and previews meanwhile. The
    // guard travels into the `spawn_blocking` closure below with the permit —
    // a handler-local would drop (and release the gate under a still-running
    // render) if the client disconnects and axum drops this future at an
    // `.await`.
    let res = Resolution::from_string(&resolution_str);

    // Cache miss: a zip-resident frame is extracted (or reused from the zip cache) now, on a blocking thread;
    // the pin travels into the render below so the cache file outlives the read. Any other path is untouched.
    let (path_buf, zip_pin) = {
        let ctx = Arc::clone(&state.ctx);
        tokio::task::spawn_blocking(move || match source {
            PreviewSource::Catalog(p) => athenaeum_core::zip_store::materialize::materialize_one(&ctx, &p),
            PreviewSource::ZipEntry { zip_path, entry } => {
                athenaeum_core::zip_store::materialize::materialize_zip_entry(&ctx, &zip_path, &entry)
                    .map(|(local, pin)| (local, Some(pin)))
            }
        })
            .await
            .map_err(|e| db_err(format!("Task join error: {}", e)))?
            .map_err(|e| {
                tracing::warn!(path = %file_path, error = %e, "preview: frame is not readable");
                db_err(format!("Failed to read {}: {}", file_path, e))
            })?
    };
    let _vng_gate = if rustafits_processor::needs_vng_gate(&path_buf, res) {
        Some(rustafits_processor::VNG_GATE.lock().await)
    } else {
        None
    };

    let sem = state.image_semaphore.read().unwrap().clone();
    let _permit = sem
        .acquire_owned()
        .await
        .map_err(|e| db_err(format!("Semaphore closed: {}", e)))?;

    let pool: Arc<rayon::ThreadPool> = Arc::clone(&state.ctx.image_pool);

    let jpeg_data = tokio::task::spawn_blocking(move || {
        let _permit = _permit; // hold permit until processing completes
        let _zip_pin = zip_pin; // and the zip-cache pin: the cache file is read until the render returns
        let _vng_gate = _vng_gate; // and the VNG gate — a cancelled request must not release it under a running render
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


#[cfg(test)]
mod zip_tests {
    use super::*;
    use crate::events::SseEvent;
    use athenaeum_core::archive::models::ArchiveCompression;
    use athenaeum_core::archive::zip_writer::{build_zip, ZipEntry};
    use athenaeum_core::services::ServiceContext;
    use std::sync::RwLock;

    /// A 64x64 16-bit FITS with a bright blob, so the render has something to stretch.
    fn small_fits() -> Vec<u8> {
        let mut out = Vec::new();
        for c in [
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                   64",
            "NAXIS2  =                   64",
            "BZERO   =                32768",
            "BSCALE  =                    1",
            "IMAGETYP= 'Light   '",
            "OBJECT  = 'M33     '",
            "END",
        ] {
            let mut card = vec![b' '; 80];
            card[..c.len()].copy_from_slice(c.as_bytes());
            out.extend(card);
        }
        while out.len() % 2880 != 0 {
            out.push(b' ');
        }
        for y in 0..64i32 {
            for x in 0..64i32 {
                let d2 = (x - 32) * (x - 32) + (y - 32) * (y - 32);
                let v = 1000 + (30000.0 * (-(d2 as f64) / 40.0).exp()) as i32;
                out.extend(((v - 32768) as i16).to_be_bytes());
            }
        }
        while out.len() % 2880 != 0 {
            out.push(0);
        }
        out
    }

    fn state_for(tmp: &std::path::Path) -> WebAppState {
        let ctx = Arc::new(ServiceContext::new_for_tests(tmp.join("db").join("catalog.db")));
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths: Vec::new(),
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    /// Zips `small_fits` as `Lights/L_001.fits` in `<tmp>/astro/<zip_name>` and catalogs it. Returns (zip, file id).
    fn catalog_zip(state: &WebAppState, tmp: &std::path::Path, zip_name: &str) -> (std::path::PathBuf, i64) {
        let astro = tmp.join("astro");
        std::fs::create_dir_all(&astro).unwrap();
        let src = tmp.join(format!("src_{zip_name}.fits"));
        std::fs::write(&src, small_fits()).unwrap();
        let zip = astro.join(zip_name);
        build_zip(&zip, &[ZipEntry { source_path: src, path_in_zip: "Lights/L_001.fits".into() }], ArchiveCompression::Deflate).unwrap();
        let conn = state.ctx.db.get().unwrap().conn();
        conn.execute("INSERT OR IGNORE INTO scan_roots (id, path) VALUES (1, ?1)", [astro.to_str().unwrap()]).unwrap();
        athenaeum_core::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
        let catalog = athenaeum_core::zip_store::scan::virtual_path(&zip, "Lights/L_001.fits");
        let id = conn.query_row("SELECT id FROM files WHERE path = ?1", [&catalog], |r| r.get(0)).unwrap();
        (zip, id)
    }

    #[tokio::test]
    async fn get_frame_preview_renders_a_zip_resident_frame() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("db")).unwrap();
        let state = state_for(tmp.path());
        let (zip, file_id) = catalog_zip(&state, tmp.path(), "night.zip");

        let first = get_frame_preview(
            State(state.clone()),
            Json(GetFramePreviewArgs { frame_id: file_id, resolution: Some("thumbnail".into()) }),
        )
        .await
        .expect("a zip-resident frame renders");
        assert_eq!(first.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "a JPEG came back");
        assert_eq!(state.ctx.zip_cache.extraction_count(), 1);

        // The memory cache serves the second request without touching the zip cache again.
        let second = get_frame_preview(
            State(state.clone()),
            Json(GetFramePreviewArgs { frame_id: file_id, resolution: Some("thumbnail".into()) }),
        )
        .await
        .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(state.ctx.zip_cache.extraction_count(), 1);

        // A zip that went away fails like a deleted file, even with the JPEG cached.
        std::fs::remove_file(&zip).unwrap();
        let gone = get_frame_preview(
            State(state.clone()),
            Json(GetFramePreviewArgs { frame_id: file_id, resolution: Some("thumbnail".into()) }),
        )
        .await
        .unwrap_err();
        assert_eq!(gone.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn two_zips_with_the_same_entry_path_never_share_a_preview() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("db")).unwrap();
        let state = state_for(tmp.path());
        let (_za, a) = catalog_zip(&state, tmp.path(), "a.zip");
        let (_zb, b) = catalog_zip(&state, tmp.path(), "b.zip");
        for id in [a, b] {
            get_frame_preview(
                State(state.clone()),
                Json(GetFramePreviewArgs { frame_id: id, resolution: Some("thumbnail".into()) }),
            )
            .await
            .unwrap();
        }
        assert_eq!(state.ctx.zip_cache.extraction_count(), 2, "each zip's entry was rendered from its own extraction");
    }

    /// An entry the catalog holds no row for renders from the zip, and the catalog is not written.
    #[tokio::test]
    async fn a_zip_entry_without_a_row_is_previewed_from_the_zip_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("db")).unwrap();
        let state = state_for(tmp.path());
        let (zip, file_id) = catalog_zip(&state, tmp.path(), "night.zip");
        let zip_s = zip.to_string_lossy().into_owned();
        // The catalog forgot the entry (a link that went away with a voided copy).
        {
            let db = state.ctx.db.get().unwrap();
            let conn = db.conn();
            for table in ["frames", "fits_header"] {
                conn.execute(&format!("DELETE FROM {table} WHERE file_id = ?1"), [file_id]).unwrap();
            }
            conn.execute("DELETE FROM files WHERE id = ?1", [file_id]).unwrap();
        }
        let rows = |state: &WebAppState| -> i64 {
            let db = state.ctx.db.get().unwrap();
            let conn = db.conn();
            conn.query_row("SELECT (SELECT COUNT(*) FROM files) + (SELECT COUNT(*) FROM zip_released_entries)", [], |r| r.get(0)).unwrap()
        };
        let before = rows(&state);

        let args = || PreviewZipEntryArgs {
            zip_path: zip_s.clone(),
            entry: "Lights/L_001.fits".into(),
            resolution: Some("thumbnail".into()),
        };
        let first = preview_zip_entry(State(state.clone()), Json(args())).await.expect("an entry without a row renders");
        assert_eq!(first.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "a JPEG came back");
        assert_eq!(state.ctx.zip_cache.extraction_count(), 1);
        assert_eq!(rows(&state), before, "no catalog write");

        // The memory cache serves the second request.
        preview_zip_entry(State(state.clone()), Json(args())).await.unwrap();
        assert_eq!(state.ctx.zip_cache.extraction_count(), 1);

        // A name that is not a safe frame entry is refused before anything is read.
        for bad in ["../x.fits", "notes.txt"] {
            let refused = preview_zip_entry(
                State(state.clone()),
                Json(PreviewZipEntryArgs { zip_path: zip_s.clone(), entry: bad.into(), resolution: None }),
            )
            .await
            .unwrap_err();
            assert_ne!(refused.0, StatusCode::OK, "{bad}");
        }
        // A zip that went away fails like a deleted file.
        std::fs::remove_file(&zip).unwrap();
        assert!(preview_zip_entry(State(state.clone()), Json(args())).await.is_err());
    }
}
