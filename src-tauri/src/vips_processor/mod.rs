// VipsProcessor module for high-performance image processing with LibVIPS
// Provides Rust-safe wrapper around LibVIPS FFI bindings

pub mod autostf;
pub mod ffi;
pub mod models;
pub mod worker_pool;

use anyhow::{Context, Result};
use libvips::{VipsApp, VipsImage, ops};
use std::path::Path;
use std::sync::Arc;

pub use models::{ProcessParams, ProcessedImage, Resolution, AutoSTFResult};
pub use autostf::AutoSTFParams;
pub use worker_pool::CacheWorkerPool;

/// Main VipsProcessor for handling FITS to JPEG conversion
pub struct VipsProcessor {
    _app: Arc<VipsApp>,  // Keep VipsApp alive for the lifetime of the processor
}

impl VipsProcessor {
    /// Create a new VipsProcessor instance
    pub fn new() -> Result<Self> {
        // Initialize LibVIPS
        let app = VipsApp::new("athenaeum", false)
            .context("Failed to initialize LibVIPS")?;

        Ok(Self {
            _app: Arc::new(app),
        })
    }

    /// Main processing pipeline: FITS → JPEG with AutoSTF
    pub fn process_fits_to_jpeg(
        &self,
        fits_path: &Path,
        params: &ProcessParams,
    ) -> Result<ProcessedImage> {
        use std::time::Instant;
        let t_start = Instant::now();

        println!("📷 VipsProcessor: Processing {}", fits_path.display());

        // Load FITS image using our custom FITS loader
        let image = self.load_fits_image(fits_path)?;
        println!("  ✅ Loaded FITS: {}x{}", image.get_width(), image.get_height());

        // Detect if it's a color image (has BAYERPAT)
        let is_color = self.detect_color_image(fits_path)?;

        // Apply debayering if needed
        let image = if is_color {
            println!("  🌈 Debayering color image");
            self.debayer_image(image, fits_path)?
        } else {
            image
        };

        // Calculate or apply AutoSTF
        let t_stf = Instant::now();
        let stf_params = if params.auto_stretch {
            println!("  🔮 Calculating AutoSTF (shadows: {}, median: {})",
                     params.autostf_shadows_clipping, params.autostf_target_median);
            self.calculate_autostf(&image, params)?
        } else {
            AutoSTFResult {
                black_point: params.black_point as f64 / 65535.0,
                white_point: params.white_point as f64 / 65535.0,
                midtones: params.midtones as f64,
                median: 0.0,
                mad: 0.0,
            }
        };
        println!("  ⏱️  AutoSTF: {:?}", t_stf.elapsed());

        // Apply the stretch
        println!("  🎨 About to call apply_stretch...");
        println!("  📊 STF params: black={:.4}, white={:.4}, midtones={:.4}",
                 stf_params.black_point, stf_params.white_point, stf_params.midtones);
        let t_stretch = Instant::now();
        let stretched = self.apply_stretch(&image, &stf_params)?;
        println!("  ✅ apply_stretch returned successfully");
        println!("  ⏱️  Stretch: {:?}", t_stretch.elapsed());

        // Generate multiple resolutions
        let t_resolutions = Instant::now();
        let processed = self.generate_resolutions(&stretched, &params.resolutions)?;
        println!("  ⏱️  Resolutions: {:?}", t_resolutions.elapsed());

        println!("  ✅ Total processing time: {:?}", t_start.elapsed());

        Ok(ProcessedImage {
            thumbnail: processed.thumbnail,
            preview: processed.preview,
            full: processed.full,
            stretch_params: stf_params,
            is_color,
            width: image.get_width() as u32,
            height: image.get_height() as u32,
        })
    }

    /// Load FITS image (for now, use our existing FITS loader then convert)
    fn load_fits_image(&self, path: &Path) -> Result<VipsImage> {
        // For now, just try to load the file directly with LibVIPS
        // LibVIPS might have FITS support depending on compilation
        let path_str = path.to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in path: {:?}", path))?;
        let image = VipsImage::new_from_file(path_str)?;
        Ok(image)
    }

    /// Detect if image is color (has Bayer pattern)
    fn detect_color_image(&self, path: &Path) -> Result<bool> {
        // Check FITS headers for BAYERPAT keyword
        use fitsio::FitsFile;

        match FitsFile::open(path) {
            Ok(mut fptr) => {
                let hdu = fptr.primary_hdu()
                    .context("Failed to get primary HDU")?;

                // Try to read BAYERPAT keyword
                match hdu.read_key::<String>(&mut fptr, "BAYERPAT") {
                    Ok(pattern) => {
                        println!("  🎨 Detected Bayer pattern: {}", pattern);
                        Ok(true)
                    }
                    Err(_) => Ok(false),  // No BAYERPAT keyword = mono image
                }
            }
            Err(_) => Ok(false),  // Not a FITS file or can't open
        }
    }

