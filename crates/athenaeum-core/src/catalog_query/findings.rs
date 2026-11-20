//! Scoped findings: duplicate groups and missing metadata over a resolved scope (MCP plan, Task 4).
//!
//! A duplicate group is the Duplicates page's own group (`get_duplicates`), tagged by the key that found it; a group
//! is in a scope when one of its copies is, and every copy is shown with where it lives. Missing metadata is the
//! Missing Metadata view's own SQL with the scope's predicate added, so a scope that covers every state returns the
//! view's rows. Black Hole files are left out by the scope's state, not by a second rule here.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::details::frames_of_files;
use super::{
    fetch_scoped_frames, file_line, parent_of, FileLine, FilePage, ScopedFrame, EXAMPLES_PER_GROUP, FILES_PAGE,
    GROUPS_SHOWN,
};
use crate::api::ApiError;
use crate::db::{self, native_separator_of, DuplicateKey};
use crate::models::{DuplicateFile, DuplicateGroup};
use crate::scope::{FrameState, ResolvedScope};
use crate::services::ServiceContext;

fn failed(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "findings: {what} failed");
    ApiError::Internal(format!("could not read {what}: {e}"))
}

// ---- duplicates ----

/// One copy of a duplicated file: the file as every list shows it, and where it lives.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyLine {
    pub line: FileLine,
    /// The scan root the copy sits under (its full path), when one does.
    pub scan_root: Option<String>,
    /// The scan root's role: `normal`, `calibration_library`, `sync_incoming` or `collaboration`.
    pub root_kind: Option<String>,
    /// An inference about the copy's folder, said as one (a folder of masters and processed files only).
    pub folder_note: Option<String>,
}

/// A duplicate group and the key that found it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFinding {
    /// `dup:<header|master|content>:<hash the page groups by>`.
    pub handle: String,
    pub key: DuplicateKey,
    /// What "duplicate" means for this group, in words.
    pub matched_by: String,
    /// Bytes of one copy.
    pub size: i64,
    pub copies: Vec<CopyLine>,
    /// Copies beyond the first.
    pub extra_copies: usize,
}

fn matched_by(key: DuplicateKey) -> &'static str {
    match key {
        DuplicateKey::Header => "same header, size and name (raw sub-frames)",
        DuplicateKey::Master => "same full-file hash (masters and processed files)",
        DuplicateKey::Content => "same content",
    }
}

/// The file line of a copy that has no frame row (only the content key can offer one): what the duplicate group
/// itself knows. Its state is not known, so it reads as a catalog file.
fn bare_line(file: &DuplicateFile) -> FileLine {
    FileLine {
        handle: format!("file:{}", file.file_id),
        file_name: file.filename.clone(),
        folder: parent_of(&file.path),
        imagetyp: None,
        object: None,
        filter: None,
        exptime_s: None,
        gain: None,
        offset: None,
        ccd_temp: None,
        binning: None,
        width: None,
        height: None,
        date_obs: None,
        night: None,
        instrume: None,
        telescop: None,
        state: FrameState::Catalog,
    }
}

fn root_kind_of(conn: &Connection, path: &str) -> Result<Option<String>, ApiError> {
    conn.query_row("SELECT kind FROM scan_roots WHERE path = ?1", [path], |r| r.get(0))
        .optional()
        .map_err(|e| failed("the scan roots", e))
}

