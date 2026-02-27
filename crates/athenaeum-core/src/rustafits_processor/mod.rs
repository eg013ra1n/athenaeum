use anyhow::{Context, Result};
use astroimage::ImageConverter;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Resolution variants for blink viewer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Thumbnail,
    Preview,
    Full,
}

impl Resolution {
    /// Parse resolution from string
    pub fn from_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "thumbnail" => Resolution::Thumbnail,
            "preview" => Resolution::Preview,
            "full" => Resolution::Full,
            _ => Resolution::Preview, // Default to preview
        }
    }

    /// Get JPEG quality for this resolution
    /// Uses custom quality if provided, otherwise defaults
    pub fn jpeg_quality(&self, custom: Option<u8>) -> u8 {
        if let Some(q) = custom {
            return q.clamp(1, 100);
        }
        match self {
            Resolution::Thumbnail => 70,
            Resolution::Preview => 85,
            Resolution::Full => 95,
        }
    }

    /// Get default JPEG quality for this resolution
    #[allow(dead_code)]
    pub fn default_quality(&self) -> u8 {
        self.jpeg_quality(None)
    }

    /// Get setting key for this resolution's quality
    pub fn quality_setting_key(&self) -> &'static str {
        match self {
            Resolution::Thumbnail => "rustafits.quality.thumbnail",
            Resolution::Preview => "rustafits.quality.preview",
            Resolution::Full => "rustafits.quality.full",
        }
    }

    /// Get downscale factor for this resolution
    pub fn downscale_factor(&self) -> usize {
        match self {
            Resolution::Thumbnail => 4,
            Resolution::Preview => 1, // preview mode uses 2x2 binning internally
            Resolution::Full => 1,
        }
    }

    /// Whether to use rustafits preview mode (2x2 binning)
    pub fn use_preview_mode(&self) -> bool {
        matches!(self, Resolution::Preview)
    }
}

/// Processed FITS image data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedImage {
    pub image_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: String, // "jpeg"
    pub is_color: bool, // Whether this is a color/Bayer image
}


/// Process a FITS/XISF file to JPEG entirely in memory (no temp files).
///
/// Uses `converter.process()` to get raw RGB pixels, then encodes JPEG
/// in-process via `image::codecs::jpeg::JpegEncoder` writing to a `Vec<u8>`.
///
/// Note: rustafits 0.2+ handles Bayer/color detection internally for both FITS and XISF
pub fn process_fits_to_jpeg<P: AsRef<Path>>(
    input_path: P,
    resolution: Resolution,
    quality: Option<u8>,
    pool: &Arc<rayon::ThreadPool>,
) -> Result<ProcessedImage> {
    let input_path = input_path.as_ref();

    // Validate input file exists
    if !input_path.exists() {
        anyhow::bail!("Input file does not exist: {}", input_path.display());
    }

    // Build rustafits converter with resolution-specific settings
    let mut converter = ImageConverter::new()
        .with_thread_pool(pool.clone());

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode for faster processing
    if resolution.use_preview_mode() {
        converter = converter.with_preview_mode();
    }

    // Process FITS/XISF to raw RGB pixels in memory
    let processed = converter
        .process(input_path)
        .with_context(|| format!("Failed to process image: {}", input_path.display()))?;

    let width = processed.width as u32;
    let height = processed.height as u32;
    let is_color = processed.is_color;

    // Strip alpha channel if RGBA (4 channels → 3)
    let rgb_data = if processed.channels == 4 {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for chunk in processed.data.chunks_exact(4) {
            rgb.push(chunk[0]);
            rgb.push(chunk[1]);
            rgb.push(chunk[2]);
        }
        rgb
    } else {
        processed.data
    };

    // Encode JPEG in memory via jpeg-encode crate (compiled outside workspace
    // so the encoder gets full optimisation without incremental compilation penalty)
    let jpeg_quality = resolution.jpeg_quality(quality);
    let image_data = jpeg_encode::encode_rgb_to_jpeg(&rgb_data, width, height, jpeg_quality)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color,
    })
}

/// Process FITS/XISF to JPEG and cache it in the specified directory
///
/// This variant writes directly to the cache directory instead of a temp file,
/// which is more efficient for the caching use case.
///
/// Note: rustafits 0.2+ handles Bayer/color detection internally for both FITS and XISF
pub fn process_fits_to_jpeg_cached<P: AsRef<Path>>(
    input_path: P,
    output_path: P,
    resolution: Resolution,
    quality: Option<u8>,
    pool: &Arc<rayon::ThreadPool>,
) -> Result<ProcessedImage> {
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();

    // Validate input file exists
    if !input_path.exists() {
        anyhow::bail!("Input file does not exist: {}", input_path.display());
    }

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create output directory: {}", parent.display())
        })?;
    }

    // Build rustafits converter with resolution-specific settings
    let mut converter = ImageConverter::new()
        .with_thread_pool(pool.clone())
        .with_quality(resolution.jpeg_quality(quality));

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode for faster processing
    // rustafits 0.2+ handles color/Bayer detection internally
    if resolution.use_preview_mode() {
        converter = converter.with_preview_mode();
    }

    // Convert FITS/XISF to JPEG
    converter
        .convert(&input_path, &output_path)
        .with_context(|| {
            format!(
                "Failed to convert image: {} -> {}",
                input_path.display(),
                output_path.display()
            )
        })?;

    // Read JPEG bytes from disk
    let image_data = std::fs::read(&output_path).with_context(|| {
        format!("Failed to read generated JPEG: {}", output_path.display())
    })?;

    // Dimensions set to 0 - frontend determines from JPEG
    let width = 0;
    let height = 0;

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color: false, // Not used - rustafits handles color internally
    })
}

