//! The scanner's zip pass: catalog the FITS/XISF entries of a `.zip` from its directory and the
//! first bytes of each entry, never reading a whole entry.
//!
//! Order per zip (see [`scan_zip`]): stat (equal to the `zip_archives` row → skip without opening),
//! directory, then the entries not yet owned by a row, through one 4 KiB-buffered header pass. A new
//! entry either takes over a catalog row that moved into the zip ([`find_moved_row`]) or becomes a
//! new row with a virtual path. Rows of entries that vanished from a changed zip are left alone:
//! nothing here deletes.

use crate::db::{insert_file, insert_fits_header, insert_frame, SavepointGuard};
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_parser::{calibrated_light_identity, parse_fits_header_bytes, parse_xisf_header_bytes};
use crate::models::{File, FileFormat, ImageType};
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

/// The row a new file at `new_path` with this header fingerprint should take over, if any: the
/// scanner's moved-file rule, shared by the disk walkers (`for_zip_entry = false`) and the zip pass
/// (`true`).
///
/// Same query for both: a row with the fingerprint at another path (first row wins). A disk file
/// never matches a zip-resident row (`archive_zip_path IS NULL`), so a manually unzipped copy of an
/// archived frame is a new row, not a takeover. The row is a move only when its old location is
/// gone: for a disk row its file, for a zip-resident row (zip entries only) its zip. If the old
/// location is still there the new file is a duplicate; if it sits under an unavailable scan root
/// (unmounted volume) it is not a move either. Both return `None`.
///
/// Returns the row's id and its `files.path`.
pub(crate) fn find_moved_row(
    conn: &Connection,
    fingerprint: &str,
    new_path: &str,
    for_zip_entry: bool,
) -> Result<Option<(i64, String)>> {
    let disk_only = if for_zip_entry { "" } else { " AND f.archive_zip_path IS NULL" };
    let sql = format!(
        "SELECT f.id, f.path, f.archive_zip_path FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE fh.header_fingerprint = ?1 AND f.path != ?2{disk_only}"
    );
    let found: Option<(i64, String, Option<String>)> = conn
        .query_row(&sql, params![fingerprint, new_path], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?;
    let Some((file_id, old_path, row_zip)) = found else {
        return Ok(None);
    };
    // What has to be gone for this to be a move: the row's file, or (a zip-resident row) its zip.
    let old_location = match row_zip {
        Some(zip) if for_zip_entry => zip,
        _ => old_path.clone(),
    };
    if Path::new(&old_location).exists() {
        tracing::debug!(
            old_path = %old_path,
            path = %new_path,
            file_id,
            "duplicate file detected (identical header)"
        );
        return Ok(None);
    }
    if let Some(offline_root) = path_under_unavailable_scan_root(conn, &old_location)? {
        // The old location is missing only because its scan root is offline (unmounted volume): not a
        // move. Falling through keeps the still-valid original untouched.
        tracing::warn!(
            old_path = %old_path,
            old_location = %old_location,
            path = %new_path,
            offline_root = %offline_root,
            file_id,
            "skipping move-detection: old path's scan root is unavailable"
        );
        return Ok(None);
    }
    Ok(Some((file_id, old_path)))
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

enum EntryOutcome {
    /// Calibrated light or stacking artifact: never cataloged.
    Skipped,
    Inserted { frame_id: i64, imagetyp: Option<ImageType> },
    Adopted,
    Updated,
}

/// Catalog one entry whose header bytes were read. `known_id` is the row that already owns the
/// entry (its CRC changed), or `None` for an entry no row owns.
#[allow(clippy::too_many_arguments)]
fn catalog_entry(
    conn: &Connection,
    zip: &Path,
    zip_str: &str,
    entry: &ZipEntryInfo,
    known_id: Option<i64>,
    header_bytes: &[u8],
    zip_modified: DateTime<Utc>,
    root_id: i64,
    unique_camera: bool,
) -> Result<EntryOutcome> {
    let vpath = virtual_path(zip, &entry.path);
    let format = if entry.path.to_ascii_lowercase().ends_with(".xisf") { FileFormat::XISF } else { FileFormat::FITS };
    let (mut frame, header) = match format {
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
        return Ok(EntryOutcome::Updated);
    }

    // Never cataloged, the same rules as a disk file (`process_file`).
    let keys = parse_stored_header_keys(format.clone(), &header);
    if calibrated_light_identity(&keys).is_some() {
        tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "calibrated artifact (CALSTAT+ATH_CSRC) — never cataloged");
        return Ok(EntryOutcome::Skipped);
    }
    if keys.contains_key("ATH_REG") || keys.contains_key("ATH_STK") {
        tracing::debug!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, "stacking artifact (ATH_REG/ATH_STK) — never cataloged");
        return Ok(EntryOutcome::Skipped);
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
        let sp = SavepointGuard::new(conn, "scan_zip_adopt")?;
        // A row of Move and ZIP's own keeps its original `files.path` (the restore target) and
        // its camera suffix: only its zip markers follow the zip. Any other row becomes a foreign
        // zip-resident row.
        let ours: bool = conn.query_row(
            "SELECT archived_in_operation IS NOT NULL FROM files WHERE id = ?1",
            params![file_id],
            |r| r.get(0),
        )?;
        if ours {
            conn.execute(
                "UPDATE files SET archive_zip_path = ?1, archive_path_in_zip = ?2, archive_entry_crc32 = ?3 WHERE id = ?4",
                params![zip_str, entry.path, crc, file_id],
            )?;
        } else {
            conn.execute(
                "UPDATE files SET path = ?1, filename = ?2, size = ?3, modified_at = ?4,
                        archive_zip_path = ?5, archive_path_in_zip = ?6, archive_entry_crc32 = ?7
                 WHERE id = ?8",
                params![vpath, filename, size, modified.to_rfc3339(), zip_str, entry.path, crc, file_id],
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
        return Ok(EntryOutcome::Adopted);
    }

    // A new row: files + frames + fits_header all commit or none do.
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

/// Catalog the frames inside one zip. The zip's stat is compared first; an unchanged zip is not
/// opened. Otherwise: read the directory; entries already owned by a row (`archive_zip_path` +
/// `archive_path_in_zip`) are known, and re-parsed in place only when their CRC changed; entries
/// in `zip_released_entries` are skipped; the rest go through one header pass and are adopted by a
/// moved row or inserted. The whole zip is one transaction (one savepoint per entry inside it).
///
/// A bad entry is reported in [`ZipScanOutcome::errors`] and the rest of the zip is still
/// cataloged; a zip that cannot be opened is an `Err` and leaves no `zip_archives` row, so the
/// next scan retries it.
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
    if let Some((zip_id, db_size, db_modified)) = &known_zip {
        if *db_size == size && *db_modified == modified_str {
            return Ok(ZipScanOutcome { zip_id: *zip_id, skipped_unchanged: true, ..Default::default() });
        }
    }

    let listing = reader::list_entries(zip).with_context(|| format!("read the directory of {}", zip.display()))?;
    let mut outcome = ZipScanOutcome { bytes_read: listing.bytes_read, ..Default::default() };

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
    let released: HashSet<String> = match &known_zip {
        Some((zip_id, _, _)) => {
            let mut stmt = conn.prepare("SELECT entry_path FROM zip_released_entries WHERE zip_id = ?1")?;
            let rows = stmt.query_map(params![zip_id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<HashSet<_>>>()?
        }
        None => HashSet::new(),
    };

    // Frames the zip holds now that a row owns after this scan; starts with the untouched known ones.
    let mut cataloged = 0usize;
    let mut to_read: Vec<(&ZipEntryInfo, Option<i64>)> = Vec::new();
    for entry in listing.entries.iter().filter(|e| e.is_frame) {
        if released.contains(&entry.path) {
            continue;
        }
        match known.get(&entry.path) {
            Some((id, Some(crc))) if *crc != i64::from(entry.crc32) => to_read.push((entry, Some(*id))),
            Some(_) => cataloged += 1,
            None => to_read.push((entry, None)),
        }
    }

    // One open of the zip, 4 KiB buffer, for every header still needed.
    let names: Vec<&str> = to_read.iter().map(|(e, _)| e.path.as_str()).collect();
    let headers = if names.is_empty() {
        Vec::new()
    } else {
        let (headers, header_bytes) =
            reader::read_headers(zip, &names).with_context(|| format!("read entry headers of {}", zip.display()))?;
        outcome.bytes_read += header_bytes;
        headers
    };

    let outer = SavepointGuard::new(conn, "scan_zip")?;
    for ((entry, known_id), (_, header)) in to_read.iter().zip(headers) {
        let vpath = virtual_path(zip, &entry.path);
        let result = match header {
            Ok(bytes) => catalog_entry(conn, zip, &zip_str, entry, *known_id, &bytes, zip_modified, root_id, unique_camera),
            Err(e) => Err(e),
        };
        match result {
            Ok(EntryOutcome::Skipped) => {}
            Ok(EntryOutcome::Inserted { frame_id, imagetyp }) => {
                cataloged += 1;
                outcome.files_inserted += 1;
                if let Some(t) = imagetyp {
                    outcome.inserted.push((frame_id, t));
                }
            }
            Ok(EntryOutcome::Adopted) => {
                cataloged += 1;
                outcome.adopted += 1;
            }
            Ok(EntryOutcome::Updated) => cataloged += 1,
            Err(e) => {
                tracing::warn!(root_id, zip_path = %zip.display(), zip_entry = %entry.path, error = %format!("{e:#}"), "zip entry not cataloged");
                outcome.errors.push(format!("{vpath}: {e:#}"));
                if known_id.is_some() {
                    cataloged += 1;
                }
            }
        }
    }

    conn.execute(
        "INSERT INTO zip_archives (path, size, modified_at, frame_entries, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, modified_at = excluded.modified_at,
                frame_entries = excluded.frame_entries, scanned_at = excluded.scanned_at",
        params![zip_str, size, modified_str, cataloged as i64, Utc::now().to_rfc3339()],
    )?;
    outcome.zip_id = conn.query_row("SELECT id FROM zip_archives WHERE path = ?1", params![zip_str], |r| r.get(0))?;
    outer.commit()?;

    tracing::info!(
        root_id,
        zip_path = %zip.display(),
        frames = cataloged,
        inserted = outcome.files_inserted,
        adopted = outcome.adopted,
        bytes_read = outcome.bytes_read,
        "zip scanned"
    );
    Ok(outcome)
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
        let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
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
        conn.execute("INSERT INTO zip_released_entries (zip_id, entry_path) VALUES (?1, 'L_001.fits')", params![first.zip_id]).unwrap();

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
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE path = '/unpacked/L_001.fits'"), 1);
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
}
