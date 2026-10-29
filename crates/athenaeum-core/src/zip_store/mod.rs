//! Zip I/O for the catalog: no other module opens a zip.
//!
//! `reader` reads a zip's directory and the first bytes of its entries (the metadata scan) and
//! extracts one entry to a local file. `cache` keeps extracted entries in a size-limited directory and
//! `materialize` turns catalog paths into local paths through it. `place` puts an entry at a permanent path
//! without ever replacing a file. Zips are read-only here.

pub mod cache;
pub mod catalog;
pub(crate) mod fs;
pub mod materialize;
pub mod place;
pub mod presence;
pub mod reader;
pub mod scan;

/// Fixtures for the consumers' tests: put cataloged disk files into a zip, the way the scanner or Move and ZIP
/// leaves their rows.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use rusqlite::Connection;

    use crate::archive::models::ArchiveCompression;
    use crate::archive::zip_writer::{build_zip, ZipEntry};

    /// How the rows look afterwards.
    #[derive(Clone, Copy)]
    pub(crate) enum Shape {
        /// A foreign zip's rows: `files.path` is the virtual path.
        Foreign,
        /// Our own Move and ZIP: `files.path` keeps the original path, `archived_in_operation` is set.
        Own { operation: i64 },
    }

    /// Zips the files of `file_ids` into `zip` (entries named by file name, stored), points their rows at the
    /// zip with the entry CRC32 and deletes the disk files. Returns each row's catalog path in order.
    pub(crate) fn zip_files(conn: &Connection, file_ids: &[i64], zip: &Path, shape: Shape) -> Vec<String> {
        let rows: Vec<(i64, String, String)> = file_ids
            .iter()
            .map(|&id| {
                conn.query_row("SELECT id, path, filename FROM files WHERE id = ?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap()
            })
            .collect();
        let entries: Vec<ZipEntry> = rows
            .iter()
            .map(|(_, path, name)| ZipEntry { source_path: path.into(), path_in_zip: name.clone() })
            .collect();
        build_zip(zip, &entries, ArchiveCompression::Store).unwrap();
        let zip_s = zip.to_string_lossy().into_owned();
        let mut paths = Vec::new();
        for (id, path, name) in rows {
            let crc = crc32fast::hash(&std::fs::read(&path).unwrap());
            let (new_path, op) = match shape {
                Shape::Foreign => (crate::zip_store::scan::virtual_path(zip, &name), None),
                Shape::Own { operation } => (path.clone(), Some(operation)),
            };
            conn.execute(
                "UPDATE files SET path = ?1, archive_zip_path = ?2, archive_path_in_zip = ?3,
                        archive_entry_crc32 = ?4, archived_in_operation = ?5 WHERE id = ?6",
                rusqlite::params![new_path, zip_s, name, i64::from(crc), op, id],
            )
            .unwrap();
            std::fs::remove_file(&path).unwrap();
            paths.push(new_path);
        }
        paths
    }
}
