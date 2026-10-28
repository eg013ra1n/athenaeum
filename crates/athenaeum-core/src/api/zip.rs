//! Zip commands that change the catalog: the permanent unpack of foreign zip-resident frames.
//!
//! `unpack_zip_frames` moves the frames of a frame set or calibration set out of the foreign zips
//! they were cataloged from and onto disk under a chosen folder. The rows stay (same `files.id`, same
//! frame, analysis and plate-solve rows): only the path and the zip markers change. The zip is never
//! written to or deleted; each unpacked entry is recorded in `zip_released_entries` so a later scan of
//! the zip does not catalog it again. Frames that Move and ZIP wrote (`archived_in_operation` set) are
//! restored with Unarchive, not here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError, PathPolicy};
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;
use crate::zip_store::place::{place_entry, record_released_entry, PlaceOutcome};
use crate::zip_store::reader::{entry_crc32_map, is_safe_entry_name, list_raw_entries};

/// The set whose zip-resident frames are unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UnpackTarget {
    /// Every light of the frame set, plus the calibration frames it reaches.
    FrameSet { id: i64 },
    /// The member frames of one calibration set.
    CalibrationSet { id: i64 },
}

/// What an unpack did. Frames that were not moved are listed in `conflicts`, each as `"<what>: <why>"`;
/// their rows are untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UnpackOutcome {
    /// Entries extracted to the target folder.
    pub unpacked: usize,
    /// Frames whose identical file was already at the target; nothing was written.
    pub adopted: usize,
    pub conflicts: Vec<String>,
}

#[derive(Serialize)]
struct UnpackProgress {
    current: usize,
    total: usize,
    message: String,
}

#[derive(Serialize)]
struct UnpackFinished {
    operation_id: i64,
    outcome: &'static str,
    kind: &'static str,
    unpacked: usize,
    adopted: usize,
    conflicts: usize,
}

/// A zip-resident row that is a candidate for unpacking.
struct ZipRow {
    id: i64,
    zip: String,
    entry: String,
    crc: Option<u32>,
    /// Move and ZIP's own row (`archived_in_operation` set): Unarchive handles it.
    ours: bool,
}

fn file_ids_of(conn: &Connection, target: UnpackTarget) -> Result<Vec<i64>, ApiError> {
    let mut ids: Vec<i64> = Vec::new();
    match target {
        UnpackTarget::FrameSet { id } => {
            let exists: Option<i64> =
                conn.query_row("SELECT id FROM frames_set WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
            if exists.is_none() {
                return Err(ApiError::NotFound(format!("frame set {id} not found")));
            }
            let mut seen = std::collections::HashSet::new();
            let lights = crate::archive::planner::collect_light_files(conn, id)?;
            for (file_id, _, _) in lights {
                if seen.insert(file_id) {
                    ids.push(file_id);
                }
            }
            use crate::archive::models::FrameRole;
            for role in [FrameRole::Flat, FrameRole::Dark, FrameRole::Bias, FrameRole::Darkflat] {
                for (file_id, _, _, _) in crate::archive::planner::collect_calibration_files(conn, id, role)? {
                    if seen.insert(file_id) {
                        ids.push(file_id);
                    }
                }
            }
        }
        UnpackTarget::CalibrationSet { id } => {
            let exists: Option<i64> =
                conn.query_row("SELECT id FROM calibration_set WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
            if exists.is_none() {
                return Err(ApiError::NotFound(format!("calibration set {id} not found")));
            }
            let mut stmt = conn.prepare(
                "SELECT DISTINCT fi.id FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id
                 JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 ORDER BY fi.id",
            )?;
            let rows = stmt.query_map(params![id], |r| r.get::<_, i64>(0))?;
            for r in rows {
                ids.push(r?);
            }
        }
    }
    Ok(ids)
}

/// The zip-resident rows among `file_ids`, in the order given (disk rows are not candidates).
fn zip_rows(conn: &Connection, file_ids: &[i64]) -> Result<Vec<ZipRow>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT archive_zip_path, archive_path_in_zip, archive_entry_crc32, archived_in_operation IS NOT NULL
         FROM files WHERE id = ?1 AND archive_zip_path IS NOT NULL AND archive_path_in_zip IS NOT NULL",
    )?;
    let mut out = Vec::new();
    for &id in file_ids {
        let row = stmt
            .query_row(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, bool>(3)?))
            })
            .optional()?;
        if let Some((zip, entry, crc, ours)) = row {
            out.push(ZipRow { id, zip, entry, crc: crc.and_then(|c| u32::try_from(c).ok()), ours });
        }
    }
    Ok(out)
}

