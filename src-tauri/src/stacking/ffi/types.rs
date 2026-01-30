//! Type definitions for stacking module
//!
//! These types are used throughout the stacking module for image processing.
//! They were originally designed for C FFI but are now used for pure Rust implementations.

use serde::{Deserialize, Serialize};
use std::ffi::c_int;

/// GLib boolean type (C int) - kept for compatibility
pub type gboolean = c_int;
pub const GTRUE: gboolean = 1;
pub const GFALSE: gboolean = 0;

// =============================================================================
// Enums
// =============================================================================

/// Data type for image pixels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    DataUshort = 0,
    DataFloat = 1,
    DataUnsupported = 2,
}

impl Default for DataType {
    fn default() -> Self {
        DataType::DataUshort
    }
}

/// Normalization type for stacking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalization {
    NoNorm = 0,
    Additive = 1,
    Multiplicative = 2,
    AdditiveScaling = 3,
    MultiplicativeScaling = 4,
}

impl Default for Normalization {
    fn default() -> Self {
        Normalization::NoNorm
    }
}

/// Pixel rejection algorithm
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    NoRejec = 0,
    Percentile = 1,
    Sigma = 2,
    Mad = 3,
    SigMedian = 4,
    Winsorized = 5,
    LinearFit = 6,
    Gesdt = 7,
}

impl Default for Rejection {
    fn default() -> Self {
        Rejection::NoRejec
    }
}

/// Image arithmetic operator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOperator {
    OperAdd = 0,
    OperSub = 1,
    OperMul = 2,
    OperDiv = 3,
}

/// OpenCV interpolation method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencvInterpolation {
    Nearest = 0,
    Linear = 1,
    Cubic = 2,
    Area = 3,
    Lanczos4 = 4,
    None = 5,
}

impl Default for OpencvInterpolation {
    fn default() -> Self {
        OpencvInterpolation::Lanczos4
    }
}

/// Stacking method enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackMethod {
    StackSum = 0,
    StackMean = 1,
    StackMedian = 2,
    StackMax = 3,
    StackMin = 4,
}

impl Default for StackMethod {
    fn default() -> Self {
        StackMethod::StackMean
    }
}

/// Weighting type for stacking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightingType {
    NoWeight = 0,
    NbstarsWeight = 1,
    WfwhmWeight = 2,
    NoiseWeight = 3,
    NbstackWeight = 4,
}

impl Default for WeightingType {
    fn default() -> Self {
        WeightingType::NoWeight
    }
}

/// Transformation type for registration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformationType {
    Undefined = -3,
    Null = -2,
    Identity = -1,
    Shift = 0,
    Similarity = 1,
    Affine = 2,
    Homography = 3,
}

impl Default for TransformationType {
    fn default() -> Self {
        TransformationType::Homography
    }
}

/// Framing type for registration output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingType {
    FramingCurrent = 0,
    FramingMax = 1,
    FramingMin = 2,
    FramingCog = 3,
}

impl Default for FramingType {
    fn default() -> Self {
        FramingType::FramingMax
    }
}

/// PSF profile type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarProfile {
    Gaussian = 0,
    Moffat = 1,
}

impl Default for StarProfile {
    fn default() -> Self {
        StarProfile::Gaussian
    }
}

/// Bayer/CFA sensor pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorPattern {
    BayerRggb = 0,
    BayerBggr = 1,
    BayerGbrg = 2,
    BayerGrbg = 3,
    XtransFilter1 = 4,
    XtransFilter2 = 5,
    XtransFilter3 = 6,
    XtransFilter4 = 7,
    BayerNone = -1,
}

impl Default for SensorPattern {
    fn default() -> Self {
        SensorPattern::BayerNone
    }
}

impl SensorPattern {
    /// Parse Bayer pattern from string (e.g., "RGGB", "BGGR")
    pub fn from_string(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "RGGB" => SensorPattern::BayerRggb,
            "BGGR" => SensorPattern::BayerBggr,
            "GBRG" => SensorPattern::BayerGbrg,
            "GRBG" => SensorPattern::BayerGrbg,
            _ => SensorPattern::BayerNone,
        }
    }

    /// Check if this is a Bayer pattern (not X-Trans)
    pub fn is_bayer(&self) -> bool {
        matches!(
            self,
            SensorPattern::BayerRggb
                | SensorPattern::BayerBggr
                | SensorPattern::BayerGbrg
                | SensorPattern::BayerGrbg
        )
    }

    /// Check if this is an X-Trans pattern
    pub fn is_xtrans(&self) -> bool {
        matches!(
            self,
            SensorPattern::XtransFilter1
                | SensorPattern::XtransFilter2
                | SensorPattern::XtransFilter3
                | SensorPattern::XtransFilter4
        )
    }
}

