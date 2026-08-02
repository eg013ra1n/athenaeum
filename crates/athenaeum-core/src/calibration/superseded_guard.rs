//! Guard shared by every on-demand calibration-set creation path (dark, bias,
//! flat): a frame group whose members already belong to a superseded raw set
//! is a lineage a master replaced — minting a fresh raw set from those frames
//! would silently divert auto-links away from the master (2026-08-02 audit C1).
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// Returns the superseding master's set id when any of `frame_ids` belongs to
/// a superseded calibration set. When several superseded sets cover the group,
/// the one covering the most frames wins (ties: lowest master id).
pub fn superseding_master_for_frames(conn: &Connection, frame_ids: &[i64]) -> Result<Option<i64>> {
    if frame_ids.is_empty() {
        return Ok(None);
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT cs.superseded_by_set_id
           FROM calibration_set_frames csf
           JOIN calibration_set cs ON cs.id = csf.set_id
          WHERE csf.frame_id IN ({placeholders})
            AND cs.superseded_by_set_id IS NOT NULL
          GROUP BY cs.superseded_by_set_id
          ORDER BY COUNT(*) DESC, cs.superseded_by_set_id ASC
          LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, rusqlite::params_from_iter(frame_ids.iter()), |r| {
            r.get::<_, i64>(0)
        })
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture: raw set 10 (superseded by master 11) owning frame 100.
    fn seed(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
                  VALUES (10, 'Dark', '2025-01-01', 0);
             INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
                  VALUES (11, 'MasterDark', '2025-01-01', 1);
             UPDATE calibration_set SET superseded_by_set_id = 11 WHERE id = 10;
             INSERT INTO files (id, path, filename, size, modified_at, format)
                  VALUES (1, '/t/a.fits', 'a.fits', 1, '2025-01-01T00:00:00Z', 'FITS');
             INSERT INTO frames (id, file_id, imagetyp) VALUES (100, 1, 'DARK');
             INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (10, 100);",
        )
        .unwrap();
    }

    #[test]
    fn detects_superseded_membership() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        seed(&conn);

        assert_eq!(
            superseding_master_for_frames(&conn, &[100]).unwrap(),
            Some(11)
        );
        assert_eq!(superseding_master_for_frames(&conn, &[999]).unwrap(), None);
        assert_eq!(superseding_master_for_frames(&conn, &[]).unwrap(), None);
    }
}
