use super::classify;
use crate::models::FileFormat;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// Additive catalog schema. Backfill only headers not previously classified.
/// Existing raw files are neither opened nor altered: evidence is the last scan.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS file_processing (
        file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        header_id INTEGER NOT NULL, detected_stage TEXT NOT NULL,
        classification TEXT NOT NULL, stage_override TEXT);
        CREATE TABLE IF NOT EXISTS exposure_versions (
        frame_id INTEGER PRIMARY KEY REFERENCES frames(id) ON DELETE CASCADE,
        exposure_id TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_file_processing_file ON
        file_processing(file_id);
        CREATE INDEX IF NOT EXISTS idx_exposure_versions_frame ON
        exposure_versions(frame_id);
        CREATE TRIGGER IF NOT EXISTS invalidate_exposure_version_metadata
        AFTER UPDATE OF date_obs,instrume,exptime,filter,imagetyp ON frames
        WHEN OLD.date_obs IS NOT NEW.date_obs OR OLD.instrume IS NOT
        NEW.instrume
          OR OLD.exptime IS NOT NEW.exptime OR OLD.filter IS NOT NEW.filter
          OR OLD.imagetyp IS NOT NEW.imagetyp
        BEGIN DELETE FROM exposure_versions WHERE frame_id=NEW.id; END;
        CREATE INDEX IF NOT EXISTS idx_exposure_versions_group ON
        exposure_versions(exposure_id);
        CREATE VIEW IF NOT EXISTS exposure_frames AS
        SELECT frame_id FROM (
          SELECT f.id AS frame_id, ROW_NUMBER() OVER (
            PARTITION BY COALESCE(v.exposure_id,'frame:' || f.id)
            ORDER BY CASE WHEN f.exptime>0 THEN 0 ELSE 1 END, f.id) AS rank
          FROM frames f LEFT JOIN exposure_versions v ON v.frame_id=f.id
          LEFT JOIN file_processing p ON p.file_id=f.file_id
        WHERE UPPER(COALESCE(f.imagetyp, '' )) != 'LIGHT' OR
        COALESCE(p.stage_override,p.detected_stage, 'unknown' ) != 'integrated'
        ) WHERE rank=1;
        CREATE VIEW IF NOT EXISTS exposure_members AS
        SELECT frames_set_id, frame_id FROM (
          SELECT DISTINCT n.frames_set_id, f.id AS frame_id,
        ROW_NUMBER() OVER (PARTITION BY n.frames_set_id, COALESCE(v.exposure_id,
        'frame:' || f.id) ORDER BY CASE WHEN f.exptime>0 THEN 0 ELSE 1 END, CASE
        WHEN f.ra IS NOT NULL AND f.dec IS NOT NULL THEN 0 ELSE 1 END, f.id) AS
        rank
          FROM frames f JOIN session_members sm ON sm.frame_id=f.id
        JOIN sessions s ON s.id=sm.session_id JOIN imaging_nights n ON
        n.id=s.imaging_night_id
          LEFT JOIN exposure_versions v ON v.frame_id=f.id
          LEFT JOIN file_processing p ON p.file_id=f.file_id
        WHERE COALESCE(p.stage_override,p.detected_stage, 'unknown' ) !=
        'integrated'
        ) WHERE rank=1;
        CREATE VIEW IF NOT EXISTS unorganized_exposure_frames AS
        SELECT frame_id FROM (
          SELECT f.id AS frame_id, ROW_NUMBER() OVER (
        PARTITION BY COALESCE(v.exposure_id, 'frame:' || f.id) ORDER BY CASE
        WHEN f.exptime>0 THEN 0 ELSE 1 END, CASE WHEN f.ra IS NOT NULL AND f.dec
        IS NOT NULL THEN 0 ELSE 1 END, f.id) AS rank
          FROM frames f LEFT JOIN exposure_versions v ON v.frame_id=f.id
          LEFT JOIN file_processing p ON p.file_id=f.file_id
        WHERE NOT EXISTS (SELECT 1 FROM session_members sm WHERE
        sm.frame_id=f.id)
        AND COALESCE(p.stage_override,p.detected_stage, 'unknown' ) !=
        'integrated'
        ) WHERE rank=1;
        ",
    )?;
    let ids = conn
        .prepare(
            "
        SELECT h.file_id FROM fits_header h LEFT JOIN file_processing p ON
        p.file_id=h.file_id GROUP BY h.file_id HAVING MAX(h.id) !=
        COALESCE(p.header_id,-1)
        ",
        )?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        refresh_file(conn, id)?;
    }
    Ok(())
}

