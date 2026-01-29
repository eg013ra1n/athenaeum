//! Image registration and plate-solving
//!
//! This module provides functions for aligning images using either:
//! - Astrometric registration (plate-solving to WCS, then aligning by coordinates)
//! - Star-based registration (matching star patterns between frames)
//!
//! Astrometric registration is preferred when possible as it provides
//! precise alignment based on absolute celestial coordinates.

use crate::stacking::error::{StackingError, StackingResult};
use crate::stacking::ffi::{
    self, AstrometryData, CatalogueType, FramingType, Homography,
    OpencvInterpolation, PlateSolveSolver, SirilSolveError, TransformationType,
    GFALSE, GTRUE,
};
use crate::stacking::fits::FitsImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// =============================================================================
// Configuration Types
// =============================================================================

/// Configuration for plate solving
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlateSolveConfig {
    /// Solver to use
    pub solver: Solver,
    /// Star catalogue for reference
    pub catalogue: Catalogue,
    /// Camera pixel size in micrometers
    pub pixel_size_um: f64,
    /// Telescope focal length in millimeters
    pub focal_length_mm: f64,
    /// Search radius in degrees (for blind solving)
    pub search_radius_deg: f64,
    /// Magnitude limit for catalogue stars
    pub magnitude_limit: MagnitudeLimit,
    /// SIP distortion correction order
    pub distortion_order: u32,
    /// Scale tolerance for matching (percent)
    pub scale_tolerance_percent: f64,
    /// Maximum stars to use for solving
    pub max_stars: u32,
    /// Downsample image before solving (faster but less accurate)
    pub downsample: bool,
    /// Initial RA hint (degrees, optional)
    pub initial_ra: Option<f64>,
    /// Initial Dec hint (degrees, optional)
    pub initial_dec: Option<f64>,
}

impl Default for PlateSolveConfig {
    fn default() -> Self {
        PlateSolveConfig {
            solver: Solver::SirilInternal,
            catalogue: Catalogue::GaiaDR3,
            pixel_size_um: 3.76,      // Common CMOS sensor
            focal_length_mm: 500.0,   // Common small refractor
            search_radius_deg: 10.0,
            magnitude_limit: MagnitudeLimit::Auto,
            distortion_order: 1,
            scale_tolerance_percent: 5.0,
            max_stars: 500,
            downsample: false,
            initial_ra: None,
            initial_dec: None,
        }
    }
}

/// Plate solver type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Solver {
    /// Siril's internal plate solver
    SirilInternal,
    /// Local astrometry.net installation
    AstrometryNet,
}

impl Default for Solver {
    fn default() -> Self {
        Solver::SirilInternal
    }
}

impl From<Solver> for PlateSolveSolver {
    fn from(s: Solver) -> Self {
        match s {
            Solver::SirilInternal => PlateSolveSolver::SolverSiril,
            Solver::AstrometryNet => PlateSolveSolver::SolverLocalAsnet,
        }
    }
}

/// Star catalogue for plate solving
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Catalogue {
    /// Gaia Data Release 3 (recommended)
    GaiaDR3,
    /// Gaia Early Data Release 3
    GaiaEDR3,
    /// Gaia Data Release 2
    GaiaDR2,
    /// Tycho-2 catalogue
    Tycho2,
    /// USNO-B1 catalogue
    UsnoB1,
    /// Local catalogue file
    Local,
}

impl Default for Catalogue {
    fn default() -> Self {
        Catalogue::GaiaDR3
    }
}

impl From<Catalogue> for CatalogueType {
    fn from(c: Catalogue) -> Self {
        match c {
            Catalogue::GaiaDR3 => CatalogueType::CatGaiaDr3,
            Catalogue::GaiaEDR3 => CatalogueType::CatGaiaEdr3,
            Catalogue::GaiaDR2 => CatalogueType::CatGaiaDr2,
            Catalogue::Tycho2 => CatalogueType::CatTycho2,
            Catalogue::UsnoB1 => CatalogueType::CatUsnoB,
            Catalogue::Local => CatalogueType::CatLocal,
        }
    }
}

/// Magnitude limit for catalogue queries
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagnitudeLimit {
    /// Automatically determine based on image depth
    Auto,
    /// Fixed magnitude limit
    Fixed(f64),
}

impl Default for MagnitudeLimit {
    fn default() -> Self {
        MagnitudeLimit::Auto
    }
}

