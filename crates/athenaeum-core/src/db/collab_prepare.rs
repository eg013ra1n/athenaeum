//! Spec 2026-10-01 §4.2 — local publish-review state: frames calibrated and
//! waiting for review (`collab_prepared_frames`) and frames the member
//! withheld (`collab_withheld_frames`). Neither is ever sent to the hub.

use anyhow::Result;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// One calibrated frame waiting for review. `calibrated_path` is `None` and
/// `external` is true for an attested set: its file IS the catalog original,
/// which nothing here may ever delete.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRow {
    pub project_id: String,
    pub source_frame_id: i64,
    pub frame_uuid: String,
    pub calibrated_path: Option<String>,
    pub external: bool,
    pub own_dir: String,
    pub recipe_hash: String,
    pub xxh3: String,
    pub byte_size: i64,
    pub size_mtime_seen: Option<String>,
    pub prepared_at: String,
    pub publish_run_id: String,
}

const COLS: &str = "project_id, source_frame_id, frame_uuid, calibrated_path, external, own_dir, \
                    recipe_hash, xxh3, byte_size, size_mtime_seen, prepared_at, publish_run_id";
const CHUNK: usize = 500;

fn from_sql(r: &rusqlite::Row<'_>) -> rusqlite::Result<PreparedRow> {
    Ok(PreparedRow {
        project_id: r.get(0)?,
        source_frame_id: r.get(1)?,
        frame_uuid: r.get(2)?,
        calibrated_path: r.get(3)?,
        external: r.get::<_, i64>(4)? != 0,
        own_dir: r.get(5)?,
        recipe_hash: r.get(6)?,
        xxh3: r.get(7)?,
        byte_size: r.get(8)?,
        size_mtime_seen: r.get(9)?,
        prepared_at: r.get(10)?,
        publish_run_id: r.get(11)?,
    })
}

/// Insert, or replace the row of the same frame (a re-calibrate).
pub fn upsert_prepared(conn: &Connection, row: &PreparedRow) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_prepared_frames
            (project_id, source_frame_id, frame_uuid, calibrated_path, external, own_dir,
             recipe_hash, xxh3, byte_size, size_mtime_seen, publish_run_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(project_id, source_frame_id) DO UPDATE SET
            frame_uuid = excluded.frame_uuid, calibrated_path = excluded.calibrated_path,
            external = excluded.external, own_dir = excluded.own_dir,
            recipe_hash = excluded.recipe_hash, xxh3 = excluded.xxh3,
            byte_size = excluded.byte_size, size_mtime_seen = excluded.size_mtime_seen,
            prepared_at = datetime('now'), publish_run_id = excluded.publish_run_id",
        params![
            row.project_id,
            row.source_frame_id,
            row.frame_uuid,
            row.calibrated_path,
            row.external as i64,
            row.own_dir,
            row.recipe_hash,
            row.xxh3,
            row.byte_size,
            row.size_mtime_seen,
            row.publish_run_id,
        ],
    )?;
    Ok(())
}

pub fn list_prepared(conn: &Connection, project_id: &str) -> Result<Vec<PreparedRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM collab_prepared_frames WHERE project_id = ?1 ORDER BY source_frame_id"
    ))?;
    let rows = st
        .query_map([project_id], from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn prepared_by_source(
    conn: &Connection,
    project_id: &str,
) -> Result<HashMap<i64, PreparedRow>> {
    Ok(list_prepared(conn, project_id)?
        .into_iter()
        .map(|r| (r.source_frame_id, r))
        .collect())
}

pub fn get_prepared(
    conn: &Connection,
    project_id: &str,
    source_frame_id: i64,
) -> Result<Option<PreparedRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM collab_prepared_frames WHERE project_id = ?1 AND source_frame_id = ?2"),
            params![project_id, source_frame_id],
            from_sql,
        )
        .optional()?)
}

