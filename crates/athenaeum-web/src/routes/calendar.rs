// Calendar route handlers - imaging activity by date.
//
// Mirrors `athenaeum-tauri/src/commands/calendar.rs` one-for-one. Thin wrapper
// only: extraction + handler call + error mapping; business logic lives in
// `athenaeum_core::api::calendar`.

use athenaeum_core::api::calendar as api;
use athenaeum_core::models::CalendarMonthData;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

use crate::WebAppState;

/// Arguments for `get_calendar_month_data`.
#[derive(Debug, Deserialize)]
pub struct CalendarMonthArgs {
    pub year: i32,
    pub month: i32,
}

/// `POST /api/get_calendar_month_data`
///
/// Returns all imaging activity grouped by date for the given calendar month.
/// Includes both organised frame sets and unorganised LIGHT frames.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calendar_month_data(
    State(state): State<WebAppState>,
    Json(args): Json<CalendarMonthArgs>,
) -> Result<Json<CalendarMonthData>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    api::get_calendar_month_data(&db.conn(), args.year, args.month)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
