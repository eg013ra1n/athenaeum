//! Trigger-only shim for the auto-publish worker (collab v3 wave 2, Task 10).
//! Plate solving itself stays where it is (`plate_solve::service` + the
//! per-host `plate_solve_batch` command — see
//! `crates/athenaeum-tauri/src/commands/plate_solve.rs` and its web mirror);
//! this module exists only so both hosts have ONE shared call after they
//! emit `plate-solve-complete`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;

use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

/// Called by both hosts right after `plate-solve-complete` fires. A
/// plate-solve batch is a free-form list of frame ids (not scoped to one
/// frame set), so mapping it cheaply to "which linked projects care" isn't
/// worth it here — this dirties every auto-publish-enabled project instead
/// (the worker's non-Manual-mode + linked-set filter still applies, and a
/// run with nothing new to generate is cheap — Task 7).
pub fn on_batch_finished(_ctx: &ServiceContext) {
    crate::api::collab_autopublish::request_auto_publish(None);
}

/// Payload of the `catalog-download-progress` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDownloadProgress {
    phase: String,
    current: usize,
    total: usize,
    percent: f64,
    tier_density: u32,
    tier_index: usize,
    n_tiers: usize,
}

/// Download the prebuilt star-catalog layers up to `target_density` into the
/// app-data directory (the catalog database's parent) and return the catalog
/// root path. Runs the download on a blocking thread and emits
/// `catalog-download-progress` through `emitter`. The cancel flag is local to
/// the call and registered nowhere, so the download cannot be cancelled.
pub async fn download_catalog_layers(
    ctx: &ServiceContext,
    emitter: Arc<dyn ProgressEmitter>,
    target_density: u32,
) -> Result<String, ApiError> {
    let app_data_dir = db(ctx)?
        .path()
        .to_path_buf()
        .parent()
        .ok_or_else(|| ApiError::Internal("Cannot determine app data directory".into()))?
        .to_path_buf();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let result = tokio::task::spawn_blocking(move || {
        use crate::catalog::gaia_prebuilt::GaiaPrebuiltProgress as P;
        let cur = std::sync::Mutex::new((0u32, 0usize, 0usize)); // (density, index, n_tiers)
        crate::catalog::gaia_prebuilt::download_catalog_layers(
            &app_data_dir,
            target_density,
            cancel_flag,
            &|progress| {
                let (td, ti, nt) = *cur.lock().unwrap();
                let event = match progress {
                    P::Tier { density, index, n_tiers } => {
                        *cur.lock().unwrap() = (density, index, n_tiers);
                        CatalogDownloadProgress {
                            phase: "tier".into(), current: index, total: n_tiers,
                            percent: 0.0, tier_density: density, tier_index: index, n_tiers,
                        }
                    }
                    P::Downloading { received, total } => CatalogDownloadProgress {
                        phase: "downloading".into(), current: received as usize, total: total as usize,
                        percent: if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 },
                        tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Verifying => CatalogDownloadProgress {
                        phase: "verifying".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Extracting { done, total } => CatalogDownloadProgress {
                        phase: "extracting".into(), current: done, total,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Complete { files } => CatalogDownloadProgress {
                        phase: "complete".into(), current: files, total: files,
                        percent: 100.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                    P::Error(_) => CatalogDownloadProgress {
                        phase: "error".into(), current: 0, total: 0,
                        percent: 0.0, tier_density: td, tier_index: ti, n_tiers: nt,
                    },
                };
                emit_event(emitter.as_ref(), "catalog-download-progress", &event);
            },
        )
    })
    .await
    .map_err(|e| ApiError::Internal(format!("download task panicked: {e}")))?;

    result
        .map(|p| p.display().to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))
}
