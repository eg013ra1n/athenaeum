//! The scanner's zip pass: catalog the FITS/XISF entries of a `.zip` from its directory and the
//! first bytes of each entry, never reading a whole entry.
//!
//! Order per zip (see [`scan_zip`]): stat (equal to the `zip_archives` row → skip without opening),
//! directory, then the entries not yet owned by a row, through one 4 KiB-buffered header pass. A new
//! entry either takes over a catalog row that moved into the zip ([`find_moved_row`]) or becomes a
//! new row with a virtual path. Rows of entries that vanished from a changed zip are left alone:
//! nothing here deletes a frame row.
//!
//! A zip that was moved or renamed outside the app keeps its identity: when its entries are adopted
//! from a zip that is gone, the gone zip's `zip_archives` row (created-by operation, released
//! entries) moves to the new path and the old row is dropped once no row points at it.

use crate::db::{insert_file, insert_fits_header, insert_frame, SavepointGuard};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_parser::{calibrated_light_identity, parse_fits_header_bytes, parse_xisf_header_bytes};
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::scanner::{path_to_utf8, path_under_unavailable_scan_root};
use crate::zip_store::reader::{self, ZipEntryInfo};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// What one [`scan_zip`] did.
#[derive(Debug, Clone, Default)]
pub struct ZipScanOutcome {
    /// `zip_archives.id` of the zip.
    pub zip_id: i64,
    /// The zip's stat equals its `zip_archives` row: nothing was opened.
    pub skipped_unchanged: bool,
    /// Frame ids of rows this scan inserted, with their image type (rows with no IMAGETYP are not listed).
    pub inserted: Vec<(i64, ImageType)>,
    /// Catalog rows that took over a moved frame instead of being inserted.
    pub adopted: usize,
    /// Known entries re-parsed in place because their CRC changed.
    pub updated: usize,
    /// Bytes pulled from the zip file (directory + entry headers).
    pub bytes_read: u64,
    /// Rows inserted, whether or not the frame has an image type.
    pub files_inserted: usize,
    /// Entries that could not be read or parsed (`"<virtual path>: <why>"`). The rest of the zip is still cataloged.
    pub errors: Vec<String>,
}

/// The catalog path of a foreign zip-resident entry: the zip path, then the entry path with `/`
/// turned into the platform separator.
pub fn virtual_path(zip: &Path, entry: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let mut out = zip.to_string_lossy().into_owned();
    out.push(sep);
    out.extend(entry.chars().map(|c| if c == '/' { sep } else { c }));
    out
}

/// Whether the row found by fingerprint has really moved away from `old_location` (the row's file,
/// or for a zip-resident row its zip): `false` when the old location is still there (the new file
/// is a duplicate) or sits under an unavailable scan root (an unmounted volume).
fn old_location_is_gone(conn: &Connection, file_id: i64, old_path: &str, old_location: &str, new_path: &str) -> Result<bool> {
    if Path::new(old_location).exists() {
        tracing::debug!(
            old_path = %old_path,
            path = %new_path,
            file_id,
            "duplicate file detected (identical header)"
        );
        return Ok(false);
    }
    if let Some(offline_root) = path_under_unavailable_scan_root(conn, old_location)? {
        // The old location is missing only because its scan root is offline (unmounted volume): not a
        // move. Falling through keeps the still-valid original untouched.
        tracing::warn!(
            old_path = %old_path,
            path = %new_path,
            offline_root = %offline_root,
            file_id,
            "skipping move-detection: old path's scan root is unavailable"
        );
        return Ok(false);
    }
    Ok(true)
}

/// The row a new file at `new_path` with this header fingerprint should take over, if any: the
/// scanner's moved-file rule, shared by the disk walkers (`for_zip_entry = false`) and the zip pass
/// (`true`).
///
/// A row with the fingerprint at another path. A disk file never matches a zip-resident row
/// (`archive_zip_path IS NULL`), so a manually unzipped copy of an archived frame is a new row, not
/// a takeover; for a disk file the first row wins, as it always did. The row is a move only when its
/// old location is gone: for a disk row its file, for a zip-resident row (zip entries only) its
/// zip. If the old location is still there the new file is a duplicate; if it sits under an
/// unavailable scan root (unmounted volume) it is not a move either. Both give `None`.
///
/// A zip entry looks at every row with the fingerprint, in id order, and takes the first one whose
/// old location is gone: a backup copy that is still present must not hide the row of the zip that
/// was just renamed.
///
/// Returns the row's id and its `files.path`.
pub(crate) fn find_moved_row(
    conn: &Connection,
    fingerprint: &str,
    new_path: &str,
    for_zip_entry: bool,
) -> Result<Option<(i64, String)>> {
    if for_zip_entry {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.path, f.archive_zip_path FROM files f
             INNER JOIN fits_header fh ON f.id = fh.file_id
             WHERE fh.header_fingerprint = ?1 AND f.path != ?2
             ORDER BY f.id",
        )?;
        let candidates = stmt
            .query_map(params![fingerprint, new_path], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (file_id, old_path, row_zip) in candidates {
            let old_location = row_zip.unwrap_or_else(|| old_path.clone());
            if old_location_is_gone(conn, file_id, &old_path, &old_location, new_path)? {
                return Ok(Some((file_id, old_path)));
            }
        }
        return Ok(None);
    }

    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT f.id, f.path FROM files f
             INNER JOIN fits_header fh ON f.id = fh.file_id
             WHERE fh.header_fingerprint = ?1 AND f.path != ?2 AND f.archive_zip_path IS NULL",
            params![fingerprint, new_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((file_id, old_path)) = found else {
        return Ok(None);
    };
    if old_location_is_gone(conn, file_id, &old_path, &old_path, new_path)? {
        Ok(Some((file_id, old_path)))
    } else {
        Ok(None)
    }
}

/// What [`reconcile_hand_restored_row`] found at a disk path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HandRestore {
    /// The row at this path is not a zip-resident row of Move and ZIP: the caller carries on as usual.
    NotApplicable,
    /// The frame was unzipped by hand to its original path: the row is a disk row again.
    BecameDiskRow,
    /// The file at the original path of an archived frame is not that frame's bytes. The archived
    /// row now carries its virtual path (`<zip>/<entry>`), so the path is free: the caller catalogs
    /// the disk file as a new file.
    Differs,
}

/// CRC32 of a whole file, read in 1 MiB chunks.
pub(crate) fn file_crc32(path: &Path) -> Result<u32> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Whether the file at `path` holds exactly the bytes of the zip entry the row stands for: the
/// size equals `files.size` (our rows keep their original size) and the CRC32 equals the one
/// recorded for the row, or the one in the zip's directory when none is recorded. A zip that
/// cannot be reached gives `false`: nothing is adopted that cannot be verified.
fn disk_file_is_the_entry(
    path: &Path,
    disk_size: u64,
    row_size: i64,
    row_crc: Option<i64>,
    zip_path: &str,
    entry: &str,
) -> bool {
    if i64::try_from(disk_size).ok() != Some(row_size) {
        return false;
    }
    let expected = match row_crc {
        Some(crc) => u32::try_from(crc).ok(),
        None => match reader::entry_crc32(Path::new(zip_path), entry) {
            Ok(crc) => crc,
            Err(e) => {
                tracing::warn!(zip_path = %zip_path, zip_entry = %entry, error = %format!("{e:#}"), "zip directory unreadable — the file at the archived frame's path is not adopted");
                None
            }
        },
    };
    let Some(expected) = expected else {
        return false;
    };
    match file_crc32(path) {
        Ok(actual) => actual == expected,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "could not checksum the file at an archived frame's path — not adopted");
            false
        }
    }
}

/// A disk file turned up at the path (`files.path`, the restore target) of a zip-resident row that
/// Move and ZIP wrote (`archived_in_operation` set), i.e. the user may have unzipped the frame by hand.
///
/// The file is adopted only when its bytes are the entry's: same size as the row, same CRC32 as the
/// row's recorded one (or the zip directory's when the row has none). Then the row becomes a disk
/// row: all four zip columns (zip path, entry, CRC, operation) cleared, size and mtime from the
/// file, the entry recorded in `zip_released_entries` (the zip gets a placeholder `zip_archives`
/// row first when it has none) so a later scan of the zip does not add it again; metadata, ids
/// and every junction row stay.
///
/// Anything else (a different frame, a half-written copy, an unverifiable one) is
/// [`HandRestore::Differs`]: in one savepoint the archived row's `files.path` becomes its virtual
/// path, which frees the path for the new file. The archive operation still restores the frame to
/// its `source_path`.
pub(crate) fn reconcile_hand_restored_row(conn: &Connection, file_id: i64, path: &Path) -> Result<HandRestore> {
    let row: Option<(Option<String>, Option<String>, Option<i64>, i64, Option<i64>)> = conn
        .query_row(
            "SELECT archive_zip_path, archive_path_in_zip, archived_in_operation, size, archive_entry_crc32
             FROM files WHERE id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((Some(zip_path), Some(entry), Some(operation), row_size, row_crc)) = row else {
        return Ok(HandRestore::NotApplicable);
    };

    let metadata = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;

    if !disk_file_is_the_entry(path, metadata.len(), row_size, row_crc, &zip_path, &entry) {
        let vpath = virtual_path(Path::new(&zip_path), &entry);
        tracing::warn!(
            file_id,
            path = %path.display(),
            zip_path = %zip_path,
            "the file at the original path of an archived frame is not that frame — the archived row moves to its zip path and the file is cataloged as new"
        );
        let sp = SavepointGuard::new(conn, "scan_hand_restore_differs")?;
        conn.execute("UPDATE files SET path = ?1 WHERE id = ?2", params![vpath, file_id])?;
        sp.commit()?;
        return Ok(HandRestore::Differs);
    }

    let modified: DateTime<Utc> = DateTime::from(metadata.modified().with_context(|| format!("mtime of {}", path.display()))?);
    let sp = SavepointGuard::new(conn, "scan_hand_restored")?;
    conn.execute(
        "UPDATE files SET archive_zip_path = NULL, archive_path_in_zip = NULL, archive_entry_crc32 = NULL,
                archived_in_operation = NULL, size = ?1, modified_at = ?2 WHERE id = ?3",
        params![metadata.len() as i64, modified.to_rfc3339(), file_id],
    )?;
    // The zip may have no row yet (Move and ZIP writes it only at its end): a placeholder keeps the
    // release, with the stat left empty so the next scan reads the zip.
    conn.execute(
        "INSERT OR IGNORE INTO zip_archives (path, size, modified_at, frame_entries, scanned_at, created_by_operation)
         VALUES (?1, 0, '', (SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1), ?2, ?3)",
        params![zip_path, Utc::now().to_rfc3339(), operation],
    )?;
    conn.execute(
        "INSERT INTO zip_released_entries (zip_id, entry_path, released_to, entry_crc32)
         SELECT id, ?1, ?3, ?4 FROM zip_archives WHERE path = ?2
         ON CONFLICT(zip_id, entry_path) DO UPDATE SET
             released_to = excluded.released_to, entry_crc32 = COALESCE(excluded.entry_crc32, entry_crc32)",
        params![entry, zip_path, path.to_string_lossy(), row_crc],
    )?;
    sp.commit()?;
    tracing::info!(file_id, path = %path.display(), zip_path = %zip_path, "archived frame unzipped by hand — the row is a disk row again");
    Ok(HandRestore::BecameDiskRow)
}

/// `INSTRUME` as stored for a frame living under `root_id`: the base name (any previous
/// ` N<digits>` suffix removed), plus ` N<root_id>` when the root keeps cameras apart.
fn instrume_for_root(stored: Option<String>, unique_camera: bool, root_id: i64) -> Option<String> {
    let base = stored.map(|i| {
        if let Some(pos) = i.rfind(" N") {
            let suffix = &i[pos + 2..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return i[..pos].to_string();
            }
        }
        i
    });
    if unique_camera {
        base.map(|i| format!("{i} N{root_id}"))
    } else {
        base
    }
}

/// A catalog row that takes over a zip entry.
#[derive(Clone, Copy)]
struct ZipAdoption<'a> {
    file_id: i64,
    old_path: &'a str,
    vpath: &'a str,
    filename: &'a str,
    size: i64,
    modified: &'a DateTime<Utc>,
    zip_str: &'a str,
    entry_path: &'a str,
    crc: i64,
    root_id: i64,
    unique_camera: bool,
}