/// Checks `dest_dir` against the host's path policy. A folder that does not exist yet is checked through
/// its canonical parent; the placement itself keeps the spelling the caller gave (catalog paths must
/// match what the scanner would produce).
fn check_dest_dir(dest_dir: &Path, policy: &PathPolicy) -> Result<(), ApiError> {
    if !dest_dir.is_absolute() {
        return Err(ApiError::Invalid(format!("{} is not an absolute folder path", dest_dir.display())));
    }
    if dest_dir.exists() {
        if !dest_dir.is_dir() {
            return Err(ApiError::Invalid(format!("{} is not a folder", dest_dir.display())));
        }
        let canonical = dest_dir
            .canonicalize()
            .map_err(|e| ApiError::Invalid(format!("cannot resolve {}: {e}", dest_dir.display())))?;
        policy.check(&canonical)
    } else {
        crate::api::files::resolve_mkdir_target(&dest_dir.to_string_lossy(), policy).map(|_| ())
    }
}

/// Where `entry` goes under `dest_dir`: the entry path, component by component. `None` for an unsafe name
/// or a result that is not under `dest_dir`.
fn dest_for_entry(dest_dir: &Path, entry: &str) -> Option<PathBuf> {
    if !is_safe_entry_name(entry) {
        return None;
    }
    let dest = crate::archive::path_layout::dest_under_root(dest_dir, entry);
    (dest != dest_dir && dest.starts_with(dest_dir)).then_some(dest)
}

/// Creates the folders above `dest` and checks, through symlinks, that they are inside `dest_dir`.
/// `dest_for_entry` only judges the spelling; a symlinked sub-folder that already exists under `dest_dir`
/// would send the write elsewhere. The deepest existing folder is checked before anything is created (so
/// nothing is made outside) and the final parent is checked again.
fn make_parent_inside(dest_dir: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().ok_or_else(|| "refused: no destination folder".to_string())?;
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("cannot create {}: {e}", dest_dir.display()))?;
    let root = dest_dir.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", dest_dir.display()))?;
    let outside = || format!("refused: {} is outside the destination", parent.display());
    let mut existing = parent;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(outside)?;
    }
    let real = existing.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", existing.display()))?;
    if !real.starts_with(&root) {
        return Err(outside());
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let real = parent.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", parent.display()))?;
    if real.starts_with(&root) {
        Ok(())
    } else {
        Err(outside())
    }
}

/// Moves the zip-resident frames of `target` out of their foreign zips into `dest_dir/<entry path>`.
///
/// Per frame: the entry is placed without replacing anything ([`place_entry`]); an identical file already
/// there is adopted, a different one is a conflict and the row stays put. A placed or adopted frame gets
/// one transaction: `files.path` = the new path, `filename`, `modified_at` and `size` from the file, zip
/// markers and CRC cleared, entry recorded as released. Cancel between frames leaves the frames already
/// moved moved. `Err` only for a bad request, a database failure or a cancel.
pub fn unpack_zip_frames(
    ctx: &ServiceContext,
    policy: &PathPolicy,
    target: UnpackTarget,
    dest_dir: &Path,
    emitter: &dyn ProgressEmitter,
    cancel: &AtomicBool,
) -> Result<UnpackOutcome, ApiError> {
    check_dest_dir(dest_dir, policy)?;
    let database = db(ctx)?;
    let conn = database.conn();
    let file_ids = file_ids_of(&conn, target)?;
    let result = unpack_file_ids(&conn, &file_ids, dest_dir, emitter, cancel);
    let (outcome, label) = match &result {
        Ok(o) if o.conflicts.is_empty() => (o.clone(), "completed"),
        Ok(o) => (o.clone(), "completed_with_conflicts"),
        Err(e) => {
            tracing::error!(error = %e, "zip unpack failed");
            (UnpackOutcome::default(), if cancel.load(Ordering::Relaxed) { "cancelled" } else { "failed" })
        }
    };
    emit_event(
        emitter,
        "archive-finished",
        &UnpackFinished {
            operation_id: 0,
            outcome: label,
            kind: "unpack",
            unpacked: outcome.unpacked,
            adopted: outcome.adopted,
            conflicts: outcome.conflicts.len(),
        },
    );
    result
}

