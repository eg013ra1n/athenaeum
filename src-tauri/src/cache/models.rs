use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a cached image entry in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub id: Option<i64>,
    pub file_id: i64,
    pub file_path: String,
    pub file_modified_at: String,
    pub cache_filename: String,
    pub cache_version: String,
    pub stretch_mode: StretchMode,
    pub black_point: Option<u16>,
    pub white_point: Option<u16>,
    pub midtones: f32,
    pub image_width: u32,
    pub image_height: u32,
    pub is_color: bool,
    pub file_size: u64,
    pub created_at: String,
    pub last_accessed_at: String,
    pub access_count: u32,
}

/// Stretch mode for cached images
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StretchMode {
    Auto,
    Manual,
}

impl std::fmt::Display for StretchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StretchMode::Auto => write!(f, "auto"),
            StretchMode::Manual => write!(f, "manual"),
        }
    }
}

/// Parameters for stretching an image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StretchParams {
    pub mode: StretchMode,
    pub black_point: u16,
    pub white_point: u16,
    pub midtones: f32,
}

impl StretchParams {
    pub fn auto(midtones: f32) -> Self {
        Self {
            mode: StretchMode::Auto,
            black_point: 0,
            white_point: 0,
            midtones,
        }
    }

    pub fn manual(black_point: u16, white_point: u16, midtones: f32) -> Self {
        Self {
            mode: StretchMode::Manual,
            black_point,
            white_point,
            midtones,
        }
    }

    /// Generate a cache key for these parameters
    pub fn cache_key(&self, file_path: &str) -> String {
        match self.mode {
            StretchMode::Auto => format!("{}|auto|{}", file_path, self.midtones),
            StretchMode::Manual => format!(
                "{}|manual|{}|{}|{}",
                file_path, self.black_point, self.white_point, self.midtones
            ),
        }
    }
}

/// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_size_bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_rate: f64,
    pub max_size_bytes: u64,
}

/// Priority levels for cache building tasks
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CachePriority {
    Immediate = 0,
    High = 1,
    Normal = 2,
    Low = 3,
}

/// A task to build a cache entry
#[derive(Debug, Clone)]
pub struct CacheBuildTask {
    pub file_id: i64,
    pub file_path: PathBuf,
    pub stretch_params: StretchParams,
    pub priority: CachePriority,
}

/// Progress information for cache building
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheProgress {
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub current_file: Option<String>,
    pub estimated_time_remaining: Option<u64>, // seconds
}