/// The write of an adoption, shared by a moved row and by a released entry whose copy was deleted: the
/// row becomes (or stays) a zip-resident row of `entry_path`, keeping its id and every junction row.
/// Returns the zip the row was resident in before, if any. One savepoint.
fn adopt_into_zip(conn: &Connection, a: &ZipAdoption) -> Result<Option<String>> {
    let ZipAdoption { file_id, old_path, vpath, filename, size, modified, zip_str, entry_path, crc, root_id, unique_camera } = *a;
    let sp = SavepointGuard::new(conn, "scan_zip_adopt")?;
    // A row of Move and ZIP's own keeps its original `files.path` (the restore target) and
    // its camera suffix: only its zip markers follow the zip. Any other row becomes a foreign
    // zip-resident row.
    let (from_zip, from_entry, current_path, ours): (Option<String>, Option<String>, String, bool) = conn.query_row(
        "SELECT archive_zip_path, archive_path_in_zip, path, archived_in_operation IS NOT NULL FROM files WHERE id = ?1",
        params![file_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if ours {
        // A row re-pointed at its zip entry (a new capture took its original path) names the
        // old zip in `files.path`: the path follows the zip like the markers do.
        if let (Some(fz), Some(fe)) = (&from_zip, &from_entry) {
            if current_path == virtual_path(Path::new(fz), fe) {
                let moved = conn.execute(
                    "UPDATE files SET path = ?1 WHERE id = ?2
                       AND NOT EXISTS (SELECT 1 FROM files WHERE path = ?1)",
                    params![vpath, file_id],
                )?;
                if moved == 0 {
                    tracing::warn!(
                        file_id,
                        path = %vpath,
                        zip_path = %zip_str,
                        "another catalog row owns the zip entry's path — the archived row keeps its old virtual path"
                    );
                }
            }
        }
        conn.execute(
            "UPDATE files SET archive_zip_path = ?1, archive_path_in_zip = ?2, archive_entry_crc32 = ?3 WHERE id = ?4",
            params![zip_str, entry_path, crc, file_id],
        )?;
    } else {
        conn.execute(
            "UPDATE files SET path = ?1, filename = ?2, size = ?3, modified_at = ?4,
                    archive_zip_path = ?5, archive_path_in_zip = ?6, archive_entry_crc32 = ?7
             WHERE id = ?8",
            params![vpath, filename, size, modified.to_rfc3339(), zip_str, entry_path, crc, file_id],
        )?;
        let stored: Option<String> = conn
            .query_row("SELECT instrume FROM frames WHERE file_id = ?1", params![file_id], |r| r.get(0))
            .optional()?
            .flatten();
        conn.execute(
            "UPDATE frames SET instrume = ?1 WHERE file_id = ?2",
            params![instrume_for_root(stored, unique_camera, root_id), file_id],
        )
        .map_err(|e| {
            tracing::error!(root_id, file_id, old_path = %old_path, path = %vpath, error = %e, "failed to update frame instrume after move");
            e
        })?;
    }
    sp.commit()?;
    Ok(from_zip)
}

enum EntryOutcome {
    /// Calibrated light or stacking artifact: never cataloged.
    Skipped,
    Inserted { frame_id: i64, imagetyp: Option<ImageType> },
    /// A row took over this entry; `from_zip` is the zip the row was resident in before (a zip that is gone).
    Adopted { from_zip: Option<String> },
    Updated,
}

/// An entry whose header is parsed and that no row took over yet: it is inserted unless the zip's
/// released entries (including those of the zip it was moved from) name it.
struct ParsedEntry {
    vpath: String,
    filename: String,
    format: FileFormat,
    frame: Frame,
    header: String,
    modified: DateTime<Utc>,
    size: i64,
    crc: i64,
}

enum Staged {
    Done(EntryOutcome),
    Pending(Box<ParsedEntry>),
}

/// First half of cataloging one entry whose header bytes were read: parse it, apply the never-cataloged
/// rules, re-parse a known entry in place (`known_id`: the row that already owns the entry, its CRC
/// changed), and let a moved catalog row adopt it. What is left is [`Staged::Pending`].
#[allow(clippy::too_many_arguments)]
fn stage_entry(
    conn: &Connection,
    zip: &Path,
    zip_str: &str,
    entry: &ZipEntryInfo,
    known_id: Option<i64>,
    header_bytes: &[u8],
    zip_modified: DateTime<Utc>,
    root_id: i64,
    unique_camera: bool,
) -> Result<Staged> {
    let vpath = virtual_path(zip, &entry.path);
    let format = if entry.path.to_ascii_lowercase().ends_with(".xisf") { FileFormat::XISF } else { FileFormat::FITS };
    let (frame, header) = match format {
        FileFormat::FITS => parse_fits_header_bytes(header_bytes, 0, Path::new(&vpath)),
        FileFormat::XISF => parse_xisf_header_bytes(header_bytes, 0, Path::new(&vpath)),
    }?;
    let modified = entry.modified.map(|n| n.and_utc()).unwrap_or(zip_modified);
    let size = entry.size as i64;
    let crc = i64::from(entry.crc32);

    if let Some(file_id) = known_id {
        // Modified in place: same row, same id, every junction table survives.
        let sp = SavepointGuard::new(conn, "scan_zip_entry")?;
        crate::scanner::apply_parsed_in_place(
            conn,
            file_id,
            size,
            &modified,
            &format,
            &frame,
            Some(&header),
            unique_camera,
            root_id,
            Path::new(&vpath),
            false,
        )?;
        conn.execute("UPDATE files SET archive_entry_crc32 = ?1 WHERE id = ?2", params![crc, file_id])?;
        sp.commit()?;
        return Ok(Staged::Done(EntryOutcome::Updated));
    }

    // Never cataloged, the same rules as a disk file (`process_file`).
    let keys = parse_stored_header_keys(format.clone(), &header);
    if calibrated_light_identity(&keys).is_some() {
        tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "calibrated artifact (CALSTAT+ATH_CSRC) — never cataloged");
        return Ok(Staged::Done(EntryOutcome::Skipped));
    }
    if keys.contains_key("ATH_REG") || keys.contains_key("ATH_STK") {
        tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "stacking artifact (ATH_REG/ATH_STK) — never cataloged");
        return Ok(Staged::Done(EntryOutcome::Skipped));
    }

    let filename = entry.path.rsplit('/').next().unwrap_or(&entry.path).to_string();

    let fingerprint = crate::fingerprint::compute_header_fingerprint(&header);
    if let Some((file_id, old_path)) = find_moved_row(conn, &fingerprint, &vpath, true)? {
        tracing::debug!(
            root_id,
            old_path = %old_path,
            path = %vpath,
            file_id,
            "moved file detected (into a zip)"
        );
        let from_zip = adopt_into_zip(
            conn,
            &ZipAdoption {
                file_id,
                old_path: &old_path,
                vpath: &vpath,
                filename: &filename,
                size,
                modified: &modified,
                zip_str,
                entry_path: &entry.path,
                crc,
                root_id,
                unique_camera,
            },
        )?;
        return Ok(Staged::Done(EntryOutcome::Adopted { from_zip }));
    }

    Ok(Staged::Pending(Box::new(ParsedEntry { vpath, filename, format, frame, header, modified, size, crc })))
}

/// Second half: insert a new row (files + frames + fits_header all commit or none do).
fn insert_entry(conn: &Connection, zip_str: &str, entry: &ZipEntryInfo, parsed: ParsedEntry, root_id: i64, unique_camera: bool) -> Result<EntryOutcome> {
    let ParsedEntry { vpath, filename, format, mut frame, header, modified, size, crc } = parsed;
    let sp = SavepointGuard::new(conn, "scan_zip_entry")?;
    let file = File {
        id: None,
        path: vpath,
        filename,
        size,
        modified_at: modified,
        format,
        created_at: Utc::now(),
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    };
    let file_id = insert_file(conn, &file)?;
    conn.execute(
        "UPDATE files SET archive_zip_path = ?1, archive_path_in_zip = ?2, archive_entry_crc32 = ?3 WHERE id = ?4",
        params![zip_str, entry.path, crc, file_id],
    )?;
    frame.file_id = file_id;
    if unique_camera {
        if let Some(ref instrume) = frame.instrume {
            frame.instrume = Some(format!("{instrume} N{root_id}"));
        }
    }
    let frame_id = insert_frame(conn, &frame)?;
    insert_fits_header(conn, file_id, &header)?;
    sp.commit()?;
    Ok(EntryOutcome::Inserted { frame_id, imagetyp: frame.imagetyp.clone() })
}

/// The warn logged once per changed zip whose rowless entries were taken for its operation's own copies.
pub(crate) const OPERATION_ENTRIES_SKIPPED: &str =
    "entries without a row in a changed zip taken for its operation's own copies; not cataloged";

/// Running totals of one [`scan_zip`].
struct Tally {
    outcome: ZipScanOutcome,
    /// Frames the zip holds now that a row owns after this scan.
    cataloged: usize,
    /// Zips that are gone and supplied at least one adopted row, in the order they were met.
    gone_zips: Vec<String>,
    /// An entry could not be read or parsed: the zip's stat is not recorded, so the next scan retries it.
    entry_failed: bool,
}

