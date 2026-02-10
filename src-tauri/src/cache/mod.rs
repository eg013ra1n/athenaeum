// Cache module is work-in-progress functionality
#[allow(dead_code)]
pub mod database;
#[allow(dead_code)]
pub mod models;
#[allow(dead_code)]
pub mod storage;
pub mod memory;

use anyhow::{Context, Result};
use chrono;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use rayon;

use crate::models::File;
use crate::rustafits_processor::{self, Resolution};
use crate::settings::SettingsManager;

pub use models::{
    CacheEntry, CacheStats, StretchMode,
    StretchParams,
};
pub use memory::{MemoryImageCache, CachedRawImage};

use database::*;
use storage::*;

const CACHE_VERSION: &str = "v1";

/// Cache manager for handling disk-based image cache
#[allow(dead_code)]
pub struct CacheManager {
    cache_dir: PathBuf,
    cache_db: Arc<Mutex<Connection>>,
    settings: Arc<SettingsManager>,
    pool: Arc<rayon::ThreadPool>,
}

#[allow(dead_code)]
impl CacheManager {
    /// Create a new cache manager
    pub fn new(app_dir: &Path, settings: Arc<SettingsManager>, pool: Arc<rayon::ThreadPool>) -> Result<Self> {
        let cache_dir = app_dir.join("cache").join("previews");
        println!("📁 Cache directory: {:?}", cache_dir);
        ensure_cache_dir(&cache_dir)?;

        let cache_db_path = app_dir.join("cache").join("cache.db");
        println!("🗄️  Cache database: {:?}", cache_db_path);
        let cache_db = init_cache_db(&cache_db_path)?;

        println!("✅ Cache manager created successfully");
        Ok(Self {
            cache_dir,
            cache_db: Arc::new(Mutex::new(cache_db)),
            settings,
            pool,
        })
    }

    /// Get cached image or create if not exists
    pub async fn get_or_create(
        &self,
        file: &File,
        file_path: &Path,
        stretch_params: &StretchParams,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        // Try to get from cache first
        if let Some(data) = self.get_cached(file, stretch_params).await? {
            return Ok(data);
        }

        // Cache miss - create new entry
        self.create_cache_entry(file, file_path, stretch_params, quality).await
    }

    /// Get cached image if it exists and is valid
    pub async fn get_cached(
        &self,
        file: &File,
        stretch_params: &StretchParams,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.cache_db.lock().unwrap();

        // Look for cache entry in database
        let entry = find_cache_entry(
            &conn,
            &file.path,
            CACHE_VERSION,
            &stretch_params.mode,
            if stretch_params.mode == StretchMode::Manual {
                Some(stretch_params.black_point)
            } else {
                None
            },
            if stretch_params.mode == StretchMode::Manual {
                Some(stretch_params.white_point)
            } else {
                None
            },
            stretch_params.midtones,
            &stretch_params.resolution,
        )?;

        let entry = match entry {
            Some(e) => e,
            None => {
                increment_cache_miss(&conn)?;
                return Ok(None);
            }
        };

        // Check if source file has been modified
        let file_path = Path::new(&file.path);
        if file_path.exists() {
            let current_modified = get_file_modified_time(file_path)?;
            if current_modified > entry.file_modified_at {
                // File has been modified, invalidate cache
                println!("Cache invalidated: file modified");
                if let Some(id) = entry.id {
                    delete_cache_entry(&conn, id)?;
                    delete_cache_file(&self.cache_dir, &entry.cache_filename)?;
                }
                increment_cache_miss(&conn)?;
                return Ok(None);
            }
        }

        // Check if cache file exists on disk
        if !cache_file_exists(&self.cache_dir, &entry.cache_filename) {
            // Cache file missing, clean up database entry
            println!("Cache file missing on disk");
            if let Some(id) = entry.id {
                delete_cache_entry(&conn, id)?;
            }
            increment_cache_miss(&conn)?;
            return Ok(None);
        }

        // Update access statistics
        if let Some(id) = entry.id {
            update_cache_access(&conn, id)?;
        }

        // Read and return cache file
        let data = read_cache_file(&self.cache_dir, &entry.cache_filename)?;
        println!("✅ Cache hit: {}", entry.cache_filename);
        Ok(Some(data))
    }