/// Demosaicing interpolation method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationMethod {
    /// Bilinear interpolation (fast, lower quality)
    BayerBilinear = 0,
    /// Variable Number of Gradients (good quality, moderate speed)
    BayerVng = 1,
    /// Adaptive Homogeneity-Directed (high quality)
    BayerAhd = 2,
    /// AMaZE (very high quality, slower)
    BayerAmaze = 3,
    /// DCB interpolation
    BayerDcb = 4,
    /// HPHD interpolation
    BayerHphd = 5,
    /// IGV interpolation
    BayerIgv = 6,
    /// LMMSE interpolation
    BayerLmmse = 7,
    /// RCD interpolation (high quality, fast)
    BayerRcd = 8,
    /// X-Trans specific
    Xtrans = 9,
}

impl Default for InterpolationMethod {
    fn default() -> Self {
        InterpolationMethod::BayerVng
    }
}

impl InterpolationMethod {
    /// Get a human-readable name for the method
    pub fn name(&self) -> &'static str {
        match self {
            InterpolationMethod::BayerBilinear => "Bilinear",
            InterpolationMethod::BayerVng => "VNG",
            InterpolationMethod::BayerAhd => "AHD",
            InterpolationMethod::BayerAmaze => "AMaZE",
            InterpolationMethod::BayerDcb => "DCB",
            InterpolationMethod::BayerHphd => "HPHD",
            InterpolationMethod::BayerIgv => "IGV",
            InterpolationMethod::BayerLmmse => "LMMSE",
            InterpolationMethod::BayerRcd => "RCD",
            InterpolationMethod::Xtrans => "X-Trans",
        }
    }
}

// =============================================================================
// Structures
// =============================================================================

/// 2D point with double precision
#[derive(Debug, Clone, Copy, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// 2D point with float precision
#[derive(Debug, Clone, Copy, Default)]
pub struct Pointf {
    pub x: f32,
    pub y: f32,
}

/// Rectangle structure
#[derive(Debug, Clone, Copy, Default)]
pub struct Rectangle {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Homography matrix (3x3)
#[derive(Debug, Clone, Copy)]
pub struct Homography {
    pub h00: f64,
    pub h01: f64,
    pub h02: f64,
    pub h10: f64,
    pub h11: f64,
    pub h12: f64,
    pub h20: f64,
    pub h21: f64,
    pub h22: f64,
    pub pair_matched: i32,
    pub inliers: i32,
}

impl Default for Homography {
    fn default() -> Self {
        // Identity matrix
        Homography {
            h00: 1.0,
            h01: 0.0,
            h02: 0.0,
            h10: 0.0,
            h11: 1.0,
            h12: 0.0,
            h20: 0.0,
            h21: 0.0,
            h22: 1.0,
            pair_matched: 0,
            inliers: 0,
        }
    }
}

/// Image statistics
#[derive(Debug, Clone, Default)]
pub struct ImStats {
    pub total: i64,
    pub ngoodpix: i64,
    pub mean: f64,
    pub median: f64,
    pub sigma: f64,
    pub avg_dev: f64,
    pub mad: f64,
    pub sqrtbwmv: f64,
    pub location: f64,
    pub scale: f64,
    pub min: f64,
    pub max: f64,
    pub norm_value: f64,
    pub bgnoise: f64,
}

/// World coordinate system solution data
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
    /// SIP A coefficients (distortion)
    pub sip_a: Option<Vec<f64>>,
    /// SIP B coefficients (distortion)
    pub sip_b: Option<Vec<f64>>,
    /// SIP AP coefficients (inverse distortion)
    pub sip_ap: Option<Vec<f64>>,
    /// SIP BP coefficients (inverse distortion)
    pub sip_bp: Option<Vec<f64>>,
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
            sip_a: None,
            sip_b: None,
            sip_ap: None,
            sip_bp: None,
        }
    }
}

/// Plate solve error codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SirilSolveError {
    /// Success
    Ok = 0,
    /// No stars found in the image
    NoStars = 1,
    /// Failed to retrieve catalogue stars
    NoCatStars = 2,
    /// Not enough matches to compute solution
    NotEnoughMatches = 3,
    /// Solver failed to converge
    NoSolution = 4,
    /// WCS computation failed
    WcsFailed = 5,
    /// User cancelled
    Cancelled = 6,
    /// Internal error
    InternalError = 7,
}

impl Default for SirilSolveError {
    fn default() -> Self {
        SirilSolveError::Ok
    }
}