impl Tally {
    fn record(&mut self, done: EntryOutcome) {
        match done {
            EntryOutcome::Skipped => {}
            EntryOutcome::Inserted { frame_id, imagetyp } => {
                self.cataloged += 1;
                self.outcome.files_inserted += 1;
                if let Some(t) = imagetyp {
                    self.outcome.inserted.push((frame_id, t));
                }
            }
            EntryOutcome::Adopted { from_zip } => {
                self.cataloged += 1;
                self.outcome.adopted += 1;
                if let Some(g) = from_zip {
                    if !self.gone_zips.contains(&g) {
                        self.gone_zips.push(g);
                    }
                }
            }
            EntryOutcome::Updated => {
                self.cataloged += 1;
                self.outcome.updated += 1;
            }
        }
    }
}

/// Entry paths the operation that created the zip at `zip_path` recorded for it in its plan (every
/// disposition). Empty when the zip has no `created_by_operation`.
fn entries_written_by_creating_operation(conn: &Connection, zip_path: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT aof.target_path_in_zip FROM archive_operation_files aof
         INNER JOIN zip_archives z ON z.created_by_operation = aof.operation_id
         WHERE z.path = ?1 AND aof.target_zip_path = ?1",
    )?;
    let rows = stmt.query_map(params![zip_path], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
}

/// A release that has an owner: it names the path of its copy and the catalog has a row at that path.
/// The scan keeps only these. A release without one (recorded before `released_to` existed, or whose row
/// went away with a removed folder or a deleted file) keeps nothing out of the catalog: its entry is
/// cataloged like any new one.
const OWNED_RELEASE: &str = "r.released_to IS NOT NULL AND EXISTS (SELECT 1 FROM files f WHERE f.path = r.released_to)";

/// The releases of the zip at `zip_path` that have an owner ([`OWNED_RELEASE`]).
fn released_entries_of(conn: &Connection, zip_path: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.entry_path FROM zip_released_entries r INNER JOIN zip_archives z ON z.id = r.zip_id
         WHERE z.path = ?1 AND {OWNED_RELEASE}"
    ))?;
    let rows = stmt.query_map(params![zip_path], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
}

/// How many releases of the zip have no owner ([`OWNED_RELEASE`] is false for them).
fn ownerless_releases(conn: &Connection, zip_path: &str) -> Result<usize> {
    let n: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM zip_released_entries r INNER JOIN zip_archives z ON z.id = r.zip_id
             WHERE z.path = ?1 AND NOT ({OWNED_RELEASE})"
        ),
        params![zip_path],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// Drops the releases of the zip that have no owner, so their entries are cataloged as new rows.
fn drop_ownerless_releases(conn: &Connection, zip_path: &str) -> Result<usize> {
    let n = conn.execute(
        &format!(
            "DELETE FROM zip_released_entries WHERE id IN (
                 SELECT r.id FROM zip_released_entries r INNER JOIN zip_archives z ON z.id = r.zip_id
                 WHERE z.path = ?1 AND NOT ({OWNED_RELEASE}))"
        ),
        params![zip_path],
    )?;
    Ok(n)
}

/// The zip at `new_zip_id` took over rows from `gone`, a zip that no longer exists: move `gone`'s
/// identity along. Its released entries are copied to the new zip, its `created_by_operation` is
/// carried (unless the new row has its own), its black-hole record follows, and the old
/// `zip_archives` row is deleted once no `files` row points at its path any more (rows of entries
/// the new zip does not hold stay pointed at it, and stay missing).
fn move_zip_identity(conn: &Connection, new_zip_id: i64, new_zip_path: &str, gone: &str) -> Result<()> {
    if gone == new_zip_path {
        return Ok(());
    }
    // The operation that built the zip finds it by this path (Unarchive extracts from it, the scanner
    // recognises the operation's entries through it). An entry whose row still points at the old zip
    // stays with it: that zip is not here to extract from either way.
    let repointed = conn.execute(
        "UPDATE archive_operation_files SET target_zip_path = ?1
         WHERE target_zip_path = ?2
           AND NOT EXISTS (SELECT 1 FROM files f
                           WHERE f.id = archive_operation_files.file_id AND f.archive_zip_path = ?2)",
        params![new_zip_path, gone],
    )?;
    if repointed > 0 {
        tracing::info!(src = %gone, dest = %new_zip_path, entries = repointed, "archive operation entries follow the renamed zip");
    }
    let old: Option<(i64, Option<i64>)> = conn
        .query_row("SELECT id, created_by_operation FROM zip_archives WHERE path = ?1", params![gone], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((old_id, old_operation)) = old else {
        return Ok(());
    };
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO zip_released_entries (zip_id, entry_path, released_to, entry_crc32)
         SELECT ?1, r.entry_path, r.released_to, r.entry_crc32 FROM zip_released_entries r WHERE r.zip_id = ?2 AND {OWNED_RELEASE}"
        ),
        params![new_zip_id, old_id],
    )?;
    if let Some(op) = old_operation {
        conn.execute(
            "UPDATE zip_archives SET created_by_operation = ?1 WHERE id = ?2 AND created_by_operation IS NULL",
            params![op, new_zip_id],
        )?;
    }
    let still_referenced: i64 =
        conn.query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1", params![gone], |r| r.get(0))?;
    if still_referenced == 0 {
        conn.execute("UPDATE OR IGNORE black_hole_zips SET zip_id = ?1 WHERE zip_id = ?2", params![new_zip_id, old_id])?;
        conn.execute("DELETE FROM zip_archives WHERE id = ?1", params![old_id])?;
    } else {
        // Rows still point at the old path: keep its row, but without a real stat, so the zip is read
        // again if it ever comes back under that name (renamed back) instead of skipped as unchanged.
        conn.execute("UPDATE zip_archives SET size = 0, modified_at = '' WHERE id = ?1", params![old_id])?;
    }
    tracing::debug!(zip_id = new_zip_id, old_path = %gone, path = %new_zip_path, "zip identity moved to the zip's new location");
    Ok(())
}

/// What [`readopt_released`] did.
#[derive(Debug, Default)]
struct Readopted {
    rows: usize,
    bytes_read: u64,
}