/// The unpack proper, over the given catalog rows.
pub(crate) fn unpack_file_ids(
    conn: &Connection,
    file_ids: &[i64],
    dest_dir: &Path,
    emitter: &dyn ProgressEmitter,
    cancel: &AtomicBool,
) -> Result<UnpackOutcome, ApiError> {
    let rows = zip_rows(conn, file_ids)?;
    let total = rows.len();
    let mut outcome = UnpackOutcome::default();

    // One directory read per zip; rows grouped by zip (BTreeMap: stable order).
    let mut by_zip: BTreeMap<String, Vec<ZipRow>> = BTreeMap::new();
    for row in rows {
        by_zip.entry(row.zip.clone()).or_default().push(row);
    }

    let mut done = 0usize;
    let conflict = |outcome: &mut UnpackOutcome, what: &str, why: String| {
        tracing::warn!(zip_entry = %what, error = %why, "zip frame not unpacked");
        outcome.conflicts.push(format!("{what}: {why}"));
    };
    for (zip, rows) in by_zip {
        let zip_path = Path::new(&zip);
        // A zip in the Black Hole stays present but its frames are not moved out until it is restored.
        if crate::db::zip_is_in_black_hole(conn, &zip)? {
            let why = crate::db::zip_in_black_hole_message(&zip);
            tracing::warn!(zip_path = %zip, "zip unpack refused: zip is in the Black Hole");
            for row in &rows {
                conflict(&mut outcome, &row.entry, why.clone());
            }
            done += rows.len();
            continue;
        }
        let crcs = match zip_path.try_exists() {
            Ok(true) => match entry_crc32_map(zip_path) {
                Ok(map) => Some(map),
                Err(e) => {
                    tracing::warn!(zip_path = %zip, error = %format!("{e:#}"), "zip directory unreadable");
                    for row in &rows {
                        conflict(&mut outcome, &row.entry, format!("{zip} cannot be read: {e:#}"));
                    }
                    None
                }
            },
            _ => {
                tracing::warn!(zip_path = %zip, "zip not reachable, its frames stay in it");
                for row in &rows {
                    conflict(&mut outcome, &row.entry, format!("{zip} is not reachable"));
                }
                None
            }
        };
        let Some(crcs) = crcs else {
            done += rows.len();
            continue;
        };

        for row in rows {
            if cancel.load(Ordering::Relaxed) {
                return Err(ApiError::Internal("unpack cancelled".into()));
            }
            done += 1;
            emit_event(
                emitter,
                "zip-unpack-progress",
                &UnpackProgress { current: done, total, message: format!("Unpacking {}/{}", done, total) },
            );
            if row.ours {
                conflict(&mut outcome, &row.entry, "this frame was archived by Move and ZIP — use Unarchive".into());
                continue;
            }
            let Some(dest) = dest_for_entry(dest_dir, &row.entry) else {
                conflict(&mut outcome, &row.entry, "refused: the entry name is not safe to unpack".into());
                continue;
            };
            let Some(&zip_crc) = crcs.get(&row.entry) else {
                conflict(&mut outcome, &row.entry, format!("not found in {zip}"));
                continue;
            };
            let dest_str = dest.to_string_lossy().into_owned();
            let owner: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?1 AND id != ?2",
                    params![dest_str, row.id],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.is_some() {
                conflict(&mut outcome, &row.entry, format!("another catalog frame owns {dest_str}"));
                continue;
            }

            if let Err(why) = make_parent_inside(dest_dir, &dest) {
                conflict(&mut outcome, &row.entry, why);
                continue;
            }

            let placed = match place_entry(zip_path, &row.entry, Some(row.crc.unwrap_or(zip_crc)), &dest, cancel) {
                Ok(p) => p,
                Err(e) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(ApiError::Internal("unpack cancelled".into()));
                    }
                    conflict(&mut outcome, &row.entry, format!("{e:#}"));
                    continue;
                }
            };
            let we_placed = placed == PlaceOutcome::Placed;
            if let PlaceOutcome::Conflict(reason) = placed {
                conflict(&mut outcome, &row.entry, reason);
                continue;
            }

            match repoint_row(conn, &row, &dest) {
                Ok(true) => {
                    if we_placed {
                        outcome.unpacked += 1;
                    } else {
                        outcome.adopted += 1;
                    }
                }
                Ok(false) => {
                    remove_placed(&dest, we_placed);
                    conflict(&mut outcome, &row.entry, "the catalog row changed while it was being unpacked".into());
                }
                Err(e) => {
                    tracing::error!(file_id = row.id, zip_path = %zip, zip_entry = %row.entry, error = %format!("{e:#}"), "could not move the catalog row to the unpacked file");
                    remove_placed(&dest, we_placed);
                    conflict(&mut outcome, &row.entry, format!("the catalog could not be updated: {e:#}"));
                }
            }
        }
    }
    tracing::info!(
        unpacked = outcome.unpacked,
        adopted = outcome.adopted,
        conflicts = outcome.conflicts.len(),
        "zip frames unpacked"
    );
    Ok(outcome)
}

/// Takes back a file this call put on disk when its row could not follow.
fn remove_placed(dest: &Path, we_placed: bool) {
    if we_placed {
        if let Err(e) = std::fs::remove_file(dest) {
            tracing::warn!(path = %dest.display(), error = %e, "could not remove the unpacked file after a failed catalog update");
        }
    }
}