/// Star catalogue type for plate solving
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogueType {
    /// No catalogue
    CatNone = 0,
    /// USNO-B1 catalogue
    CatUsnoB = 1,
    /// Tycho-2 catalogue
    CatTycho2 = 2,
    /// NOMAD catalogue
    CatNomad = 3,
    /// Gaia DR2
    CatGaiaDr2 = 4,
    /// Gaia EDR3
    CatGaiaEdr3 = 5,
    /// Gaia DR3
    CatGaiaDr3 = 6,
    /// APASS
    CatApass = 7,
    /// 2MASS
    Cat2mass = 8,
    /// Local catalogue file
    CatLocal = 9,
}

impl Default for CatalogueType {
    fn default() -> Self {
        CatalogueType::CatGaiaDr3
    }
}

/// Plate solver type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlateSolveSolver {
    /// Siril's internal solver
    SolverSiril = 0,
    /// Local astrometry.net installation
    SolverLocalAsnet = 1,
}

impl Default for PlateSolveSolver {
    fn default() -> Self {
        PlateSolveSolver::SolverSiril
    }
}

// =============================================================================
// Return codes
// =============================================================================

pub const ST_ALLOC_ERROR: i32 = -10;
pub const ST_CANCEL: i32 = -9;
pub const ST_SEQUENCE_ERROR: i32 = -2;
pub const ST_GENERIC_ERROR: i32 = -1;
pub const ST_OK: i32 = 0;

// =============================================================================
// Stub structures for backwards compatibility
// These were previously used for FFI but are now stub types for compatibility
// =============================================================================

/// Star finder parameters (stub for compatibility)
#[derive(Debug, Clone, Default)]
pub struct StarFinderParams {
    pub sigma: f64,
    pub roundness_limit: f64,
    pub max_stars: i32,
    pub min_beta: f64,
    pub max_beta: f64,
    pub min_a: f64,
    pub max_a: f64,
    pub profile: StarProfile,
}

/// PSF Star structure (stub for compatibility)
#[derive(Debug, Clone, Default)]
pub struct PsfStar {
    pub x: f64,
    pub y: f64,
    pub fwhmx: f64,
    pub fwhmy: f64,
    pub a: f64,
    pub b: f64,
    pub has_saturated: i32,
}

impl PsfStar {
    pub fn fwhm(&self) -> f64 {
        (self.fwhmx * self.fwhmy).sqrt()
    }

    pub fn roundness(&self) -> f64 {
        if self.fwhmx > self.fwhmy {
            self.fwhmy / self.fwhmx
        } else {
            self.fwhmx / self.fwhmy
        }
    }
}

/// Preprocessing data structure (stub for compatibility)
#[derive(Debug, Clone, Default)]
pub struct PreprocessingData {
    pub use_bias: gboolean,
    pub bias: *const std::ffi::c_void,
    pub use_dark: gboolean,
    pub dark: *const std::ffi::c_void,
    pub use_dark_optim: gboolean,
    pub use_exposure: gboolean,
    pub use_flat: gboolean,
    pub flat: *const std::ffi::c_void,
    pub autolevel: gboolean,
    pub normalisation: f32,
    pub allow_32bit_output: gboolean,
    pub is_sequence: gboolean,
    pub debayer: gboolean,
}

impl PreprocessingData {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Stacking arguments structure (stub for compatibility)
#[derive(Debug, Clone, Default)]
pub struct StackingArgs {
    pub nb_images_to_stack: i32,
    pub ref_image: i32,
    pub use_32bit_output: gboolean,
    pub retval: i32,
}

impl StackingArgs {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Astrometry data structure (stub for compatibility)
#[derive(Debug, Clone, Default)]
pub struct AstrometryData {
    pub solver: PlateSolveSolver,
    pub catalogue: CatalogueType,
    pub pixel_size: f64,
    pub focal_length: f64,
    pub search_radius: f64,
    pub trans_order: i32,
    pub scale_tolerance: f64,
    pub max_stars: i32,
    pub downsample: gboolean,
    pub fit: *const std::ffi::c_void,
    pub scale: f64,
    pub ra: f64,
    pub dec: f64,
    pub auto_mag: gboolean,
    pub mag_limit: f64,
    pub ret: SirilSolveError,
    pub solved_ra: f64,
    pub solved_dec: f64,
    pub solved_scale: f64,
    pub solved_rotation: f64,
    pub matched_stars: i32,
    pub rms_arcsec: f64,
}

impl AstrometryData {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Sequence structure (stub for compatibility)
#[derive(Debug, Clone)]
pub struct Sequence {
    pub number: i32,
    pub selnum: i32,
}

impl Default for Sequence {
    fn default() -> Self {
        Sequence {
            number: 0,
            selnum: 0,
        }
    }
}

impl Sequence {
    pub fn new() -> Self {
        Self::default()
    }
}