/// Delete these frames' rows; returns the deleted rows so the caller removes
/// their (non-external) files. One `DELETE … RETURNING` per chunk, so a row
/// is returned exactly when this call removed it.
pub fn delete_prepared(
    conn: &Connection,
    project_id: &str,
    source_frame_ids: &[i64],
) -> Result<Vec<PreparedRow>> {
    let mut gone = Vec::new();
    for chunk in source_frame_ids.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut st = conn.prepare(&format!(
            "DELETE FROM collab_prepared_frames
             WHERE project_id = ? AND source_frame_id IN ({marks})
             RETURNING {COLS}"
        ))?;
        let args = std::iter::once(rusqlite::types::Value::from(project_id.to_string()))
            .chain(chunk.iter().map(|id| rusqlite::types::Value::from(*id)));
        let rows = st
            .query_map(params_from_iter(args), from_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        gone.extend(rows);
    }
    gone.sort_by_key(|r| r.source_frame_id);
    Ok(gone)
}

/// Atomically delete the prepared rows of every frame withheld in this
/// project (one statement, so a Release landing meanwhile cannot lose a file);
/// returns the deleted rows.
pub fn delete_withheld_prepared(conn: &Connection, project_id: &str) -> Result<Vec<PreparedRow>> {
    let mut st = conn.prepare(&format!(
        "DELETE FROM collab_prepared_frames
         WHERE project_id = ?1 AND source_frame_id IN
               (SELECT source_frame_id FROM collab_withheld_frames WHERE project_id = ?1)
         RETURNING {COLS}"
    ))?;
    let mut rows = st
        .query_map([project_id], from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.sort_by_key(|r| r.source_frame_id);
    Ok(rows)
}

/// Whether the member withheld this frame from the project.
pub fn is_withheld(conn: &Connection, project_id: &str, source_frame_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM collab_withheld_frames WHERE project_id = ?1 AND source_frame_id = ?2",
            params![project_id, source_frame_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn delete_project_prepared(conn: &Connection, project_id: &str) -> Result<Vec<PreparedRow>> {
    let rows = list_prepared(conn, project_id)?;
    conn.execute(
        "DELETE FROM collab_prepared_frames WHERE project_id = ?1",
        [project_id],
    )?;
    Ok(rows)
}

/// Every prepared file name in this project (landing-name reservation).
pub fn prepared_paths(conn: &Connection, project_id: &str) -> Result<HashSet<String>> {
    let mut st = conn.prepare(
        "SELECT calibrated_path FROM collab_prepared_frames WHERE project_id = ?1 AND calibrated_path IS NOT NULL",
    )?;
    let paths = st
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(paths)
}

pub fn is_prepared_path(conn: &Connection, path: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM collab_prepared_frames WHERE calibrated_path = ?1 LIMIT 1",
            [path],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The own folder pinned by this project's first non-external prepared row.
pub fn own_dir(conn: &Connection, project_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT own_dir FROM collab_prepared_frames WHERE project_id = ?1 AND external = 0
             ORDER BY source_frame_id LIMIT 1",
            [project_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Every pinned own folder of this project (a peer's folder must avoid them).
pub fn own_dirs(conn: &Connection, project_id: &str) -> Result<HashSet<String>> {
    let mut st = conn.prepare(
        "SELECT DISTINCT own_dir FROM collab_prepared_frames WHERE project_id = ?1 AND external = 0",
    )?;
    let dirs = st
        .query_map([project_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(dirs)
}

/// Withhold (`true`) or release (`false`); returns rows actually changed.
pub fn set_withheld(
    conn: &Connection,
    project_id: &str,
    ids: &[i64],
    withheld: bool,
) -> Result<usize> {
    let mut n = 0;
    for id in ids {
        n += if withheld {
            conn.execute(
                "INSERT OR IGNORE INTO collab_withheld_frames (project_id, source_frame_id) VALUES (?1, ?2)",
                params![project_id, id],
            )?
        } else {
            conn.execute(
                "DELETE FROM collab_withheld_frames WHERE project_id = ?1 AND source_frame_id = ?2",
                params![project_id, id],
            )?
        };
    }
    Ok(n)
}

pub fn withheld_ids(conn: &Connection, project_id: &str) -> Result<HashSet<i64>> {
    let mut st =
        conn.prepare("SELECT source_frame_id FROM collab_withheld_frames WHERE project_id = ?1")?;
    let ids = st
        .query_map([project_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(ids)
}

pub fn delete_project_withheld(conn: &Connection, project_id: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_withheld_frames WHERE project_id = ?1",
        [project_id],
    )?)
}

/// Frames (by id) whose file has a `black_hole` row.
pub fn black_holed_frame_ids(conn: &Connection, frame_ids: &[i64]) -> Result<HashSet<i64>> {
    let mut out = HashSet::new();
    for chunk in frame_ids.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut st = conn.prepare(&format!(
            "SELECT f.id FROM frames f JOIN black_hole bh ON bh.file_id = f.file_id WHERE f.id IN ({marks})"
        ))?;
        for id in st.query_map(params_from_iter(chunk.iter()), |r| r.get::<_, i64>(0))? {
            out.insert(id?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_project() -> (tempfile::TempDir, crate::db::Database) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::new(tmp.path().join("t.db")).unwrap();
        {
            let conn = db.conn();
            conn.execute(
                "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name, target_ra_deg, target_dec_deg, target_radius_deg, membership_version, snapshot_payload_b64, snapshot_signature_b64, members_json)
                 VALUES ('p1','m31','M31','send_receive','M31',10.6,41.2,1.0,1,'','','[]')",
                [],
            )
            .unwrap();
        }
        (tmp, db)
    }

    fn row(id: i64, path: Option<&str>, external: bool) -> PreparedRow {
        PreparedRow {
            project_id: "p1".into(),
            source_frame_id: id,
            frame_uuid: format!("u-{id}"),
            calibrated_path: path.map(str::to_string),
            external,
            own_dir: "/collab/m31/me".into(),
            recipe_hash: "r1".into(),
            xxh3: "abc".into(),
            byte_size: 10,
            size_mtime_seen: Some("10:1".into()),
            prepared_at: String::new(),
            publish_run_id: "run-1".into(),
        }
    }

    #[test]
    fn a_new_project_column_reads_manual_and_round_trips() {
        let (_t, db) = conn_with_project();
        let conn = db.conn();
        let p = crate::db::collab::get_project(&conn, "p1")
            .unwrap()
            .unwrap();
        assert_eq!(p.publish_mode, crate::db::collab::PublishMode::Manual);
        crate::db::collab::set_publish_mode(
            &conn,
            "p1",
            crate::db::collab::PublishMode::AutoCalibrate,
        )
        .unwrap();
        let p = crate::db::collab::get_project(&conn, "p1")
            .unwrap()
            .unwrap();
        assert_eq!(
            p.publish_mode,
            crate::db::collab::PublishMode::AutoCalibrate
        );
    }

    #[test]
    fn prepared_rows_upsert_list_and_delete_return_the_rows() {
        let (_t, db) = conn_with_project();
        let conn = db.conn();
        upsert_prepared(&conn, &row(1, Some("/collab/m31/me/c_a.fits"), false)).unwrap();
        upsert_prepared(&conn, &row(2, None, true)).unwrap();
        let mut again = row(1, Some("/collab/m31/me/c_a.fits"), false);
        again.recipe_hash = "r2".into();
        upsert_prepared(&conn, &again).unwrap();
        let all = list_prepared(&conn, "p1").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            get_prepared(&conn, "p1", 1).unwrap().unwrap().recipe_hash,
            "r2"
        );
        assert!(
            !all[0].prepared_at.is_empty(),
            "prepared_at is stamped by SQL"
        );
        assert!(is_prepared_path(&conn, "/collab/m31/me/c_a.fits").unwrap());
        assert_eq!(
            prepared_paths(&conn, "p1").unwrap().len(),
            1,
            "an external row has no path"
        );
        assert_eq!(
            own_dir(&conn, "p1").unwrap().as_deref(),
            Some("/collab/m31/me")
        );
        let gone = delete_prepared(&conn, "p1", &[1]).unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(
            gone[0].calibrated_path.as_deref(),
            Some("/collab/m31/me/c_a.fits")
        );
        assert!(
            delete_prepared(&conn, "p1", &[1, 99]).unwrap().is_empty(),
            "a row is returned only by the call that removed it"
        );
        assert_eq!(delete_project_prepared(&conn, "p1").unwrap().len(), 1);
    }

    #[test]
    fn delete_withheld_prepared_takes_only_withheld_rows() {
        let (_t, db) = conn_with_project();
        let conn = db.conn();
        for id in [1, 2] {
            upsert_prepared(
                &conn,
                &PreparedRow {
                    project_id: "p1".into(),
                    source_frame_id: id,
                    frame_uuid: format!("u{id}"),
                    calibrated_path: Some(format!("/x/{id}")),
                    external: false,
                    own_dir: "/x".into(),
                    recipe_hash: "r".into(),
                    xxh3: "x".into(),
                    byte_size: 1,
                    size_mtime_seen: None,
                    prepared_at: String::new(),
                    publish_run_id: "r".into(),
                },
            )
            .unwrap();
        }
        set_withheld(&conn, "p1", &[2], true).unwrap();
        let gone = delete_withheld_prepared(&conn, "p1").unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].source_frame_id, 2);
        assert_eq!(list_prepared(&conn, "p1").unwrap().len(), 1);
        assert!(delete_withheld_prepared(&conn, "p1").unwrap().is_empty());
    }

    #[test]
    fn withheld_set_and_release() {
        let (_t, db) = conn_with_project();
        let conn = db.conn();
        assert_eq!(set_withheld(&conn, "p1", &[5, 6], true).unwrap(), 2);
        assert!(is_withheld(&conn, "p1", 5).unwrap());
        assert!(!is_withheld(&conn, "p1", 7).unwrap());
        assert!(!is_withheld(&conn, "p2", 5).unwrap(), "per project");
        assert_eq!(
            set_withheld(&conn, "p1", &[5], true).unwrap(),
            0,
            "already withheld"
        );
        assert_eq!(
            withheld_ids(&conn, "p1").unwrap(),
            [5, 6].into_iter().collect()
        );
        assert_eq!(set_withheld(&conn, "p1", &[5], false).unwrap(), 1);
        assert_eq!(
            withheld_ids(&conn, "p1").unwrap(),
            [6].into_iter().collect()
        );
        assert_eq!(delete_project_withheld(&conn, "p1").unwrap(), 1);
    }

    #[test]
    fn black_holed_frame_ids_follow_the_file() {
        let (_t, db) = conn_with_project();
        let conn = db.conn();
        conn.execute_batch(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at) VALUES (11,'/d/a.fits','a.fits',1,'2026-01-01T00:00:00Z','FITS','2026-01-01T00:00:00Z');
             INSERT INTO files (id, path, filename, size, modified_at, format, created_at) VALUES (12,'/d/b.fits','b.fits',1,'2026-01-01T00:00:00Z','FITS','2026-01-01T00:00:00Z');
             INSERT INTO frames (id, file_id, imagetyp) VALUES (21, 11, 'Light');
             INSERT INTO frames (id, file_id, imagetyp) VALUES (22, 12, 'Light');",
        )
        .unwrap();
        crate::db::add_to_black_hole(&conn, 12, "light", "/d/b.fits").unwrap();
        assert_eq!(
            black_holed_frame_ids(&conn, &[21, 22]).unwrap(),
            [22].into_iter().collect()
        );
    }
}
