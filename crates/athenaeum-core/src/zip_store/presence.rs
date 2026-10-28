//! Presence of catalog rows: a plain row is present when its file exists; a zip-resident row is
//! present when its zip exists and lists the entry. A missing zip makes its rows missing and
//! never deletes them. Every missing-files site decides through [`row_is_present`], so the
//! hosts cannot drift.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rayon::prelude::*;

use super::reader::list_entry_names;

/// Per call, reads each distinct zip's raw directory names once (no safety filter, no local headers).
#[derive(Debug, Default)]
pub struct ZipPresence {
    /// zip path → the entry names its directory holds, or `None` when the zip is missing or unreadable.
    cache: HashMap<String, Option<HashSet<String>>>,
}

impl ZipPresence {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when `zip` exists, can be read, and lists `entry`.
    pub fn is_present(&mut self, zip: &str, entry: &str) -> bool {
        if !self.cache.contains_key(zip) {
            let listed = Self::open(zip);
            self.cache.insert(zip.to_string(), listed);
        }
        match self.cache.get(zip) {
            Some(Some(entries)) => entries.contains(entry),
            _ => false,
        }
    }

    fn open(zip: &str) -> Option<HashSet<String>> {
        let path = Path::new(zip);
        // A zip that is not there is the normal offline case (an unmounted share): not an error.
        match path.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(zip_path = %zip, "zip not present");
                return None;
            }
            Err(e) => {
                tracing::warn!(zip_path = %zip, error = %e, "zip presence: cannot stat the zip");
                return None;
            }
        }
        match list_entry_names(path) {
            Ok(names) => Some(names),
            Err(e) => {
                tracing::warn!(zip_path = %zip, error = %e, "zip presence: cannot read the zip directory");
                None
            }
        }
    }
}

/// The one presence rule. A row with a zip and an entry is present when [`ZipPresence`] says so;
/// any other row is present when its file exists.
pub fn row_is_present(
    presence: &mut ZipPresence,
    path: &str,
    archive_zip_path: Option<&str>,
    archive_path_in_zip: Option<&str>,
) -> bool {
    match (archive_zip_path, archive_path_in_zip) {
        (Some(zip), Some(entry)) => presence.is_present(zip, entry),
        _ => Path::new(path).exists(),
    }
}

/// One row to check: `(path, archive_zip_path, archive_path_in_zip)`.
pub type PresenceKey<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