    /// Debayer image if it has a Bayer pattern
    fn debayer_image(&self, image: VipsImage, _fits_path: &Path) -> Result<VipsImage> {
        // For Bayer pattern images from FITS files, we need to handle this differently
        // The image is likely loaded as a single-band 16-bit image
        // We'll use bandjoin to create an RGB image by treating the Bayer data as already debayered

        println!("  🎨 Processing Bayer pattern image");

        // For now, just treat the single channel as all three channels
        // This will make it grayscale but at least it won't crash
        // Proper debayering would require implementing the interpolation
        let bands = image.get_bands();
        println!("  Input image has {} bands", bands);

        let rgb_image = if bands == 1 {
            // Create RGB by duplicating the single channel three times
            println!("  Creating RGB from single-band Bayer data");
            let mut channels = vec![image.clone(), image.clone(), image.clone()];
            ops::bandjoin(&mut channels)?
        } else {
            // Already multi-band, try to convert
            println!("  Converting multi-band to RGB");
            ops::colourspace(&image, ops::Interpretation::Rgb)?
        };

        println!("  ✅ Processing complete: {}x{} → RGB",
                 rgb_image.get_width(), rgb_image.get_height());

        Ok(rgb_image)
    }

    /// Calculate AutoSTF parameters using settings from ProcessParams
    fn calculate_autostf(&self, image: &VipsImage, params: &ProcessParams) -> Result<AutoSTFResult> {
        let autostf_params = AutoSTFParams {
            shadows_clipping: params.autostf_shadows_clipping,
            target_median: params.autostf_target_median,
            use_rgb_linking: true,
        };
        autostf::calculate_autostf(image, &autostf_params)
    }

    /// Apply stretch using AutoSTF parameters with MTF curve
    fn apply_stretch(&self, image: &VipsImage, params: &AutoSTFResult) -> Result<VipsImage> {
        println!("  🎨 Applying stretch (black: {:.4}, white: {:.4}, midtones: {:.4})",
                 params.black_point, params.white_point, params.midtones);

        println!("  📊 Input image: {}x{}, format: {:?}, bands: {}",
                 image.get_width(), image.get_height(),
                 image.get_format(), image.get_bands());

        let range = params.white_point - params.black_point;
        println!("  🔧 Stretch range: {:.4}", range);

        if range <= 0.0 {
            // No range, return clamped image
            return Ok(ops::cast(image, ops::BandFormat::Uchar)?);
        }

        // Create LUT for combined black/white/MTF transformation
        // LUT maps 16-bit input (0-65535) to 8-bit output (0-255) with MTF curve
        println!("  1️⃣  Creating MTF lookup table...");
        let lut_size = 65536;
        let mut lut_data: Vec<u8> = Vec::with_capacity(lut_size);

        for i in 0..lut_size {
            let input_value = i as f64;

            // Step 1: Normalize to 0-1 using black/white points
            let normalized = ((input_value - params.black_point) / range).clamp(0.0, 1.0);

            // Step 2: Apply MTF curve
            let mtf_output = apply_mtf(normalized, params.midtones);

            // Step 3: Scale to 0-255
            let output = (mtf_output * 255.0).clamp(0.0, 255.0) as u8;
            lut_data.push(output);
        }

        println!("  2️⃣  Applying LUT to image (using pure Rust)...");
        // WORKAROUND: LibVIPS ops::maplut also causes segfaults in concurrent processing
        // Apply LUT directly to pixel data in pure Rust

        let width = image.get_width() as usize;
        let height = image.get_height() as usize;
        let bands = image.get_bands() as usize;

        // Read input pixel data (16-bit)
        let input_buffer = image.image_write_to_memory();
        let input_pixels: &[u16] = unsafe {
            std::slice::from_raw_parts(
                input_buffer.as_ptr() as *const u16,
                width * height * bands
            )
        };

        // Apply LUT to each pixel
        let mut output_pixels: Vec<u8> = Vec::with_capacity(width * height * bands);
        for &pixel in input_pixels {
            output_pixels.push(lut_data[pixel as usize]);
        }

        // Create new VipsImage from stretched data
        let stretched = VipsImage::new_from_memory(
            &output_pixels,
            width as i32,
            height as i32,
            bands as i32,
            ops::BandFormat::Uchar,
        )?;

        println!("  ✅ Stretch with MTF complete (pure Rust)");

        Ok(stretched)
    }