/// Configuration for image registration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationConfig {
    /// Output framing type
    pub framing: Framing,
    /// Interpolation method for transformation
    pub interpolation: Interpolation,
    /// Transformation type
    pub transformation: Transformation,
    /// Output scale (1.0 = native, 2.0 = 2x drizzle)
    pub output_scale: f64,
    /// Use clamping to reduce ringing artifacts
    pub clamp: bool,
    /// Clamping factor (typically 3.0)
    pub clamping_factor: f64,
    /// Minimum star pairs for alignment
    pub min_pairs: u32,
    /// Maximum candidate stars for matching
    pub max_stars: u32,
    /// Use two-pass registration for higher accuracy
    pub two_pass: bool,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        RegistrationConfig {
            framing: Framing::Max,
            interpolation: Interpolation::Lanczos4,
            transformation: Transformation::Homography,
            output_scale: 1.0,
            clamp: true,
            clamping_factor: 3.0,
            min_pairs: 10,
            max_stars: 500,
            two_pass: false,
        }
    }
}

/// Output framing type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// Use reference frame dimensions
    Current,
    /// Union of all frames (maximum coverage)
    Max,
    /// Intersection of all frames (no black borders)
    Min,
    /// Center on mean position of all frames
    CenterOfGravity,
}

impl Default for Framing {
    fn default() -> Self {
        Framing::Max
    }
}

impl From<Framing> for FramingType {
    fn from(f: Framing) -> Self {
        match f {
            Framing::Current => FramingType::FramingCurrent,
            Framing::Max => FramingType::FramingMax,
            Framing::Min => FramingType::FramingMin,
            Framing::CenterOfGravity => FramingType::FramingCog,
        }
    }
}

/// Interpolation method for image transformation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// Nearest neighbor (fastest, lowest quality)
    Nearest,
    /// Bilinear (fast, good quality)
    Bilinear,
    /// Bicubic (slower, better quality)
    Bicubic,
    /// Lanczos4 (slowest, best quality)
    Lanczos4,
    /// Area-based (good for downscaling)
    Area,
}

impl Default for Interpolation {
    fn default() -> Self {
        Interpolation::Lanczos4
    }
}

impl From<Interpolation> for OpencvInterpolation {
    fn from(i: Interpolation) -> Self {
        match i {
            Interpolation::Nearest => OpencvInterpolation::Nearest,
            Interpolation::Bilinear => OpencvInterpolation::Linear,
            Interpolation::Bicubic => OpencvInterpolation::Cubic,
            Interpolation::Lanczos4 => OpencvInterpolation::Lanczos4,
            Interpolation::Area => OpencvInterpolation::Area,
        }
    }
}

/// Transformation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transformation {
    /// Translation only (shift)
    Shift,
    /// Shift + rotation + scale
    Similarity,
    /// Affine (6 DOF)
    Affine,
    /// Full homography (8 DOF)
    Homography,
}

impl Default for Transformation {
    fn default() -> Self {
        Transformation::Homography
    }
}

impl From<Transformation> for TransformationType {
    fn from(t: Transformation) -> Self {
        match t {
            Transformation::Shift => TransformationType::Shift,
            Transformation::Similarity => TransformationType::Similarity,
            Transformation::Affine => TransformationType::Affine,
            Transformation::Homography => TransformationType::Homography,
        }
    }
}

// =============================================================================
// Result Types
// =============================================================================

/// World coordinate system solution data
///
/// This holds the key WCS parameters that can be extracted from
/// a successful plate solve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WcsSolution {
    /// Reference pixel X (CRPIX1)
    pub crpix1: f64,
    /// Reference pixel Y (CRPIX2)
    pub crpix2: f64,
    /// Reference RA (CRVAL1) in degrees
    pub crval1: f64,
    /// Reference Dec (CRVAL2) in degrees
    pub crval2: f64,
    /// CD matrix element 1,1
    pub cd1_1: f64,
    /// CD matrix element 1,2
    pub cd1_2: f64,
    /// CD matrix element 2,1
    pub cd2_1: f64,
    /// CD matrix element 2,2
    pub cd2_2: f64,
    /// Image width
    pub naxis1: i32,
    /// Image height
    pub naxis2: i32,
    /// Coordinate type for axis 1 (e.g., "RA---TAN")
    pub ctype1: String,
    /// Coordinate type for axis 2 (e.g., "DEC--TAN")
    pub ctype2: String,
}

impl WcsSolution {
    /// Calculate pixel scale in arcseconds per pixel
    pub fn pixel_scale(&self) -> f64 {
        let scale = (self.cd1_1 * self.cd1_1 + self.cd1_2 * self.cd1_2).sqrt();
        scale * 3600.0 // Convert degrees to arcseconds
    }