/// Refresh header evidence after a scan; a newly detected integration cannot
/// retain an old single-exposure relationship. Explicit user stages survive scans.
pub fn refresh_file(conn: &Connection, file_id: i64) -> Result<()> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "
        SELECT h.id,h.header,f.format FROM fits_header h JOIN files f ON
        f.id=h.file_id WHERE h.file_id=?1 ORDER BY h.id DESC LIMIT 1
        ",
            [file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((id, header, format)) = row {
        let old_header: Option<String> = conn
            .query_row(
                "
        SELECT h.header FROM file_processing p JOIN fits_header h ON
        h.id=p.header_id WHERE p.file_id=?1
        ",
                [file_id],
                |r| r.get(0),
            )
            .optional()?;
        if old_header.as_ref().is_some_and(|old| old != &header) {
            // Changed provenance invalidates a reviewed link; do not silently
            // reuse it for a newly processed/replaced file at the same path.
            conn.execute(
                "
        DELETE FROM exposure_versions WHERE frame_id IN (SELECT id FROM frames
        WHERE file_id=?1)
        ",
                [file_id],
            )?;
        }
        let c = classify(
            if format == "XISF" {
                FileFormat::XISF
            } else {
                FileFormat::FITS
            },
            &header,
        );
        conn.execute(
            "
        INSERT INTO
        file_processing(file_id,header_id,detected_stage,classification) VALUES
        (?1,?2,?3,?4)
        ON CONFLICT(file_id) DO UPDATE SET
        header_id=excluded.header_id,detected_stage=excluded.detected_stage,classification=excluded.classification
        ",
            rusqlite::params![file_id,id,c.stage,serde_json::to_string(&c)?])?;
        if c.stage == "integrated" {
            conn.execute(
                "
        UPDATE file_processing SET stage_override=NULL WHERE file_id=?1
        ",
                [file_id],
            )?;
            conn.execute(
                "
        DELETE FROM exposure_versions WHERE frame_id IN (SELECT id FROM frames
        WHERE file_id=?1)
        ",
                [file_id],
            )?;
        }
    }
    Ok(())
}

/// One available representative per confirmed exposure within the supplied
/// frame IDs. No raw-file preference is assumed; integrated products are omitted.
/// This subset-local reduction keeps an exposure countable when another version
/// lives outside the currently selected object/session.
///
/// IDs refer to `frames.id`, not `files.id`; absent IDs are ignored. Unconfirmed
/// frames remain separate. Prefer positive EXPTIME, then available coordinates,
/// then the lowest ID; quality scores do not affect selection. Results are sorted
/// by frame ID. Database/serialization errors propagate without changing links.
pub fn effective_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "
        SELECT id FROM (SELECT f.id,ROW_NUMBER() OVER (
        PARTITION BY COALESCE(v.exposure_id,'frame:' || f.id)
        ORDER BY CASE WHEN f.exptime>0 THEN 0 ELSE 1 END, CASE WHEN f.ra IS NOT
        NULL AND f.dec IS NOT NULL THEN 0 ELSE 1 END, f.id) AS rank
        FROM frames f LEFT JOIN exposure_versions v ON v.frame_id=f.id LEFT JOIN
        file_processing p ON p.file_id=f.file_id
        WHERE f.id IN (SELECT value FROM json_each(?1))
        AND COALESCE(p.stage_override,p.detected_stage, 'unknown' ) !=
        'integrated' ) WHERE rank=1 ORDER BY id
        ",
    )?;
    let result = stmt
        .query_map([serde_json::to_string(ids)?], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}

/// Sum positive exposure seconds once per confirmed exposure within this selection.
/// Files and their per-file EXPTIME values remain unchanged.
pub fn exposure_seconds(conn: &Connection, ids: &[i64]) -> Result<f64> {
    let ids = effective_ids(conn, ids)?;
    Ok(conn.query_row(
        "
        SELECT COALESCE(SUM(CASE WHEN exptime>0 THEN exptime ELSE 0 END),0) FROM
        frames WHERE id IN (SELECT value FROM json_each(?1))
        ",
        [serde_json::to_string(&ids)?],
        |row| row.get(0),
    )?)
}

pub fn get_effective_frame_ids(
    ctx: &crate::services::ServiceContext,
    ids: Vec<i64>,
) -> Result<Vec<i64>> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;
    effective_ids(&db.conn(), &ids)
}
