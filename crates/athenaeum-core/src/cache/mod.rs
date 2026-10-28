pub mod memory;
pub use memory::{CachedImage, MemoryImageCache};

use std::path::Path;
use std::time::UNIX_EPOCH;

/// Cache key for a rendered preview: `path:mtime_nanos:resolution`.
///
/// The mtime component makes in-place file changes (re-calibration, archive
/// restore, scanner re-parse) invalidate naturally — a plain `path:resolution`
/// key kept serving the stale JPEG until LRU eviction. Stat failure (file
/// deleted/disconnected) is returned to the caller so the handler can
/// fail fast before doing any work.
pub fn preview_cache_key(path: &Path, resolution: &str) -> std::io::Result<String> {
    let mtime_nanos = std::fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}:{}:{}", path.display(), mtime_nanos, resolution))
}

/// [`preview_cache_key`] for a catalog path. A zip-resident row is keyed by the catalog path plus the zip's
/// and the entry's identity (`zip_store::materialize::preview_identity`), so a cache hit needs no extraction
/// and two zips holding the same entry path never share a key; any other path is keyed as before. An
/// unreachable zip fails like a deleted file does (`NotFound`).
pub fn catalog_preview_cache_key(
    ctx: &crate::services::ServiceContext,
    catalog_path: &str,
    resolution: &str,
) -> std::io::Result<String> {
    match crate::zip_store::materialize::preview_identity(ctx, catalog_path) {
        Ok(Some(identity)) => Ok(format!("{catalog_path}:{identity}:{resolution}")),
        Ok(None) => preview_cache_key(Path::new(catalog_path), resolution),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_changes_when_file_is_touched() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("frame.fits");
        std::fs::write(&f, b"v1").unwrap();
        let k1 = preview_cache_key(&f, "preview").unwrap();

        // Rewrite with a strictly later mtime (filesystem tick).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&f, b"v2").unwrap();
        let k2 = preview_cache_key(&f, "preview").unwrap();

        assert_ne!(k1, k2, "in-place modification must change the cache key");
    }

    #[test]
    fn key_differs_per_resolution_and_errors_on_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("frame.fits");
        std::fs::write(&f, b"v1").unwrap();
        assert_ne!(
            preview_cache_key(&f, "preview").unwrap(),
            preview_cache_key(&f, "full").unwrap()
        );
        assert!(preview_cache_key(&dir.path().join("missing.fits"), "preview").is_err());
    }
}
