// Models for VipsProcessor module

use serde::{Deserialize, Serialize};

/// Parameters for processing an image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessParams {
    /// Use automatic stretching (AutoSTF)
    pub auto_stretch: bool,
    /// Manual black point (0-65535)
    pub black_point: u16,
    /// Manual white point (0-65535)
    pub white_point: u16,
    /// Midtones adjustment (0.01-0.99, default 0.25)
    pub midtones: f32,
    /// Bayer pattern for debayering (e.g., "RGGB", "BGGR")
    pub debayer_pattern: Option<String>,
    /// Output resolutions to generate
    pub resolutions: Vec<Resolution>,
    /// JPEG quality for each resolution
    pub jpeg_quality: JpegQuality,
}

impl Default for ProcessParams {
    fn default() -> Self {
        Self {
            auto_stretch: true,
            black_point: 0,
            white_point: 65535,
            midtones: 0.25,
            debayer_pattern: None,
            resolutions: vec![
                Resolution::Thumbnail,
                Resolution::Preview,
                Resolution::Full,
            ],
            jpeg_quality: JpegQuality::default(),
        }
    }
}

/// Output resolutions for processed images
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Resolution {
    /// Small thumbnail (256x256)
    Thumbnail,
    /// Medium preview (1024x1024)
    Preview,
    /// Full resolution
    Full,
}

impl Resolution {
    pub fn max_dimension(&self) -> u32 {
        match self {
            Resolution::Thumbnail => 256,
            Resolution::Preview => 1024,
            Resolution::Full => u32::MAX,
        }
    }

    pub fn jpeg_quality(&self, quality: &JpegQuality) -> u8 {
        match self {
            Resolution::Thumbnail => quality.thumbnail,
            Resolution::Preview => quality.preview,
            Resolution::Full => quality.full,
        }
    }
}

/// JPEG quality settings for different resolutions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JpegQuality {
    pub thumbnail: u8,
    pub preview: u8,
    pub full: u8,
}

impl Default for JpegQuality {
    fn default() -> Self {
        Self {
            thumbnail: 70,
            preview: 80,
            full: 85,
        }
    }
}

/// Result of processing an image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedImage {
    /// Thumbnail JPEG data
    pub thumbnail: Vec<u8>,
    /// Preview JPEG data
    pub preview: Vec<u8>,
    /// Full resolution JPEG data
    pub full: Vec<u8>,
    /// AutoSTF parameters used
    pub stretch_params: AutoSTFResult,
    /// Whether the image is color
    pub is_color: bool,
    /// Original image dimensions
    pub width: u32,
    pub height: u32,
}

/// Result of AutoSTF calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoSTFResult {
    /// Black point (0.0-1.0)
    pub black_point: f64,
    /// White point (0.0-1.0)
    pub white_point: f64,
    /// Midtones parameter for MTF
    pub midtones: f64,
    /// Median value of the image
    pub median: f64,
    /// Median Absolute Deviation
    pub mad: f64,
}

impl Default for AutoSTFResult {
    fn default() -> Self {
        Self {
            black_point: 0.0,
            white_point: 1.0,
            midtones: 0.5,
            median: 0.0,
            mad: 0.0,
        }
    }
}

/// Image pyramid for multi-resolution support
#[derive(Debug, Clone)]
pub struct ImagePyramid {
    pub levels: Vec<PyramidLevel>,
}

#[derive(Debug, Clone)]
pub struct PyramidLevel {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Cache entry metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntryMetadata {
    pub file_path: String,
    pub file_modified: String,
    pub stretch_params: AutoSTFResult,
    pub resolution: Resolution,
    pub file_size: u64,
    pub created_at: String,
    pub access_count: u32,
}