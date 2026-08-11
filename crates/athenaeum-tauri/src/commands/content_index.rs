// Content-index commands (`files.content_hash` for the transfer dedup
// handshake) — thin wrappers only. Trigger policy and the worker live in
// `athenaeum_core::api::content_index`.

use std::sync::Arc;

use athenaeum_core::api;
use athenaeum_core::api::content_index::ContentIndexStatus;
use athenaeum_core::monitor::ScanCompletionHook;
use athenaeum_core::services::ServiceContext;
use tauri::{AppHandle, State};

use super::AppState;

/// What the Settings card renders: pending/total rows and whether a pass is
/// in flight or the automatic trigger is even armed on this node.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_content_index_status(
    state: State<'_, AppState>,
) -> Result<ContentIndexStatus, String> {
    api::content_index::get_content_index_status(&state.ctx).map_err(|e| e.to_string())
}

/// Manual "Index now". Returns false when a pass is already in flight.
///
/// This is the ONLY seam (with its Axum mirror) that clears a cancel: pressing
/// the button is the user changing their mind, so the automatic trigger is
/// armed again too. Core deliberately does not clear inside
/// `start_content_index` — the autostart calls that as well, and clearing there
/// would let an autostart racing a cancelling worker erase the suppression.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn start_content_index(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<bool, String> {
    let db = state
        .ctx
        .db
        .get()
        .cloned()
        .ok_or_else(|| "database not initialized".to_string())?;
    api::content_index::clear_cancelled_by_user(db.path());
    let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
        Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle));
    Ok(api::content_index::start_content_index(
        db,
        state.ctx.compute_queue.clone(),
        emitter,
    ))
}

/// Re-arms the content index after an UNATTENDED monitor scan ingests new
/// files.
///
/// The monitor polls scan roots on its own timer and calls
/// `scanner::run_registered_scan` directly, so it mints NULL-hash rows without
/// ever passing the command boundary where the interactive re-arm lives. This
/// hook is the only reach into that path, and core installs nothing itself —
/// `monitor` must not depend on `api`, hence a host-installed trait object.
pub(crate) struct ContentIndexRearmHook {
    ctx: Arc<ServiceContext>,
    app_handle: AppHandle,
}

impl ContentIndexRearmHook {
    pub(crate) fn new(ctx: Arc<ServiceContext>, app_handle: AppHandle) -> Self {
        Self { ctx, app_handle }
    }
}

impl ScanCompletionHook for ContentIndexRearmHook {
    /// `new_file_ids` is deliberately ignored: the pass re-derives its own work
    /// from the catalog (every NULL-hash row), so narrowing it to this cycle's
    /// ids would make the trigger less complete, not more precise.
    ///
    /// Spawns and returns — the hook contract forbids blocking the monitor's
    /// cycle thread, and the gate/single-flight/convergence checks inside
    /// `autostart_content_index` are what make a repeat fire free.
    fn on_scan_completed(&self, _new_file_ids: Vec<i64>) {
        let ctx = Arc::clone(&self.ctx);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> = Arc::new(
            crate::tauri_events::TauriProgressEmitter(self.app_handle.clone()),
        );
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }
}
