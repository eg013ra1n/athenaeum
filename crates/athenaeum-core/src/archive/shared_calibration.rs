//! Detect which calibration sets linked to a given frame set are also linked
//! to other (non-archived) frame sets. Used to disable "Move" radios in the UI.

use crate::archive::models::{FrameRole, SharedCalibrationWarning};
use anyhow::Result;
use rusqlite::{params, Connection};

fn role_to_calibration_type(role: FrameRole) -> &'static str {
    match role {
        FrameRole::Flat => "Flat",
        FrameRole::Dark => "Dark",
        FrameRole::Bias => "Bias",
        FrameRole::Darkflat => "DarkFlat",
        FrameRole::Light => "", // not applicable
    }
}

/// For each calibration type linked to this frame set, return a list of
/// (calibration_set_id, [other_frames_set_ids...]) where the cal set is also
/// referenced by frames in other (non-archived) frame sets.
pub fn find_shared_calibration_sets(
    conn: &Connection,
    frames_set_id: i64,
) -> Result<Vec<SharedCalibrationWarning>> {
    let mut warnings = Vec::new();

    for role in [FrameRole::Flat, FrameRole::Dark, FrameRole::Bias, FrameRole::Darkflat] {
        let cal_type = role_to_calibration_type(role);

        // Calibration sets linked to LIGHT frames in this frame set.
        let cal_set_ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT cstf.calibration_set_id
                 FROM calibration_set_to_frames cstf
                 JOIN frames f ON f.id = cstf.source_id AND cstf.source_type = 'frame'
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1
                   AND cstf.calibration_type = ?2
                   AND f.imagetyp = 'Light'",
            )?;
            let ids = stmt.query_map(params![frames_set_id, cal_type], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            ids
        };

        for cs_id in cal_set_ids {
            // Find OTHER frame sets that reference this cal set, excluding archived ones.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT n.frames_set_id
                 FROM calibration_set_to_frames cstf
                 JOIN frames f ON f.id = cstf.source_id AND cstf.source_type = 'frame'
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 JOIN frames_set fs ON fs.id = n.frames_set_id
                 WHERE cstf.calibration_set_id = ?1
                   AND cstf.calibration_type = ?2
                   AND n.frames_set_id != ?3
                   AND fs.archived_at IS NULL",
            )?;
            let others: Vec<i64> = stmt.query_map(params![cs_id, cal_type, frames_set_id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;

            if !others.is_empty() {
                warnings.push(SharedCalibrationWarning {
                    frame_role: role,
                    calibration_set_id: cs_id,
                    other_frames_set_ids: others,
                });
            }
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    /// Create two frame sets, both referencing the same dark cal set, and verify
    /// that planning archive of frame set A flags the dark as shared with B.
    #[test]
    fn detects_shared_dark() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Frame sets
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'A'), (2, 'B')", []).unwrap();
        // Imaging nights
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES
             (10, 1, '2025-01-01T00:00:00Z', '2025-01-02T00:00:00Z'),
             (11, 2, '2025-01-03T00:00:00Z', '2025-01-04T00:00:00Z')",
            [],
        ).unwrap();
        // Sessions
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C'), (101, 11, 'C')",
            [],
        ).unwrap();
        // Files
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES
             (1000, '/a/L1.fits', 'L1.fits', 1, '2025-01-01T00:00:00Z', 'FITS'),
             (1001, '/b/L2.fits', 'L2.fits', 1, '2025-01-03T00:00:00Z', 'FITS')",
            [],
        ).unwrap();
        // Frames
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES
             (10000, 1000, 'Light'),
             (10001, 1001, 'Light')",
            [],
        ).unwrap();
        // session_members
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000), (101, 10001)",
            [],
        ).unwrap();
        // Cal set
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-01-01')",
            [],
        ).unwrap();
        // Both frames link to same cal set
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-01-01'),
                    (10001, 'frame', 500, 'Dark', '2025-01-03')",
            [],
        ).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].frame_role, FrameRole::Dark);
        assert_eq!(warnings[0].calibration_set_id, 500);
        assert_eq!(warnings[0].other_frames_set_ids, vec![2]);
    }

    /// If the only other frame set referencing the cal set is itself archived,
    /// it doesn't count — Move is allowed.
    #[test]
    fn ignores_archived_other_sets() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO frames_set (id, name, archived_at) VALUES
             (1, 'A', NULL),
             (2, 'B-archived', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES
             (10, 1, '2025-01-01', '2025-01-02'),
             (11, 2, '2025-01-03', '2025-01-04')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C'), (101, 11, 'C')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES
             (1000, '/a/L1.fits', 'L1.fits', 1, '2025-01-01', 'FITS'),
             (1001, '/b/L2.fits', 'L2.fits', 1, '2025-01-03', 'FITS')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES
             (10000, 1000, 'Light'), (10001, 1001, 'Light')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000), (101, 10001)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-01-01')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10000, 'frame', 500, 'Dark', '2025-01-01'),
                    (10001, 'frame', 500, 'Dark', '2025-01-03')",
            [],
        ).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 0, "archived other sets should not flag share");
    }

    #[test]
    fn returns_empty_when_no_calibrations() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'X')", []).unwrap();

        let warnings = find_shared_calibration_sets(&conn, 1).unwrap();
        assert_eq!(warnings.len(), 0);
    }
}