/// One transaction: the row becomes a disk row at `dest`. `false` when the row no longer has the zip
/// markers it was read with.
fn repoint_row(conn: &Connection, row: &ZipRow, dest: &Path) -> anyhow::Result<bool> {
    use anyhow::Context;
    let meta = std::fs::metadata(dest).with_context(|| format!("stat {}", dest.display()))?;
    let modified = chrono::DateTime::<chrono::Utc>::from(
        meta.modified().with_context(|| format!("modified time of {}", dest.display()))?,
    )
    .to_rfc3339();
    let filename = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE files SET path = ?1, filename = ?2, modified_at = ?3, size = ?4,
                archive_zip_path = NULL, archive_path_in_zip = NULL, archive_entry_crc32 = NULL
         WHERE id = ?5 AND archive_zip_path = ?6 AND archive_path_in_zip = ?7 AND archived_in_operation IS NULL",
        params![dest.to_string_lossy(), filename, modified, meta.len() as i64, row.id, row.zip, row.entry],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    record_released_entry(&tx, &row.zip, &row.entry, None)?;
    tx.commit()?;
    Ok(true)
}

/// One level of a zip, for the browser's zip view.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ZipDirListing {
    /// Sub-folders of the listed folder, each as the full entry path of the folder (`/`-separated, no leading
    /// or trailing `/`): pass one back as the inner folder to open it.
    pub subdirectories: Vec<String>,
    /// Cataloged frames at this level, with their frame metadata.
    pub files: Vec<crate::models::FileWithFrame>,
    /// Entries at this level the catalog does not hold as frames (other files, names the scan skipped,
    /// names that are unsafe to unpack). They are listed by name and size only; nothing opens them.
    pub other_entries: Vec<ZipOtherEntry>,
}

/// An entry of a zip that is not a cataloged frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ZipOtherEntry {
    /// The name at this level (for an unsafe name that has no level, the whole stored name, at the top).
    pub name: String,
    pub size: i64,
}

/// `true` for a stored name that cannot be placed in a tree: absolute, or with a `..` component.
fn is_untreeable_name(name: &str) -> bool {
    name.starts_with('/') || name.starts_with('\\') || name.split(['/', '\\']).any(|c| c == "..")
}

/// Lists one level of `zip_path` under `inner_dir` (`""` is the top).
///
/// The tree comes from the zip's central directory, read live and unfiltered ([`list_raw_entries`]), so the
/// names our own Move and ZIP writes (`:`, trailing space, device names) appear like any other. Catalog rows
/// are joined by the entry they were cataloged under (`archive_zip_path` + `archive_path_in_zip`, ours and
/// foreign) or by the virtual path under the zip. A zip is a whole file in the browser: this reads the
/// directory and never opens or modifies an entry.
pub fn list_zip_entries(
    ctx: &ServiceContext,
    policy: &PathPolicy,
    zip_path: &str,
    inner_dir: &str,
) -> Result<ZipDirListing, ApiError> {
    let zip = Path::new(zip_path);
    let candidate = zip.canonicalize().unwrap_or_else(|_| zip.to_path_buf());
    policy.check(&candidate)?;

    // The zip is read before the catalog lock is taken: a share can be slow.
    let raw = match list_raw_entries(zip) {
        Ok(raw) => raw,
        Err(e) if !zip.exists() => {
            tracing::warn!(zip_path, error = %e, "zip listing: the zip is not reachable");
            return Err(ApiError::NotFound(format!("{zip_path} is not reachable")));
        }
        Err(e) => {
            tracing::error!(zip_path, error = %e, "zip listing failed");
            return Err(ApiError::Internal(format!("Could not read {zip_path}: {e:#}")));
        }
    };
    let rows = {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::zip_store::catalog::rows_of_zip(&conn, zip_path).map_err(|e| {
            tracing::error!(zip_path, error = %e, "zip listing: catalog read failed");
            ApiError::Internal(format!("Could not read the catalog rows of {zip_path}: {e:#}"))
        })?
    };

    // Entry path -> row.
    let virtual_prefix = format!("{zip_path}{}", std::path::MAIN_SEPARATOR);
    let mut by_entry: BTreeMap<String, crate::models::FileWithFrame> = BTreeMap::new();
    for (file, frame) in rows {
        let entry = match (file.archive_zip_path.as_deref(), file.archive_path_in_zip.as_deref()) {
            (Some(z), Some(e)) if z == zip_path => e.to_string(),
            _ => match file.path.strip_prefix(&virtual_prefix) {
                Some(rest) => rest.replace(std::path::MAIN_SEPARATOR, "/"),
                None => continue,
            },
        };
        by_entry.entry(entry).or_insert(crate::models::FileWithFrame { file, frame });
    }

    let inner = inner_dir.replace('\\', "/").split('/').filter(|c| !c.is_empty()).collect::<Vec<_>>().join("/");
    let prefix = if inner.is_empty() { String::new() } else { format!("{inner}/") };

    let mut subdirectories: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    let mut other_entries = Vec::new();
    let to_i64 = |size: u64| i64::try_from(size).unwrap_or(i64::MAX);
    for (name, size) in raw {
        if is_untreeable_name(&name) {
            if inner.is_empty() {
                other_entries.push(ZipOtherEntry { name, size: to_i64(size) });
            }
            continue;
        }
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((first, _)) if !first.is_empty() => {
                subdirectories.insert(format!("{prefix}{first}"));
            }
            // "a//b": an empty folder name has no folder to open.
            Some(_) => other_entries.push(ZipOtherEntry { name: rest.to_string(), size: to_i64(size) }),
            None => match by_entry.remove(&name) {
                Some(row) => files.push(row),
                None => other_entries.push(ZipOtherEntry { name: rest.to_string(), size: to_i64(size) }),
            },
        }
    }
    files.sort_by(|a, b| a.file.filename.cmp(&b.file.filename));
    other_entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ZipDirListing { subdirectories: subdirectories.into_iter().collect(), files, other_entries })
}

