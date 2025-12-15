use anyhow::{Context, Result};
use fits_converter::FitsConverter;
use serde::{Deserialize, Serialize};
use std::path::Path;

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

/// Detect if a FITS file contains a color/Bayer pattern image
///
/// Checks for common color indicators in FITS headers:
/// - BAYERPAT (Bayer pattern identifier)
/// - COLORTYP (Color type)
/// - NAXIS3 = 3 (RGB channels)
fn detect_color_image<P: AsRef<Path>>(path: P) -> bool {
    use fitsio::FitsFile;

    let path = path.as_ref();

    // Try to open FITS file and check for color indicators
    if let Ok(mut fitsfile) = FitsFile::open(path) {
        if let Ok(hdu) = fitsfile.primary_hdu() {
            // Check for BAYERPAT keyword (most common for OSC cameras)
            if let Ok(_) = hdu.read_key::<String>(&mut fitsfile, "BAYERPAT") {
                return true;
            }

            // Check for COLORTYP keyword
            if let Ok(colortyp) = hdu.read_key::<String>(&mut fitsfile, "COLORTYP") {
                if colortyp.to_uppercase().contains("COLOR") ||
                   colortyp.to_uppercase().contains("RGB") ||
                   colortyp.to_uppercase().contains("BAYER") {
                    return true;
                }
            }

            // Check for XBAYEROFF/YBAYEROFF (Bayer offset indicators)
            if hdu.read_key::<i64>(&mut fitsfile, "XBAYROFF").is_ok() ||
               hdu.read_key::<i64>(&mut fitsfile, "YBAYROFF").is_ok() {
                return true;
            }

            // Check for NAXIS3 = 3 (3-channel RGB image)
            if let Ok(naxis) = hdu.read_key::<i64>(&mut fitsfile, "NAXIS") {
                if naxis >= 3 {
                    if let Ok(naxis3) = hdu.read_key::<i64>(&mut fitsfile, "NAXIS3") {
                        if naxis3 == 3 {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}

/// Process a FITS file to JPEG using rustafits
///
/// This function:
/// 1. Creates a temporary output path for the JPEG
/// 2. Calls rustafits with appropriate settings for the resolution
/// 3. Reads the JPEG bytes from disk
/// 4. Cleans up the temporary file
/// 5. Returns the JPEG data
pub fn process_fits_to_jpeg<P: AsRef<Path>>(
    input_path: P,
    resolution: Resolution,
    quality: Option<u8>,
) -> Result<ProcessedImage> {
    let input_path = input_path.as_ref();

    // Validate input file exists
    if !input_path.exists() {
        anyhow::bail!("Input FITS file does not exist: {}", input_path.display());
    }

    // Detect if this is a color image
    let is_color = detect_color_image(input_path);

    // Create temporary output path
    let temp_dir = std::env::temp_dir();
    let temp_filename = format!(
        "athenaeum_{}_{:?}.jpg",
        input_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let output_path = temp_dir.join(temp_filename);

    // Build rustafits converter with resolution-specific settings
    let mut converter = FitsConverter::new()
        .with_quality(resolution.jpeg_quality(quality));

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode ONLY for mono images
    // For color images, skip preview_mode to avoid binning before debayering
    if resolution.use_preview_mode() && !is_color {
        converter = converter.with_preview_mode();
    }

    // Convert FITS to JPEG
    converter
        .convert(&input_path, &output_path)
        .with_context(|| {
            format!(
                "Failed to convert FITS to JPEG: {} -> {}",
                input_path.display(),
                output_path.display()
            )
        })?;

    // Read JPEG bytes from disk
    let image_data = std::fs::read(&output_path).with_context(|| {
        format!("Failed to read generated JPEG: {}", output_path.display())
    })?;

    // Try to get image dimensions using image crate (if available) or estimate
    // For now, we'll just set dummy values since rustafits doesn't expose dimensions
    // The frontend can determine actual dimensions when loading the JPEG
    let width = 0;
    let height = 0;

    // Clean up temporary file
    let _ = std::fs::remove_file(&output_path);

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color,
    })
}

/// Process FITS to JPEG and cache it in the specified directory
///
/// This variant writes directly to the cache directory instead of a temp file,
/// which is more efficient for the caching use case.
pub fn process_fits_to_jpeg_cached<P: AsRef<Path>>(
    input_path: P,
    output_path: P,
    resolution: Resolution,
    quality: Option<u8>,
) -> Result<ProcessedImage> {
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();

    // Validate input file exists
    if !input_path.exists() {
        anyhow::bail!("Input FITS file does not exist: {}", input_path.display());
    }

    // Detect if this is a color image
    let is_color = detect_color_image(input_path);

    // Ensure output directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create output directory: {}", parent.display())
        })?;
    }

    // Build rustafits converter with resolution-specific settings
    let mut converter = FitsConverter::new()
        .with_quality(resolution.jpeg_quality(quality));

    // Apply downscale if needed (for thumbnails)
    if resolution.downscale_factor() > 1 {
        converter = converter.with_downscale(resolution.downscale_factor());
    }

    // Apply preview mode ONLY for mono images
    // For color images, skip preview_mode to avoid binning before debayering
    if resolution.use_preview_mode() && !is_color {
        converter = converter.with_preview_mode();
    }

    // Convert FITS to JPEG
    converter
        .convert(&input_path, &output_path)
        .with_context(|| {
            format!(
                "Failed to convert FITS to JPEG: {} -> {}",
                input_path.display(),
                output_path.display()
            )
        })?;

    // Read JPEG bytes from disk
    let image_data = std::fs::read(&output_path).with_context(|| {
        format!("Failed to read generated JPEG: {}", output_path.display())
    })?;

    // Dimensions are set to 0 for now (frontend determines from JPEG)
    let width = 0;
    let height = 0;

    Ok(ProcessedImage {
        image_data,
        width,
        height,
        format: "jpeg".to_string(),
        is_color,
    })
}
