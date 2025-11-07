use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::xxh3_64;

use super::models::StretchParams;

/// Generate a cache filename using XXH3_64 hash
pub fn generate_cache_filename(file_path: &str, params: &StretchParams) -> String {
    let key = params.cache_key(file_path);
    let hash = xxh3_64(key.as_bytes());
    format!("{:016x}.jpg", hash)
}

/// Get the full path for a cache file
pub fn get_cache_file_path(cache_dir: &Path, cache_filename: &str) -> PathBuf {
    cache_dir.join(cache_filename)
}

/// Ensure cache directory exists
pub fn ensure_cache_dir(cache_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("Failed to create cache directory: {:?}", cache_dir))
}

/// Check if a cache file exists on disk
pub fn cache_file_exists(cache_dir: &Path, cache_filename: &str) -> bool {
    get_cache_file_path(cache_dir, cache_filename).exists()
}

/// Read cache file from disk
pub fn read_cache_file(cache_dir: &Path, cache_filename: &str) -> Result<Vec<u8>> {
    let path = get_cache_file_path(cache_dir, cache_filename);
    std::fs::read(&path)
        .with_context(|| format!("Failed to read cache file: {:?}", path))
}

/// Write cache file to disk
pub fn write_cache_file(
    cache_dir: &Path,
    cache_filename: &str,
    data: &[u8],
) -> Result<()> {
    ensure_cache_dir(cache_dir)?;
    let path = get_cache_file_path(cache_dir, cache_filename);
    std::fs::write(&path, data)
        .with_context(|| format!("Failed to write cache file: {:?}", path))
}

/// Delete cache file from disk
pub fn delete_cache_file(cache_dir: &Path, cache_filename: &str) -> Result<()> {
    let path = get_cache_file_path(cache_dir, cache_filename);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to delete cache file: {:?}", path))?;
    }
    Ok(())
}

/// Get file modification time
pub fn get_file_modified_time(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Failed to get metadata for: {:?}", path))?;

    let modified = metadata
        .modified()
        .context("Failed to get modification time")?;

    let datetime = chrono::DateTime::<chrono::Utc>::from(modified);
    Ok(datetime.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// Calculate total size of cache directory
pub fn calculate_cache_size(cache_dir: &Path) -> Result<u64> {
    let mut total_size = 0u64;

    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() && entry.path().extension() == Some(std::ffi::OsStr::new("jpg")) {
            total_size += metadata.len();
        }
    }

    Ok(total_size)
}

/// Clean up orphaned cache files (files not in database)
pub fn cleanup_orphaned_files(
    cache_dir: &Path,
    valid_filenames: &[String],
) -> Result<usize> {
    let mut deleted_count = 0;

    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension() == Some(std::ffi::OsStr::new("jpg")) {
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if !valid_filenames.contains(&filename.to_string()) {
                    std::fs::remove_file(&path)?;
                    deleted_count += 1;
                }
            }
        }
    }

    Ok(deleted_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_filename_generation() {
        let params1 = StretchParams::auto(0.25);
        let params2 = StretchParams::manual(850, 64000, 0.25);

        let file1 = generate_cache_filename("/path/to/file.fits", &params1);
        let file2 = generate_cache_filename("/path/to/file.fits", &params2);

        // Different parameters should produce different filenames
        assert_ne!(file1, file2);

        // Same parameters should produce same filename
        let file3 = generate_cache_filename("/path/to/file.fits", &params1);
        assert_eq!(file1, file3);
    }
}