//! Durable solve outcomes follow frame identity across catalog renames/regrouping.
//! No image header is edited. A successful retry clears the current failure label
//! while earlier attempts remain in the history table.
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS plate_solve_attempts (
        id INTEGER PRIMARY KEY, frame_id INTEGER NOT NULL REFERENCES frames(id)
        ON DELETE CASCADE,
        status TEXT NOT NULL, code TEXT, error TEXT,
        attempted_at TEXT NOT NULL DEFAULT (strftime( '%Y-%m-%dT%H:%M:%fZ' ,
        'now' )));
        CREATE INDEX IF NOT EXISTS idx_solve_attempts_frame ON
        plate_solve_attempts(frame_id,id);
        ",
    )
}

pub fn record(
    conn: &Connection,
    frame_id: i64,
    status: &str,
    code: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    // A disappeared catalog row has no image to label. Missing-file rows that
    // still exist are retained and can therefore carry a failure label.
    conn.execute(
        "
        INSERT INTO plate_solve_attempts(frame_id,status,code,error) SELECT
        id,?2,?3,?4 FROM frames WHERE id=?1
        ",
        rusqlite::params![frame_id, status, code, error],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SolveAttempt {
    pub frame_id: i64,
    pub filename: String,
    pub path: String,
    pub status: String,
    pub code: Option<String>,
    pub error: Option<String>,
    pub attempted_at: String,
}

pub fn latest(conn: &Connection, ids: &[i64], failed_only: bool) -> Result<Vec<SolveAttempt>> {
    let mut stmt = conn.prepare(
        "
        SELECT a.frame_id,fi.filename,fi.path,a.status,a.code,a.error,a.attempted_at
        FROM plate_solve_attempts a JOIN frames f ON f.id=a.frame_id JOIN files
        fi ON fi.id=f.file_id
        WHERE a.id=(SELECT MAX(b.id) FROM plate_solve_attempts b WHERE
        b.frame_id=a.frame_id AND b.status!= 'cancelled' )
        AND (?1=0 OR a.status= 'failed' ) AND (?2= '[]' OR a.frame_id IN (SELECT
        value FROM json_each(?2)))
        ORDER BY a.id DESC
        ",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![failed_only, serde_json::to_string(ids)?],
            |r| {
                Ok(SolveAttempt {
                    frame_id: r.get(0)?,
                    filename: r.get(1)?,
                    path: r.get(2)?,
                    status: r.get(3)?,
                    code: r.get(4)?,
                    error: r.get(5)?,
                    attempted_at: r.get(6)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get(
    ctx: &crate::services::ServiceContext,
    ids: Vec<i64>,
    failed_only: bool,
) -> Result<Vec<SolveAttempt>> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;
    latest(&db.conn(), &ids, failed_only)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn failure_survives_reopen_regroup_and_rename_but_success_clears_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("catalog.db");
        {
            let conn = Connection::open(&path).unwrap();
            db::schema::init_db(&conn).unwrap();
            conn.execute(
                "
        INSERT INTO files(id,path,filename,size,modified_at,format) VALUES (1,
        '/old/light.fits' , 'light.fits' ,0, '2026-09-08' , 'FITS' )
        ",
                [],
            )
            .unwrap();
            conn.execute(
                "
        INSERT INTO frames(id,file_id,imagetyp) VALUES (1,1,'Light')
        ",
                [],
            )
            .unwrap();
            conn.execute(
                "
        INSERT INTO frames_set(id,name) VALUES (1,'Old object'),(2,'Regrouped object')
        ",
                [],
            )
            .unwrap();
            let night = db::create_imaging_night(&conn, 1, "2026-09-08", "2026-09-08").unwrap();
            let session = db::create_session(&conn, night, "Camera", 1, None).unwrap();
            db::insert_session_members(&conn, session, &[1]).unwrap();
            record(
                &conn,
                1,
                "failed",
                Some("TIMEOUT"),
                Some("Time limit exceeded"),
            )
            .unwrap();
            conn.execute(
                "
        UPDATE imaging_nights SET frames_set_id=2 WHERE id=?1
        ",
                [night],
            )
            .unwrap();
            conn.execute(
                "
        UPDATE files SET path='/tidy/renamed.fits',filename='renamed.fits' WHERE id=1
        ",
                [],
            )
            .unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        db::schema::init_db(&conn).unwrap();
        let rows = latest(&conn, &[], true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename, "renamed.fits");
        assert_eq!(rows[0].path, "/tidy/renamed.fits");
        assert_eq!(rows[0].code.as_deref(), Some("TIMEOUT"));
        assert!(!rows[0].attempted_at.is_empty());
        assert!(latest(&conn, &[99], true).unwrap().is_empty());
        record(&conn, 1, "cancelled", Some("CANCELLED"), None).unwrap();
        assert_eq!(latest(&conn, &[1], true).unwrap().len(), 1);
        record(&conn, 1, "solved", None, None).unwrap();
        assert!(latest(&conn, &[], true).unwrap().is_empty());
        assert_eq!(latest(&conn, &[1], false).unwrap()[0].status, "solved");
        assert_eq!(
            conn.query_row(
                "
        SELECT COUNT(*) FROM plate_solve_attempts
        ",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        conn.execute("PRAGMA foreign_keys=ON", []).unwrap();
        conn.execute(
            "
        DELETE FROM frames WHERE id=1
        ",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "
        SELECT COUNT(*) FROM plate_solve_attempts
        ",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

/// One wire contract for desktop events and web SSE.
#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlateSolveProgressEvent {
    pub frame_id: i64,
    pub current: usize,
    pub total: usize,
    pub status: String,
    pub matched_stars: Option<usize>,
    pub rms_arcsec: Option<f64>,
    pub error: Option<String>,
    pub failure_code: Option<String>,
    pub filename: Option<String>,
}

#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PlateSolveCompleteEvent {
    pub solved: usize,
    pub failed: usize,
    pub total: usize,
    pub total_time_ms: u64,
    pub cancelled: bool,
    pub not_processed: usize,
}
