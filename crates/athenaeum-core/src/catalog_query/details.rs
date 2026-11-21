//! Everything the catalog keeps about one file (MCP plan, Task 3): header values, the stored measurements,
//! calibration links, where its copies live. The file is named by its handle (`file:<id>`) or by the path the
//! catalog stores; a path is matched by equality after separator normalisation, in the catalog, and never opened,
//! listed or canonicalised.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{fetch_scoped_frames, file_line, FileLine, ScopedFrame};
use crate::api::ApiError;
use crate::models::FrameAnalysis;
use crate::scope::ScopeFilter;
use crate::services::ServiceContext;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetails {
    pub line: FileLine,
    /// The path as the catalog stores it.
    pub path: String,
    pub size: i64,
    /// `YYYY-MM-DD HH:MM:SS`, UTC.
    pub modified_at: String,
    /// The stored header keywords and their values, by keyword.
    pub header: Vec<(String, String)>,
    pub analysis: Option<FrameAnalysis>,
    /// The analysis row was written by the current measurement version; `false` when it is an older one (or when
    /// there is no row).
    pub analysis_current: bool,
    /// Transparency loss in magnitudes; `None` where it was not measured.
    pub transparency_loss: Option<f64>,
    /// Why there is no transparency loss, in the app's words (the action that cures it, or the app's own sentence);
    /// `None` when there is a loss or the app states no reason.
    pub transparency_reason: Option<String>,
    /// The linked calibration, one file each.
    pub calibration: Vec<CalibrationLink>,
    /// Other catalog files the duplicate groups name with this one.
    pub copies: Vec<FileLine>,
    /// A plate solve of Athenaeum's own is stored for the frame.
    pub solved: bool,
}

/// One calibration link of a file: the master file, or, for a raw set not built into a master, its first member (the
/// file name then reads `<n> raw frames, e.g. <file name>`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationLink {
    /// `Dark`, `Flat`, `Bias`, ...
    pub kind: String,
    /// `file:<id>`.
    pub handle: String,
    pub file_name: String,
    pub folder: String,
}

fn internal(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "file details: {what} failed");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

/// The id of the file `file` names (a `file:<n>` handle or a full catalog path), if the catalog holds one.
pub(crate) fn file_id_of(conn: &Connection, file: &str) -> Result<Option<i64>, ApiError> {
    if let Some(id) = file.strip_prefix("file:") {
        let Ok(id) = id.trim().parse::<i64>() else { return Ok(None) };
        return conn
            .query_row("SELECT id FROM files WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| internal("the file", e));
    }
    let path = crate::db::normalize_separators(file.trim());
    conn.query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
        .optional()
        .map_err(|e| internal("the file", e))
}

/// The catalog frames of the given files, in the scope's own shape (so a [`FileLine`] reads as everywhere else).
pub(super) fn frames_of_files(conn: &Connection, file_ids: &[i64]) -> Result<Vec<ScopedFrame>, ApiError> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; file_ids.len()].join(", ");
    let filter = ScopeFilter {
        sql: format!("fr.file_id IN ({marks})"),
        params: file_ids.iter().map(|id| Value::Integer(*id)).collect(),
    };
    fetch_scoped_frames(conn, &filter).map_err(|e| internal("the files", format!("{e:#}")))
}

/// File id, file name and folder of a calibration set: its master file when it has one, else its first member.
fn set_file(conn: &Connection, set_id: i64) -> Result<Option<(i64, String, String)>, ApiError> {
    let members: i64 = conn
        .query_row("SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1", [set_id], |r| r.get(0))
        .map_err(|e| internal("the calibration set", e))?;
    let first: Option<(i64, String, String, bool)> = conn
        .query_row(
            "SELECT fi.id, fi.filename, fi.path, fr.is_master FROM calibration_set_frames csf \
             JOIN frames fr ON fr.id = csf.frame_id JOIN files fi ON fi.id = fr.file_id \
             WHERE csf.set_id = ?1 ORDER BY fr.is_master DESC, fr.id LIMIT 1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0)),
        )
        .optional()
        .map_err(|e| internal("the calibration set", e))?;
    Ok(first.map(|(id, name, path, is_master)| {
        let name = if is_master || members <= 1 { name } else { format!("{members} raw frames, e.g. {name}") };
        (id, name, super::parent_of(&path))
    }))
}

