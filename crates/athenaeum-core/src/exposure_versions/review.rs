//! Reviewable suggestions. Metadata agreement is evidence, never an automatic merge.
use super::{classify, Classification};
use crate::services::ServiceContext;
use anyhow::{anyhow, bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecord {
    pub assessment: super::ProcessingAssessment,
    pub frame_id: i64,
    pub filename: String,
    pub path: String,
    pub date_obs: Option<String>,
    pub camera: Option<String>,
    pub exposure_seconds: Option<f64>,
    pub filter: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub classification: Classification,
    pub exposure_id: Option<String>,
    pub manual_stage: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct VersionSuggestion {
    pub left_id: i64,
    pub right_id: i64,
    pub confidence: String,
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct VersionReview {
    pub versions: Vec<VersionRecord>,
    pub suggestions: Vec<VersionSuggestion>,
    pub suggestion_count: usize,
    pub exposure_count: usize,
    pub exposure_seconds: f64,
}

fn records(conn: &Connection, ids: &[i64]) -> Result<Vec<VersionRecord>> {
    let mut stmt = conn.prepare(
        "
        SELECT
        f.id,fi.filename,f.date_obs,f.instrume,f.exptime,f.filter,f.naxis1,f.naxis2,

        p.classification,p.stage_override,v.exposure_id,fi.path,fi.modified_at,fi.format,
        (SELECT h.header FROM fits_header h WHERE h.file_id=fi.id ORDER BY h.id
        DESC LIMIT 1) FROM frames f JOIN files fi ON fi.id=f.file_id
        LEFT JOIN file_processing p ON p.file_id=f.file_id LEFT JOIN
        exposure_versions v ON v.frame_id=f.id
        WHERE f.id IN (SELECT value FROM json_each(?1)) ORDER BY f.id
        ",
    )?;
    let rows = stmt
        .query_map([serde_json::to_string(ids)?], |r| {
            let json: Option<String> = r.get(8)?;
            let mut c: Classification = match json {
                Some(s) => serde_json::from_str(&s).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                None => classify(crate::models::FileFormat::FITS, ""),
            };
            let manual: Option<String> = r.get(9)?;
            if let Some(ref stage) = manual {
                c.stage = stage.clone();
                c.confidence = "user confirmed".into();
                c.evidence
                    .push("Processing stage manually specified by user".into());
            }
            let path: String = r.get(11)?;
            let modified: String = r.get(12)?;
            let format: String = r.get(13)?;
            let header: Option<String> = r.get(14)?;
            let assessment = super::assessment::assess(
                &c,
                if format == "XISF" {
                    crate::models::FileFormat::XISF
                } else {
                    crate::models::FileFormat::FITS
                },
                header.as_deref().unwrap_or(""),
                &path,
                &modified,
            );
            Ok(VersionRecord {
                assessment,
                frame_id: r.get(0)?,
                filename: r.get(1)?,
                path: r.get(11)?,
                date_obs: r.get(2)?,
                camera: r.get(3)?,
                exposure_seconds: r.get(4)?,
                filter: r.get(5)?,
                width: r.get(6)?,
                height: r.get(7)?,
                classification: c,
                exposure_id: r.get(10)?,
                manual_stage: manual.is_some(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn same_text(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a,b),(Some(a),Some(b)) if !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim()))
}
fn same_time(a: &Option<String>, b: &Option<String>) -> bool {
    let normalize = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp_micros())
    };
    match (a, b) {
        (Some(a), Some(b)) if !a.is_empty() => match (normalize(a), normalize(b)) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        },
        _ => false,
    }
}
fn suggestion(a: &VersionRecord, b: &VersionRecord) -> Option<VersionSuggestion> {
    if a.classification.stage == "integrated" || b.classification.stage == "integrated" {
        return None;
    }
    if a.exposure_id.is_some() && a.exposure_id == b.exposure_id {
        return None;
    }
    let source = same_text(&a.classification.source_id, &b.classification.source_id);
    let metadata = same_time(&a.date_obs, &b.date_obs)
        && same_text(&a.camera, &b.camera)
        && same_text(&a.filter, &b.filter)
        && matches!((a.exposure_seconds,b.exposure_seconds),(Some(x),Some(y)) if x.is_finite() && x>0.0 && x==y);
    if !source && !metadata {
        return None;
    }
    let mut evidence = Vec::new();
    if source {
        evidence.push(
            "Same preserved source UUID; verify external metadata was retained correctly".into(),
        );
    }
    if metadata {
        evidence.push("Observation time, camera, exposure duration and filter agree".into());
    }
    let dimensions =
        a.width.is_some() && a.height.is_some() && a.width == b.width && a.height == b.height;
    evidence.push(
        if dimensions {
            "Image dimensions agree"
        } else {
            "Dimensions differ or are missing; cropping/resampling may explain this"
        }
        .into(),
    );
    if same_text(&a.classification.source_name, &b.classification.source_name) {
        evidence.push("Source filename agrees (supporting evidence only)".into());
    }
    Some(VersionSuggestion {
        left_id: a.frame_id,
        right_id: b.frame_id,
        confidence: if source && metadata && dimensions {
            "strong suggestion"
        } else {
            "needs review"
        }
        .into(),
        evidence,
    })
}

/// Review all LIGHT versions belonging to the selected objects. Large selections
/// fail explicitly rather than silently omitting possible matches. Reads headers
/// from the catalog only; no original pixels/files are accessed.
pub fn get_review(ctx: &ServiceContext, frames_set_ids: Vec<i64>) -> Result<VersionReview> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let conn = db.conn();
    let ids = conn
        .prepare(
            "
        SELECT DISTINCT f.id FROM frames f JOIN session_members sm ON sm.frame_id=f.id
        JOIN sessions s ON s.id=sm.session_id JOIN imaging_nights n ON
        n.id=s.imaging_night_id
        WHERE n.frames_set_id IN (SELECT value FROM json_each(?1)) AND
        UPPER(f.imagetyp)= 'LIGHT' LIMIT 1001
        ",
        )?
        .query_map([serde_json::to_string(&frames_set_ids)?], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    if ids.len() > 1000 {
        bail!("Review supports 1000 files at a time. Select fewer objects.");
    }
    let versions = records(&conn, &ids)?;
    let mut suggestions = Vec::new();
    let mut suggestion_count = 0;
    for (i, a) in versions.iter().enumerate() {
        for b in &versions[i + 1..] {
            if let Some(s) = suggestion(a, b) {
                suggestion_count += 1;
                if suggestions.len() < 200 {
                    suggestions.push(s);
                }
            }
        }
    }
    let effective = super::effective_ids(&conn, &ids)?;
    let exposure_seconds = versions
        .iter()
        .filter(|v| effective.contains(&v.frame_id))
        .filter_map(|v| v.exposure_seconds)
        .filter(|v| v.is_finite() && *v > 0.0)
        .sum();
    Ok(VersionReview {
        versions,
        suggestions,
        suggestion_count,
        exposure_count: effective.len(),
        exposure_seconds,
    })
}

/// User confirmation only. Re-evaluate evidence at commit time; merge complete
/// existing groups atomically. No source frame is created and no file is changed.
pub fn confirm_link(ctx: &ServiceContext, left_id: i64, right_id: i64) -> Result<()> {
    if left_id == right_id {
        bail!("Select two different versions");
    }
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let tx = conn.transaction()?;
    let rows = records(&tx, &[left_id, right_id])?;
    if rows.len() != 2 || suggestion(&rows[0], &rows[1]).is_none() {
        bail!("No current compatible suggestion. Refresh and review the evidence.");
    }
    // Check every member of both groups, preventing a transitive merge from
    // joining observations with conflicting exposure durations or metadata.
    let group_ids: Vec<String> = rows.iter().filter_map(|v| v.exposure_id.clone()).collect();
    let mut all = tx
        .prepare(
            "
        SELECT frame_id FROM exposure_versions WHERE exposure_id IN (SELECT
        value FROM json_each(?1))
        ",
        )?
        .query_map([serde_json::to_string(&group_ids)?], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    all.extend([left_id, right_id]);
    all.sort_unstable();
    all.dedup();
    let members = records(&tx, &all)?;
    for a in &members {
        if a.classification.stage == "integrated" {
            bail!("Integrated products cannot be single-exposure versions");
        }
    }
    for a in &members {
        for b in &members {
            if a.date_obs.is_some() && b.date_obs.is_some() && !same_time(&a.date_obs, &b.date_obs)
            {
                bail!("Observation times conflict; review metadata before linking");
            }
            if a.camera.is_some() && b.camera.is_some() && !same_text(&a.camera, &b.camera) {
                bail!("Camera metadata conflict; review before linking");
            }
            if a.filter.is_some() && b.filter.is_some() && !same_text(&a.filter, &b.filter) {
                bail!("Filters conflict; review before linking");
            }
            if let (Some(x), Some(y)) = (a.exposure_seconds, b.exposure_seconds) {
                if x != y {
                    bail!("Exposure durations conflict; resolve metadata before linking");
                }
            }
        }
    }
    let group = rows
        .iter()
        .find_map(|v| v.exposure_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    for id in all {
        tx.execute(
            "
        INSERT INTO exposure_versions(frame_id,exposure_id) VALUES (?1,?2) ON
        CONFLICT(frame_id) DO UPDATE SET exposure_id=excluded.exposure_id
        ",
            rusqlite::params![id, group],
        )?;
    }
    tx.commit()?;
    Ok(())
}
/// Remove one confirmed membership. Remaining available files are preserved.
pub fn unlink_version(ctx: &ServiceContext, frame_id: i64) -> Result<()> {
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    db.conn().execute(
        "
        DELETE FROM exposure_versions WHERE frame_id=?1
        ",
        [frame_id],
    )?;
    Ok(())
}
/// Set or clear a user classification. Explicit multi-input header evidence cannot
/// be overridden into a single exposure; that would silently inflate counts.
pub fn set_stage(ctx: &ServiceContext, frame_id: i64, stage: Option<String>) -> Result<()> {
    if let Some(ref s) = stage {
        if ![
            "raw",
            "calibrated",
            "debayered",
            "registered",
            "integrated",
            "unknown",
        ]
        .contains(&s.as_str())
        {
            bail!("Unknown processing stage");
        }
    }
    let db = ctx
        .db
        .get()
        .ok_or_else(|| anyhow!("Database not initialized"))?;
    let mut conn = db.conn();
    let tx = conn.transaction()?;
    let file: i64 = tx.query_row(
        "
        SELECT file_id FROM frames WHERE id=?1
        ",
        [frame_id],
        |r| r.get(0),
    )?;
    let detected: Option<String> = tx
        .query_row(
            "
        SELECT detected_stage FROM file_processing WHERE file_id=?1
        ",
            [file],
            |r| r.get(0),
        )
        .optional()?;
    if detected.as_deref() == Some("integrated")
        && stage.as_deref().is_some_and(|s| s != "integrated")
    {
        bail!("Header identifies a multi-input product; it cannot be marked as a single exposure");
    }
    tx.execute(
        "
        INSERT INTO
        file_processing(file_id,header_id,detected_stage,classification,stage_override)
        VALUES (?1,-1, 'unknown' ,?2,?3) ON CONFLICT(file_id) DO UPDATE SET
        stage_override=excluded.stage_override
        ",
        rusqlite::params![
            file,
            serde_json::to_string(&classify(crate::models::FileFormat::FITS, ""))?,
            stage
        ],
    )?;
    if stage.as_deref() == Some("integrated") {
        tx.execute(
            "
        DELETE FROM exposure_versions WHERE frame_id IN (SELECT id FROM frames
        WHERE file_id=?1)
        ",
            [file],
        )?;
    }
    tx.commit()?;
    Ok(())
}
