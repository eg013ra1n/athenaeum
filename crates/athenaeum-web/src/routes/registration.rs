//! Axum routes for the stacking-preparation registration feature.
//!
//! Mirror of `crates/athenaeum-tauri/src/commands/registration.rs`.
//! Real logic lives in `athenaeum_core::registration`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use athenaeum_core::plate_solve::config;
use athenaeum_core::registration;
use athenaeum_core::registration::db::{
    get_registration_for_frame_set, RegistrationRecord,
};
use athenaeum_core::registration::{
    clear_frame_set_reference as core_clear_frame_set_reference,
    get_frame_set_reference as core_get_frame_set_reference,
    set_frame_set_reference as core_set_frame_set_reference,
    FrameSetReference,
};
use athenaeum_core::services::RegistrationHandle;

use crate::events::SseProgressEmitter;
use crate::WebAppState;

// ── request shapes ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterFrameSetArgs {
    pub frames_set_id: i64,
    pub reference_frame_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frames_set_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFrameSetReferenceArgs {
    pub frames_set_id: i64,
    pub frame_id: i64,
}

// ── require_*_cache helpers (mirrors plate_solve.rs pattern) ─────────────────

fn require_star_cache(
    state: &WebAppState,
) -> Result<Arc<solvemyastro::StarCache>, (StatusCode, String)> {
    {
        let guard = state.ctx.star_cache.read().unwrap();
        if let Some(ref sc) = *guard {
            return Ok(sc.clone());
        }
    }
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let db_path = db.path();
    let parent = std::path::Path::new(&db_path)
        .parent()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Cannot determine app data dir".into()))?;
    let cache_dir = parent.join("catalogs").join("smac_gaia");
    if !cache_dir.exists() {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            format!(
                "solvemyastro star cache not found at {}. \
                 Please download the Gaia DR3 prebuilt catalog.",
                cache_dir.display()
            ),
        ));
    }
    let sc = solvemyastro::StarCache::open(&cache_dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to open star cache: {e}")))?;
    let arc = Arc::new(sc);
    {
        let mut guard = state.ctx.star_cache.write().unwrap();
        *guard = Some(arc.clone());
    }
    Ok(arc)
}

fn require_bright_cache(state: &WebAppState) -> Option<Arc<solvemyastro::StarCache>> {
    {
        let guard = state.ctx.bright_cache.read().unwrap();
        if let Some(ref bc) = *guard {
            return Some(bc.clone());
        }
    }
    let db = state.ctx.db.get()?;
    let ps_config = config::load_config(&db.conn());
    let cache_dir: std::path::PathBuf = match &ps_config.bright_cache_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let parent = std::path::Path::new(&db.path()).parent()?.to_path_buf();
            parent.join("catalogs").join("smac_gaia_bright")
        }
    };
    if !cache_dir.exists() {
        return None;
    }
    match solvemyastro::StarCache::open(&cache_dir) {
        Ok(bc) => {
            let arc = Arc::new(bc);
            let mut guard = state.ctx.bright_cache.write().unwrap();
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(_) => None,
    }
}

// ── route handlers ────────────────────────────────────────────────────────────

/// POST /api/register_frame_set
///
/// Begin (or re-run) registration for all LIGHT members of `framesSetId`.
/// Runs synchronously in a blocking task; progress is broadcast via SSE on
/// `stacking-prep-progress`, summary on `stacking-prep-complete`.
pub async fn register_frame_set(
    State(state): State<WebAppState>,
    Json(args): Json<RegisterFrameSetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let star_cache = require_star_cache(&state)?;
    let bright_cache = require_bright_cache(&state);
    let event_tx = state.event_tx.clone();

    let ps_config = {
        let db = state
            .ctx
            .db
            .get()
            .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
        config::load_config(&db.conn())
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut handles = state.ctx.active_registrations.lock().unwrap();
        handles.insert(args.frames_set_id, RegistrationHandle { cancel_flag: cancel_flag.clone() });
    }

    let ctx = state.ctx.clone();
    let cancel = cancel_flag.clone();

    let result = tokio::task::spawn_blocking(move || {
        let db = ctx
            .db
            .get()
            .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".to_string()))?;
        let conn = db.conn();
        let emitter = SseProgressEmitter::new(event_tx);

        registration::register_frame_set(
            &conn,
            args.frames_set_id,
            args.reference_frame_id,
            star_cache.as_ref(),
            bright_cache.as_deref(),
            &ps_config,
            &emitter,
            Some(cancel.as_ref()),
        )
        .map(|_| ())
        .map_err(|e| {
            eprintln!("registration: register_frame_set error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Registration task panicked: {e}"),
        )
    })??;

    {
        let mut handles = state.ctx.active_registrations.lock().unwrap();
        handles.remove(&args.frames_set_id);
    }

    let _ = result;
    Ok(Json(()))
}

/// POST /api/get_frame_set_registration
///
/// Return all persisted registration rows for a frame set.
pub async fn get_frame_set_registration(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<Vec<RegistrationRecord>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    get_registration_for_frame_set(&conn, args.frames_set_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/cancel_frame_set_registration
///
/// Signal the running registration for `framesSetId` to stop cooperatively.
pub async fn cancel_frame_set_registration(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let handles = state.ctx.active_registrations.lock().unwrap();
    if let Some(handle) = handles.get(&args.frames_set_id) {
        handle.cancel_flag.store(true, Ordering::Relaxed);
        eprintln!("registration: cancel flag set for frames_set_id={}", args.frames_set_id);
    } else {
        eprintln!("registration: no active registration for frames_set_id={}", args.frames_set_id);
    }
    Ok(Json(()))
}

/// POST /api/set_frame_set_reference
///
/// Persist the user-chosen reference frame for a frame set.
/// Validates that `frame_id` is a LIGHT member of the set.
pub async fn set_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<SetFrameSetReferenceArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    core_set_frame_set_reference(&conn, args.frames_set_id, args.frame_id)
        .map(|()| Json(()))
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// POST /api/get_frame_set_reference
///
/// Return the persisted user-chosen reference frame for a frame set, if any.
pub async fn get_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<Option<FrameSetReference>>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    core_get_frame_set_reference(&conn, args.frames_set_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/clear_frame_set_reference
///
/// Remove the persisted user-chosen reference for a frame set.
pub async fn clear_frame_set_reference(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let db = state
        .ctx
        .db
        .get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "DB not initialized".into()))?;
    let conn = db.conn();
    core_clear_frame_set_reference(&conn, args.frames_set_id)
        .map(|()| Json(()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
