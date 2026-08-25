//! Calendar view query layer — imaging activity by date.
//!
//! Single source of truth for the calendar month aggregation: the Tauri command
//! (`commands/calendar.rs`) and the Axum route (`routes/calendar.rs`) are thin
//! delegations onto `get_calendar_month_data`. The SQL is carried over verbatim
//! from the pre-move Tauri copy.

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::collections::HashMap;

use crate::coordinates::{parse_dec_sexagesimal, parse_ra_sexagesimal};
use crate::models::{
    CalendarDayEvent, CalendarFrameSetSummary, CalendarMonthData, CalendarUnorganizedGroup,
};

/// Get calendar data for a specific month
///
/// Returns all imaging activity grouped by date for calendar rendering.
/// Includes both organized frame sets and unorganized LIGHT frames.
pub fn get_calendar_month_data(
    conn: &Connection,
    year: i32,
    month: i32,
) -> Result<CalendarMonthData> {
    // Calculate date range for the month
    let start_date = format!("{:04}-{:02}-01", year, month);
    let end_date = if month == 12 {
        format!("{:04}-01-01", year + 1)
    } else {
        format!("{:04}-{:02}-01", year, month + 1)
    };

    // Use a HashMap to collect events by date
    let mut events_by_date: HashMap<String, CalendarDayEvent> = HashMap::new();

    // Query 1: Organized Frame Sets
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
        .map_err(|e| anyhow!("Failed to prepare frame sets query: {}", e))?;

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

            // Parse sexagesimal coordinates to decimal degrees
            let avg_ra: Option<f64> = ra_str.as_ref().and_then(|s| parse_ra_sexagesimal(s).ok());
            let avg_dec: Option<f64> = dec_str.as_ref().and_then(|s| parse_dec_sexagesimal(s).ok());

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
        .map_err(|e| anyhow!("Failed to query frame sets: {}", e))?;

    for row_result in frame_set_rows {
        let (obs_date, frame_set) = row_result.map_err(|e| anyhow!("Failed to read row: {}", e))?;

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

    // Query 2: Unorganized LIGHT Frames (not in any session)
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
        .map_err(|e| anyhow!("Failed to prepare unorganized query: {}", e))?;

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

            // Create a pseudo-ID based on date and location status
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
        .map_err(|e| anyhow!("Failed to query unorganized frames: {}", e))?;

    for row_result in unorganized_rows {
        let (obs_date, unorganized_group) =
            row_result.map_err(|e| anyhow!("Failed to read row: {}", e))?;

        let event = events_by_date
            .entry(obs_date.clone())
            .or_insert_with(|| CalendarDayEvent {
                date: obs_date,
                frame_sets: Vec::new(),
                unorganized_groups: Vec::new(),
                total_frame_count: 0,
                total_exposure_seconds: 0.0,
            });

        event.total_frame_count += unorganized_group.frame_count;
        event.total_exposure_seconds += unorganized_group.total_exposure_seconds;
        event.unorganized_groups.push(unorganized_group);
    }

    // Convert HashMap to sorted Vec
    let mut days: Vec<CalendarDayEvent> = events_by_date.into_values().collect();
    days.sort_by(|a, b| a.date.cmp(&b.date));

    // Calculate totals
    let total_frame_count: i32 = days.iter().map(|d| d.total_frame_count).sum();
    let total_exposure_seconds: f64 = days.iter().map(|d| d.total_exposure_seconds).sum();

    Ok(CalendarMonthData {
        year,
        month,
        days,
        total_frame_count,
        total_exposure_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::{params, Connection};

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn insert_light(conn: &Connection, id: i64, object: &str, date_obs: &str, exptime: f64) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 1024, '2026-01-01T00:00:00Z', 'FITS')",
            params![
                id,
                format!("/tmp/frame_{id}.fits"),
                format!("frame_{id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames
             (id, file_id, object, date_obs, imagetyp, exptime, filter, ra, dec, instrume)
             VALUES (?1, ?1, ?2, ?3, 'Light', ?4, 'L', 10.0, 41.0, 'TestCam')",
            params![id, object, date_obs, exptime],
        )
        .unwrap();
    }

    #[test]
    fn month_data_groups_unorganized_frames_by_day() {
        let conn = test_db();
        insert_light(&conn, 1, "M31", "2026-03-05T22:10:00", 300.0);
        insert_light(&conn, 2, "M31", "2026-03-07T21:00:00", 120.0);
        // A frame outside the requested month must not leak in.
        insert_light(&conn, 3, "M31", "2026-04-02T21:00:00", 600.0);

        let data = get_calendar_month_data(&conn, 2026, 3).unwrap();

        assert_eq!(data.year, 2026);
        assert_eq!(data.month, 3);
        assert_eq!(
            data.days.len(),
            2,
            "one entry per imaging date in the month"
        );
        assert_eq!(data.total_frame_count, 2);
        assert_eq!(data.total_exposure_seconds, 420.0);

        assert_eq!(data.days[0].date, "2026-03-05");
        assert_eq!(data.days[0].total_frame_count, 1);
        assert_eq!(data.days[0].total_exposure_seconds, 300.0);
        assert_eq!(data.days[0].unorganized_groups.len(), 1);
        assert_eq!(data.days[0].unorganized_groups[0].frame_ids, vec![1]);
        assert!(data.days[0].frame_sets.is_empty());

        assert_eq!(data.days[1].date, "2026-03-07");
        assert_eq!(data.days[1].total_frame_count, 1);
        assert_eq!(data.days[1].total_exposure_seconds, 120.0);
        assert_eq!(data.days[1].unorganized_groups[0].frame_ids, vec![2]);
    }

    #[test]
    fn december_month_rolls_over_to_next_year() {
        let conn = test_db();
        insert_light(&conn, 1, "M42", "2026-12-24T22:10:00", 60.0);
        insert_light(&conn, 2, "M42", "2027-01-03T22:10:00", 60.0);

        let data = get_calendar_month_data(&conn, 2026, 12).unwrap();
        assert_eq!(data.days.len(), 1);
        assert_eq!(data.days[0].date, "2026-12-24");
        assert_eq!(data.total_frame_count, 1);
    }
}
