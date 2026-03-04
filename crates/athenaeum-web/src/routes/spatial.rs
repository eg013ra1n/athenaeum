// Spatial query route handlers - sky coordinates and calendar data.
//
// These handlers mirror the logic from the Tauri spatial and calendar commands.
// The SQL queries and result mapping are kept identical to the Tauri originals
// so that the web API returns the same data shapes as the desktop app.

use axum::{extract::State, http::StatusCode, Json};
use athenaeum_core::models::{
    CalendarMonthData, ImagingLocation, SelectionBounds, SelectionCandidates, SelectionCandidate,
};
use serde::Deserialize;

use crate::WebAppState;

/// camelCase wrapper for the core `SelectionBounds` type.
/// Frontend sends `raMin`, `raMax`, etc. but the core struct uses snake_case.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBoundsArgs {
    pub ra_min: f64,
    pub ra_max: f64,
    pub dec_min: f64,
    pub dec_max: f64,
    #[serde(default)]
    pub crosses_meridian: Option<bool>,
    #[serde(default)]
    pub selected_object_ids: Option<Vec<i64>>,
}

impl From<SelectionBoundsArgs> for SelectionBounds {
    fn from(a: SelectionBoundsArgs) -> Self {
        SelectionBounds {
            ra_min: a.ra_min,
            ra_max: a.ra_max,
            dec_min: a.dec_min,
            dec_max: a.dec_max,
            crosses_meridian: a.crosses_meridian,
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Calculate field of view in degrees given camera and telescope parameters.
///
/// Mirrors `athenaeum-tauri::commands::utils::calculate_fov`.
fn calculate_fov(
    pixel_size_um: Option<f64>,
    focal_length_mm: Option<f64>,
    naxis: Option<i32>,
    _binning: Option<i32>,
) -> Option<f64> {
    match (pixel_size_um, focal_length_mm, naxis) {
        (Some(pixel_size), Some(focal_len), Some(sensor_pixels))
            if focal_len > 0.0 && sensor_pixels > 0 =>
        {
            let pixel_size_mm = pixel_size / 1000.0;
            let sensor_mm = pixel_size_mm * sensor_pixels as f64;
            let fov_radians = 2.0 * (sensor_mm / (2.0 * focal_len)).atan();
            Some(fov_radians.to_degrees())
        }
        _ => None,
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /api/get_imaging_locations`
///
/// Returns all imaging locations: organised frame sets and unorganised
/// coordinate clusters, so the sky map can display targets before the user
/// runs auto-generate.
pub async fn get_imaging_locations(
    State(state): State<WebAppState>,
) -> Result<Json<Vec<ImagingLocation>>, (StatusCode, String)> {
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare(
            "
        -- Organised locations: frames that are part of frame sets
        SELECT
            fs.id as frame_set_id,
            fs.name as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            COALESCE(SUM(fr.exptime), 0) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.ypixsz) as avg_ypixsz,
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'frameset' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            fs.is_custom as is_custom,
            AVG(SIN(fr.rotation * 3.14159265358979 / 180.0)) as avg_sin_rot,
            AVG(COS(fr.rotation * 3.14159265358979 / 180.0)) as avg_cos_rot
        FROM frames_set fs
        JOIN imaging_nights ino ON ino.frames_set_id = fs.id
        JOIN sessions s ON s.imaging_night_id = ino.id
        JOIN session_members sm ON sm.session_id = s.id
        JOIN frames fr ON fr.id = sm.frame_id
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
        GROUP BY fs.id
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL

        UNION ALL

        -- Unorganised locations: frames not in any session, clustered by position
        SELECT
            NULL as frame_set_id,
            COALESCE(fr.object, 'Unknown') as object_name,
            AVG(fr.ra) as avg_ra,
            AVG(fr.dec) as avg_dec,
            COUNT(DISTINCT fr.id) as frame_count,
            COALESCE(SUM(fr.exptime), 0) as total_exposure,
            GROUP_CONCAT(DISTINCT fr.filter) as filters,
            MIN(fr.date_obs) as first_date,
            MAX(fr.date_obs) as last_date,
            AVG(fr.xpixsz) as avg_xpixsz,
            AVG(fr.ypixsz) as avg_ypixsz,
            AVG(fr.focallen) as avg_focallen,
            AVG(fr.naxis1) as avg_naxis1,
            AVG(fr.naxis2) as avg_naxis2,
            AVG(fr.xbinning) as avg_xbinning,
            AVG(fr.ybinning) as avg_ybinning,
            'cluster' as location_type,
            GROUP_CONCAT(DISTINCT fr.instrume) as cameras,
            GROUP_CONCAT(DISTINCT CAST(fr.focallen AS TEXT)) as focal_lengths,
            0 as is_custom,
            AVG(SIN(fr.rotation * 3.14159265358979 / 180.0)) as avg_sin_rot,
            AVG(COS(fr.rotation * 3.14159265358979 / 180.0)) as avg_cos_rot
        FROM frames fr
        WHERE fr.ra IS NOT NULL
          AND fr.dec IS NOT NULL
          AND fr.imagetyp = 'Light'
          AND NOT EXISTS (
              SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
          )
        GROUP BY COALESCE(fr.object, 'Unknown'), ROUND(fr.ra, 1), ROUND(fr.dec, 1)
        HAVING avg_ra IS NOT NULL AND avg_dec IS NOT NULL
        ",
        )
        .map_err(|e| {
            eprintln!("get_imaging_locations: prepare failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let locations = stmt
        .query_map([], |row| {
            let frame_set_id: Option<i64> = row.get(0)?;
            let object_name: Option<String> = row.get(1)?;
            let ra: f64 = row.get(2)?;
            let dec: f64 = row.get(3)?;
            let frame_count: i32 = row.get(4)?;
            let total_exposure: f64 = row.get(5)?;
            let filters_str: Option<String> = row.get(6)?;
            let first_date: Option<String> = row.get(7)?;
            let last_date: Option<String> = row.get(8)?;
            let avg_xpixsz: Option<f64> = row.get(9)?;
            let avg_ypixsz: Option<f64> = row.get(10)?;
            let avg_focallen: Option<f64> = row.get(11)?;
            let avg_naxis1: Option<f64> = row.get(12)?;
            let avg_naxis2: Option<f64> = row.get(13)?;
            let avg_xbinning: Option<f64> = row.get(14)?;
            let avg_ybinning: Option<f64> = row.get(15)?;
            let location_type: String = row.get(16)?;
            let cameras_str: Option<String> = row.get(17)?;
            let focal_lengths_str: Option<String> = row.get(18)?;
            let is_custom: i64 = row.get(19)?;
            let avg_sin_rot: Option<f64> = row.get(20)?;
            let avg_cos_rot: Option<f64> = row.get(21)?;

            let avg_rotation: Option<f64> = match (avg_sin_rot, avg_cos_rot) {
                (Some(s), Some(c)) => Some(s.atan2(c).to_degrees()),
                _ => None,
            };

            let filters: Vec<String> = filters_str
                .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
                .unwrap_or_default();

            let fov_width = calculate_fov(
                avg_xpixsz,
                avg_focallen,
                avg_naxis1.map(|n| n.round() as i32),
                avg_xbinning.map(|b| b.round() as i32),
            );
            let fov_height = calculate_fov(
                avg_ypixsz.or(avg_xpixsz),
                avg_focallen,
                avg_naxis2.map(|n| n.round() as i32),
                avg_ybinning.map(|b| b.round() as i32),
            );

            let id = if let Some(fs_id) = frame_set_id {
                fs_id
            } else {
                ((ra.to_bits() as i64) ^ (dec.to_bits() as i64)).abs()
            };

            Ok(ImagingLocation {
                id,
                ra,
                dec,
                object_name,
                frame_count,
                total_exposure,
                filters,
                date_range: (
                    first_date.unwrap_or_default(),
                    last_date.unwrap_or_default(),
                ),
                frame_set_id,
                fov_width,
                fov_height,
                location_type,
                cameras: cameras_str,
                focal_lengths: focal_lengths_str,
                is_custom: is_custom != 0,
                rotation: avg_rotation,
            })
        })
        .map_err(|e| {
            eprintln!("get_imaging_locations: query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let result: Vec<ImagingLocation> = locations
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            eprintln!("get_imaging_locations: row collect failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    println!(
        "get_imaging_locations: {} locations ({} framesets, {} clusters)",
        result.len(),
        result.iter().filter(|l| l.location_type == "frameset").count(),
        result.iter().filter(|l| l.location_type == "cluster").count(),
    );

    Ok(Json(result))
}

/// `POST /api/query_frames_in_bounds`
///
/// Returns candidate LIGHT frames whose RA/Dec falls inside the supplied
/// bounding box.  Handles the RA wrap-around at 0°/360°.
pub async fn query_frames_in_bounds(
    State(state): State<WebAppState>,
    Json(args): Json<SelectionBoundsArgs>,
) -> Result<Json<SelectionCandidates>, (StatusCode, String)> {
    let bounds: SelectionBounds = args.into();
    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let ra_wrap_around = bounds
        .crosses_meridian
        .unwrap_or_else(|| bounds.ra_min > bounds.ra_max);

    println!(
        "query_frames_in_bounds: ra_min={}, ra_max={}, dec_min={}, dec_max={}, wrap={}",
        bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max, ra_wrap_around
    );

    let query = if ra_wrap_around {
        "SELECT id, ra, dec, COALESCE(exptime, 0) FROM frames
         WHERE ra IS NOT NULL AND dec IS NOT NULL AND imagetyp = 'Light'
         AND (ra >= ?1 OR ra <= ?2) AND dec BETWEEN ?3 AND ?4"
    } else {
        "SELECT id, ra, dec, COALESCE(exptime, 0) FROM frames
         WHERE ra IS NOT NULL AND dec IS NOT NULL AND imagetyp = 'Light'
         AND ra BETWEEN ?1 AND ?2 AND dec BETWEEN ?3 AND ?4"
    };

    let mut stmt = conn.prepare(query).map_err(|e| {
        eprintln!("query_frames_in_bounds: prepare failed: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    let candidates: Vec<SelectionCandidate> = stmt
        .query_map(
            rusqlite::params![bounds.ra_min, bounds.ra_max, bounds.dec_min, bounds.dec_max],
            |row| {
                Ok(SelectionCandidate {
                    id: row.get(0)?,
                    ra: row.get(1)?,
                    dec: row.get(2)?,
                    exposure: row.get(3)?,
                })
            },
        )
        .map_err(|e| {
            eprintln!("query_frames_in_bounds: query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            eprintln!("query_frames_in_bounds: row collect failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let total_exposure: f64 = candidates.iter().map(|c| c.exposure).sum();
    let count = candidates.len();

    println!(
        "query_frames_in_bounds: {} candidates (wrap={})",
        count, ra_wrap_around
    );

    Ok(Json(SelectionCandidates {
        candidates,
        count,
        total_exposure_seconds: total_exposure,
    }))
}

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
pub async fn get_calendar_month_data(
    State(state): State<WebAppState>,
    Json(args): Json<CalendarMonthArgs>,
) -> Result<Json<CalendarMonthData>, (StatusCode, String)> {
    use athenaeum_core::coordinates::{parse_ra_sexagesimal, parse_dec_sexagesimal};
    use athenaeum_core::models::{
        CalendarDayEvent, CalendarFrameSetSummary, CalendarUnorganizedGroup,
    };
    use std::collections::HashMap;

    let db = state.ctx.db.get()
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Database not initialized".to_string()))?;
    let conn = db.conn();

    let year = args.year;
    let month = args.month;

    let start_date = format!("{:04}-{:02}-01", year, month);
    let end_date = if month == 12 {
        format!("{:04}-01-01", year + 1)
    } else {
        format!("{:04}-{:02}-01", year, month + 1)
    };

    let mut events_by_date: HashMap<String, CalendarDayEvent> = HashMap::new();

    // Query 1: organised frame sets
    let mut stmt = conn
        .prepare(
            "SELECT
                DATE(fr.date_obs) as obs_date,
                fs.id as frame_set_id,
                fs.name as set_name,
                COALESCE(fs.name, fr.object) as object_name,
                COUNT(DISTINCT fr.id) as frame_count,
                COALESCE(SUM(fr.exptime), 0) as total_exposure,
                fs.objctra as avg_ra,
                fs.objctdec as avg_dec,
                GROUP_CONCAT(DISTINCT fr.filter) as filters
            FROM frames fr
            JOIN session_members sm ON sm.frame_id = fr.id
            JOIN sessions s ON s.id = sm.session_id
            JOIN imaging_nights ino ON ino.id = s.imaging_night_id
            JOIN frames_set fs ON fs.id = ino.frames_set_id
            WHERE fr.imagetyp = 'Light'
              AND DATE(fr.date_obs) >= ?1
              AND DATE(fr.date_obs) < ?2
            GROUP BY DATE(fr.date_obs), fs.id
            ORDER BY obs_date, object_name",
        )
        .map_err(|e| {
            eprintln!("get_calendar_month_data: prepare frame sets query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let frame_set_rows = stmt
        .query_map(rusqlite::params![start_date, end_date], |row| {
            let obs_date: String = row.get(0)?;
            let frame_set_id: i64 = row.get(1)?;
            let set_name: Option<String> = row.get(2)?;
            let object_name: Option<String> = row.get(3)?;
            let frame_count: i32 = row.get(4)?;
            let total_exposure: f64 = row.get(5)?;
            let ra_str: Option<String> = row.get(6)?;
            let dec_str: Option<String> = row.get(7)?;
            let filters_str: Option<String> = row.get(8)?;

            let avg_ra: Option<f64> =
                ra_str.as_ref().and_then(|s| parse_ra_sexagesimal(s).ok());
            let avg_dec: Option<f64> =
                dec_str.as_ref().and_then(|s| parse_dec_sexagesimal(s).ok());

            let filters: Vec<String> = filters_str
                .map(|s| {
                    s.split(',')
                        .filter(|f| !f.is_empty())
                        .map(|f| f.trim().to_string())
                        .collect()
                })
                .unwrap_or_default();

            Ok((
                obs_date,
                CalendarFrameSetSummary {
                    id: frame_set_id,
                    name: set_name,
                    object_name,
                    frame_count,
                    total_exposure_seconds: total_exposure,
                    ra: avg_ra,
                    dec: avg_dec,
                    filters,
                },
            ))
        })
        .map_err(|e| {
            eprintln!("get_calendar_month_data: frame sets query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    for row_result in frame_set_rows {
        let (obs_date, frame_set) = row_result.map_err(|e| {
            eprintln!("get_calendar_month_data: frame set row read failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

        let event = events_by_date
            .entry(obs_date.clone())
            .or_insert_with(|| CalendarDayEvent {
                date: obs_date,
                frame_sets: Vec::new(),
                unorganized_groups: Vec::new(),
                total_frame_count: 0,
                total_exposure_seconds: 0.0,
            });

        event.total_frame_count += frame_set.frame_count;
        event.total_exposure_seconds += frame_set.total_exposure_seconds;
        event.frame_sets.push(frame_set);
    }

    // Query 2: unorganised LIGHT frames not in any session
    let mut stmt = conn
        .prepare(
            "SELECT
                DATE(fr.date_obs) as obs_date,
                CASE WHEN fr.ra IS NULL THEN 'Unlocated Frames'
                     ELSE COALESCE(fr.object, 'Unknown') END as object_name,
                COUNT(DISTINCT fr.id) as frame_count,
                COALESCE(SUM(fr.exptime), 0) as total_exposure,
                AVG(fr.ra) as avg_ra,
                AVG(fr.dec) as avg_dec,
                GROUP_CONCAT(DISTINCT fr.filter) as filters,
                GROUP_CONCAT(fr.id) as frame_ids
            FROM frames fr
            WHERE fr.imagetyp = 'Light'
              AND DATE(fr.date_obs) >= ?1
              AND DATE(fr.date_obs) < ?2
              AND NOT EXISTS (
                  SELECT 1 FROM session_members sm WHERE sm.frame_id = fr.id
              )
            GROUP BY DATE(fr.date_obs),
                     CASE WHEN fr.ra IS NULL THEN 'unlocated' ELSE 'located' END
            ORDER BY obs_date",
        )
        .map_err(|e| {
            eprintln!("get_calendar_month_data: prepare unorganised query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let unorganized_rows = stmt
        .query_map(rusqlite::params![start_date, end_date], |row| {
            let obs_date: String = row.get(0)?;
            let object_name: Option<String> = row.get(1)?;
            let frame_count: i32 = row.get(2)?;
            let total_exposure: f64 = row.get(3)?;
            let avg_ra: Option<f64> = row.get(4)?;
            let avg_dec: Option<f64> = row.get(5)?;
            let filters_str: Option<String> = row.get(6)?;
            let frame_ids_str: Option<String> = row.get(7)?;

            let filters: Vec<String> = filters_str
                .map(|s| {
                    s.split(',')
                        .filter(|f| !f.is_empty())
                        .map(|f| f.trim().to_string())
                        .collect()
                })
                .unwrap_or_default();

            let frame_ids: Vec<i64> = frame_ids_str
                .map(|s| {
                    s.split(',')
                        .filter_map(|id| id.trim().parse::<i64>().ok())
                        .collect()
                })
                .unwrap_or_default();

            let id = if avg_ra.is_some() {
                format!(
                    "{}_{:.1}_{:.1}",
                    obs_date,
                    avg_ra.unwrap_or(0.0),
                    avg_dec.unwrap_or(0.0)
                )
            } else {
                format!("{}_unlocated", obs_date)
            };

            Ok((
                obs_date,
                CalendarUnorganizedGroup {
                    id,
                    object_name,
                    frame_count,
                    total_exposure_seconds: total_exposure,
                    ra: avg_ra,
                    dec: avg_dec,
                    filters,
                    frame_ids,
                },
            ))
        })
        .map_err(|e| {
            eprintln!("get_calendar_month_data: unorganised query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    for row_result in unorganized_rows {
        let (obs_date, group) = row_result.map_err(|e| {
            eprintln!("get_calendar_month_data: unorganised row read failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

        let event = events_by_date
            .entry(obs_date.clone())
            .or_insert_with(|| CalendarDayEvent {
                date: obs_date,
                frame_sets: Vec::new(),
                unorganized_groups: Vec::new(),
                total_frame_count: 0,
                total_exposure_seconds: 0.0,
            });

        event.total_frame_count += group.frame_count;
        event.total_exposure_seconds += group.total_exposure_seconds;
        event.unorganized_groups.push(group);
    }

    let mut days: Vec<CalendarDayEvent> = events_by_date.into_values().collect();
    days.sort_by(|a, b| a.date.cmp(&b.date));

    let total_frame_count: i32 = days.iter().map(|d| d.total_frame_count).sum();
    let total_exposure_seconds: f64 = days.iter().map(|d| d.total_exposure_seconds).sum();

    Ok(Json(CalendarMonthData {
        year,
        month,
        days,
        total_frame_count,
        total_exposure_seconds,
    }))
}