/// The duplicate groups with a copy in `scope`, page `page` (zero-based, [`GROUPS_SHOWN`] groups to a page), in the
/// Duplicates page's order; with the number of such groups and the bytes their extra copies take.
pub fn find_duplicates_in_scope(
    ctx: &ServiceContext,
    scope: &ResolvedScope,
    page: u32,
) -> Result<(Vec<DuplicateFinding>, usize, i64), ApiError> {
    // The page's groups first: `get_duplicates` takes `ctx`, so no connection is held across it.
    let groups = crate::api::files::get_duplicates(ctx)?;
    let Some(filter) = scope.filter.as_ref() else {
        return Ok((Vec::new(), 0, 0));
    };
    let database = crate::api::db(ctx)?;
    let conn = database.conn();

    let in_scope: HashSet<i64> = {
        let sql = format!("SELECT fr.file_id FROM frames fr WHERE {}", filter.sql);
        let mut stmt = conn.prepare(&sql).map_err(|e| failed("the files in scope", e))?;
        let rows = stmt
            .query_map(params_from_iter(filter.params.iter()), |r| r.get::<_, i64>(0))
            .map_err(|e| failed("the files in scope", e))?;
        rows.collect::<rusqlite::Result<HashSet<i64>>>().map_err(|e| failed("the files in scope", e))?
    };
    let chosen: Vec<&DuplicateGroup> = groups.iter().filter(|g| g.file_ids.iter().any(|id| in_scope.contains(id))).collect();
    let total = chosen.len();
    let extra_bytes: i64 = chosen.iter().map(|g| g.size * (g.file_ids.len().saturating_sub(1) as i64)).sum();

    let start = (page as usize).saturating_mul(GROUPS_SHOWN);
    let shown: Vec<&DuplicateGroup> = chosen.into_iter().skip(start).take(GROUPS_SHOWN).collect();

    let mut file_ids: Vec<i64> = shown.iter().flat_map(|g| g.file_ids.iter().copied()).collect();
    file_ids.sort_unstable();
    file_ids.dedup();
    let mut frames: HashMap<i64, ScopedFrame> = HashMap::new();
    for chunk in file_ids.chunks(500) {
        for sf in frames_of_files(&conn, chunk)? {
            frames.entry(sf.file.file.id.unwrap_or_default()).or_insert(sf);
        }
    }

    let mut notes: HashMap<String, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(shown.len());
    for g in shown {
        let Some(key) = g.key else {
            tracing::error!(bytes = g.size, "findings: a duplicate group has no key");
            return Err(ApiError::Internal("a duplicate group came back without the key that found it".into()));
        };
        let mut copies = Vec::with_capacity(g.file_ids.len());
        for file_id in &g.file_ids {
            let known = g.files.iter().find(|f| f.file_id == *file_id);
            let (line, scan_root, root_kind) = match (frames.get(file_id), known) {
                (Some(sf), _) => {
                    let (root, kind) = sf.scan_root.clone().unzip();
                    (file_line(sf), root, kind)
                }
                (None, Some(file)) => {
                    let root = file.scan_root_path.clone();
                    let kind = match &root {
                        Some(path) => root_kind_of(&conn, path)?,
                        None => None,
                    };
                    (bare_line(file), root, kind)
                }
                (None, None) => {
                    tracing::error!(file_id = *file_id, "findings: a duplicate copy is not in the catalog");
                    return Err(ApiError::Internal(format!("file {file_id} of a duplicate group is not in the catalog")));
                }
            };
            let folder_note = match notes.get(&line.folder) {
                Some(note) => note.clone(),
                None => {
                    let note = processing_folder_note(&conn, &line.folder).map_err(|e| failed("the folder", format!("{e:#}")))?;
                    notes.insert(line.folder.clone(), note.clone());
                    note
                }
            };
            copies.push(CopyLine { line, scan_root, root_kind, folder_note });
        }
        out.push(DuplicateFinding {
            handle: format!("dup:{}:{}", key.hash_type(), g.content_hash),
            key,
            matched_by: matched_by(key).to_string(),
            size: g.size,
            extra_copies: copies.len().saturating_sub(1),
            copies,
        });
    }
    Ok((out, total, extra_bytes))
}