/// A released entry whose copy is gone: the catalog row that was pointed at the copy goes back into the
/// zip (finding B of the 2026-10-29 smoke: the user deleted the unpacked files, the zip still held the
/// frames, and the entries stayed released, so the frames were lost to the catalog).
///
/// Mirrors the move-into-zip adoption: the row must be a disk row at `released_to`, that path must be gone
/// from disk with its scan root online (the same offline-root guard as a moved file, so an unmounted volume
/// never sends rows back into a zip), and the entry's header must carry the row's fingerprint. Then the
/// release is dropped and the row is adopted exactly as a moved row is (same id, analysis, plate solve, set
/// membership). A release with no `released_to` (recorded before the column) and a copy that is still on
/// disk stay as they are. Never deletes a row, never writes into the zip.
///
/// Runs for an unchanged zip too: it costs one stat per release that has a `released_to`, and the zip is
/// opened only when a copy is gone. `listing` is the directory when the caller already read it.
fn readopt_released(
    conn: &Connection,
    zip: &Path,
    zip_str: &str,
    zip_modified: DateTime<Utc>,
    listing: Option<&reader::ZipListing>,
    root_id: i64,
    unique_camera: bool,
) -> Result<Readopted> {
    struct Candidate {
        release_id: i64,
        entry: String,
        released_to: String,
        file_id: i64,
        fingerprint: Option<String>,
    }
    let mut stmt = conn.prepare(
        "SELECT r.id, r.entry_path, r.released_to, f.id, fh.header_fingerprint
         FROM zip_released_entries r
         INNER JOIN zip_archives z ON z.id = r.zip_id
         INNER JOIN files f ON f.path = r.released_to
         LEFT JOIN fits_header fh ON fh.file_id = f.id
         WHERE z.path = ?1 AND r.released_to IS NOT NULL AND f.archive_zip_path IS NULL
         ORDER BY r.id",
    )?;
    let released = stmt
        .query_map(params![zip_str], |r| {
            Ok(Candidate {
                release_id: r.get(0)?,
                entry: r.get(1)?,
                released_to: r.get(2)?,
                file_id: r.get(3)?,
                fingerprint: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut gone = Vec::new();
    for c in released {
        if Path::new(&c.released_to).exists() {
            continue;
        }
        // The copy is not there: a deleted file, or a volume that is not mounted.
        let vpath = virtual_path(zip, &c.entry);
        if old_location_is_gone(conn, c.file_id, &c.released_to, &c.released_to, &vpath)? {
            gone.push(c);
        }
    }
    let mut out = Readopted::default();
    if gone.is_empty() {
        return Ok(out);
    }

    let own_listing;
    let listing = match listing {
        Some(l) => l,
        None => {
            own_listing = reader::list_entries(zip).with_context(|| format!("read the directory of {}", zip.display()))?;
            out.bytes_read += own_listing.bytes_read;
            &own_listing
        }
    };
    let by_name: HashMap<&str, &ZipEntryInfo> = listing.entries.iter().filter(|e| e.is_frame).map(|e| (e.path.as_str(), e)).collect();
    let present: Vec<&Candidate> = gone.iter().filter(|c| by_name.contains_key(c.entry.as_str())).collect();
    for c in gone.iter().filter(|c| !by_name.contains_key(c.entry.as_str())) {
        tracing::warn!(zip_path = %zip.display(), zip_entry = %c.entry, path = %c.released_to, "unpacked copy is gone and the zip no longer holds the entry — the release stays");
    }
    if present.is_empty() {
        return Ok(out);
    }
    let names: Vec<&str> = present.iter().map(|c| c.entry.as_str()).collect();
    let (headers, header_bytes) =
        reader::read_headers(zip, &names).with_context(|| format!("read entry headers of {}", zip.display()))?;
    out.bytes_read += header_bytes;

    for (c, (_, header)) in present.iter().zip(headers) {
        let entry = by_name[c.entry.as_str()];
        let vpath = virtual_path(zip, &c.entry);
        let verdict: Result<bool> = (|| {
            let bytes = header?;
            let format = if c.entry.to_ascii_lowercase().ends_with(".xisf") { FileFormat::XISF } else { FileFormat::FITS };
            let (_, parsed) = match format {
                FileFormat::FITS => parse_fits_header_bytes(&bytes, 0, Path::new(&vpath)),
                FileFormat::XISF => parse_xisf_header_bytes(&bytes, 0, Path::new(&vpath)),
            }?;
            Ok(c.fingerprint.as_deref() == Some(crate::fingerprint::compute_header_fingerprint(&parsed).as_str()))
        })();
        match verdict {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(zip_path = %zip.display(), zip_entry = %c.entry, path = %c.released_to, file_id = c.file_id, "unpacked copy is gone but the zip entry is not that frame — the release stays");
                continue;
            }
            Err(e) => {
                tracing::warn!(zip_path = %zip.display(), zip_entry = %c.entry, error = %format!("{e:#}"), "could not read the zip entry to take an unpacked frame back — the release stays");
                continue;
            }
        }
        let modified = entry.modified.map(|n| n.and_utc()).unwrap_or(zip_modified);
        let filename = c.entry.rsplit('/').next().unwrap_or(&c.entry);
        let sp = SavepointGuard::new(conn, "scan_zip_readopt")?;
        let done = (|| -> Result<()> {
            // The release goes first: the path swap of the adoption would otherwise carry it along.
            conn.execute("DELETE FROM zip_released_entries WHERE id = ?1", params![c.release_id])?;
            adopt_into_zip(
                conn,
                &ZipAdoption {
                    file_id: c.file_id,
                    old_path: &c.released_to,
                    vpath: &vpath,
                    filename,
                    size: entry.size as i64,
                    modified: &modified,
                    zip_str,
                    entry_path: &c.entry,
                    crc: i64::from(entry.crc32),
                    root_id,
                    unique_camera,
                },
            )?;
            Ok(())
        })();
        match done {
            Ok(()) => {
                sp.commit()?;
                out.rows += 1;
                tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %c.entry, old_path = %c.released_to, file_id = c.file_id, "unpacked copy gone — the row is back in the zip");
            }
            Err(e) => {
                drop(sp);
                tracing::warn!(zip_path = %zip.display(), zip_entry = %c.entry, file_id = c.file_id, error = %format!("{e:#}"), "could not take an unpacked frame back into its zip — the release stays");
            }
        }
    }
    if out.rows > 0 {
        tracing::info!(zip_path = %zip.display(), rows = out.rows, "unpacked frames whose copy is gone are back in their zip");
    }
    Ok(out)
}

/// Catalog the frames inside one zip. The zip's stat is compared first; an unchanged zip is not
/// opened. Otherwise: read the directory; entries already owned by a row (`archive_zip_path` +
/// `archive_path_in_zip`) are known, and re-parsed in place only when their CRC changed; entries
/// in `zip_released_entries` are skipped; the rest go through one header pass and are adopted by a
/// moved row or inserted. All adoptions are decided before the first insert, because an adoption
/// from a zip that is gone also brings that zip's released entries (frames the user unpacked), which
/// must not come back as new rows. The whole zip is one transaction (one savepoint per entry inside it).
///
/// A bad entry is reported in [`ZipScanOutcome::errors`] and the rest of the zip is still
/// cataloged, but the zip's stat is then recorded as size 0, so the next scan reads it again, as a
/// disk file that failed is retried. A zip that cannot be opened is an `Err` and leaves no
/// `zip_archives` row, so the next scan retries it.
pub fn scan_zip(conn: &Connection, zip: &Path, root_id: i64, unique_camera: bool) -> Result<ZipScanOutcome> {
    let zip_str = path_to_utf8(zip)?;
    let meta = std::fs::metadata(zip).with_context(|| format!("stat {}", zip.display()))?;
    let size = meta.len() as i64;
    let zip_modified = DateTime::<Utc>::from(meta.modified().with_context(|| format!("mtime of {}", zip.display()))?);
    let modified_str = zip_modified.to_rfc3339();

    let known_zip: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT id, size, modified_at FROM zip_archives WHERE path = ?1",
            params![zip_str],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    // Releases without an owner are cataloged again, so such a zip is read even when its stat is unchanged
    // (the release itself is dropped in the scan's transaction, below).
    let ownerless = if known_zip.is_some() { ownerless_releases(conn, &zip_str)? } else { 0 };
    if let Some((zip_id, db_size, db_modified)) = &known_zip {
        if *db_size == size && *db_modified == modified_str && ownerless == 0 {
            // Nothing to read in the zip itself, but an unpacked copy deleted since the last scan sends its
            // row back into the zip (the stat of each release's copy is the whole cost when none was).
            let back = readopt_released(conn, zip, &zip_str, zip_modified, None, root_id, unique_camera)?;
            if back.rows > 0 {
                conn.execute(
                    "UPDATE zip_archives SET frame_entries = (SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1) WHERE id = ?2",
                    params![zip_str, zip_id],
                )?;
            }
            return Ok(ZipScanOutcome {
                zip_id: *zip_id,
                skipped_unchanged: back.bytes_read == 0,
                adopted: back.rows,
                bytes_read: back.bytes_read,
                ..Default::default()
            });
        }
    }

    let listing = reader::list_entries(zip).with_context(|| format!("read the directory of {}", zip.display()))?;
    let mut tally = Tally {
        outcome: ZipScanOutcome { bytes_read: listing.bytes_read, ..Default::default() },
        cataloged: 0,
        gone_zips: Vec::new(),
        entry_failed: false,
    };
    // Released entries whose copy is gone come back first, so the passes below see them as the zip's own.
    if known_zip.is_some() {
        let back = readopt_released(conn, zip, &zip_str, zip_modified, Some(&listing), root_id, unique_camera)?;
        tally.outcome.adopted += back.rows;
        tally.outcome.bytes_read += back.bytes_read;
    }

    // Entries a row already owns: row id and the CRC recorded for it (NULL for rows that predate it).
    let mut known: HashMap<String, (i64, Option<i64>)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, archive_path_in_zip, archive_entry_crc32 FROM files
             WHERE archive_zip_path = ?1 AND archive_path_in_zip IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![zip_str], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))?;
        for row in rows {
            let (id, entry, crc) = row?;
            known.insert(entry, (id, crc));
        }
    }
    let mut released: HashSet<String> = match &known_zip {
        Some(_) => released_entries_of(conn, &zip_str)?,
        None => HashSet::new(),
    };

    // Entries the zip's own operation wrote into it. A Copy entry (a library master, copied darks) has no
    // row by design: it is the original on disk that is cataloged, so it must not become a foreign row.
    let written_by_operation: HashSet<String> = match &known_zip {
        Some(_) => entries_written_by_creating_operation(conn, &zip_str)?,
        None => HashSet::new(),
    };

    let mut to_read: Vec<(&ZipEntryInfo, Option<i64>)> = Vec::new();
    let mut skipped_as_operation_entries = 0usize;
    for entry in listing.entries.iter().filter(|e| e.is_frame) {
        if released.contains(&entry.path) {
            continue;
        }
        match known.get(&entry.path) {
            Some((id, Some(crc))) if *crc != i64::from(entry.crc32) => to_read.push((entry, Some(*id))),
            Some(_) => tally.cataloged += 1,
            None if written_by_operation.contains(&entry.path) => {
                skipped_as_operation_entries += 1;
                tally.cataloged += 1;
            }
            None => to_read.push((entry, None)),
        }
    }
    if skipped_as_operation_entries > 0 {
        // The zip's stat changed (it was not skipped as unchanged above) and entries with no row were
        // taken for the operation's own Copy entries. If the user swapped our zip for another of the same
        // name, those entries are real frames that stay out of the catalog; say so once per zip.
        tracing::warn!(
            root_id,
            zip_path = %zip.display(),
            skipped = skipped_as_operation_entries,
            "{}",
            OPERATION_ENTRIES_SKIPPED
        );
    }

    // The safety filter keeps NEW entries out of the catalog, but an entry a row already owns is known
    // whatever its name: our own Move and ZIP entries carry `:` or a trailing space from object names.
    // Such a row is neither re-added nor left uncounted.
    for (name, _) in &listing.skipped {
        if known.contains_key(name) {
            tally.cataloged += 1;
        }
    }

    // One open of the zip, 4 KiB buffer, for every header still needed.
    let names: Vec<&str> = to_read.iter().map(|(e, _)| e.path.as_str()).collect();
    let headers = if names.is_empty() {
        Vec::new()
    } else {
        let (headers, header_bytes) =
            reader::read_headers(zip, &names).with_context(|| format!("read entry headers of {}", zip.display()))?;
        tally.outcome.bytes_read += header_bytes;
        headers
    };

    let outer = SavepointGuard::new(conn, "scan_zip")?;
    if ownerless > 0 {
        let dropped = drop_ownerless_releases(conn, &zip_str)?;
        tracing::info!(root_id, zip_path = %zip.display(), count = dropped, "released entries with no copy in the catalog are cataloged again");
    }
    // FOREIGN rows that predate the entry CRC learn it from the directory this scan already read, with no
    // further read of the zip. Rows of our own operations never do: their CRC comes only from finalize or
    // from a materialize that verified the extracted bytes against the expected hash, because a zip that
    // was replaced since it was archived would otherwise hand its new CRC to the old row and switch the
    // only check off.
    let crc_by_entry: HashMap<&str, u32> = listing.entries.iter().map(|e| (e.path.as_str(), e.crc32)).collect();
    let mut backfilled = 0usize;
    for (entry, (id, crc)) in &known {
        if crc.is_some() || released.contains(entry) {
            continue;
        }
        if let Some(listed) = crc_by_entry.get(entry.as_str()) {
            backfilled += conn.execute(
                "UPDATE files SET archive_entry_crc32 = ?1
                 WHERE id = ?2 AND archive_entry_crc32 IS NULL AND archived_in_operation IS NULL",
                params![i64::from(*listed), id],
            )?;
        }
    }
    if backfilled > 0 {
        tracing::info!(root_id, zip_path = %zip.display(), count = backfilled, "entry crcs recorded for rows that had none");
    }
    // Pass 1: parse, update known entries, let moved rows adopt entries.
    let mut pending: Vec<(&ZipEntryInfo, Box<ParsedEntry>)> = Vec::new();
    for ((entry, known_id), (_, header)) in to_read.iter().zip(headers) {
        let staged = match header {
            Ok(bytes) => {
                stage_entry(conn, zip, &zip_str, entry, *known_id, &bytes, zip_modified, root_id, unique_camera)
            }
            Err(e) => Err(e),
        };
        match staged {
            Ok(Staged::Done(done)) => tally.record(done),
            Ok(Staged::Pending(parsed)) => pending.push((entry, parsed)),
            Err(e) => {
                tracing::warn!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, error = %format!("{e:#}"), "zip entry not cataloged");
                tally.outcome.errors.push(format!("{}: {e:#}", virtual_path(zip, &entry.path)));
                tally.entry_failed = true;
                if known_id.is_some() {
                    tally.cataloged += 1;
                }
            }
        }
    }

    // Entries the user unpacked from a zip that was then moved are released there, not here. Entries the
    // operation that built that zip copied into it (a library master, copied darks) have no row by design,
    // exactly as in a zip the catalog already knows: they are not new frames of the renamed zip.
    let mut written_by_gone_operation: HashSet<String> = HashSet::new();
    for gone in &tally.gone_zips {
        released.extend(released_entries_of(conn, gone)?);
        written_by_gone_operation.extend(entries_written_by_creating_operation(conn, gone)?);
    }

    // Pass 2: whatever no row adopted is a new row, unless it was unpacked by the user.
    for (entry, parsed) in pending {
        if released.contains(&entry.path) {
            tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "released entry not re-added");
            continue;
        }
        if written_by_gone_operation.contains(&entry.path) {
            tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "entry copied in by the zip's own operation not cataloged");
            tally.cataloged += 1;
            continue;
        }
        match insert_entry(conn, &zip_str, entry, *parsed, root_id, unique_camera) {
            Ok(done) => tally.record(done),
            Err(e) => {
                tracing::warn!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, error = %format!("{e:#}"), "zip entry not cataloged");
                tally.outcome.errors.push(format!("{}: {e:#}", virtual_path(zip, &entry.path)));
                tally.entry_failed = true;
            }
        }
    }

    // A zip with a failed entry keeps no stat: size 0 never equals a real zip, so the next scan reads it again.
    let (recorded_size, recorded_modified) = if tally.entry_failed { (0, String::new()) } else { (size, modified_str) };
    conn.execute(
        "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, modified_at = excluded.modified_at,
                frame_entries = excluded.frame_entries, scanned_at = excluded.scanned_at",
        params![zip_str, recorded_size, recorded_modified, tally.cataloged as i64, Utc::now().to_rfc3339()],
    )?;
    tally.outcome.zip_id = conn.query_row("SELECT id FROM zip_archives WHERE path = ?1", params![zip_str], |r| r.get(0))?;
    for gone in &tally.gone_zips {
        move_zip_identity(conn, tally.outcome.zip_id, &zip_str, gone)?;
    }
    outer.commit()?;

    tracing::info!(
        root_id,
        zip_path = %zip.display(),
        count = tally.cataloged,
        new = tally.outcome.files_inserted,
        updated = tally.outcome.adopted + tally.outcome.updated,
        bytes_read = tally.outcome.bytes_read,
        "zip scanned"
    );
    Ok(tally.outcome)
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::archive::models::ArchiveCompression;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use std::path::{Path, PathBuf};

    fn card(s: &str) -> Vec<u8> {
        assert!(s.len() <= 80, "card too long: {s:?}");
        let mut c = vec![b' '; 80];
        c[..s.len()].copy_from_slice(s.as_bytes());
        c
    }

    /// A FITS file (empty primary HDU) with the usual light-frame cards plus `extra`.
    pub(crate) fn fits_bytes(extra: &[String]) -> Vec<u8> {
        fits_bytes_typed("Light", extra)
    }

    /// Like [`fits_bytes`] with the given IMAGETYP.
    pub(crate) fn fits_bytes_typed(imagetyp: &str, extra: &[String]) -> Vec<u8> {
        let mut out = Vec::new();
        let imagetyp_card = format!("IMAGETYP= '{imagetyp:<8}'");
        for c in [
            "SIMPLE  =                    T / file conforms to FITS standard",
            "BITPIX  =                    8 / number of bits per data pixel",
            "NAXIS   =                    0 / number of data axes",
            "OBJECT  = 'M33     '           / target name",
            "TELESCOP= 'T       '",
            "INSTRUME= 'C       '",
            imagetyp_card.as_str(),
        ] {
            out.extend(card(c));
        }
        for c in extra {
            out.extend(card(c));
        }
        out.extend(card("END"));
        while out.len() % 2880 != 0 {
            out.push(b' ');
        }
        out
    }

    /// A light whose header differs from every other seed (so its fingerprint does).
    pub(crate) fn light(seed: u32) -> Vec<u8> {
        fits_bytes(&[format!("SEQNUM  = {seed:>20}")])
    }

    pub(crate) fn make_zip(dir: &Path, name: &str, files: &[(&str, Vec<u8>)], compression: ArchiveCompression) -> PathBuf {
        // Sources live outside `dir`, so a scanned root holds nothing but the zip.
        let src_guard = tempfile::TempDir::new().unwrap();
        let src = src_guard.path();
        let entries: Vec<ZipEntry> = files
            .iter()
            .enumerate()
            .map(|(i, (entry, bytes))| {
                let p = src.join(format!("s{i}"));
                std::fs::write(&p, bytes).unwrap();
                ZipEntry { source_path: p, path_in_zip: (*entry).to_string() }
            })
            .collect();
        let zip = dir.join(name);
        build_zip(&zip, &entries, compression).unwrap();
        zip
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use super::test_support::{fits_bytes, light, make_zip};
    use crate::db::schema::init_db;
    use crate::fits_parser::parse_fits_with_header;
    use crate::zip_store::reader::list_entries;
    use rusqlite::params;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn db(root: &Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [root.to_str().unwrap()]).unwrap();
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn id_of(conn: &Connection, path: &str) -> i64 {
        conn.query_row("SELECT id FROM files WHERE path = ?1", params![path], |r| r.get(0)).unwrap()
    }

    fn fingerprint_of(conn: &Connection, file_id: i64) -> String {
        conn.query_row("SELECT header_fingerprint FROM fits_header WHERE file_id = ?1", params![file_id], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn store_and_deflate_zips_catalog_every_frame_with_virtual_paths() {
        for compression in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            let tmp = TempDir::new().unwrap();
            let conn = db(tmp.path());
            let (a, b) = (light(1), light(2));
            let zip = make_zip(
                tmp.path(),
                "n.zip",
                &[("Lights/L_001.fits", a.clone()), ("Lights/L_002.fits", b), ("notes.txt", b"hello hello hello".to_vec())],
                compression,
            );
            let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
            assert!(!outcome.skipped_unchanged);
            assert_eq!(outcome.inserted.len(), 2);
            assert!(outcome.inserted.iter().all(|(_, t)| *t == ImageType::Light));
            assert_eq!(outcome.adopted, 0);
            assert!(outcome.bytes_read > 0);
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2, "notes.txt is not a frame");
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 2);

            let vpath = virtual_path(&zip, "Lights/L_001.fits");
            assert_eq!(
                vpath,
                format!("{}{}Lights{}L_001.fits", zip.display(), std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)
            );
            let (path, size, format, zip_col, entry_col, crc, filename, op): (String, i64, String, String, String, i64, String, Option<i64>) =
                conn.query_row(
                    "SELECT path, size, format, archive_zip_path, archive_path_in_zip, archive_entry_crc32, filename, archived_in_operation
                     FROM files WHERE path = ?1",
                    params![vpath],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
                )
                .unwrap();
            assert_eq!(path, vpath);
            assert_eq!(size, a.len() as i64, "size is the uncompressed entry size");
            assert_eq!(format, "FITS");
            assert_eq!(zip_col, zip.to_str().unwrap());
            assert_eq!(entry_col, "Lights/L_001.fits");
            assert_eq!(filename, "L_001.fits");
            assert_eq!(op, None, "a foreign zip row is not one of ours");
            let listed = list_entries(&zip).unwrap();
            let want_crc = listed.entries.iter().find(|e| e.path == "Lights/L_001.fits").unwrap().crc32;
            assert_eq!(crc, want_crc as i64);

            // The header fingerprint is the one a disk copy of the same bytes gets.
            let disk = tmp.path().join("disk.fits");
            std::fs::write(&disk, &a).unwrap();
            let (_, disk_header) = parse_fits_with_header(&disk, 0).unwrap();
            assert_eq!(
                fingerprint_of(&conn, id_of(&conn, &vpath)),
                crate::fingerprint::compute_header_fingerprint(&disk_header)
            );

            let (zsize, zframes): (i64, i64) = conn
                .query_row("SELECT size, frame_entries FROM zip_archives WHERE path = ?1", params![zip.to_str().unwrap()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(zsize, std::fs::metadata(&zip).unwrap().len() as i64);
            assert_eq!(zframes, 2);
        }
    }

    #[test]
    fn unchanged_zip_is_skipped_without_opening() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        let second = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(second.skipped_unchanged);
        assert_eq!(second.bytes_read, 0);
        assert!(second.inserted.is_empty());
        assert_eq!(second.zip_id, first.zip_id);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
    }

    #[test]
    fn changed_zip_keeps_known_rows_adds_new_and_leaves_vanished_missing() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("L_001.fits", light(1)), ("L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &zip, 1, false).unwrap();
        let id1 = id_of(&conn, &virtual_path(&zip, "L_001.fits"));
        let id2 = id_of(&conn, &virtual_path(&zip, "L_002.fits"));

        // Re-zip: L_002 gone, L_003 new (and a bigger note so the zip's stat certainly differs).
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("L_001.fits", light(1)), ("L_003.fits", light(3)), ("notes.txt", vec![b'x'; 5000])],
            ArchiveCompression::Store,
        );
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!outcome.skipped_unchanged);
        assert_eq!(outcome.inserted.len(), 1, "only the new entry is parsed and inserted");
        assert_eq!(id_of(&conn, &virtual_path(&zip, "L_001.fits")), id1, "known entry keeps its row");
        assert_eq!(id_of(&conn, &virtual_path(&zip, "L_002.fits")), id2, "a vanished entry's row is never deleted");
        id_of(&conn, &virtual_path(&zip, "L_003.fits"));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 3);
        let frames: i64 = conn
            .query_row("SELECT frame_entries FROM zip_archives WHERE id = ?1", params![outcome.zip_id], |r| r.get(0))
            .unwrap();
        assert_eq!(frames, 2, "frame_entries counts the entries the zip holds now");
    }

    #[test]
    fn known_entry_with_a_changed_crc_is_reparsed_in_place() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1))], ArchiveCompression::Store);
        scan_zip(&conn, &zip, 1, false).unwrap();
        let vpath = virtual_path(&zip, "L_001.fits");
        let id = id_of(&conn, &vpath);
        let frame_id: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();
        let old_crc: i64 = conn.query_row("SELECT archive_entry_crc32 FROM files WHERE id = ?1", params![id], |r| r.get(0)).unwrap();

        let changed = fits_bytes(&["SEQNUM  =                    1".to_string(), "FILTER  = 'Ha      '".to_string()]);
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", changed)], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.updated, 1, "an in-place re-parse is counted");
        assert!(outcome.inserted.is_empty(), "an in-place update inserts nothing");
        assert_eq!(id_of(&conn, &vpath), id);
        let (filter, fid): (Option<String>, i64) = conn
            .query_row("SELECT filter, id FROM frames WHERE file_id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(fid, frame_id, "the frames row keeps its id");
        assert_eq!(filter.as_deref(), Some("Ha"));
        let new_crc: i64 = conn.query_row("SELECT archive_entry_crc32 FROM files WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_ne!(new_crc, old_crc);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM fits_header"), 1);
    }

    #[test]
    fn appledouble_entries_are_not_cataloged() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[
                ("Lights/L_001.fits", light(1)),
                ("__MACOSX/Lights/._L_001.fits", vec![0u8; 120]),
                ("Lights/._L_001.fits", vec![0u8; 120]),
            ],
            ArchiveCompression::Deflate,
        );
        // The listing filter is what keeps them out: both are named, with their reason. Without the
        // filter they are `.fits` frames of 120 zero bytes, which fail to parse and land in `errors`.
        let listing = list_entries(&zip).unwrap();
        let reasons: Vec<(&str, &str)> = listing.skipped.iter().map(|(n, r)| (n.as_str(), *r)).collect();
        assert!(reasons.contains(&("__MACOSX/Lights/._L_001.fits", "appledouble")), "{reasons:?}");
        assert!(reasons.contains(&("Lights/._L_001.fits", "appledouble")), "{reasons:?}");
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.inserted.len(), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE path LIKE '%._L_001%'"), 0);
    }

    #[test]
    fn calibrated_light_entries_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let calibrated = fits_bytes(&["CALSTAT = 'BDF     '".to_string(), "ATH_CSRC= 'uuid-1  '".to_string()]);
        let registered = fits_bytes(&["ATH_REG = 'yes     '".to_string()]);
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("cal.fits", calibrated), ("reg.fits", registered), ("L_001.fits", light(1))],
            ArchiveCompression::Store,
        );
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.inserted.len(), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1, "calibrated lights and stacking artifacts are never cataloged");
        let entries: i64 = conn.query_row("SELECT frame_entries FROM zip_archives", [], |r| r.get(0)).unwrap();
        assert_eq!(entries, 1);
    }

    #[test]
    fn entries_that_fail_to_parse_are_reported_and_do_not_stop_the_zip() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("Bad.fits", b"this is not a fits file at all".to_vec()), ("L_001.fits", light(1))],
            ArchiveCompression::Store,
        );
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.inserted.len(), 1);
        assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
        assert!(outcome.errors[0].contains("Bad.fits"), "{:?}", outcome.errors);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
    }

    /// Catalog a disk frame (as the scanner would), with an analysis row hanging off it.
    fn catalog_disk_frame(conn: &Connection, root: &Path, name: &str, bytes: &[u8]) -> (PathBuf, i64) {
        let path = root.join(name);
        std::fs::write(&path, bytes).unwrap();
        crate::scanner::scan_directory(root, conn, None, false, 1);
        let id = id_of(conn, path.to_str().unwrap());
        let frame_id: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO frame_analysis (frame_id, file_id, stars_detected, median_fwhm, median_eccentricity, median_snr,
                 median_hfr, frame_snr, snr_weight, psf_signal, background, noise, detection_threshold, width, height, source_channels)
             VALUES (?1, ?2, 10, 2.5, 0.3, 20.0, 2.0, 15.0, 1.0, 100.0, 50.0, 3.0, 5.0, 100, 100, 1)",
            params![frame_id, id],
        )
        .unwrap();
        (path, id)
    }

    #[test]
    fn cataloged_frame_zipped_elsewhere_with_original_deleted_moves_into_the_zip() {
        let tmp = TempDir::new().unwrap();
        let scan_root = tmp.path().join("scan");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&scan_root).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let conn = db(&scan_root);
        let bytes = light(7);
        let (disk, id) = catalog_disk_frame(&conn, &scan_root, "L_001.fits", &bytes);
        let frame_before: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();

        std::fs::remove_file(&disk).unwrap();
        let zip = make_zip(&elsewhere, "n.zip", &[("Lights/L_001.fits", bytes)], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        assert!(outcome.inserted.is_empty());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1, "no second row");

        let vpath = virtual_path(&zip, "Lights/L_001.fits");
        let (path, zip_col, entry_col, crc, filename): (String, String, String, Option<i64>, String) = conn
            .query_row(
                "SELECT path, archive_zip_path, archive_path_in_zip, archive_entry_crc32, filename FROM files WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(path, vpath);
        assert_eq!(zip_col, zip.to_str().unwrap());
        assert_eq!(entry_col, "Lights/L_001.fits");
        assert!(crc.is_some());
        assert_eq!(filename, "L_001.fits");
        let frame_after: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(frame_after, frame_before);
        assert_eq!(count(&conn, &format!("SELECT COUNT(*) FROM frame_analysis WHERE file_id = {id}")), 1, "the analysis row is kept");
        let known = conn
            .query_row("SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(known, 1);

        // A second pass over the changed-stat-free zip is a skip, and the entry is now known.
        assert!(scan_zip(&conn, &zip, 1, false).unwrap().skipped_unchanged);
    }

    #[test]
    fn adoption_recomputes_instrume_from_the_zip_root() {
        let tmp = TempDir::new().unwrap();
        let scan_root = tmp.path().join("scan");
        std::fs::create_dir_all(&scan_root).unwrap();
        let conn = db(&scan_root);
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (2, ?1)", [tmp.path().join("zips").to_str().unwrap()]).unwrap();
        std::fs::create_dir_all(tmp.path().join("zips")).unwrap();
        let bytes = light(7);
        let disk = scan_root.join("L_001.fits");
        std::fs::write(&disk, &bytes).unwrap();
        crate::scanner::scan_directory(&scan_root, &conn, None, true, 1);
        let id = id_of(&conn, disk.to_str().unwrap());
        let before: String = conn.query_row("SELECT instrume FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(before, "C N1");

        std::fs::remove_file(&disk).unwrap();
        let zip = make_zip(&tmp.path().join("zips"), "n.zip", &[("L_001.fits", bytes)], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &zip, 2, true).unwrap();
        assert_eq!(outcome.adopted, 1);
        let after: String = conn.query_row("SELECT instrume FROM frames WHERE file_id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(after, "C N2", "the suffix follows the root that now holds the frame");
    }

    #[test]
    fn original_still_on_disk_makes_the_zip_copy_a_duplicate() {
        let tmp = TempDir::new().unwrap();
        let scan_root = tmp.path().join("scan");
        std::fs::create_dir_all(&scan_root).unwrap();
        let conn = db(&scan_root);
        let bytes = light(7);
        let (disk, id) = catalog_disk_frame(&conn, &scan_root, "L_001.fits", &bytes);

        let zip = make_zip(tmp.path(), "n.zip", &[("Lights/L_001.fits", bytes)], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.adopted, 0);
        assert_eq!(outcome.inserted.len(), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
        let (path, zip_col): (String, Option<String>) = conn
            .query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(path, disk.to_str().unwrap(), "the disk row is untouched");
        assert_eq!(zip_col, None);
        let fps: i64 = count(&conn, "SELECT COUNT(DISTINCT header_fingerprint) FROM fits_header");
        assert_eq!(fps, 1, "the two rows carry the same fingerprint");
    }

    #[test]
    fn a_disk_row_under_an_offline_root_is_not_taken_over_by_a_zip_entry() {
        let tmp = TempDir::new().unwrap();
        let offline = tmp.path().join("offline-volume");
        let conn = db(tmp.path());
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (2, ?1)", [offline.to_str().unwrap()]).unwrap();
        // A catalog row whose file lives under the unmounted root.
        let bytes = light(7);
        let probe = tmp.path().join("probe.fits");
        std::fs::write(&probe, &bytes).unwrap();
        let (_, header) = parse_fits_with_header(&probe, 0).unwrap();
        let old = offline.join("L_001.fits");
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (900, ?1, 'L_001.fits', 1, '2025-01-01T00:00:00Z', 'FITS')",
            params![old.to_str().unwrap()],
        )
        .unwrap();
        crate::db::insert_fits_header(&conn, 900, &header).unwrap();

        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", bytes)], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.adopted, 0, "the volume guard applies to zip entries too");
        assert_eq!(outcome.inserted.len(), 1);
        let path: String = conn.query_row("SELECT path FROM files WHERE id = 900", [], |r| r.get(0)).unwrap();
        assert_eq!(path, old.to_str().unwrap());
    }

    #[test]
    fn zip_moved_outside_the_app_is_adopted_by_its_new_location() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "old.zip",
            &[("Lights/L_001.fits", light(1)), ("Lights/L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &zip, 1, false).unwrap();
        let id1 = id_of(&conn, &virtual_path(&zip, "Lights/L_001.fits"));
        let id2 = id_of(&conn, &virtual_path(&zip, "Lights/L_002.fits"));

        let moved = tmp.path().join("renamed.zip");
        std::fs::rename(&zip, &moved).unwrap();
        let outcome = scan_zip(&conn, &moved, 1, false).unwrap();
        assert_eq!(outcome.adopted, 2);
        assert!(outcome.inserted.is_empty());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
        let (path, zip_col): (String, String) = conn
            .query_row("SELECT path, archive_zip_path FROM files WHERE id = ?1", params![id1], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(path, virtual_path(&moved, "Lights/L_001.fits"));
        assert_eq!(zip_col, moved.to_str().unwrap());
        assert_eq!(id_of(&conn, &virtual_path(&moved, "Lights/L_002.fits")), id2);
    }

    #[test]
    fn a_zip_row_of_ours_follows_its_moved_zip_without_losing_its_original_path() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let bytes = light(1);
        let zip = make_zip(tmp.path(), "ours.zip", &[("Lights/L_001.fits", bytes.clone())], ArchiveCompression::Store);
        // A row as Move and ZIP leaves it: original disk path, zip markers, the operation that archived it.
        let probe = tmp.path().join("probe.fits");
        std::fs::write(&probe, &bytes).unwrap();
        let (_, header) = parse_fits_with_header(&probe, 0).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (500, '/data/original/L_001.fits', 'L_001.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?1, 'Lights/L_001.fits')",
            params![zip.to_str().unwrap()],
        )
        .unwrap();
        crate::db::insert_fits_header(&conn, 500, &header).unwrap();
        conn.execute("INSERT INTO frames (file_id) VALUES (500)", []).unwrap();
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        conn.execute("UPDATE files SET archived_in_operation = 77 WHERE id = 500", []).unwrap();

        let moved = tmp.path().join("moved.zip");
        std::fs::rename(&zip, &moved).unwrap();
        let outcome = scan_zip(&conn, &moved, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        let (path, zip_col, entry, op): (String, String, String, Option<i64>) = conn
            .query_row(
                "SELECT path, archive_zip_path, archive_path_in_zip, archived_in_operation FROM files WHERE id = 500",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(path, "/data/original/L_001.fits", "our rows keep the restore target as files.path");
        assert_eq!(zip_col, moved.to_str().unwrap());
        assert_eq!(entry, "Lights/L_001.fits");
        assert_eq!(op, Some(77));
    }

    #[test]
    fn released_entries_are_not_re_added() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        // A permanent unpack moved L_001 to disk: markers cleared, entry released.
        let id1 = id_of(&conn, &virtual_path(&zip, "L_001.fits"));
        conn.execute(
            "UPDATE files SET path = '/unpacked/L_001.fits', archive_zip_path = NULL, archive_path_in_zip = NULL,
                    archive_entry_crc32 = NULL WHERE id = ?1",
            params![id1],
        )
        .unwrap();
        let copy = tmp.path().join("unpacked").join("L_001.fits");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, light(1)).unwrap();
        conn.execute("UPDATE files SET path = ?1 WHERE id = ?2", params![copy.to_str().unwrap(), id1]).unwrap();
        conn.execute(
            "INSERT INTO zip_released_entries (zip_id, entry_path, released_to) VALUES (?1, 'L_001.fits', ?2)",
            params![first.zip_id, copy.to_str().unwrap()],
        )
        .unwrap();

        // The zip changes (a new entry) and is scanned again.
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("L_001.fits", light(1)), ("L_002.fits", light(2)), ("L_003.fits", light(3))],
            ArchiveCompression::Store,
        );
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.inserted.len(), 1, "only L_003 is new");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 3);
        assert_eq!(id_of(&conn, copy.to_str().unwrap()), id1);
    }

    /// Two zips entries (L_001, L_002); the row of L_001 is unpacked to `copy_name` under the root (the file is
    /// written when `copy_on_disk`), and the release is recorded with `released_to`.
    fn unpack_first_entry(conn: &Connection, zip: &Path, zip_id: i64, released_to: Option<&str>) -> i64 {
        let id1 = id_of(conn, &virtual_path(zip, "L_001.fits"));
        conn.execute(
            "UPDATE files SET path = '/gone/place/L_001.fits', archive_zip_path = NULL, archive_path_in_zip = NULL,
                    archive_entry_crc32 = NULL WHERE id = ?1",
            params![id1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO zip_released_entries (zip_id, entry_path, released_to) VALUES (?1, 'L_001.fits', ?2)",
            params![zip_id, released_to],
        )
        .unwrap();
        id1
    }

    #[test]
    fn a_release_with_no_destination_and_no_row_is_cataloged_again_even_when_the_zip_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        // The pre-r1 release (released_to NULL) whose row went away with a removed folder.
        let id1 = unpack_first_entry(&conn, &zip, first.zip_id, None);
        conn.execute("DELETE FROM frames WHERE file_id = ?1", params![id1]).unwrap();
        conn.execute("DELETE FROM fits_header WHERE file_id = ?1", params![id1]).unwrap();
        conn.execute("DELETE FROM files WHERE id = ?1", params![id1]).unwrap();

        let again = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!again.skipped_unchanged, "the zip is read to take the entry back");
        assert_eq!(again.inserted.len(), 1, "L_001 is cataloged again");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 0, "the release is gone");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 2);
        assert_eq!(count(&conn, "SELECT frame_entries FROM zip_archives"), 2);
        // And the next scan is the cheap one again.
        assert!(scan_zip(&conn, &zip, 1, false).unwrap().skipped_unchanged);
    }

    #[test]
    fn a_release_whose_destination_has_no_row_is_cataloged_again() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        let id1 = unpack_first_entry(&conn, &zip, first.zip_id, Some("/gone/place/L_001.fits"));
        conn.execute("DELETE FROM frames WHERE file_id = ?1", params![id1]).unwrap();
        conn.execute("DELETE FROM fits_header WHERE file_id = ?1", params![id1]).unwrap();
        conn.execute("DELETE FROM files WHERE id = ?1", params![id1]).unwrap();

        let again = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(again.inserted.len(), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 2);
    }

    #[test]
    fn a_release_whose_destination_has_a_row_stays_released_when_the_zip_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "n.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        // The copy is on disk, under the root, with its row.
        let copy = tmp.path().join("L_001.fits");
        std::fs::write(&copy, light(1)).unwrap();
        let id1 = unpack_first_entry(&conn, &zip, first.zip_id, Some(copy.to_str().unwrap()));
        conn.execute("UPDATE files SET path = ?1 WHERE id = ?2", params![copy.to_str().unwrap(), id1]).unwrap();

        let again = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(again.skipped_unchanged);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
    }

    #[test]
    fn removing_the_folder_forgets_its_zips_and_a_re_added_folder_catalogs_every_entry() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("L_001.fits", light(1)), ("L_002.fits", light(2)), ("L_003.fits", light(3))],
            ArchiveCompression::Store,
        );
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        // Two entries were unpacked into the folder (one release recorded before the column existed), then the
        // user deleted the unpacked files and removed the folder from the catalog.
        let copy = tmp.path().join("unpacked").join("L_001.fits");
        unpack_first_entry(&conn, &zip, first.zip_id, Some(copy.to_str().unwrap()));
        conn.execute("UPDATE files SET path = ?1 WHERE path = '/gone/place/L_001.fits'", params![copy.to_str().unwrap()]).unwrap();
        let id2 = id_of(&conn, &virtual_path(&zip, "L_002.fits"));
        conn.execute(
            "UPDATE files SET path = ?1, archive_zip_path = NULL, archive_path_in_zip = NULL, archive_entry_crc32 = NULL WHERE id = ?2",
            params![tmp.path().join("unpacked").join("L_002.fits").to_str().unwrap(), id2],
        )
        .unwrap();
        conn.execute("INSERT INTO zip_released_entries (zip_id, entry_path) VALUES (?1, 'L_002.fits')", params![first.zip_id]).unwrap();

        crate::db::delete_scan_root(&conn, 1).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 0);
        assert_eq!(zip_archive_count(&conn), 0, "the zip is forgotten with its folder");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM zip_released_entries"), 0);

        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().to_str().unwrap()]).unwrap();
        let again = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!again.skipped_unchanged);
        assert_eq!(again.inserted.len(), 3, "every entry is cataloged again");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE archive_zip_path IS NOT NULL"), 3);
    }

    #[test]
    fn zip_without_frames_is_recorded_with_zero_entries() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(tmp.path(), "docs.zip", &[("readme.txt", b"hello hello hello".to_vec())], ArchiveCompression::Deflate);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(outcome.inserted.is_empty());
        let frames: i64 = conn
            .query_row("SELECT frame_entries FROM zip_archives WHERE path = ?1", params![zip.to_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(frames, 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 0);
        assert!(scan_zip(&conn, &zip, 1, false).unwrap().skipped_unchanged, "the next scan skips it by stat");
    }

    #[test]
    fn a_repointed_row_of_ours_gets_the_new_virtual_path_when_its_zip_moves() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let bytes = light(1);
        let zip = make_zip(tmp.path(), "ours.zip", &[("Lights/L_001.fits", bytes.clone())], ArchiveCompression::Store);
        let probe = tmp.path().join("probe.fits");
        std::fs::write(&probe, &bytes).unwrap();
        let (_, header) = parse_fits_with_header(&probe, 0).unwrap();
        // A row of ours that a newer capture pushed off its original path: files.path is the virtual path.
        let old_virtual = virtual_path(&zip, "Lights/L_001.fits");
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (500, ?1, 'L_001.fits', 1, '2025-01-01T00:00:00Z', 'FITS', ?2, 'Lights/L_001.fits')",
            params![old_virtual, zip.to_str().unwrap()],
        )
        .unwrap();
        crate::db::insert_fits_header(&conn, 500, &header).unwrap();
        conn.execute("INSERT INTO frames (file_id) VALUES (500)", []).unwrap();
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        conn.execute("UPDATE files SET archived_in_operation = 77 WHERE id = 500", []).unwrap();

        let moved = tmp.path().join("moved.zip");
        std::fs::rename(&zip, &moved).unwrap();
        let outcome = scan_zip(&conn, &moved, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        let (path, zip_col): (String, String) = conn
            .query_row("SELECT path, archive_zip_path FROM files WHERE id = 500", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(path, virtual_path(&moved, "Lights/L_001.fits"), "the virtual path follows the zip");
        assert_eq!(zip_col, moved.to_str().unwrap());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
    }

    #[test]
    fn a_scan_that_reads_a_zip_fills_the_entry_crc_of_rows_that_have_none() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "ours.zip",
            &[("Lights/L_001.fits", light(1)), ("Lights/L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &zip, 1, false).unwrap();
        let expected: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT archive_path_in_zip, archive_entry_crc32 FROM files ORDER BY archive_path_in_zip")
                .unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            rows
        };
        assert_eq!(expected.len(), 2);

        // Rows written before the CRC was recorded: no CRC; the zip's stat is stale so the scan reads it.
        conn.execute("UPDATE files SET archive_entry_crc32 = NULL", []).unwrap();
        conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!outcome.skipped_unchanged);
        assert_eq!(outcome.files_inserted, 0, "no new rows");

        let after: Vec<(String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT archive_path_in_zip, archive_entry_crc32 FROM files ORDER BY archive_path_in_zip")
                .unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            rows
        };
        assert_eq!(after, expected, "the CRCs come back from the directory");
    }

    fn zip_archive_count(conn: &Connection) -> i64 {
        count(conn, "SELECT COUNT(*) FROM zip_archives")
    }

    /// An `archive_operations` row for `created_by_operation` to point at.
    fn insert_operation(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO archive_operations (archive_root_path, compression, status, started_at)
             VALUES ('/archive', 'store', 'completed', '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn a_zip_renamed_and_renamed_back_keeps_one_identity_and_its_operation() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let a = make_zip(
            tmp.path(),
            "a.zip",
            &[("Lights/L_001.fits", light(1)), ("Lights/L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        scan_zip(&conn, &a, 1, false).unwrap();
        let op = insert_operation(&conn);
        conn.execute("UPDATE zip_archives SET created_by_operation = ?1", params![op]).unwrap();
        let id1 = id_of(&conn, &virtual_path(&a, "Lights/L_001.fits"));

        // Renamed in Finder: A -> B.
        let b = tmp.path().join("b.zip");
        std::fs::rename(&a, &b).unwrap();
        let outcome = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(outcome.adopted, 2);
        assert_eq!(zip_archive_count(&conn), 1, "the old zip's row moved, it is not left behind as a ghost");
        let (path, op_b): (String, Option<i64>) = conn
            .query_row("SELECT path, created_by_operation FROM zip_archives", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(path, b.to_str().unwrap());
        assert_eq!(op_b, Some(op), "the zip is still recognised as one of ours");

        // ... and back: B -> A.
        std::fs::rename(&b, &a).unwrap();
        let outcome = scan_zip(&conn, &a, 1, false).unwrap();
        assert!(!outcome.skipped_unchanged, "a stale row must not make the zip look unchanged");
        assert_eq!(outcome.adopted, 2);
        assert_eq!(zip_archive_count(&conn), 1);
        let (path, op_a): (String, Option<i64>) = conn
            .query_row("SELECT path, created_by_operation FROM zip_archives", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(path, a.to_str().unwrap());
        assert_eq!(op_a, Some(op));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
        assert_eq!(id_of(&conn, &virtual_path(&a, "Lights/L_001.fits")), id1);
        assert_eq!(
            count(&conn, &format!("SELECT COUNT(*) FROM files WHERE archive_zip_path = '{}'", a.to_str().unwrap())),
            2,
            "every row points at A again"
        );
    }

    #[test]
    fn an_old_zip_row_kept_for_rows_that_still_point_at_it_has_no_stat_so_a_rename_back_is_read() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let files = [("L_001.fits", light(1)), ("L_002.fits", light(2))];
        let a = make_zip(tmp.path(), "a.zip", &files, ArchiveCompression::Store);
        scan_zip(&conn, &a, 1, false).unwrap();

        // A is replaced by B, which holds only L_001: L_002's row still points at A.
        std::fs::remove_file(&a).unwrap();
        let b = make_zip(tmp.path(), "b.zip", &files[..1], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        assert_eq!(zip_archive_count(&conn), 2, "A's row stays for the row that still points at it");
        let (size, modified): (i64, String) = conn
            .query_row("SELECT size, modified_at FROM zip_archives WHERE path = ?1", params![a.to_str().unwrap()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((size, modified.as_str()), (0, ""), "no stat is kept for a zip that is gone");

        // A comes back under its name: it is read, not skipped as unchanged.
        let a_again = make_zip(tmp.path(), "a.zip", &files, ArchiveCompression::Store);
        assert_eq!(a_again, a);
        let outcome = scan_zip(&conn, &a, 1, false).unwrap();
        assert!(!outcome.skipped_unchanged);
    }

    #[test]
    fn an_entry_unpacked_before_the_zip_was_moved_is_not_added_again() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let a = make_zip(tmp.path(), "a.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let first = scan_zip(&conn, &a, 1, false).unwrap();
        // The user unpacked L_001 to disk through the app: a disk row, a released entry.
        let unpacked = tmp.path().join("unpacked").join("L_001.fits");
        std::fs::create_dir_all(unpacked.parent().unwrap()).unwrap();
        std::fs::write(&unpacked, light(1)).unwrap();
        let id1 = id_of(&conn, &virtual_path(&a, "L_001.fits"));
        conn.execute(
            "UPDATE files SET path = ?1, archive_zip_path = NULL, archive_path_in_zip = NULL, archive_entry_crc32 = NULL WHERE id = ?2",
            params![unpacked.to_str().unwrap(), id1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO zip_released_entries (zip_id, entry_path, released_to) VALUES (?1, 'L_001.fits', ?2)",
            params![first.zip_id, unpacked.to_str().unwrap()],
        )
        .unwrap();

        let b = tmp.path().join("b.zip");
        std::fs::rename(&a, &b).unwrap();
        let outcome = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1, "L_002 follows the zip");
        assert!(outcome.inserted.is_empty(), "L_001 was unpacked: it is not a new entry of the moved zip");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
        assert_eq!(zip_archive_count(&conn), 1);
        let released: i64 = count(
            &conn,
            "SELECT COUNT(*) FROM zip_released_entries r JOIN zip_archives z ON z.id = r.zip_id WHERE r.entry_path = 'L_001.fits'",
        );
        assert_eq!(released, 1, "the released entry belongs to the zip's new row");
        // And it stays released on the next change of the zip.
        let b = make_zip(
            tmp.path(),
            "b.zip",
            &[("L_001.fits", light(1)), ("L_002.fits", light(2)), ("L_003.fits", light(3))],
            ArchiveCompression::Store,
        );
        let again = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(again.inserted.len(), 1, "only L_003");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 3);
    }

    #[test]
    fn the_operation_follows_the_entries_the_renamed_zip_holds_and_not_the_ones_it_lost() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let files = [("L_001.fits", light(1)), ("L_002.fits", light(2))];
        let a = make_zip(tmp.path(), "a.zip", &files, ArchiveCompression::Store);
        let a_str = a.to_str().unwrap().to_string();
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        let op = insert_operation(&conn);
        // Rows as Move and ZIP leaves them: original path, zip markers, the operation.
        for (i, (entry, bytes)) in files.iter().enumerate() {
            let id = 500 + i as i64;
            let probe = tmp.path().join(format!("probe{i}.fits"));
            std::fs::write(&probe, bytes).unwrap();
            let (_, header) = parse_fits_with_header(&probe, 0).unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format, archived_in_operation, archive_zip_path, archive_path_in_zip)
                 VALUES (?1, ?2, ?3, 1, '2025-01-01T00:00:00Z', 'FITS', ?4, ?5, ?3)",
                params![id, format!("/data/original/{entry}"), entry, op, a_str],
            )
            .unwrap();
            crate::db::insert_fits_header(&conn, id, &header).unwrap();
            conn.execute("INSERT INTO frames (file_id) VALUES (?1)", params![id]).unwrap();
            conn.execute(
                "INSERT INTO archive_operation_files (operation_id, file_id, source_path, target_zip_path, target_path_in_zip, expected_hash, disposition, frame_role, file_size_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'h', 'move', 'light', 1)",
                params![op, id, format!("/data/original/{entry}"), a_str, entry],
            )
            .unwrap();
        }
        // The zip is replaced by one that holds only L_001.
        std::fs::remove_file(&a).unwrap();
        let b = make_zip(tmp.path(), "b.zip", &files[..1], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        let target_of = |entry: &str| -> String {
            conn.query_row(
                "SELECT target_zip_path FROM archive_operation_files WHERE target_path_in_zip = ?1",
                params![entry],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(target_of("L_001.fits"), b.to_str().unwrap(), "the held entry's operation file follows the zip");
        assert_eq!(target_of("L_002.fits"), a_str, "the lost entry's row still points at the old zip, and so does its operation file");
    }

    #[test]
    fn the_old_zip_row_stays_while_a_row_still_points_at_it() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let a = make_zip(tmp.path(), "a.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        scan_zip(&conn, &a, 1, false).unwrap();
        std::fs::remove_file(&a).unwrap();
        // The new zip holds only one of the two frames.
        let b = make_zip(tmp.path(), "b.zip", &[("L_001.fits", light(1))], ArchiveCompression::Store);
        let outcome = scan_zip(&conn, &b, 1, false).unwrap();
        assert_eq!(outcome.adopted, 1);
        assert_eq!(zip_archive_count(&conn), 2, "L_002 still belongs to the missing a.zip");
        assert_eq!(
            count(&conn, &format!("SELECT COUNT(*) FROM files WHERE archive_zip_path = '{}'", a.to_str().unwrap())),
            1
        );
    }

    #[test]
    fn a_backup_copy_of_the_zip_does_not_hide_the_row_of_the_renamed_one() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        std::fs::create_dir_all(tmp.path().join("backup")).unwrap();
        let entries = [("L_001.fits", light(1)), ("L_002.fits", light(2))];
        // The backup is scanned first, so its rows have the lower ids.
        let backup = make_zip(&tmp.path().join("backup"), "night.zip", &entries, ArchiveCompression::Store);
        scan_zip(&conn, &backup, 1, false).unwrap();
        let night = make_zip(tmp.path(), "night.zip", &entries, ArchiveCompression::Store);
        let dup = scan_zip(&conn, &night, 1, false).unwrap();
        assert_eq!(dup.inserted.len(), 2, "while the backup exists, the second copy is a duplicate");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 4);

        let moved = tmp.path().join("moved.zip");
        std::fs::rename(&night, &moved).unwrap();
        let outcome = scan_zip(&conn, &moved, 1, false).unwrap();
        assert_eq!(outcome.adopted, 2, "the rows of the zip that is gone are taken, not the backup's");
        assert!(outcome.inserted.is_empty());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 4, "no third copy");
        assert_eq!(
            count(&conn, &format!("SELECT COUNT(*) FROM files WHERE archive_zip_path = '{}'", backup.to_str().unwrap())),
            2,
            "the backup's rows are untouched"
        );
    }

    #[test]
    fn a_failed_entry_leaves_the_zip_unrecorded_so_the_next_scan_retries_it() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "n.zip",
            &[("Bad.fits", b"this is not a fits file at all".to_vec()), ("L_001.fits", light(1))],
            ArchiveCompression::Store,
        );
        let first = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(first.errors.len(), 1);
        let (size, modified): (i64, String) =
            conn.query_row("SELECT size, modified_at FROM zip_archives", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((size, modified.as_str()), (0, ""), "no real stat is recorded");

        let second = scan_zip(&conn, &zip, 1, false).unwrap();
        assert!(!second.skipped_unchanged, "the zip is read again");
        assert_eq!(second.errors.len(), 1, "and the entry is reported again");
        assert!(second.inserted.is_empty(), "L_001 is known and not duplicated");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);

        // A clean zip records its stat as before.
        let clean = make_zip(tmp.path(), "clean.zip", &[("L_009.fits", light(9))], ArchiveCompression::Store);
        scan_zip(&conn, &clean, 1, false).unwrap();
        let size: i64 = conn
            .query_row("SELECT size FROM zip_archives WHERE path = ?1", params![clean.to_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(size, std::fs::metadata(&clean).unwrap().len() as i64);
    }

    #[test]
    fn backslash_named_entries_are_not_cataloged() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let zip = make_zip(
            tmp.path(),
            "win.zip",
            &[("Lights\\L_001.fits", light(1)), ("Lights/L_002.fits", light(2))],
            ArchiveCompression::Store,
        );
        let listing = list_entries(&zip).unwrap();
        assert!(listing.skipped.iter().any(|(n, r)| n == "Lights\\L_001.fits" && *r == "backslash name"), "{:?}", listing.skipped);
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.inserted.len(), 1);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
    }
    #[test]
    fn our_own_rows_with_odd_entry_names_stay_known_and_present() {
        let tmp = TempDir::new().unwrap();
        let conn = db(tmp.path());
        let odd = "Lights: M33 /L_001.fits";
        let zip = make_zip(tmp.path(), "n.zip", &[(odd, light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        let zip_s = zip.to_str().unwrap().to_string();
        // The shape a Move/ZIP leaves: the original disk path (gone), the zip and the entry name from the object name.
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archived_in_operation, archive_zip_path, archive_path_in_zip)
             VALUES (700, '/gone/Lights: M33 /L_001.fits', 'L_001.fits', 10, '2026-01-01T00:00:00Z', 'FITS', 7, ?1, ?2)",
            params![zip_s, odd],
        )
        .unwrap();

        let mut presence = crate::zip_store::presence::ZipPresence::new();
        assert!(
            crate::zip_store::presence::row_is_present(&mut presence, "/gone/Lights: M33 /L_001.fits", Some(&zip_s), Some(odd)),
            "the zip lists the entry, the safety filter does not apply to presence"
        );

        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
        assert_eq!(outcome.inserted.len(), 1, "only L_002 is new: {:?}", outcome.inserted);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2, "the odd entry is not inserted again");
        let (path, zip_col, entry): (String, Option<String>, Option<String>) = conn
            .query_row("SELECT path, archive_zip_path, archive_path_in_zip FROM files WHERE id = 700", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((path.as_str(), zip_col.as_deref(), entry.as_deref()), ("/gone/Lights: M33 /L_001.fits", Some(zip_s.as_str()), Some(odd)));
        let frames: i64 = count(&conn, "SELECT frame_entries FROM zip_archives");
        assert_eq!(frames, 2, "the known odd entry counts as cataloged");
    }
}
