//! Tauri commands for the stacking-preparation registration feature.
//!
//! These are thin wrappers: real logic lives in `athenaeum_core::registration`.
//! Progress is emitted via Tauri `app.emit` on `stacking-prep-progress` and
//! `stacking-prep-complete`, mirroring the pattern in `plate_solve.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{Emitter, State};

use athenaeum_core::plate_solve::config;
use athenaeum_core::registration;
use athenaeum_core::registration::db::{
    get_registration_for_frame_set, RegistrationRecord,
};
use athenaeum_core::services::PlateSolveHandle;

use super::AppState;
use super::plate_solve::{require_bright_cache, require_star_cache};

// ── cancel handle key ─────────────────────────────────────────────────────────

/// Key into `active_plate_solves` used to store the registration cancel flag.
/// Distinct from the plate-solve keys (0 = solve batch, 1 = autofind).
const CANCEL_KEY: i64 = 2;

// ── progress event forwarding emitter ────────────────────────────────────────

/// Bridges `ProgressEmitter::emit_json` into Tauri's event system.
struct TauriEmitter {
    app: tauri::AppHandle,
}

impl athenaeum_core::events::ProgressEmitter for TauriEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        let _ = self.app.emit(event_name, payload);
    }
}

// ── commands ──────────────────────────────────────────────────────────────────

/// Begin (or re-run) registration for all LIGHT members of `frames_set_id`.
///
/// Resolves the star cache, registers a cancellable handle, then runs
/// `registration::register_frame_set` on a blocking thread.  Progress events
/// are emitted on `stacking-prep-progress`; the completion summary on
/// `stacking-prep-complete`.
#[tauri::command]
pub async fn register_frame_set(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    frames_set_id: i64,
    reference_frame_id: Option<i64>,
) -> Result<(), String> {
    let star_cache = require_star_cache(&state)?;
    let bright_cache = require_bright_cache(&state);
    let ps_config = {
        let db = state.ctx.db.get().ok_or("Database not initialized")?;
        config::load_config(&db.conn())
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.insert(CANCEL_KEY, PlateSolveHandle { cancel_flag: cancel_flag.clone() });
    }

    let ctx = state.ctx.clone();
    let app_clone = app.clone();
    let cancel = cancel_flag.clone();

    let result = tokio::task::spawn_blocking(move || {
        let db = ctx.db.get().ok_or_else(|| "Database not initialized".to_string())?;
        let conn = db.conn();
        let emitter = TauriEmitter { app: app_clone };

        registration::register_frame_set(
            &conn,
            frames_set_id,
            reference_frame_id,
            star_cache.as_ref(),
            bright_cache.as_deref(),
            &ps_config,
            &emitter,
            Some(cancel.as_ref()),
        )
        .map(|_summary| ())
        .map_err(|e| {
            eprintln!("registration: register_frame_set error: {e}");
            e.to_string()
        })
    })
    .await
    .map_err(|e| format!("Registration task panicked: {e}"))?;

    {
        let mut handles = state.ctx.active_plate_solves.lock().unwrap();
        handles.remove(&CANCEL_KEY);
    }

    result
}

/// Retrieve all persisted registration rows for a frame set.
#[tauri::command]
pub async fn get_frame_set_registration(
    state: State<'_, AppState>,
    frames_set_id: i64,
) -> Result<Vec<RegistrationRecord>, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    let conn = db.conn();
    get_registration_for_frame_set(&conn, frames_set_id)
        .map_err(|e| e.to_string())
}

/// Signal the running registration (if any) to stop cooperatively.
#[tauri::command]
pub async fn cancel_frame_set_registration(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let handles = state.ctx.active_plate_solves.lock().unwrap();
    if let Some(handle) = handles.get(&CANCEL_KEY) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
        eprintln!("registration: cancel flag set for key {CANCEL_KEY}");
    } else {
        eprintln!("registration: no active registration to cancel");
    }
    Ok(())
}
