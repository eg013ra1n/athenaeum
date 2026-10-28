//! Thin wrapper over the `zip` crate. Builds a single zip from a list of entries.

use crate::archive::models::ArchiveCompression;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
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

/// A zip built beside its target, not yet in place: `<target name>.<pid>-<n>.partial` (the naming of
/// `zip_store::reader::partial_path`). Dropping it removes the file; [`PartialZip::publish`] renames it
/// onto the target instead.
#[derive(Debug)]
pub struct PartialZip {
    path: PathBuf,
    armed: bool,
}

impl PartialZip {
    /// Where the finished, fsynced zip sits until it is published.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Renames the zip onto `target`, replacing what is there in one step (on Windows too: std's rename
    /// replaces an existing file). On error the partial is removed and `target` is untouched.
    pub fn publish(mut self, target: &Path) -> Result<()> {
        std::fs::rename(&self.path, target)
            .with_context(|| format!("failed to move {} onto {}", self.path.display(), target.display()))?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for PartialZip {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "could not remove a partial zip");
            }
        }
    }
}

/// Build a zip file at `zip_path` from the given entries.
///
/// The zip is built in a partial beside `zip_path` and renamed onto it only when it is complete, so an
/// existing zip there is never truncated: until the rename it is untouched, and after any failure it
/// still is. Caller is responsible for ensuring `zip_path`'s parent directory exists.
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
    build_zip_partial(zip_path, entries, compression, on_entry)?.publish(zip_path)
}

/// Builds the zip into a unique partial beside `target` and fsyncs it; the caller verifies it and then
/// [`PartialZip::publish`]es it. `target` itself is not touched. On any error the partial is removed.
pub fn build_zip_partial(
    target: &Path,
    entries: &[ZipEntry],
    compression: ArchiveCompression,
    on_entry: Option<&dyn Fn(usize, usize)>,
) -> Result<PartialZip> {
    let partial_path = crate::zip_store::reader::partial_path(target);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial_path)
        .with_context(|| format!("failed to create zip file {}", partial_path.display()))?;
    let partial = PartialZip { path: partial_path, armed: true };
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
            zw.write_all(&buf[..n])
                .with_context(|| format!("zip write failed for {}", entry.path_in_zip))?;
        }

        if let Some(cb) = on_entry {
            cb(idx + 1, total);
        }
    }

    let mut out = zw.finish().context("failed to finalize zip")?;
    out.flush().context("failed to flush zip")?;
    let file = out.into_inner().map_err(|e| e.into_error()).context("failed to flush zip")?;
    file.sync_all().with_context(|| format!("failed to sync {}", partial.path.display()))?;
    Ok(partial)
}

/// Removes the `<target name>.<pid>-<n>.partial` files [`build_zip_partial`] leaves when a build dies
/// with its process, beside `target`. Only names of exactly that pattern: a user's own `x.zip.partial`
/// is never ours. A partial whose process is still running is kept, except that this process's own id
/// counts as dead when `own_pid_is_stale` (rollback: a pid reused by a container's next run). Returns
/// how many were removed.
pub fn sweep_stale_partials_beside(target: &Path, own_pid_is_stale: bool) -> usize {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else { return 0 };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let own = std::process::id();
    let mut removed = 0;
    for f in rd.flatten() {
        let fname = f.file_name().to_string_lossy().into_owned();
        let Some(pid) = partial_pid(&fname, &prefix) else { continue };
        let stale = if pid == own { own_pid_is_stale } else { !crate::zip_store::cache::pid_alive(pid) };
        if !stale {
            continue;
        }
        match std::fs::remove_file(f.path()) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %f.path().display(), error = %e, "could not remove a stale partial zip");
            }
        }
    }
    removed
}

/// The process id in `<prefix><pid>-<n>.partial`; `None` for any other name.
fn partial_pid(file_name: &str, prefix: &str) -> Option<u32> {
    let stem = file_name.strip_prefix(prefix)?.strip_suffix(".partial")?;
    let (pid, n) = stem.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid) || !digits(n) {
        return None;
    }
    pid.parse().ok()
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