    /// Calculate rotation angle in degrees
    pub fn rotation(&self) -> f64 {
        self.cd1_2.atan2(self.cd1_1).to_degrees()
    }
}

impl Default for WcsSolution {
    fn default() -> Self {
        WcsSolution {
            crpix1: 0.0,
            crpix2: 0.0,
            crval1: 0.0,
            crval2: 0.0,
            cd1_1: 1.0,
            cd1_2: 0.0,
            cd2_1: 0.0,
            cd2_2: 1.0,
            naxis1: 0,
            naxis2: 0,
            ctype1: "RA---TAN".to_string(),
            ctype2: "DEC--TAN".to_string(),
        }
    }
}

/// Result of plate solving a single frame
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlateSolveResult {
    /// Whether solving succeeded
    pub success: bool,
    /// Error code if failed
    pub error: Option<String>,
    /// Solved center RA (degrees)
    pub ra_center: f64,
    /// Solved center Dec (degrees)
    pub dec_center: f64,
    /// Image scale (arcseconds per pixel)
    pub scale_arcsec_per_pixel: f64,
    /// Rotation angle (degrees)
    pub rotation_deg: f64,
    /// Number of matched stars
    pub matched_stars: u32,
    /// RMS of solution (arcseconds)
    pub rms_arcsec: f64,
    /// WCS solution (if available)
    pub wcs: Option<WcsSolution>,
}

impl Default for PlateSolveResult {
    fn default() -> Self {
        PlateSolveResult {
            success: false,
            error: None,
            ra_center: 0.0,
            dec_center: 0.0,
            scale_arcsec_per_pixel: 0.0,
            rotation_deg: 0.0,
            matched_stars: 0,
            rms_arcsec: 0.0,
            wcs: None,
        }
    }
}

impl PlateSolveResult {
    /// Create a failed result with error message
    pub fn failed(error: &str) -> Self {
        PlateSolveResult {
            success: false,
            error: Some(error.to_string()),
            ..Default::default()
        }
    }
}

/// Result of registering multiple frames
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationResult {
    /// Homography for each frame
    pub homographies: Vec<HomographyData>,
    /// Output image width
    pub output_width: u32,
    /// Output image height
    pub output_height: u32,
    /// Index of reference frame
    pub reference_index: usize,
    /// Number of frames successfully registered
    pub success_count: usize,
    /// Number of frames that failed
    pub failed_count: usize,
}

/// Homography matrix data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomographyData {
    /// Matrix elements (row-major 3x3)
    pub h: [[f64; 3]; 3],
    /// Number of matched star pairs
    pub matched_pairs: i32,
    /// Number of inliers after RANSAC
    pub inliers: i32,
    /// Frame path
    pub frame_path: String,
    /// Whether registration succeeded for this frame
    pub success: bool,
}

impl From<Homography> for HomographyData {
    fn from(h: Homography) -> Self {
        HomographyData {
            h: [
                [h.h00, h.h01, h.h02],
                [h.h10, h.h11, h.h12],
                [h.h20, h.h21, h.h22],
            ],
            matched_pairs: h.pair_matched,
            inliers: h.inliers,
            frame_path: String::new(),
            success: true,
        }
    }
}

impl From<HomographyData> for Homography {
    fn from(d: HomographyData) -> Self {
        Homography {
            h00: d.h[0][0],
            h01: d.h[0][1],
            h02: d.h[0][2],
            h10: d.h[1][0],
            h11: d.h[1][1],
            h12: d.h[1][2],
            h20: d.h[2][0],
            h21: d.h[2][1],
            h22: d.h[2][2],
            pair_matched: d.matched_pairs,
            inliers: d.inliers,
        }
    }
}

/// Batch plate solve result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchPlateSolveResult {
    /// Results per frame (path, result)
    pub results: Vec<(String, PlateSolveResult)>,
    /// Number of successful solves
    pub success_count: usize,
    /// Number of failed solves
    pub failed_count: usize,
}

// =============================================================================
// Plate Solving Functions
// =============================================================================

