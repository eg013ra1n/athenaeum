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
    /// Transparency loss in magnitudes; `None` where it was not measured.
    pub transparency_loss: Option<f64>,
    /// The linked calibration: (type, master file name, folder). A raw set that is not built into a master reads
    /// `<n> raw frames, e.g. <file name>`.
    pub calibration: Vec<(String, String, String)>,
    /// Other catalog files the duplicate groups name with this one.
    pub copies: Vec<FileLine>,
    /// A plate solve of Athenaeum's own is stored for the frame.
    pub solved: bool,
}

fn internal(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "file details: {what} failed");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

/// The id of the file `file` names, if the catalog holds one.
fn file_id_of(conn: &Connection, file: &str) -> Result<Option<i64>, ApiError> {
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
fn frames_of_files(conn: &Connection, file_ids: &[i64]) -> Result<Vec<ScopedFrame>, ApiError> {
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

/// Master file name and folder of a calibration set: its master file when it has one, else its first member.
fn set_file(conn: &Connection, set_id: i64) -> Result<Option<(String, String)>, ApiError> {
    let members: i64 = conn
        .query_row("SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1", [set_id], |r| r.get(0))
        .map_err(|e| internal("the calibration set", e))?;
    let first: Option<(String, String, bool)> = conn
        .query_row(
            "SELECT fi.filename, fi.path, fr.is_master FROM calibration_set_frames csf \
             JOIN frames fr ON fr.id = csf.frame_id JOIN files fi ON fi.id = fr.file_id \
             WHERE csf.set_id = ?1 ORDER BY fr.is_master DESC, fr.id LIMIT 1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
        )
        .optional()
        .map_err(|e| internal("the calibration set", e))?;
    Ok(first.map(|(name, path, is_master)| {
        let name = if is_master || members <= 1 { name } else { format!("{members} raw frames, e.g. {name}") };
        (name, super::parent_of(&path))
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
    let transparency_loss = {
        let catalog = ctx.app_data_dir().and_then(|d| crate::analysis::transparency::installed_catalog_identity(&d));
        crate::analysis::transparency::transparency_for_frames(&conn, catalog.as_deref(), &[frame_id])
            .map_err(|e| internal("the transparency", e))?
            .get(&frame_id)
            .and_then(|t| t.loss_mag)
    };
    #[cfg(not(all(feature = "render", feature = "solver")))]
    let transparency_loss = None;

    let mut calibration = Vec::new();
    for link in crate::db::calibration_links::get_links_for_frame(&conn, frame_id)
        .map_err(|e| internal("the calibration links", e))?
    {
        if let Some((name, folder)) = set_file(&conn, link.calibration_set_id)? {
            calibration.push((link.calibration_type, name, folder));
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
        transparency_loss,
        calibration,
        copies,
        solved,
    })
}