    /// Create a new cache entry by processing the FITS file
    pub async fn create_cache_entry(
        &self,
        file: &File,
        file_path: &Path,
        stretch_params: &StretchParams,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        // Generate cache filename first
        let cache_filename = generate_cache_filename(&file.path, stretch_params);
        let cache_path = self.cache_dir.join(&cache_filename);

        // Process the FITS file to JPEG using rustafits
        // Use resolution from stretch_params (user setting)
        let resolution = Resolution::from_string(&stretch_params.resolution);
        let result = rustafits_processor::process_fits_to_jpeg_cached(
            file_path,
            &cache_path,
            resolution,
            quality,
            &self.pool,
        )
        .context("Failed to process FITS image with rustafits")?;

        // Note: Cache file already written by rustafits directly to cache_path
        // No need to call write_cache_file again

        // Get file modification time
        let file_modified_at = get_file_modified_time(file_path)?;

        // Create database entry
        let entry = CacheEntry {
            id: None,
            file_id: file.id.unwrap_or(-1), // Use -1 for files not in database
            file_path: file.path.clone(),
            file_modified_at,
            cache_filename: cache_filename.clone(),
            cache_version: CACHE_VERSION.to_string(),
            stretch_mode: StretchMode::Auto, // rustafits always uses auto-stretch
            black_point: None,
            white_point: None,
            midtones: 0.35, // rustafits uses fixed midtones
            resolution: stretch_params.resolution.clone(),
            image_width: result.width,
            image_height: result.height,
            is_color: result.is_color, // Detected from FITS metadata
            file_size: result.image_data.len() as u64,
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            last_accessed_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            access_count: 0,
        };

        let conn = self.cache_db.lock().unwrap();
        insert_cache_entry(&conn, &entry)?;

        println!("💾 Cached as: {}", cache_filename);
        Ok(result.image_data)
    }

    /// Get metadata for a cached image
    pub async fn get_metadata(&self, file_path: &str, stretch_params: &StretchParams) -> Result<CacheEntry> {
        let conn = self.cache_db.lock().unwrap();

        find_cache_entry(
            &conn,
            file_path,
            CACHE_VERSION,
            &stretch_params.mode,
            if stretch_params.mode == StretchMode::Manual {
                Some(stretch_params.black_point)
            } else {
                None
            },
            if stretch_params.mode == StretchMode::Manual {
                Some(stretch_params.white_point)
            } else {
                None
            },
            stretch_params.midtones,
            &stretch_params.resolution,
        )?
        .ok_or_else(|| anyhow::anyhow!("Cache entry not found"))
    }

    /// Invalidate cache for a specific file
    pub async fn invalidate_file(&self, file_id: i64) -> Result<()> {
        let conn = self.cache_db.lock().unwrap();

        // Find all cache entries for this file
        let mut stmt = conn.prepare(
            "SELECT id, cache_filename FROM cache_entries WHERE file_id = ?1",
        )?;

        let entries = stmt
            .query_map([file_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Delete each entry
        for (id, cache_filename) in entries {
            delete_cache_entry(&conn, id)?;
            delete_cache_file(&self.cache_dir, &cache_filename)?;
        }

        Ok(())
    }

    /// Clear all cache entries
    pub async fn invalidate_all(&self) -> Result<()> {
        let conn = self.cache_db.lock().unwrap();
        clear_all_cache_entries(&conn)?;

        // Delete all JPEG files in cache directory
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            if entry.path().extension() == Some(std::ffi::OsStr::new("jpg")) {
                std::fs::remove_file(entry.path())?;
            }
        }

        Ok(())
    }

    /// Clean up cache to stay within size limit using LRU eviction
    pub async fn cleanup(&self, max_size_bytes: u64) -> Result<usize> {
        let conn = self.cache_db.lock().unwrap();

        let stats = get_cache_stats(&conn)?;
        if stats.total_size_bytes <= max_size_bytes {
            return Ok(0); // No cleanup needed
        }

        let mut deleted_count = 0;
        let mut current_size = stats.total_size_bytes;

        // Get oldest entries and delete until we're under the limit
        while current_size > max_size_bytes {
            let oldest = get_oldest_entries(&conn, 100)?;
            if oldest.is_empty() {
                break;
            }

            for entry in oldest {
                if let Some(id) = entry.id {
                    delete_cache_entry(&conn, id)?;
                    delete_cache_file(&self.cache_dir, &entry.cache_filename)?;
                    current_size = current_size.saturating_sub(entry.file_size);
                    deleted_count += 1;

                    if current_size <= max_size_bytes {
                        break;
                    }
                }
            }
        }

        Ok(deleted_count)
    }

    /// Get cache statistics
    pub async fn get_stats(&self) -> Result<CacheStats> {
        let conn = self.cache_db.lock().unwrap();
        get_cache_stats(&conn)
    }

    /// Update max cache size setting
    pub async fn set_max_size(&self, max_size_bytes: u64) -> Result<()> {
        let conn = self.cache_db.lock().unwrap();
        conn.execute(
            "UPDATE cache_stats SET value = ?1, updated_at = datetime('now') WHERE key = 'max_size_bytes'",
            [max_size_bytes.to_string()],
        )?;
        Ok(())
    }
}