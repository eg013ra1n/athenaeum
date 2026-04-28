//! Helpers for the per-operation staging directory.
//!
//! Layout: `<archive_root>/.athenaeum_staging/op_<operation_id>/<path-in-zip>`

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const STAGING_DIRNAME: &str = ".athenaeum_staging";

/// Compute the staging directory path for an operation.
pub fn staging_dir(archive_root: &Path, operation_id: i64) -> PathBuf {
    archive_root.join(STAGING_DIRNAME).join(format!("op_{}", operation_id))
}

/// Compute the staging file path for a given path-in-zip.
pub fn staging_file_path(archive_root: &Path, operation_id: i64, path_in_zip: &str) -> PathBuf {
    staging_dir(archive_root, operation_id).join(path_in_zip)
}

/// Create the staging directory tree (idempotent).
pub fn ensure_staging_dir(archive_root: &Path, operation_id: i64) -> Result<PathBuf> {
    let dir = staging_dir(archive_root, operation_id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create staging dir {}", dir.display()))?;
    Ok(dir)
}

/// Copy a source file into staging, creating any intermediate directories.
/// Returns the destination path.
pub fn copy_into_staging(
    archive_root: &Path,
    operation_id: i64,
    source_path: &Path,
    path_in_zip: &str,
) -> Result<PathBuf> {
    let dest = staging_file_path(archive_root, operation_id, path_in_zip);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create staging subdir {}", parent.display()))?;
    }
    std::fs::copy(source_path, &dest)
        .with_context(|| format!("failed to copy {} into staging", source_path.display()))?;
    Ok(dest)
}

/// Delete the entire staging directory for an operation. No-op if missing.
pub fn cleanup_staging(archive_root: &Path, operation_id: i64) -> Result<()> {
    let dir = staging_dir(archive_root, operation_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove staging dir {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn staging_paths() {
        let root = Path::new("/arch");
        assert_eq!(
            staging_dir(root, 7),
            PathBuf::from("/arch/.athenaeum_staging/op_7")
        );
        assert_eq!(
            staging_file_path(root, 7, "Lights/M31/x.fits"),
            PathBuf::from("/arch/.athenaeum_staging/op_7/Lights/M31/x.fits")
        );
    }

    #[test]
    fn ensure_creates_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = ensure_staging_dir(tmp.path(), 1).unwrap();
        assert!(dir.exists());
        // Idempotent
        ensure_staging_dir(tmp.path(), 1).unwrap();
    }

    #[test]
    fn copy_creates_subdirs() {
        let tmp = TempDir::new().unwrap();
        let arch = tmp.path().join("arch");
        std::fs::create_dir_all(&arch).unwrap();
        let src = tmp.path().join("src.fits");
        std::fs::write(&src, b"hello").unwrap();

        let dest = copy_into_staging(&arch, 5, &src, "Lights/M31/x.fits").unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn cleanup_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        cleanup_staging(tmp.path(), 99).unwrap(); // doesn't exist
        ensure_staging_dir(tmp.path(), 99).unwrap();
        cleanup_staging(tmp.path(), 99).unwrap();
        cleanup_staging(tmp.path(), 99).unwrap(); // again, idempotent
    }
}
