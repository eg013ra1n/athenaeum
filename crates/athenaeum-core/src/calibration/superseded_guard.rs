//! Guard shared by every calibration-set creation path (dark, bias, flat, and
//! the scanner's own dark/darkflat path): a frame group whose members already
//! belong to a superseded raw set is a lineage a master replaced — minting a
//! fresh raw set from those frames would silently divert auto-links away from
//! the master (2026-08-02 audit C1).
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// A superseded lineage covering some or all of a candidate frame group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupersededMatch {
    /// The master set that replaced the raw lineage — reuse this id instead of
    /// minting a new set.
    pub master_set_id: i64,
    /// How many of the queried frames that lineage actually covers. Less than
    /// the group length means the group is only *partially* superseded: the
    /// caller returns the master, so the uncovered frames get no set this pass.
    pub covered: usize,
}

/// Returns the superseding master when any of `frame_ids` belongs to a
/// superseded calibration set. When several superseded sets cover the group,
/// the one covering the most frames wins (ties: lowest master id).
///
/// Logging lives here rather than at the four call sites so every path reports
/// identically, and so partial coverage can never be diverted silently.
pub fn superseding_master_for_frames(
    conn: &Connection,
    frame_ids: &[i64],
) -> Result<Option<SupersededMatch>> {
    if frame_ids.is_empty() {
        return Ok(None);
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // COUNT(DISTINCT csf.frame_id), not COUNT(*): a frame could sit in two
    // superseded sets sharing one master, and an inflated count would both skew
    // the most-covering tie-break and underflow `uncovered` below.
    let sql = format!(
        "SELECT cs.superseded_by_set_id, COUNT(DISTINCT csf.frame_id)
           FROM calibration_set_frames csf
           JOIN calibration_set cs ON cs.id = csf.set_id
          WHERE csf.frame_id IN ({placeholders})
            AND cs.superseded_by_set_id IS NOT NULL
          GROUP BY cs.superseded_by_set_id
          ORDER BY COUNT(DISTINCT csf.frame_id) DESC, cs.superseded_by_set_id ASC
          LIMIT 1"
    );
    let found: Option<(i64, i64)> = conn
        .query_row(&sql, rusqlite::params_from_iter(frame_ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .optional()?;

    let Some((master_set_id, covered)) = found else {
        return Ok(None);
    };
    let covered = covered.max(0) as usize;
    let group_len = frame_ids.len();

    if covered < group_len {
        tracing::warn!(
            set_id = master_set_id,
            count = group_len,
            covered,
            uncovered = group_len - covered,
            "group PARTIALLY superseded — uncovered frames get no set this pass"
        );
    } else {
        tracing::info!(
            set_id = master_set_id,
            count = group_len,
            "group superseded by master — reusing it"
        );
    }

    Ok(Some(SupersededMatch {
        master_set_id,
        covered,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_set(conn: &rusqlite::Connection, id: i64, imagetyp: &str, is_master: i64) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2025-01-01', ?3)",
            rusqlite::params![id, imagetyp, is_master],
        )
        .unwrap();
    }

    fn insert_frame(conn: &rusqlite::Connection, frame_id: i64) {
        let file_id = frame_id + 100_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2025-01-01T00:00:00Z', 'FITS')",
            rusqlite::params![
                file_id,
                format!("/t/g_{}.fits", frame_id),
                format!("g_{}.fits", frame_id),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, 'DARK')",
            rusqlite::params![frame_id, file_id],
        )
        .unwrap();
    }

    fn link(conn: &rusqlite::Connection, set_id: i64, frame_id: i64) {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![set_id, frame_id],
        )
        .unwrap();
    }

    fn supersede(conn: &rusqlite::Connection, raw_id: i64, master_id: i64) {
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
            rusqlite::params![master_id, raw_id],
        )
        .unwrap();
    }

    /// Fixture: raw set 10 (superseded by master 11) owning frame 100.
    fn seed(conn: &rusqlite::Connection) {
        insert_set(conn, 10, "Dark", 0);
        insert_set(conn, 11, "MasterDark", 1);
        supersede(conn, 10, 11);
        insert_frame(conn, 100);
        link(conn, 10, 100);
    }

    #[test]
    fn detects_superseded_membership() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        seed(&conn);

        let hit = superseding_master_for_frames(&conn, &[100])
            .unwrap()
            .unwrap();
        assert_eq!(hit.master_set_id, 11);
        assert_eq!(hit.covered, 1);

        assert_eq!(superseding_master_for_frames(&conn, &[999]).unwrap(), None);
        assert_eq!(superseding_master_for_frames(&conn, &[]).unwrap(), None);
    }

    #[test]
    fn reports_partial_coverage_for_a_mixed_group() {
        // A group of 3 where only 2 frames belong to the superseded lineage —
        // e.g. new darks copied in after the master was built. The caller still
        // reuses the master, but `covered` makes the shortfall visible (and the
        // guard warns) instead of orphaning the extras silently.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        seed(&conn);
        insert_frame(&conn, 101);
        link(&conn, 10, 101);
        insert_frame(&conn, 102); // fresh frame, in no set

        let hit = superseding_master_for_frames(&conn, &[100, 101, 102])
            .unwrap()
            .unwrap();
        assert_eq!(hit.master_set_id, 11);
        assert_eq!(hit.covered, 2, "only the two linked frames are covered");
    }

    #[test]
    fn most_covering_master_wins_and_ties_break_to_lowest_id() {
        // Two superseded lineages both touch the group: raw 10 → master 11
        // covers 1 frame, raw 20 → master 21 covers 2. The bigger claim wins
        // even though its master id is higher.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        seed(&conn); // set 10 → master 11, frame 100
        insert_set(&conn, 20, "Dark", 0);
        insert_set(&conn, 21, "MasterDark", 1);
        supersede(&conn, 20, 21);
        insert_frame(&conn, 200);
        insert_frame(&conn, 201);
        link(&conn, 20, 200);
        link(&conn, 20, 201);

        let hit = superseding_master_for_frames(&conn, &[100, 200, 201])
            .unwrap()
            .unwrap();
        assert_eq!(hit.master_set_id, 21, "most-covering master must win");
        assert_eq!(hit.covered, 2);

        // Now an exact tie — one frame each — must break to the LOWEST master id.
        let tie = superseding_master_for_frames(&conn, &[100, 200])
            .unwrap()
            .unwrap();
        assert_eq!(
            tie.master_set_id, 11,
            "tie must break to the lowest master id"
        );
        assert_eq!(tie.covered, 1);
    }
}

