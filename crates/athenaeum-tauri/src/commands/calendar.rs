// Calendar view commands - imaging activity by date
//
// Thin wrapper only: state extraction + handler call + error mapping.
// Business logic lives in `athenaeum_core::api::calendar`.

use crate::models::*;
use tauri::State;

use athenaeum_core::api::calendar as api;

use super::AppState;

/// Get calendar data for a specific month
///
/// Returns all imaging activity grouped by date for calendar rendering.
/// Includes both organized frame sets and unorganized LIGHT frames.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_calendar_month_data(
    year: i32,
    month: i32,
    state: State<'_, AppState>,
) -> Result<CalendarMonthData, String> {
    let db = state.ctx.db.get().ok_or("Database not initialized")?;
    api::get_calendar_month_data(&db.conn(), year, month).map_err(|e| e.to_string())
}