/// What the folder's files say about it, as an inference: at least one master or processed file (a frame flagged
/// `is_master`, or an IMAGETYP of a master kind) and no raw light directly in the folder. Black Hole files are not
/// counted. Subfolders are not the folder.
pub fn processing_folder_note(conn: &Connection, folder: &str) -> Result<Option<String>> {
    if folder.is_empty() {
        return Ok(None);
    }
    let sep = native_separator_of(folder);
    let prefix = format!("{}{}", folder.trim_end_matches(sep), sep);
    let sql = format!(
        "SELECT COUNT(DISTINCT CASE WHEN fr.is_master = 1 OR UPPER(fr.imagetyp) LIKE 'MASTER%' THEN fl.id END), \
                COUNT(DISTINCT CASE WHEN UPPER(fr.imagetyp) = 'LIGHT' AND fr.is_master = 0 THEN fl.id END) \
         FROM files fl JOIN frames fr ON fr.file_id = fl.id \
         WHERE SUBSTR(fl.path, 1, ?1) = ?2 AND INSTR(SUBSTR(fl.path, ?1 + 1), ?3) = 0 AND {}",
        db::file_not_in_black_hole_sql("fl.id"),
    );
    let (masters, raw_lights): (i64, i64) = conn.query_row(
        &sql,
        rusqlite::params![prefix.chars().count() as i64, prefix, sep.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((masters >= 1 && raw_lights == 0).then(|| {
        let held = if masters == 1 { "1 master or processed file".to_string() } else { format!("{masters} masters or processed files") };
        format!("this folder holds {held} and no raw lights; it looks like a processing folder")
    }))
}

// ---- missing metadata ----

/// The Missing Metadata view's categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum MissingCategory {
    /// No usable RA/Dec, on light frames.
    Coordinates,
    /// No OBJECT, on light frames.
    Object,
    /// No observation time.
    Date,
    /// No camera name.
    Camera,
    /// No frame type.
    FrameType,
}

impl MissingCategory {
    pub const ALL: [MissingCategory; 5] = [
        MissingCategory::Coordinates,
        MissingCategory::Object,
        MissingCategory::Date,
        MissingCategory::Camera,
        MissingCategory::FrameType,
    ];

    /// The name the view's query takes.
    pub fn db_name(self) -> &'static str {
        match self {
            MissingCategory::Coordinates => "coordinates",
            MissingCategory::Object => "object",
            MissingCategory::Date => "datetime",
            MissingCategory::Camera => "instrument",
            MissingCategory::FrameType => "frametype",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingFinding {
    /// Files missing each category asked for (a file can be in several).
    pub counts: Vec<(MissingCategory, usize)>,
    /// The files missing any category asked for, by folder: folder, how many, a few of them. Most files first.
    pub by_folder: Vec<(String, usize, Vec<FileLine>)>,
    /// One page of those files, oldest observation first.
    pub page: FilePage,
}

/// Frames of `scope` missing the metadata of `cats` (every category when empty), as the Missing Metadata view
/// finds them.
pub fn find_missing_in_scope(
    conn: &Connection,
    scope: &ResolvedScope,
    cats: &[MissingCategory],
    page: u32,
) -> Result<MissingFinding, ApiError> {
    let mut wanted: Vec<MissingCategory> = Vec::new();
    for c in if cats.is_empty() { &MissingCategory::ALL[..] } else { cats } {
        if !wanted.contains(c) {
            wanted.push(*c);
        }
    }
    let empty_page = FilePage { page, page_count: 0, items: Vec::new() };
    let Some(filter) = scope.filter.as_ref() else {
        return Ok(MissingFinding {
            counts: wanted.into_iter().map(|c| (c, 0)).collect(),
            by_folder: Vec::new(),
            page: empty_page,
        });
    };

    let mut counts = Vec::with_capacity(wanted.len());
    let mut frame_ids: BTreeSet<i64> = BTreeSet::new();
    for cat in &wanted {
        let rows = db::get_frames_with_missing_metadata_in(conn, cat.db_name(), filter)
            .map_err(|e| failed("the missing metadata", e))?;
        counts.push((*cat, rows.len()));
        frame_ids.extend(rows.iter().filter_map(|r| r.frame.id));
    }

    let missing: Vec<ScopedFrame> = fetch_scoped_frames(conn, filter)
        .map_err(|e| failed("the frames in scope", format!("{e:#}")))?
        .into_iter()
        .filter(|sf| sf.file.frame.as_ref().and_then(|f| f.id).is_some_and(|id| frame_ids.contains(&id)))
        .collect();

    let mut folders: BTreeMap<&str, Vec<&ScopedFrame>> = BTreeMap::new();
    for sf in &missing {
        folders.entry(sf.folder.as_str()).or_default().push(sf);
    }
    let mut by_folder: Vec<(String, usize, Vec<FileLine>)> = folders
        .into_iter()
        .map(|(folder, frames)| {
            (folder.to_string(), frames.len(), frames.iter().take(EXAMPLES_PER_GROUP).map(|sf| file_line(sf)).collect())
        })
        .collect();
    by_folder.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let page_count = u32::try_from(missing.len().div_ceil(FILES_PAGE as usize)).unwrap_or(u32::MAX);
    let start = (page as usize).saturating_mul(FILES_PAGE as usize);
    let items = missing.iter().skip(start).take(FILES_PAGE as usize).map(file_line).collect();
    Ok(MissingFinding { counts, by_folder, page: FilePage { page, page_count, items } })
}

#[cfg(test)]
mod tests;
