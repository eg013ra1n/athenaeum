use anyhow::{Context, Result};
use astroimage::{annotate_image, AnnotationConfig, ImageConverter, ColorScheme};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::analysis::{analyzer::build_analyzer, config::AnalysisConfig};

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

/// Analysis summary returned alongside annotated images
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationMetrics {
    pub stars_detected: usize,
    pub median_fwhm: f32,
    pub median_eccentricity: f32,
    pub median_snr: f32,
    pub median_hfr: f32,
    pub frame_snr: f32,
    pub snr_weight: f32,
    pub psf_signal: f32,
    pub trail_r_squared: f32,
    pub possibly_trailed: bool,
    pub median_beta: Option<f32>,
}

/// User-configurable annotation display settings.
/// Stored as JSON in the settings table under key "blink.annotation_config".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationSettings {
    /// Color scheme: "eccentricity", "fwhm", or "uniform"
    pub color_scheme: String,
    /// Draw a direction tick along the elongation axis
    pub show_direction_tick: bool,
    /// Minimum ellipse semi-axis radius in output pixels
    pub min_radius: f32,
    /// Maximum ellipse semi-axis radius in output pixels
    pub max_radius: f32,
    /// Line thickness: 1 = single pixel, 2 = 3px cross, 3 = 5px diamond
    pub line_width: u8,
    /// Eccentricity threshold: below this is green (good)
    pub ecc_good: f32,
    /// Eccentricity threshold: above this is red (problem)
    pub ecc_warn: f32,
    /// FWHM ratio threshold: below this is green (good)
    pub fwhm_good: f32,
    /// FWHM ratio threshold: above this is red (problem)
    pub fwhm_warn: f32,
}

impl Default for AnnotationSettings {
    fn default() -> Self {
        Self {
            color_scheme: "eccentricity".to_string(),
            show_direction_tick: true,
            min_radius: 6.0,
            max_radius: 60.0,
            line_width: 2,
            ecc_good: 0.5,
            ecc_warn: 0.6,
            fwhm_good: 1.3,
            fwhm_warn: 2.0,
        }
    }
}

impl AnnotationSettings {
    /// Convert to rustafits AnnotationConfig
    pub fn to_rustafits_config(&self) -> AnnotationConfig {
        let color_scheme = match self.color_scheme.as_str() {
            "fwhm" => ColorScheme::Fwhm,
            "uniform" => ColorScheme::Uniform,
            _ => ColorScheme::Eccentricity,
        };
        AnnotationConfig {
            color_scheme,
            show_direction_tick: self.show_direction_tick,
            min_radius: self.min_radius,
            max_radius: self.max_radius,
            line_width: self.line_width.clamp(1, 3),
            ecc_good: self.ecc_good,
            ecc_warn: self.ecc_warn,
            fwhm_good: self.fwhm_good,
            fwhm_warn: self.fwhm_warn,
        }
    }
}

/// Result of annotated image processing — JPEG bytes + analysis metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedImageResult {
    pub image_data: Vec<u8>,
    pub metrics: Option<AnnotationMetrics>,
}

/// Process a FITS/XISF file to JPEG with star annotations burned in.
///
/// Runs image conversion and star analysis **in parallel** (both read the FITS
/// file independently). Uses the same `AnalysisConfig` as batch analysis for
/// consistent metrics.
pub fn process_fits_to_jpeg_annotated<P: AsRef<Path>>(
    input_path: P,
    resolution: Resolution,
    quality: Option<u8>,
    pool: &Arc<rayon::ThreadPool>,
    annotation_settings: Option<&AnnotationSettings>,
    analysis_config: &AnalysisConfig,
) -> Result<AnnotatedImageResult> {
    let input_path = input_path.as_ref();

    if !input_path.exists() {
        anyhow::bail!("Input file does not exist: {}", input_path.display());
    }

    // Build converter with resolution-specific settings
    let mut converter = ImageConverter::new().with_thread_pool(pool.clone());

    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }
    if resolution.use_preview_mode() {
        converter = converter.with_preview_mode();
    }

    // Build analyzer and start it on a separate thread immediately.
    // The analyzer uses the pool internally (via with_thread_pool), so no
    // outer pool.install() is needed. Conversion runs on the calling thread
    // in parallel — both overlap naturally via work-stealing on the shared pool.
    let analyzer = build_analyzer(analysis_config, Some(pool.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    let path_owned = input_path.to_path_buf();
    std::thread::spawn(move || {
        let result = analyzer.analyze(&path_owned);
        let _ = tx.send(result);
    });

    // Run conversion on the calling thread — overlaps with analysis above.
    // converter.process() enters the pool internally via with_thread_pool.
    let mut processed = converter
        .process(input_path)
        .with_context(|| format!("Failed to process image: {}", input_path.display()))?;

    // Wait for analysis to complete (may have finished during conversion).
    // 5-minute safety net prevents permanently stuck semaphore permits if
    // analysis hangs on a corrupted file. Not a functional timeout — analysis
    // has the full 5 minutes to finish.
    let analysis_result = rx.recv_timeout(std::time::Duration::from_secs(300))
        .ok();

    let width = processed.width as u32;
    let height = processed.height as u32;

    // Apply annotations if analysis succeeded
    let metrics = match analysis_result {
        Some(Ok(result)) => {
            let ann_config = annotation_settings
                .map(|s| s.to_rustafits_config())
                .unwrap_or_default();
            annotate_image(&mut processed, &result, &ann_config);

            Some(AnnotationMetrics {
                stars_detected: result.stars.len(),
                median_fwhm: result.median_fwhm,
                median_eccentricity: result.median_eccentricity,
                median_snr: result.median_snr,
                median_hfr: result.median_hfr,
                frame_snr: result.frame_snr,
                snr_weight: result.snr_weight,
                psf_signal: result.psf_signal,
                trail_r_squared: result.trail_r_squared,
                possibly_trailed: result.possibly_trailed,
                median_beta: result.median_beta,
            })
        }
        Some(Err(e)) => {
            eprintln!(
                "WARNING: Star analysis failed for {}: {}. Returning unannotated image.",
                input_path.display(),
                e
            );
            None
        }
        None => None, // Timed out
    };

    // Strip alpha channel if RGBA
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

    // Encode JPEG
    let jpeg_quality = resolution.jpeg_quality(quality);
    let image_data = jpeg_encode::encode_rgb_to_jpeg(&rgb_data, width, height, jpeg_quality)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(AnnotatedImageResult {
        image_data,
        metrics,
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
        .with_thread_pool(pool.clone());

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode for faster processing
    if resolution.use_preview_mode() {
        converter = converter.with_preview_mode();
    }

    // Process FITS/XISF to raw RGB pixels
    let processed = converter
        .process(input_path)
        .with_context(|| format!("Failed to process image: {}", input_path.display()))?;

    let width = processed.width as u32;
    let height = processed.height as u32;
    let is_color = processed.is_color;

    // Save processed image to disk as JPEG
    let jpeg_quality = resolution.jpeg_quality(quality);
    ImageConverter::save_processed(&processed, output_path, jpeg_quality)
        .with_context(|| {
            format!(
                "Failed to save image: {} -> {}",
                input_path.display(),
                output_path.display()
            )
        })?;

    // Read JPEG bytes from disk
    let image_data = std::fs::read(&output_path).with_context(|| {
        format!("Failed to read generated JPEG: {}", output_path.display())
    })?;

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color,
    })
}

