// Sky Chart HiPS background commands (issue #18). Thin wrappers over
// `athenaeum_core::api::hips`; the Axum mirror is `routes/hips.rs`.

use athenaeum_core::api::hips as api;
use tauri::State;

use super::AppState;

/// Survey presets with attribution, license and local cache usage.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_hips_surveys(state: State<'_, AppState>) -> Result<Vec<api::HipsSurveyInfo>, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::get_hips_surveys(&ctx))
        .await
        .map_err(|e| format!("HiPS survey task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// One tile's image bytes; an empty payload means no coverage there.
/// Returns [`tauri::ipc::Response`] so the bytes are not serialized as a
/// JSON number array. `level = "debug"`: the chart asks for tiles per view,
/// and a tile not cached while offline is a normal answer, so the boundary
/// error is debug too; a real fetch failure is already a `warn` in core.
#[tauri::command]
#[tracing::instrument(skip_all, err(level = "debug"), level = "debug")]
pub async fn get_hips_tile(
    state: State<'_, AppState>,
    survey_id: String,
    order: u8,
    npix: u64,
) -> Result<tauri::ipc::Response, String> {
    let ctx = state.ctx.clone();
    let tile = tokio::task::spawn_blocking(move || api::get_hips_tile(&ctx, &survey_id, order, npix))
        .await
        .map_err(|e| format!("HiPS tile task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(match tile {
        api::HipsTile::Image { bytes, .. } => bytes,
        api::HipsTile::Absent => Vec::new(),
    }))
}

/// Deletes a survey's local tiles; returns the bytes freed.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn clear_hips_cache(state: State<'_, AppState>, survey_id: String) -> Result<u64, String> {
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || api::clear_hips_cache(&ctx, &survey_id))
        .await
        .map_err(|e| format!("HiPS cache task panicked: {e}"))?
        .map_err(|e| e.to_string())
}