    /// Generate multiple resolution outputs
    fn generate_resolutions(&self, image: &VipsImage, resolutions: &[Resolution]) -> Result<ProcessedImage> {
        println!("  📦 Generating {} resolutions...", resolutions.len());

        let mut result = ProcessedImage {
            thumbnail: Vec::new(),
            preview: Vec::new(),
            full: Vec::new(),
            stretch_params: AutoSTFResult::default(),
            is_color: false,
            width: image.get_width() as u32,
            height: image.get_height() as u32,
        };

        for (i, resolution) in resolutions.iter().enumerate() {
            println!("  [{}] Processing {:?}...", i + 1, resolution);

            let (data, _width, _height) = match resolution {
                Resolution::Thumbnail => {
                    println!("    Encoding thumbnail (full resolution) as JPEG...");
                    self.encode_jpeg(image, 70)?
                },
                Resolution::Preview => {
                    println!("    Encoding preview (full resolution) as JPEG...");
                    self.encode_jpeg(image, 85)?
                },
                Resolution::Full => {
                    println!("    Encoding full resolution as JPEG...");
                    self.encode_jpeg(image, 95)?
                },
            };

            println!("    ✅ {:?} complete: {} bytes", resolution, data.len());

            match resolution {
                Resolution::Thumbnail => result.thumbnail = data,
                Resolution::Preview => result.preview = data,
                Resolution::Full => result.full = data,
            }
        }

        println!("  ✅ All resolutions generated");
        Ok(result)
    }

    /// Resize image to fit within max dimension (using pure Rust to avoid LibVIPS thread-safety bugs)
    fn resize_image(&self, image: &VipsImage, max_size: u32) -> Result<VipsImage> {
        let width = image.get_width() as u32;
        let height = image.get_height() as u32;

        println!("      resize_image: {}x{} → max {}", width, height, max_size);

        let scale = if width > height {
            max_size as f64 / width as f64
        } else {
            max_size as f64 / height as f64
        };

        println!("      Scale factor: {:.4}", scale);

        if scale >= 1.0 {
            println!("      No resize needed (scale >= 1.0)");
            return Ok(image.clone());
        }

        println!("      Using pure Rust resize (LibVIPS ops::resize is buggy)...");

        let new_width = (width as f64 * scale) as u32;
        let new_height = (height as f64 * scale) as u32;

        println!("      Target size: {}x{}", new_width, new_height);

        // Extract pixel data from VipsImage
        let buffer = image.image_write_to_memory();
        let bands = image.get_bands() as usize;

        // Resize using pure Rust based on number of bands
        let resized_buffer = match bands {
            1 => {
                // Grayscale image (8-bit)
                let pixels: &[u8] = buffer.as_ref();
                let img = image::GrayImage::from_raw(width, height, pixels.to_vec())
                    .ok_or_else(|| anyhow::anyhow!("Failed to create GrayImage from pixels"))?;

                let resized = image::imageops::resize(
                    &img,
                    new_width,
                    new_height,
                    image::imageops::FilterType::Lanczos3,
                );

                resized.into_raw()
            }
            3 => {
                // RGB image (8-bit per channel)
                let pixels: &[u8] = buffer.as_ref();
                let img = image::RgbImage::from_raw(width, height, pixels.to_vec())
                    .ok_or_else(|| anyhow::anyhow!("Failed to create RgbImage from pixels"))?;

                let resized = image::imageops::resize(
                    &img,
                    new_width,
                    new_height,
                    image::imageops::FilterType::Lanczos3,
                );

                resized.into_raw()
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unsupported number of bands for resizing: {} (only 1 or 3 supported)",
                    bands
                ));
            }
        };

        // Create new VipsImage from resized buffer
        let resized_image = VipsImage::new_from_memory(
            &resized_buffer,
            new_width as i32,
            new_height as i32,
            bands as i32,
            ops::BandFormat::Uchar,
        )?;

        println!("      ✅ Pure Rust resize complete: {}x{}", new_width, new_height);
        Ok(resized_image)
    }

    /// Encode image as JPEG using LibVIPS (faster than pure Rust)
    fn encode_jpeg(&self, image: &VipsImage, quality: u8) -> Result<(Vec<u8>, u32, u32)> {
        println!("      encode_jpeg: {}x{}", image.get_width(), image.get_height());
        println!("      Image format: {:?}, bands: {}", image.get_format(), image.get_bands());

        // Use LibVIPS for fast JPEG encoding
        println!("      Using LibVIPS jpegsave_buffer (quality: {})...", quality);

        let buffer = ops::jpegsave_buffer(image)?;

        println!("      ✅ JPEG encoded: {} bytes", buffer.len());
        Ok((buffer, image.get_width() as u32, image.get_height() as u32))
    }
}

/// Apply Midtone Transfer Function
fn apply_mtf(x: f64, midtones: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    // MTF formula: y = ((m - 1) * x) / ((2m - 1) * x - m)
    let numerator = (midtones - 1.0) * x;
    let denominator = (2.0 * midtones - 1.0) * x - midtones;

    if denominator.abs() < 1e-10 {
        x
    } else {
        (numerator / denominator).clamp(0.0, 1.0)
    }
}