/// Plate solve a single frame
///
/// Attempts to determine the WCS (World Coordinate System) solution
/// for the image by matching detected stars against a reference catalogue.
///
/// # Arguments
///
/// * `frame` - The FITS image to solve
/// * `config` - Plate solving configuration
///
/// # Returns
///
/// Plate solve result with WCS if successful
pub fn platesolve_frame(
    frame: &mut FitsImage,
    config: &PlateSolveConfig,
) -> StackingResult<PlateSolveResult> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    #[cfg(feature = "siril-ffi")]
    {
        let mut astro_data = AstrometryData::new();

        // Configure solver
        astro_data.solver = config.solver.into();
        astro_data.catalogue = config.catalogue.into();
        astro_data.pixel_size = config.pixel_size_um;
        astro_data.focal_length = config.focal_length_mm;
        astro_data.search_radius = config.search_radius_deg;
        astro_data.trans_order = config.distortion_order as i32;
        astro_data.scale_tolerance = config.scale_tolerance_percent;
        astro_data.max_stars = config.max_stars as i32;
        astro_data.downsample = if config.downsample { GTRUE } else { GFALSE };
        astro_data.fit = frame.as_ptr();

        // Calculate expected scale
        astro_data.scale = unsafe {
            ffi::compute_image_scale(config.pixel_size_um, config.focal_length_mm)
        };

        // Set initial coordinates if provided
        if let (Some(ra), Some(dec)) = (config.initial_ra, config.initial_dec) {
            astro_data.ra = ra;
            astro_data.dec = dec;
        } else {
            // Try to get from FITS header
            unsafe {
                ffi::get_center_from_wcs(frame.as_ptr(), &mut astro_data.ra, &mut astro_data.dec);
            }
        }

        // Set magnitude limit
        match config.magnitude_limit {
            MagnitudeLimit::Auto => {
                astro_data.auto_mag = GTRUE;
            }
            MagnitudeLimit::Fixed(mag) => {
                astro_data.auto_mag = GFALSE;
                astro_data.mag_limit = mag;
            }
        }

        // Run plate solver
        let result = unsafe { ffi::siril_platesolve(frame.as_ptr(), &mut astro_data) };

        if result != 0 || astro_data.ret != SirilSolveError::Ok {
            let error_msg = match astro_data.ret {
                SirilSolveError::NoStars => "No stars detected in image",
                SirilSolveError::NoCatStars => "Failed to retrieve catalogue stars",
                SirilSolveError::NotEnoughMatches => "Not enough star matches",
                SirilSolveError::NoSolution => "Solver failed to converge",
                SirilSolveError::WcsFailed => "WCS computation failed",
                SirilSolveError::Cancelled => "Solving cancelled",
                _ => "Internal error",
            };
            return Ok(PlateSolveResult::failed(error_msg));
        }

        // Build successful result
        Ok(PlateSolveResult {
            success: true,
            error: None,
            ra_center: astro_data.solved_ra,
            dec_center: astro_data.solved_dec,
            scale_arcsec_per_pixel: astro_data.solved_scale,
            rotation_deg: astro_data.solved_rotation,
            matched_stars: astro_data.matched_stars as u32,
            rms_arcsec: astro_data.rms_arcsec,
            wcs: None, // WCS is stored in frame's FITS structure
        })
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Plate solve multiple frames
///
/// # Arguments
///
/// * `frame_paths` - Paths to frames to solve
/// * `config` - Plate solving configuration
/// * `progress` - Optional progress callback (current, total, message)
///
/// # Returns
///
/// Batch result with individual solve results
pub fn platesolve_batch<P: AsRef<Path>>(
    frame_paths: &[P],
    config: &PlateSolveConfig,
    progress: Option<Box<dyn Fn(usize, usize, &str)>>,
) -> StackingResult<BatchPlateSolveResult> {
    if frame_paths.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    let mut results = Vec::with_capacity(frame_paths.len());
    let mut success_count = 0;
    let mut failed_count = 0;

    for (i, path) in frame_paths.iter().enumerate() {
        let path_ref = path.as_ref();

        if let Some(ref cb) = progress {
            cb(
                i + 1,
                frame_paths.len(),
                &format!(
                    "Solving {}",
                    path_ref.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }

        match FitsImage::open(path_ref) {
            Ok(mut frame) => {
                // Use previous successful result as hint for next frame
                let mut frame_config = config.clone();
                if let Some((_, prev_result)) = results.last() {
                    let prev: &PlateSolveResult = prev_result;
                    if prev.success && frame_config.initial_ra.is_none() {
                        frame_config.initial_ra = Some(prev.ra_center);
                        frame_config.initial_dec = Some(prev.dec_center);
                    }
                }

                match platesolve_frame(&mut frame, &frame_config) {
                    Ok(result) => {
                        if result.success {
                            success_count += 1;
                            // Save WCS back to file
                            if let Err(e) = frame.save(path_ref) {
                                eprintln!("Warning: Failed to save WCS to {}: {}", path_ref.display(), e);
                            }
                        } else {
                            failed_count += 1;
                        }
                        results.push((path_ref.display().to_string(), result));
                    }
                    Err(e) => {
                        failed_count += 1;
                        results.push((
                            path_ref.display().to_string(),
                            PlateSolveResult::failed(&e.to_string()),
                        ));
                    }
                }
            }
            Err(e) => {
                failed_count += 1;
                results.push((
                    path_ref.display().to_string(),
                    PlateSolveResult::failed(&format!("Failed to open: {}", e)),
                ));
            }
        }
    }

    Ok(BatchPlateSolveResult {
        results,
        success_count,
        failed_count,
    })
}

// =============================================================================
// Registration Functions
// =============================================================================

/// Register frames using astrometric solution
///
/// Uses WCS (plate-solved coordinates) to compute alignment transformations.
/// All input frames must have valid WCS data from plate solving.
///
/// # Arguments
///
/// * `frame_paths` - Paths to plate-solved frames
/// * `reference_index` - Index of reference frame
/// * `config` - Registration configuration
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Registration result with homographies for each frame
pub fn register_astrometric<P: AsRef<Path>>(
    frame_paths: &[P],
    reference_index: usize,
    config: &RegistrationConfig,
    progress: Option<Box<dyn Fn(usize, usize, &str)>>,
) -> StackingResult<RegistrationResult> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    if frame_paths.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    if reference_index >= frame_paths.len() {
        return Err(StackingError::InvalidParameter(
            "Reference index out of bounds".to_string(),
        ));
    }

    #[cfg(feature = "siril-ffi")]
    {
        // This would use the FFI to compute homographies from WCS
        // For now, return a placeholder
        let mut homographies = Vec::with_capacity(frame_paths.len());

        for (i, path) in frame_paths.iter().enumerate() {
            if let Some(ref cb) = progress {
                cb(
                    i + 1,
                    frame_paths.len(),
                    &format!(
                        "Computing transform for {}",
                        path.as_ref().file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
            }

            let h = if i == reference_index {
                // Identity for reference
                HomographyData {
                    h: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    matched_pairs: 0,
                    inliers: 0,
                    frame_path: path.as_ref().display().to_string(),
                    success: true,
                }
            } else {
                // Would compute from WCS difference
                HomographyData {
                    h: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    matched_pairs: 0,
                    inliers: 0,
                    frame_path: path.as_ref().display().to_string(),
                    success: false, // Mark as needing implementation
                }
            };
            homographies.push(h);
        }

        Ok(RegistrationResult {
            homographies,
            output_width: 0,  // Would be computed from framing
            output_height: 0,
            reference_index,
            success_count: 1,
            failed_count: frame_paths.len() - 1,
        })
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Register frames using star pattern matching
///
/// Detects stars in each frame and matches patterns to compute alignment.
/// Does not require plate-solved images.
///
/// # Arguments
///
/// * `frame_paths` - Paths to frames to register
/// * `reference_index` - Index of reference frame
/// * `config` - Registration configuration
/// * `progress` - Optional progress callback
///
/// # Returns
///
/// Registration result with homographies for each frame
pub fn register_star_alignment<P: AsRef<Path>>(
    frame_paths: &[P],
    reference_index: usize,
    config: &RegistrationConfig,
    progress: Option<Box<dyn Fn(usize, usize, &str)>>,
) -> StackingResult<RegistrationResult> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    if frame_paths.is_empty() {
        return Err(StackingError::EmptySequence);
    }

    if reference_index >= frame_paths.len() {
        return Err(StackingError::InvalidParameter(
            "Reference index out of bounds".to_string(),
        ));
    }

    #[cfg(feature = "siril-ffi")]
    {
        // Would use register_star_alignment FFI function
        // For now, placeholder implementation
        let homographies: Vec<HomographyData> = frame_paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                if let Some(ref cb) = progress {
                    cb(
                        i + 1,
                        frame_paths.len(),
                        &format!(
                            "Aligning {}",
                            path.as_ref().file_name().unwrap_or_default().to_string_lossy()
                        ),
                    );
                }

                HomographyData {
                    h: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    matched_pairs: 0,
                    inliers: 0,
                    frame_path: path.as_ref().display().to_string(),
                    success: i == reference_index,
                }
            })
            .collect();

        Ok(RegistrationResult {
            homographies,
            output_width: 0,
            output_height: 0,
            reference_index,
            success_count: 1,
            failed_count: frame_paths.len() - 1,
        })
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Apply registration transformation to a frame
///
/// Transforms the input image using the provided homography matrix.
///
/// # Arguments
///
/// * `frame` - Input frame to transform
/// * `homography` - Transformation matrix
/// * `output_size` - Output image dimensions (width, height)
/// * `config` - Registration configuration (for interpolation settings)
///
/// # Returns
///
/// Transformed image
pub fn apply_registration(
    frame: &FitsImage,
    homography: &HomographyData,
    output_size: (u32, u32),
    config: &RegistrationConfig,
) -> StackingResult<FitsImage> {
    if !ffi::is_ffi_available() {
        return Err(StackingError::NotInitialized);
    }

    #[cfg(feature = "siril-ffi")]
    {
        let (out_width, out_height) = output_size;
        let (_, _, channels) = frame.dimensions();

        // Create output image
        let mut output = FitsImage::new(out_width, out_height, channels, frame.is_float())?;

        // Convert homography
        let h: Homography = homography.clone().into();

        // Apply transformation
        let result = unsafe {
            ffi::cvTransformImage(
                frame.as_ptr(),
                output.as_ptr(),
                h,
                config.interpolation.into(),
                if config.clamp { GTRUE } else { GFALSE },
                config.clamping_factor,
            )
        };

        if result != 0 {
            return Err(StackingError::TransformFailed);
        }

        Ok(output)
    }

    #[cfg(not(feature = "siril-ffi"))]
    {
        Err(StackingError::NotInitialized)
    }
}

/// Calculate image scale from camera and telescope parameters
///
/// # Arguments
///
/// * `pixel_size_um` - Camera pixel size in micrometers
/// * `focal_length_mm` - Telescope focal length in millimeters
///
/// # Returns
///
/// Image scale in arcseconds per pixel
pub fn calculate_image_scale(pixel_size_um: f64, focal_length_mm: f64) -> f64 {
    if focal_length_mm <= 0.0 {
        return 0.0;
    }
    // Scale = (pixel_size / focal_length) * 206265 arcsec/radian
    (pixel_size_um / focal_length_mm) * 206.265
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plate_solve_config_default() {
        let config = PlateSolveConfig::default();
        assert_eq!(config.solver, Solver::SirilInternal);
        assert_eq!(config.catalogue, Catalogue::GaiaDR3);
        assert!(config.search_radius_deg > 0.0);
    }

    #[test]
    fn test_registration_config_default() {
        let config = RegistrationConfig::default();
        assert_eq!(config.framing, Framing::Max);
        assert_eq!(config.interpolation, Interpolation::Lanczos4);
        assert_eq!(config.transformation, Transformation::Homography);
    }

    #[test]
    fn test_calculate_image_scale() {
        // 3.76um pixels, 500mm focal length
        let scale = calculate_image_scale(3.76, 500.0);
        // Expected: about 1.55 arcsec/pixel
        assert!((scale - 1.55).abs() < 0.1);
    }

    #[test]
    fn test_homography_conversion() {
        let h = Homography {
            h00: 1.0,
            h01: 0.1,
            h02: 10.0,
            h10: -0.1,
            h11: 1.0,
            h12: 20.0,
            h20: 0.0,
            h21: 0.0,
            h22: 1.0,
            pair_matched: 50,
            inliers: 45,
        };

        let data: HomographyData = h.into();
        assert_eq!(data.h[0][0], 1.0);
        assert_eq!(data.h[0][2], 10.0);
        assert_eq!(data.matched_pairs, 50);

        let h2: Homography = data.into();
        assert_eq!(h2.h00, 1.0);
        assert_eq!(h2.pair_matched, 50);
    }

    #[test]
    fn test_solver_conversion() {
        assert_eq!(
            PlateSolveSolver::from(Solver::SirilInternal),
            PlateSolveSolver::SolverSiril
        );
        assert_eq!(
            PlateSolveSolver::from(Solver::AstrometryNet),
            PlateSolveSolver::SolverLocalAsnet
        );
    }

    #[test]
    fn test_framing_conversion() {
        assert_eq!(FramingType::from(Framing::Current), FramingType::FramingCurrent);
        assert_eq!(FramingType::from(Framing::Max), FramingType::FramingMax);
        assert_eq!(FramingType::from(Framing::Min), FramingType::FramingMin);
        assert_eq!(FramingType::from(Framing::CenterOfGravity), FramingType::FramingCog);
    }
}
