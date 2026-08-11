// Content-index route handlers — mirrors athenaeum-tauri/src/commands/content_index.rs.
// Thin wrappers only; trigger policy and the worker live in
// `athenaeum_core::api::content_index`.

use std::sync::Arc;

use athenaeum_core::api;
use athenaeum_core::api::content_index::ContentIndexStatus;
use athenaeum_core::events::ProgressEmitter;
use athenaeum_core::monitor::ScanCompletionHook;
use athenaeum_core::services::ServiceContext;
use axum::{extract::State, http::StatusCode, Json};
use tokio::sync::broadcast;

use crate::events::{SseEvent, SseProgressEmitter};
use crate::routes::api_err;
use crate::WebAppState;

/// POST /api/get_content_index_status
///
/// What the Settings card renders: pending/total rows and whether a pass is
/// in flight or the automatic trigger is even armed on this node. No args;
/// the client's `{}` body is ignored.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_content_index_status(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<ContentIndexStatus>, (StatusCode, String)> {
    api::content_index::get_content_index_status(&state.ctx)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/start_content_index
///
/// Manual "Index now". Returns false when a pass is already in flight.
///
/// This is the ONLY seam (with its Tauri mirror) that clears a cancel: pressing
/// the button is the user changing their mind, so the automatic trigger is
/// armed again too. Core deliberately does not clear inside
/// `start_content_index` — the autostart calls that as well, and clearing there
/// would let an autostart racing a cancelling worker erase the suppression.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_content_index(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<bool>, (StatusCode, String)> {
    let db = state.ctx.db.get().cloned().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "database not initialized".to_string(),
    ))?;
    api::content_index::clear_cancelled_by_user(db.path());
    let emitter: Arc<dyn ProgressEmitter> =
        Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    Ok(Json(api::content_index::start_content_index(
        db,
        state.ctx.compute_queue.clone(),
        emitter,
    )))
}

/// Re-arms the content index after an UNATTENDED monitor scan ingests new
/// files.
///
/// The monitor polls scan roots on its own timer and calls
/// `scanner::run_registered_scan` directly, so it mints NULL-hash rows without
/// ever passing the route boundary where the interactive re-arm lives. This
/// hook is the only reach into that path, and core installs nothing itself —
/// `monitor` must not depend on `api`, hence a host-installed trait object.
pub(crate) struct ContentIndexRearmHook {
    ctx: Arc<ServiceContext>,
    event_tx: broadcast::Sender<SseEvent>,
}

impl ContentIndexRearmHook {
    pub(crate) fn new(ctx: Arc<ServiceContext>, event_tx: broadcast::Sender<SseEvent>) -> Self {
        Self { ctx, event_tx }
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
        let emitter: Arc<dyn ProgressEmitter> =
            Arc::new(SseProgressEmitter::new(self.event_tx.clone()));
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }
}
