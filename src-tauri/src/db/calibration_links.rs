use rusqlite::{Connection, Result, params};
use crate::models::{CalibrationLink, CalibrationStats, FrameCalibrationStatus};

/// Insert a new calibration link
pub fn insert_calibration_link(conn: &Connection, link: &CalibrationLink) -> Result<i64> {
    let matched_at = link.matched_at.clone();
    let date_warning = if link.date_warning { 1 } else { 0 };
    let temp_warning = if link.temp_warning { 1 } else { 0 };

    conn.execute(
        "INSERT INTO calibration_set_to_frames
         (source_id, source_type, calibration_set_id, calibration_type, matched_at, match_score, date_warning, temp_warning)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, source_type, calibration_type) DO UPDATE SET
         calibration_set_id = excluded.calibration_set_id,
         match_score = excluded.match_score,
         date_warning = excluded.date_warning,
         temp_warning = excluded.temp_warning,
         matched_at = excluded.matched_at",
        params![
            link.source_id,
            &link.source_type,
            link.calibration_set_id,
            &link.calibration_type,
            &matched_at,
            link.match_score,
            date_warning,
            temp_warning
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Get all calibration links for a specific frame
pub fn get_links_for_frame(conn: &Connection, frame_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'
         ORDER BY calibration_type"
    )?;

    let links = stmt.query_map([frame_id], |row| {
        Ok(CalibrationLink {
            id: Some(row.get(0)?),
            source_id: row.get(1)?,
            source_type: row.get(2)?,
            calibration_set_id: row.get(3)?,
            calibration_type: row.get(4)?,
            matched_at: row.get(5)?,
            match_score: row.get(6)?,
            date_warning: row.get::<_, i32>(7)? == 1,
            temp_warning: row.get::<_, i32>(8)? == 1,
        })
    })?;

    links.collect()
}

/// Get all calibration links for a specific calibration set
pub fn get_links_for_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<CalibrationLink>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, source_type, calibration_set_id, calibration_type,
                matched_at, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'
         ORDER BY calibration_type"
    )?;

    let links = stmt.query_map([set_id], |row| {
        Ok(CalibrationLink {
            id: Some(row.get(0)?),
            source_id: row.get(1)?,
            source_type: row.get(2)?,
            calibration_set_id: row.get(3)?,
            calibration_type: row.get(4)?,
            matched_at: row.get(5)?,
            match_score: row.get(6)?,
            date_warning: row.get::<_, i32>(7)? == 1,
            temp_warning: row.get::<_, i32>(8)? == 1,
        })
    })?;

    links.collect()
}

/// Get calibration status for a specific frame
pub fn get_frame_calibration_status(conn: &Connection, frame_id: i64) -> Result<FrameCalibrationStatus> {
    let links = get_links_for_frame(conn, frame_id)?;

    let mut status = FrameCalibrationStatus {
        frame_id,
        has_flats: false,
        has_darks: false,
        has_bias: false,
        has_darkflats: false,
        flats_warning: false,
        darks_warning: false,
        bias_warning: false,
        flat_set_id: None,
        dark_set_id: None,
        bias_set_id: None,
        darkflat_set_id: None,
    };

    for link in links {
        match link.calibration_type.as_str() {
            "Flat" => {
                status.has_flats = true;
                status.flats_warning = link.date_warning || link.temp_warning;
                status.flat_set_id = Some(link.calibration_set_id);
            }
            "Dark" => {
                status.has_darks = true;
                status.darks_warning = link.date_warning || link.temp_warning;
                status.dark_set_id = Some(link.calibration_set_id);
            }
            "Bias" => {
                status.has_bias = true;
                status.bias_warning = link.date_warning || link.temp_warning;
                status.bias_set_id = Some(link.calibration_set_id);
            }
            "DarkFlat" => {
                status.has_darkflats = true;
                status.darkflat_set_id = Some(link.calibration_set_id);
            }
            _ => {}
        }
    }

    Ok(status)
}

