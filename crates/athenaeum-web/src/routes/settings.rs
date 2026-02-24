// Settings route handlers — mirrors athenaeum-tauri/src/commands/settings.rs

use athenaeum_core::db;
use athenaeum_core::models::Setting;
use axum::{extract::State, http::StatusCode, Json};

use crate::WebAppState;

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GetSettingArgs {
    pub key: String,
    #[serde(rename = "defaultValue")]
    pub default_value: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct SetSettingArgs {
    pub key: String,
    pub value: String,
}

#[derive(serde::Deserialize)]
pub struct DeleteSettingArgs {
    pub key: String,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/get_setting
///
/// Returns the value for `key` using three-tier precedence:
/// runtime override → database → `defaultValue`. If the key is absent from the
/// database and no `defaultValue` was provided, an empty string is returned.
pub async fn get_setting(
    State(state): State<WebAppState>,
    Json(args): Json<GetSettingArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let default = args.default_value.unwrap_or_default();
    let value = state
        .ctx
        .settings
        .get_with_precedence(&conn, &args.key, &default)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(value))
}

/// POST /api/set_setting
///
/// Persists a key-value pair to the `settings` table. If the key already
/// exists it is updated; otherwise it is inserted.
pub async fn set_setting(
    State(state): State<WebAppState>,
    Json(args): Json<SetSettingArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    state
        .ctx
        .settings
        .persist_setting(&conn, &args.key, &args.value)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(()))
}

/// POST /api/get_all_settings
///
/// Returns every row in the `settings` table, ordered alphabetically by key.
pub async fn get_all_settings(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<Setting>>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let settings = db::get_all_settings(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(settings))
}

/// POST /api/delete_setting
///
/// Removes the setting row for the given key. A no-op if the key does not
/// exist.
pub async fn delete_setting(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteSettingArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    db::delete_setting(&conn, &args.key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(()))
}

/// POST /api/get_grouping_threshold_deg
///
/// Returns the frame-set clustering threshold in decimal degrees. Reads the
/// `grouping_threshold_arcmin` setting (with runtime and database precedence)
/// and converts it to degrees.
pub async fn get_grouping_threshold_deg(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<f64>, (StatusCode, String)> {
    let lock = state.ctx.db.lock().unwrap();
    let db = lock
        .as_ref()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let threshold = state
        .ctx
        .settings
        .get_grouping_threshold_deg(&conn)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(threshold))
}