/// [`row_is_present`] over many rows, one flag per item in order. Disk rows are checked in
/// parallel (I/O bound); zip rows share one [`ZipPresence`], so each zip's directory is read once.
pub fn present_flags<T: Sync>(items: &[T], key: impl Fn(&T) -> PresenceKey<'_> + Sync) -> Vec<bool> {
    let mut flags = vec![false; items.len()];
    let disk: Vec<(usize, bool)> = items
        .par_iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let (path, zip, entry) = key(item);
            if zip.is_some() && entry.is_some() {
                None
            } else {
                Some((i, Path::new(path).exists()))
            }
        })
        .collect();
    for (i, present) in disk {
        flags[i] = present;
    }
    let mut presence = ZipPresence::new();
    for (i, item) in items.iter().enumerate() {
        let (path, zip, entry) = key(item);
        if zip.is_some() && entry.is_some() {
            flags[i] = row_is_present(&mut presence, path, zip, entry);
        }
    }
    flags
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use crate::api::scan_roots::{add_scan_root, check_missing_files_in_scan_root};
    use crate::archive::models::ArchiveCompression;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use crate::api::PathPolicy;
    use crate::services::ServiceContext;
    use std::path::PathBuf;

    fn make_zip(dir: &Path, name: &str, entries: &[&str]) -> PathBuf {
        let src_guard = tempfile::TempDir::new().unwrap();
        let list: Vec<ZipEntry> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let p = src_guard.path().join(format!("s{i}"));
                std::fs::write(&p, vec![i as u8; 64]).unwrap();
                ZipEntry { source_path: p, path_in_zip: (*entry).to_string() }
            })
            .collect();
        let zip = dir.join(name);
        build_zip(&zip, &list, ArchiveCompression::Store).unwrap();
        zip
    }

    #[test]
    fn zip_resident_row_is_present_while_its_zip_lists_the_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), "n.zip", &["Lights/L_001.fits"]);
        let zip_s = zip.to_string_lossy().to_string();
        let virt = format!("{zip_s}{}Lights{}L_001.fits", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);

        let mut p = ZipPresence::new();
        // The virtual path does not exist on disk; the zip decides.
        assert!(row_is_present(&mut p, &virt, Some(&zip_s), Some("Lights/L_001.fits")));
        assert!(!row_is_present(&mut p, &virt, Some(&zip_s), Some("Lights/L_002.fits")), "entry not listed");
        // A plain row still uses the disk.
        let plain = tmp.path().join("plain.fits");
        assert!(!row_is_present(&mut p, plain.to_str().unwrap(), None, None));
        std::fs::write(&plain, b"x").unwrap();
        assert!(row_is_present(&mut p, plain.to_str().unwrap(), None, None));
    }

    #[test]
    fn present_flags_keeps_order_and_reads_each_zip_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), "n.zip", &["a.fits", "b.fits"]);
        let zip_s = zip.to_string_lossy().to_string();
        let plain = tmp.path().join("p.fits");
        std::fs::write(&plain, b"x").unwrap();
        let plain_s = plain.to_string_lossy().to_string();
        let rows: Vec<(String, Option<String>, Option<String>)> = vec![
            ("v1".into(), Some(zip_s.clone()), Some("a.fits".into())),
            (plain_s.clone(), None, None),
            ("v2".into(), Some(zip_s.clone()), Some("gone.fits".into())),
            ("/no/such/file.fits".into(), None, None),
            ("v3".into(), Some(zip_s), Some("b.fits".into())),
        ];
        let flags = present_flags(&rows, |(p, z, e)| (p.as_str(), z.as_deref(), e.as_deref()));
        assert_eq!(flags, vec![true, true, false, false, true]);
    }

    #[test]
    fn zip_resident_row_is_missing_when_the_zip_is_gone_and_is_never_deleted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let root_s = crate::test_support::canonical_path(&root).to_string_lossy().to_string();
        let added = add_scan_root(&ctx, root_s.clone(), &PathPolicy::AllowAll, None).unwrap();
        let root_id = added.id.unwrap();

        let zip = make_zip(Path::new(&root_s), "n.zip", &["L_001.fits", "L_002.fits"]);
        let zip_s = zip.to_string_lossy().to_string();
        let sep = std::path::MAIN_SEPARATOR;
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for (entry, archived_op) in [("L_001.fits", None), ("L_002.fits", Some(7))] {
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format,
                                        archived_in_operation, archive_zip_path, archive_path_in_zip)
                     VALUES (?1, ?2, 64, '2026-01-01T00:00:00Z', 'FITS', ?3, ?4, ?2)",
                    rusqlite::params![format!("{zip_s}{sep}{entry}"), entry, archived_op, zip_s],
                )
                .unwrap();
            }
            // Listed in neither: an entry the zip does not hold.
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
                 VALUES (?1, 'L_999.fits', 64, '2026-01-01T00:00:00Z', 'FITS', ?2, 'L_999.fits')",
                rusqlite::params![format!("{zip_s}{sep}L_999.fits"), zip_s],
            )
            .unwrap();
        }

        let missing = check_missing_files_in_scan_root(&ctx, root_id, &crate::events::NullEmitter).unwrap();
        let names: Vec<&str> = missing.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["L_999.fits"], "zip present: only the unlisted entry is missing");

        // The zip goes away (an unmounted share): every row of it is missing, none is deleted.
        std::fs::remove_file(&zip).unwrap();
        let missing = check_missing_files_in_scan_root(&ctx, root_id, &crate::events::NullEmitter).unwrap();
        let mut names: Vec<&str> = missing.iter().map(|f| f.filename.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["L_001.fits", "L_002.fits", "L_999.fits"]);
        let db = ctx.db.get().unwrap();
        let rows: i64 = db.conn().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 3, "missing never deletes");
    }
    #[test]
    fn move_shaped_row_follows_the_zip_not_its_original_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let root = tmp.path().join("astro");
        std::fs::create_dir_all(&root).unwrap();
        let root_s = crate::test_support::canonical_path(&root).to_string_lossy().to_string();
        let root_id = add_scan_root(&ctx, root_s.clone(), &PathPolicy::AllowAll, None).unwrap().id.unwrap();

        // Entry names as path_layout builds them: not sanitised, so ':' and a trailing space survive.
        let odd = "Lights: M33 /L_001.fits";
        let zip = make_zip(Path::new(&root_s), "n.zip", &[odd]);
        let zip_s = zip.to_string_lossy().to_string();
        // The row keeps its ORIGINAL disk path (the file is gone from it) plus the operation and the zip.
        let original = format!("{root_s}{}Lights: M33 {}L_001.fits", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
        {
            let db = ctx.db.get().unwrap();
            db.conn()
                .execute(
                    "INSERT INTO files (path, filename, size, modified_at, format,
                                        archived_in_operation, archive_zip_path, archive_path_in_zip)
                     VALUES (?1, 'L_001.fits', 64, '2026-01-01T00:00:00Z', 'FITS', 7, ?2, ?3)",
                    rusqlite::params![original, zip_s, odd],
                )
                .unwrap();
        }
        assert!(!Path::new(&original).exists());

        let missing = check_missing_files_in_scan_root(&ctx, root_id, &crate::events::NullEmitter).unwrap();
        assert!(missing.is_empty(), "the zip holds the entry: {missing:?}");

        // A file appearing at the original path does not matter either way: the zip decides.
        std::fs::remove_file(&zip).unwrap();
        std::fs::create_dir_all(Path::new(&original).parent().unwrap()).unwrap();
        std::fs::write(&original, b"x").unwrap();
        let missing = check_missing_files_in_scan_root(&ctx, root_id, &crate::events::NullEmitter).unwrap();
        assert_eq!(missing.len(), 1, "the zip is gone, so the row is missing even though a file sits at its path");
        let db = ctx.db.get().unwrap();
        let rows: i64 = db.conn().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "missing never deletes");
    }
}