/// Delete all calibration links for frames in a specific frame set
pub fn delete_links_for_frame_set(conn: &Connection, frame_set_id: i64) -> Result<usize> {
    // First get all frame IDs in the frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1"
    )?;

    let frame_ids: Vec<i64> = stmt.query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>>>()?;

    if frame_ids.is_empty() {
        return Ok(0);
    }

    // Build placeholders for IN clause
    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let placeholders_str = placeholders.join(",");

    let delete_query = format!(
        "DELETE FROM calibration_set_to_frames
         WHERE source_id IN ({}) AND source_type = 'frame'",
        placeholders_str
    );

    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let deleted = conn.execute(&delete_query, params.as_slice())?;
    Ok(deleted)
}

/// Delete a specific calibration link
pub fn delete_calibration_link(conn: &Connection, link_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM calibration_set_to_frames WHERE id = ?1",
        [link_id],
    )?;
    Ok(())
}

/// Get calibration statistics for a frame set
pub fn get_calibration_statistics(conn: &Connection, frame_set_id: i64) -> Result<CalibrationStats> {
    // Get all frame IDs in the frame set
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.id
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    )?;

    let frame_ids: Vec<i64> = stmt.query_map([frame_set_id], |row| row.get(0))?
        .collect::<Result<Vec<i64>>>()?;

    let total_frames = frame_ids.len();

    let mut frames_with_flats = 0;
    let mut frames_with_darks = 0;
    let mut frames_with_bias = 0;
    let mut frames_complete = 0;
    let mut frames_partial = 0;
    let mut frames_none = 0;
    let mut total_warnings = 0;

    for frame_id in frame_ids {
        let status = get_frame_calibration_status(conn, frame_id)?;

        if status.has_flats { frames_with_flats += 1; }
        if status.has_darks { frames_with_darks += 1; }
        if status.has_bias { frames_with_bias += 1; }

        if status.flats_warning || status.darks_warning || status.bias_warning {
            total_warnings += 1;
        }

        // Check if frame has complete calibration
        let has_any = status.has_flats || status.has_darks || status.has_bias;
        let has_complete = status.has_flats && (status.has_darks || status.has_bias);

        if has_complete {
            frames_complete += 1;
        } else if has_any {
            frames_partial += 1;
        } else {
            frames_none += 1;
        }
    }

    Ok(CalibrationStats {
        total_frames,
        frames_with_flats,
        frames_with_darks,
        frames_with_bias,
        frames_complete,
        frames_partial,
        frames_none,
        total_warnings,
    })
}

/// Get all frames that use a specific calibration set
pub fn get_frames_using_calibration_set(conn: &Connection, set_id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT source_id
         FROM calibration_set_to_frames
         WHERE calibration_set_id = ?1 AND source_type = 'frame'
         ORDER BY source_id"
    )?;

    let frame_ids = stmt.query_map([set_id], |row| row.get(0))?;
    frame_ids.collect()
}

/// Check if a calibration link exists
pub fn link_exists(
    conn: &Connection,
    source_id: i64,
    source_type: &str,
    calibration_type: &str
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = ?2 AND calibration_type = ?3",
        params![source_id, source_type, calibration_type],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use chrono::Utc;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_link() {
        let conn = create_test_db();

        let link = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
        };

        let link_id = insert_calibration_link(&conn, &link).unwrap();
        assert!(link_id > 0);

        let links = get_links_for_frame(&conn, 1).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].calibration_type, "Dark");
    }

    #[test]
    fn test_link_upsert() {
        let conn = create_test_db();

        let link1 = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
        };

        insert_calibration_link(&conn, &link1).unwrap();

        // Insert again with different set ID - should update
        let link2 = CalibrationLink {
            calibration_set_id: 20,
            ..link1
        };

        insert_calibration_link(&conn, &link2).unwrap();

        let links = get_links_for_frame(&conn, 1).unwrap();
        assert_eq!(links.len(), 1);  // Still only one link
        assert_eq!(links[0].calibration_set_id, 20);  // Updated set ID
    }

    #[test]
    fn test_link_exists() {
        let conn = create_test_db();

        let link = CalibrationLink {
            id: None,
            source_id: 1,
            source_type: "frame".to_string(),
            calibration_set_id: 10,
            calibration_type: "Dark".to_string(),
            matched_at: Utc::now().to_rfc3339(),
            match_score: Some(0.95),
            date_warning: false,
            temp_warning: false,
        };

        assert!(!link_exists(&conn, 1, "frame", "Dark").unwrap());
        insert_calibration_link(&conn, &link).unwrap();
        assert!(link_exists(&conn, 1, "frame", "Dark").unwrap());
    }
}