/// The extraction cache bound to the catalog's directory (a host that did not bind it yet), with the
/// configured size limit read into it. A failed settings read is logged and the previous limit stays.
fn zip_cache_ready(ctx: &ServiceContext) -> Result<&crate::zip_store::cache::ZipCache, ApiError> {
    let database = db(ctx)?;
    if let Some(app_data) = database.path().parent() {
        ctx.zip_cache.bind(app_data);
    }
    let conn = database.conn();
    match ctx.settings.get_archive_zip_cache_limit_bytes(&conn) {
        Ok(limit) => ctx.zip_cache.set_limit_bytes(limit),
        Err(e) => tracing::warn!(error = %e, "could not read the zip cache limit; keeping the previous one"),
    }
    Ok(&ctx.zip_cache)
}

/// Size, entry count and limit of the zip extraction cache, for Settings → General → Archive.
pub fn get_zip_cache_status(ctx: &ServiceContext) -> Result<crate::zip_store::cache::ZipCacheStatus, ApiError> {
    let cache = zip_cache_ready(ctx)?;
    cache.status().map_err(|e| {
        tracing::error!(error = %e, "zip cache status failed");
        ApiError::from(e)
    })
}

/// Removes every cached extraction nothing is using, then returns the new status.
pub fn clear_zip_cache(ctx: &ServiceContext) -> Result<crate::zip_store::cache::ZipCacheStatus, ApiError> {
    let cache = zip_cache_ready(ctx)?;
    cache.clear_unpinned().map_err(|e| {
        tracing::error!(error = %e, "zip cache clear failed");
        ApiError::from(e)
    })?;
    cache.status().map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use crate::zip_store::scan::scan_zip;
    use crate::zip_store::scan::test_support::{light, make_zip};
    use tempfile::TempDir;

    fn catalog(root: &Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()]).unwrap();
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn file_id(conn: &Connection, entry_suffix: &str) -> i64 {
        conn.query_row(
            "SELECT id FROM files WHERE archive_path_in_zip = ?1 OR path LIKE ?2",
            params![entry_suffix, format!("%{entry_suffix}")],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Scans a zip of two lights into the catalog and gives the rows frame-set membership.
    fn two_light_zip(tmp: &Path) -> (Connection, PathBuf, Vec<i64>) {
        let conn = catalog(tmp);
        let zip = make_zip(
            tmp,
            "night.zip",
            &[("Lights/L_001.fits", light(1)), ("Lights/L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &zip, 1, false).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (10, 1, '2025-10-12', '2025-10-13')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for id in &ids {
            let frame: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", [id], |r| r.get(0)).unwrap();
            conn.execute("UPDATE frames SET imagetyp = 'Light' WHERE id = ?1", [frame]).unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)", [frame]).unwrap();
        }
        (conn, zip, ids)
    }

    fn run(conn: &Connection, ids: &[i64], dest: &Path) -> UnpackOutcome {
        unpack_file_ids(conn, ids, dest, &NullEmitter, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn unpack_refuses_a_zip_in_the_black_hole_until_it_is_restored() {
        let tmp = TempDir::new().unwrap();
        let (conn, zip, ids) = two_light_zip(tmp.path());
        let zip_s = zip.to_string_lossy().into_owned();
        let bh = crate::db::add_zip_to_black_hole(&conn, &zip_s, "files").unwrap();
        let out = TempDir::new().unwrap();

        let refused = run(&conn, &ids, out.path());
        assert_eq!(refused.unpacked, 0);
        assert_eq!(refused.conflicts.len(), 2);
        assert!(
            refused.conflicts.iter().all(|c| c.ends_with("night.zip is in the Black Hole \u{2014} restore it first")),
            "{:?}",
            refused.conflicts
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 2);
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0, "nothing was written");

        crate::db::restore_zip_from_black_hole(&conn, bh).unwrap();
        let ok = run(&conn, &ids, out.path());
        assert_eq!(ok.unpacked, 2);
    }

    #[test]
    fn unpack_moves_rows_to_disk_with_metadata_intact() {
        let tmp = TempDir::new().unwrap();
        let (conn, zip, ids) = two_light_zip(tmp.path());
        let out = TempDir::new().unwrap();
        let before: Vec<(i64, i64)> = ids
            .iter()
            .map(|id| (*id, conn.query_row("SELECT id FROM frames WHERE file_id = ?1", [id], |r| r.get(0)).unwrap()))
            .collect();
        let headers = count(&conn, "SELECT COUNT(*) FROM fits_header");

        // Through the public entry point: the frame set resolves to its two lights.
        let ctx_free = file_ids_of(&conn, UnpackTarget::FrameSet { id: 1 }).unwrap();
        assert_eq!(ctx_free.len(), 2);
        let outcome = run(&conn, &ctx_free, out.path());
        assert_eq!(outcome, UnpackOutcome { unpacked: 2, adopted: 0, conflicts: vec![] });

        let l1 = out.path().join("Lights").join("L_001.fits");
        assert_eq!(std::fs::read(&l1).unwrap(), light(1));
        for (id, frame) in before {
            let (path, name, size, zip_col, entry, crc): (String, String, i64, Option<String>, Option<String>, Option<i64>) = conn
                .query_row(
                    "SELECT path, filename, size, archive_zip_path, archive_path_in_zip, archive_entry_crc32 FROM files WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .unwrap();
            assert!(path.starts_with(out.path().to_str().unwrap()), "{path}");
            assert_eq!(std::fs::metadata(&path).unwrap().len() as i64, size);
            assert!(name.starts_with("L_00"));
            assert!(zip_col.is_none() && entry.is_none() && crc.is_none(), "zip markers cleared");
            let same_frame: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", [id], |r| r.get(0)).unwrap();
            assert_eq!(same_frame, frame, "the frame row is the same row");
        }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM session_members"), 2, "set membership survives");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM fits_header"), headers, "header rows survive");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 2);
        assert!(zip.is_file(), "the zip is never touched");

        // A rescan of the zip adds nothing and takes nothing back.
        let rescan = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(rescan.inserted.is_empty(), "released entries are not cataloged again");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 0);
    }

    #[test]
    fn unpack_adopts_an_identical_existing_file_and_reports_a_different_one_as_conflict() {
        let tmp = TempDir::new().unwrap();
        let (conn, _zip, ids) = two_light_zip(tmp.path());
        let out = TempDir::new().unwrap();
        std::fs::create_dir_all(out.path().join("Lights")).unwrap();
        std::fs::write(out.path().join("Lights/L_001.fits"), light(1)).unwrap();
        std::fs::write(out.path().join("Lights/L_002.fits"), b"a different capture").unwrap();

        let outcome = run(&conn, &ids, out.path());
        assert_eq!(outcome.unpacked, 0);
        assert_eq!(outcome.adopted, 1);
        assert_eq!(outcome.conflicts.len(), 1, "{:?}", outcome.conflicts);
        assert!(outcome.conflicts[0].contains("L_002.fits"));
        assert_eq!(std::fs::read(out.path().join("Lights/L_002.fits")).unwrap(), b"a different capture");

        let adopted_path: String = conn
            .query_row("SELECT path FROM files WHERE id = ?1", [file_id(&conn, "L_001.fits")], |r| r.get(0))
            .unwrap();
        assert_eq!(Path::new(&adopted_path), out.path().join("Lights").join("L_001.fits"));
        let untouched: (Option<String>, String) = conn
            .query_row(
                "SELECT archive_zip_path, path FROM files WHERE archive_path_in_zip = 'Lights/L_002.fits'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(untouched.0.is_some(), "the conflicted row stays zip-resident");
        assert!(untouched.1.contains("night.zip"));
    }

    #[test]
    fn unpack_never_writes_outside_the_target() {
        let tmp = TempDir::new().unwrap();
        let (conn, zip, ids) = two_light_zip(tmp.path());
        // Force hostile entry names into the rows (the zip itself has no such entry: they must be refused
        // by name before anything is looked up).
        conn.execute("UPDATE files SET archive_path_in_zip = '../evil.fits' WHERE id = ?1", [ids[0]]).unwrap();
        conn.execute("UPDATE files SET archive_path_in_zip = '/abs/evil.fits' WHERE id = ?1", [ids[1]]).unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("target");
        std::fs::create_dir_all(&out).unwrap();

        let outcome = run(&conn, &ids, &out);
        assert_eq!(outcome.unpacked + outcome.adopted, 0);
        assert_eq!(outcome.conflicts.len(), 2);
        assert!(outcome.conflicts.iter().all(|c| c.contains("not safe")), "{:?}", outcome.conflicts);
        assert!(!base.path().join("evil.fits").exists());
        assert!(!Path::new("/abs/evil.fits").exists());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
        assert!(zip.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_subfolder_pointing_outside_refuses_that_entry() {
        let tmp = TempDir::new().unwrap();
        let (conn, _zip, ids) = two_light_zip(tmp.path());
        let base = TempDir::new().unwrap();
        let out = base.path().join("target");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, out.join("Lights")).unwrap();

        let outcome = run(&conn, &ids, &out);
        assert_eq!(outcome.unpacked + outcome.adopted, 0);
        assert_eq!(outcome.conflicts.len(), 2);
        assert!(outcome.conflicts.iter().all(|c| c.contains("outside the destination")), "{:?}", outcome.conflicts);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing is written outside");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 2);
    }

    #[test]
    fn our_own_rows_are_left_for_unarchive() {
        let tmp = TempDir::new().unwrap();
        let (conn, _zip, ids) = two_light_zip(tmp.path());
        // Mark one row as Move and ZIP's own.
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        conn.execute("UPDATE files SET archived_in_operation = 5 WHERE id = ?1", [ids[0]]).unwrap();
        let out = TempDir::new().unwrap();
        let outcome = run(&conn, &ids, out.path());
        assert_eq!(outcome.unpacked, 1);
        assert_eq!(outcome.conflicts.len(), 1);
        assert!(outcome.conflicts[0].contains("Unarchive"), "{:?}", outcome.conflicts);
        let still: Option<String> = conn
            .query_row("SELECT archive_zip_path FROM files WHERE id = ?1", [ids[0]], |r| r.get(0))
            .unwrap();
        assert!(still.is_some());
    }

    #[test]
    fn a_missing_zip_leaves_every_row_and_reports_it() {
        let tmp = TempDir::new().unwrap();
        let (conn, zip, ids) = two_light_zip(tmp.path());
        std::fs::remove_file(&zip).unwrap();
        let out = TempDir::new().unwrap();
        let outcome = run(&conn, &ids, out.path());
        assert_eq!(outcome.unpacked, 0);
        assert_eq!(outcome.conflicts.len(), 2);
        assert!(outcome.conflicts.iter().all(|c| c.contains("not reachable")));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 2);
    }

    #[test]
    fn a_cancel_between_frames_keeps_the_moved_rows_moved() {
        let tmp = TempDir::new().unwrap();
        let (conn, _zip, ids) = two_light_zip(tmp.path());
        let out = TempDir::new().unwrap();

        struct CancelAfterFirst<'a> {
            cancel: &'a AtomicBool,
        }
        impl ProgressEmitter for CancelAfterFirst<'_> {
            fn emit_json(&self, name: &str, payload: serde_json::Value) {
                // The second frame's progress event arrives after the first was fully moved.
                if name == "zip-unpack-progress" && payload["current"] == 2 {
                    self.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        let cancel = AtomicBool::new(false);
        let err = unpack_file_ids(&conn, &ids, out.path(), &CancelAfterFirst { cancel: &cancel }, &cancel).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NULL"), 1, "the first stays moved");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 1);
    }

    #[test]
    fn the_destination_folder_is_checked_by_the_path_policy() {
        let allowed = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let policy = PathPolicy::AllowedRoots(vec![crate::test_support::canonical_path(allowed.path())]);
        assert!(check_dest_dir(allowed.path(), &policy).is_ok());
        assert!(check_dest_dir(&allowed.path().join("new/sub"), &policy).is_err(), "parent of a new folder must exist");
        assert!(check_dest_dir(&allowed.path().join("new"), &policy).is_ok());
        assert!(matches!(check_dest_dir(elsewhere.path(), &policy), Err(ApiError::Forbidden(_))));
        assert!(matches!(check_dest_dir(Path::new("relative/dir"), &PathPolicy::AllowAll), Err(ApiError::Invalid(_))));
    }

    /// A catalog with a scan root and one zip scanned into it; the context owns the catalog file.
    fn listing_fixture(entries: &[(&str, Vec<u8>)]) -> (TempDir, ServiceContext, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let zip = make_zip(&root, "night.zip", entries, ArchiveCompression::Store);
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()]).unwrap();
            scan_zip(&conn, &zip, 1, false).unwrap();
        }
        (tmp, ctx, zip)
    }

    #[test]
    fn list_zip_entries_returns_one_level_with_catalog_metadata() {
        let (_tmp, ctx, zip) = listing_fixture(&[
            ("Lights/L_001.fits", light(1)),
            ("Lights/L_002.fits", light(2)),
            ("Lights/Ha/L_003.fits", light(3)),
            ("notes.txt", b"hello".to_vec()),
            ("top.fits", light(4)),
        ]);
        let zip_s = zip.to_string_lossy().into_owned();

        let top = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "").unwrap();
        assert_eq!(top.subdirectories, vec!["Lights".to_string()]);
        let names: Vec<_> = top.files.iter().map(|f| f.file.filename.as_str()).collect();
        assert_eq!(names, vec!["top.fits"]);
        assert_eq!(top.other_entries, vec![ZipOtherEntry { name: "notes.txt".into(), size: 5 }]);

        let lights = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "Lights").unwrap();
        assert_eq!(lights.subdirectories, vec!["Lights/Ha".to_string()]);
        let names: Vec<_> = lights.files.iter().map(|f| f.file.filename.as_str()).collect();
        assert_eq!(names, vec!["L_001.fits", "L_002.fits"]);
        assert!(lights.files.iter().all(|f| f.frame.is_some() && f.file.archive_zip_path.as_deref() == Some(zip_s.as_str())));
        assert!(lights.other_entries.is_empty());

        // Slashes at either end and a backslash spelling name the same folder.
        let ha = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "/Lights\\Ha/").unwrap();
        assert_eq!(ha.files.len(), 1);
        assert_eq!(ha.files[0].file.filename, "L_003.fits");

        // Nothing in a folder that does not exist.
        let none = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "Darks").unwrap();
        assert!(none.subdirectories.is_empty() && none.files.is_empty() && none.other_entries.is_empty());
    }

    #[test]
    fn list_zip_entries_shows_the_names_our_layout_writes_and_never_opens_unsafe_ones() {
        let (_tmp, ctx, zip) = listing_fixture(&[
            ("Lights/ok.fits", light(1)),
            ("Lights/M31: Core.fits", light(2)),
            ("Lights/trailing .fits ", light(3)),
            ("Lights/Aux.fits", light(4)),
            ("../escape.fits", light(5)),
            ("/absolute.fits", light(6)),
        ]);
        let zip_s = zip.to_string_lossy().into_owned();
        // The scan cataloged only the safe name; the others are not frames of the catalog.
        let lights = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "Lights").unwrap();
        let files: Vec<_> = lights.files.iter().map(|f| f.file.filename.as_str()).collect();
        assert_eq!(files, vec!["ok.fits"]);
        let others: Vec<_> = lights.other_entries.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(others, vec!["Aux.fits", "M31: Core.fits", "trailing .fits "], "{others:?}");

        // Our own layout's rows with such names are listed as cataloged files: mark one the way Move and ZIP does.
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, archived_in_operation, archive_zip_path, archive_path_in_zip)
                 VALUES ('/orig/M31: Core.fits', 'M31: Core.fits', 1, '2026-01-01T00:00:00Z', 'FITS', 9, ?1, 'Lights/M31: Core.fits')",
                [&zip_s],
            )
            .unwrap();
        }
        let lights = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "Lights").unwrap();
        let files: Vec<_> = lights.files.iter().map(|f| f.file.filename.as_str()).collect();
        assert_eq!(files, vec!["M31: Core.fits", "ok.fits"]);
        let others: Vec<_> = lights.other_entries.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(others, vec!["Aux.fits", "trailing .fits "]);

        // Names that cannot sit in a tree appear once, at the top, and open nothing.
        let top = list_zip_entries(&ctx, &PathPolicy::AllowAll, &zip_s, "").unwrap();
        let top_others: Vec<_> = top.other_entries.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(top_others, vec!["../escape.fits", "/absolute.fits"]);
        assert_eq!(top.subdirectories, vec!["Lights".to_string()]);
    }

    #[test]
    fn cache_status_and_clear_work_from_the_catalog_directory() {
        let (_tmp, ctx, _zip) = listing_fixture(&[("a.fits", light(1))]);
        let status = get_zip_cache_status(&ctx).unwrap();
        assert_eq!(status.bytes, 0);
        assert_eq!(status.entries, 0);
        assert_eq!(status.limit_bytes, 32 * 1024 * 1024 * 1024);
        let after = clear_zip_cache(&ctx).unwrap();
        assert_eq!(after.bytes, 0);
    }

    #[test]
    fn zip_listing_carries_the_creating_operation() {
        let (_tmp, ctx, zip) = listing_fixture(&[("a.fits", light(1))]);
        let database = ctx.db.get().unwrap();
        let conn = database.conn();
        let dir = zip.parent().unwrap().to_string_lossy().into_owned();
        let zips = crate::zip_store::catalog::zips_in_directory(&conn, &dir, None).unwrap();
        assert_eq!(zips.len(), 1);
        assert_eq!(zips[0].created_by_operation, None, "a foreign zip has no creating operation");
    }

    #[test]
    fn list_zip_entries_checks_the_path_policy_and_the_zip() {
        let (tmp, ctx, zip) = listing_fixture(&[("a.fits", light(1))]);
        let zip_s = zip.to_string_lossy().into_owned();
        let elsewhere = TempDir::new().unwrap();
        let policy = PathPolicy::AllowedRoots(vec![crate::test_support::canonical_path(elsewhere.path())]);
        assert!(matches!(list_zip_entries(&ctx, &policy, &zip_s, ""), Err(ApiError::Forbidden(_))));
        let missing = tmp.path().join("astro").join("gone.zip");
        assert!(matches!(
            list_zip_entries(&ctx, &PathPolicy::AllowAll, &missing.to_string_lossy(), ""),
            Err(ApiError::NotFound(_))
        ));
    }
}
