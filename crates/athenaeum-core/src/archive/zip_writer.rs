//! Thin wrapper over the `zip` crate. Builds a single zip from a list of entries.

use crate::archive::models::ArchiveCompression;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// One file to add to the zip.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    /// Source file on disk (will be read).
    pub source_path: PathBuf,
    /// Path inside the zip (forward slashes).
    pub path_in_zip: String,
}

/// Build a zip file at `zip_path` from the given entries.
///
/// Caller is responsible for ensuring `zip_path`'s parent directory exists.
/// Overwrites any existing file at `zip_path`.
pub fn build_zip(zip_path: &Path, entries: &[ZipEntry], compression: ArchiveCompression) -> Result<()> {
    build_zip_with_progress(zip_path, entries, compression, None)
}

/// Variant of `build_zip` that calls `on_entry(idx_done, total)` after each
/// entry is fully written, where `idx_done` ranges 1..=total. Useful for
/// emitting per-entry progress to the UI during a long-running zip build.
pub fn build_zip_with_progress(
    zip_path: &Path,
    entries: &[ZipEntry],
    compression: ArchiveCompression,
    on_entry: Option<&dyn Fn(usize, usize)>,
) -> Result<()> {
    let file = File::create(zip_path)
        .with_context(|| format!("failed to create zip file {}", zip_path.display()))?;
    let mut zw = ZipWriter::new(BufWriter::new(file));

    let method = match compression {
        ArchiveCompression::Store => CompressionMethod::Stored,
        ArchiveCompression::Deflate => CompressionMethod::Deflated,
    };
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true); // safe even for sub-4GB files; required for >4GB

    let mut buf = vec![0u8; 64 * 1024];
    let total = entries.len();

    for (idx, entry) in entries.iter().enumerate() {
        zw.start_file(&entry.path_in_zip, options)
            .with_context(|| format!("zip start_file failed for {}", entry.path_in_zip))?;

        let f = File::open(&entry.source_path)
            .with_context(|| format!("failed to open {} for zipping", entry.source_path.display()))?;
        let mut reader = BufReader::new(f);
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            use std::io::Write;
            zw.write_all(&buf[..n])
                .with_context(|| format!("zip write failed for {}", entry.path_in_zip))?;
        }

        if let Some(cb) = on_entry {
            cb(idx + 1, total);
        }
    }

    zw.finish().context("failed to finalize zip")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_zip_stores_files_correctly() {
        let tmp = TempDir::new().unwrap();
        let src1 = tmp.path().join("a.fits");
        let src2 = tmp.path().join("b.fits");
        std::fs::write(&src1, b"file-a-content").unwrap();
        std::fs::write(&src2, b"file-b-content").unwrap();

        let zip_path = tmp.path().join("out.zip");
        let entries = vec![
            ZipEntry { source_path: src1.clone(), path_in_zip: "Lights/a.fits".into() },
            ZipEntry { source_path: src2.clone(), path_in_zip: "Lights/sub/b.fits".into() },
        ];

        build_zip(&zip_path, &entries, ArchiveCompression::Store).unwrap();
        assert!(zip_path.exists());

        // Read back and verify contents.
        let f = File::open(&zip_path).unwrap();
        let mut zr = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        assert_eq!(zr.len(), 2);

        let mut by_name = std::collections::HashMap::new();
        for i in 0..zr.len() {
            let mut entry = zr.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut data = String::new();
            entry.read_to_string(&mut data).unwrap();
            by_name.insert(name, data);
        }
        assert_eq!(by_name.get("Lights/a.fits").unwrap(), "file-a-content");
        assert_eq!(by_name.get("Lights/sub/b.fits").unwrap(), "file-b-content");
    }

    #[test]
    fn build_zip_with_deflate() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("a.fits");
        std::fs::write(&src, vec![b'x'; 4096]).unwrap();
        let zip_path = tmp.path().join("out.zip");

        build_zip(
            &zip_path,
            &[ZipEntry { source_path: src, path_in_zip: "x".into() }],
            ArchiveCompression::Deflate,
        ).unwrap();

        // Deflate should produce a smaller zip than the source.
        let zsz = std::fs::metadata(&zip_path).unwrap().len();
        assert!(zsz < 4096, "expected compressed zip to be smaller than 4096 bytes, got {}", zsz);
    }

    #[test]
    fn build_zip_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("out.zip");
        std::fs::write(&zip_path, b"old garbage").unwrap();

        let src = tmp.path().join("a.fits");
        std::fs::write(&src, b"hello").unwrap();
        build_zip(
            &zip_path,
            &[ZipEntry { source_path: src, path_in_zip: "a.fits".into() }],
            ArchiveCompression::Store,
        ).unwrap();

        // Should be a valid zip now, not garbage.
        let f = File::open(&zip_path).unwrap();
        let zr = zip::ZipArchive::new(BufReader::new(f)).unwrap();
        assert_eq!(zr.len(), 1);
    }
}