/// The details of one file. `file` is `file:<id>` or a path as the catalog stores it.
pub fn file_details(ctx: &ServiceContext, file: &str) -> Result<FileDetails, ApiError> {
    let not_found = || ApiError::NotFound(format!("no file {file} in the catalog"));

    // Resolve the file first and find its copies before any connection is held: `get_duplicates` takes `ctx`.
    let file_id = {
        let db = crate::api::db(ctx)?;
        let conn = db.conn();
        file_id_of(&conn, file)?
    }
    .ok_or_else(not_found)?;
    let copy_ids: Vec<i64> = {
        let mut ids: Vec<i64> = crate::api::files::get_duplicates(ctx)?
            .into_iter()
            .filter(|g| g.file_ids.contains(&file_id))
            .flat_map(|g| g.file_ids)
            .filter(|id| *id != file_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };

    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let mut scoped = frames_of_files(&conn, &[file_id])?;
    if scoped.is_empty() {
        return Err(ApiError::NotFound(format!("file {file} is in the catalog but holds no image frame")));
    }
    let item = scoped.swap_remove(0);
    let frame = item.file.frame.as_ref().ok_or_else(not_found)?;
    let frame_id = frame.id.ok_or_else(not_found)?;
    let stored = &item.file.file;

    let header_text: Option<String> = conn
        .query_row("SELECT header FROM fits_header WHERE file_id = ?1 LIMIT 1", [file_id], |r| r.get(0))
        .optional()
        .map_err(|e| internal("the header", e))?;
    let mut header: Vec<(String, String)> = header_text
        .map(|text| crate::fits_parser::stored_header::parse_stored_header_keys(stored.format.clone(), &text))
        .unwrap_or_default()
        .into_iter()
        .collect();
    header.sort();

    let analysis = crate::db::analysis::get_frame_analysis(&conn, frame_id).map_err(|e| internal("the measurements", e))?;
    let solved: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM plate_solves WHERE frame_id = ?1)", [frame_id], |r| r.get(0))
        .map_err(|e| internal("the plate solve", e))?;

    #[cfg(all(feature = "render", feature = "solver"))]
    let (transparency_loss, transparency_reason) = {
        let catalog = ctx.app_data_dir().and_then(|d| crate::analysis::transparency::installed_catalog_identity(&d));
        let found = crate::analysis::transparency::transparency_for_frames(&conn, catalog.as_deref(), &[frame_id])
            .map_err(|e| internal("the transparency", e))?;
        let t = found.get(&frame_id);
        let loss = t.and_then(|t| t.loss_mag);
        let reason = if loss.is_some() {
            None
        } else {
            t.and_then(|t| t.reason.as_deref()).map(super::quality::transparency_reason_text)
        };
        (loss, reason)
    };
    #[cfg(not(all(feature = "render", feature = "solver")))]
    let (transparency_loss, transparency_reason) = (None, None);

    #[cfg(feature = "render")]
    let analysis_current = analysis.as_ref().is_some_and(|a| a.measure_version == Some(crate::star_measure::MEASURE_VERSION));
    #[cfg(not(feature = "render"))]
    let analysis_current = false;

    let mut calibration = Vec::new();
    for link in crate::db::calibration_links::get_links_for_frame(&conn, frame_id)
        .map_err(|e| internal("the calibration links", e))?
    {
        if let Some((id, file_name, folder)) = set_file(&conn, link.calibration_set_id)? {
            calibration.push(CalibrationLink { kind: link.calibration_type, handle: format!("file:{id}"), file_name, folder });
        }
    }

    let copies = frames_of_files(&conn, &copy_ids)?.iter().map(file_line).collect();

    Ok(FileDetails {
        line: file_line(&item),
        path: stored.path.clone(),
        size: stored.size,
        modified_at: stored.modified_at.format("%Y-%m-%d %H:%M:%S").to_string(),
        header,
        analysis,
        analysis_current,
        transparency_loss,
        transparency_reason,
        calibration,
        copies,
        solved,
    })
}
