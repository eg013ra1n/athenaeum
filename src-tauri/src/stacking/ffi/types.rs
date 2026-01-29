//! C type definitions for Siril FFI
//!
//! These types mirror the C structures defined in Siril's header files.
//! All types use #[repr(C)] to ensure compatible memory layout.

use std::ffi::{c_char, c_double, c_float, c_int, c_long, c_uint, c_void};
use std::ptr;

/// GLib boolean type (C int)
pub type gboolean = c_int;
pub const GTRUE: gboolean = 1;
pub const GFALSE: gboolean = 0;

/// GLib pointer type
pub type gpointer = *mut c_void;

/// FITS WORD type (16-bit unsigned)
pub type WORD = u16;

/// FITS BYTE type (8-bit unsigned)
pub type BYTE = u8;

// =============================================================================
// Enums
// =============================================================================

/// Data type for image pixels
#[repr(C)]
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

/// Image type enum (file format)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    TypeUndef = 1 << 1,
    TypeFits = 1 << 2,
    TypeTiff = 1 << 3,
    TypeBmp = 1 << 4,
    TypePng = 1 << 5,
    TypeJpg = 1 << 6,
    TypeHeif = 1 << 7,
    TypePnm = 1 << 8,
    TypePic = 1 << 9,
    TypeRaw = 1 << 10,
    TypeXisf = 1 << 11,
    TypeJxl = 1 << 12,
    TypeAvif = 1 << 13,
    TypeAvi = 1 << 20,
    TypeSer = 1 << 21,
}

/// Normalization type for stacking
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOperator {
    OperAdd = 0,
    OperSub = 1,
    OperMul = 2,
    OperDiv = 3,
}

/// OpenCV interpolation method
#[repr(C)]
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

/// Sequence type
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceType {
    SeqRegular = 0,
    SeqSer = 1,
    SeqFitseq = 2,
    SeqAvi = 3,
    SeqInternal = 4,
}

/// Threading type
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadingType {
    MultiThreaded = 0,
    SingleThreaded = 1,
}

impl Default for ThreadingType {
    fn default() -> Self {
        ThreadingType::MultiThreaded
    }
}

/// Stacking method enum
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
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
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Point {
    pub x: c_double,
    pub y: c_double,
}

/// 2D point with float precision
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Pointf {
    pub x: c_float,
    pub y: c_float,
}

/// 2D point with integer coordinates
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Pointi {
    pub x: c_int,
    pub y: c_int,
}

/// Rectangle structure
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rectangle {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

/// Homography matrix (3x3)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Homography {
    pub h00: c_double,
    pub h01: c_double,
    pub h02: c_double,
    pub h10: c_double,
    pub h11: c_double,
    pub h12: c_double,
    pub h20: c_double,
    pub h21: c_double,
    pub h22: c_double,
    pub pair_matched: c_int,
    pub inliers: c_int,
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
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ImStats {
    pub total: c_long,
    pub ngoodpix: c_long,
    pub mean: c_double,
    pub median: c_double,
    pub sigma: c_double,
    pub avg_dev: c_double,
    pub mad: c_double,
    pub sqrtbwmv: c_double,
    pub location: c_double,
    pub scale: c_double,
    pub min: c_double,
    pub max: c_double,
    pub norm_value: c_double,
    pub bgnoise: c_double,
    pub _nb_refs: c_int,
}

/// WCS info from FITS header
#[repr(C)]
pub struct WcsInfo {
    pub objctra: [c_char; 71],  // FLEN_VALUE
    pub objctdec: [c_char; 71], // FLEN_VALUE
    pub ra: c_double,
    pub dec: c_double,
    pub pltsolvd: gboolean,
    pub pltsolvd_comment: [c_char; 73], // FLEN_COMMENT
}

/// FITS keywords structure (subset of fkeywords)
/// This is a simplified version - full struct has many more fields
#[repr(C)]
pub struct FitsKeywords {
    pub bscale: c_double,
    pub bzero: c_double,
    pub lo: WORD,
    pub hi: WORD,
    pub flo: c_float,
    pub fhi: c_float,
    pub program: [c_char; 71],
    pub filename: [c_char; 71],
    pub data_max: c_double,
    pub data_min: c_double,
    pub pixel_size_x: c_double,
    pub pixel_size_y: c_double,
    pub binning_x: c_uint,
    pub binning_y: c_uint,
    pub row_order: [c_char; 71],
    // Dates - opaque pointers to GDateTime
    pub date: gpointer,
    pub date_obs: gpointer,
    pub mjd_obs: c_double,
    pub expstart: c_double,
    pub expend: c_double,
    pub filter: [c_char; 71],
    pub image_type: [c_char; 71],
    pub object: [c_char; 71],
    pub instrume: [c_char; 71],
    pub telescop: [c_char; 71],
    pub observer: [c_char; 71],
    pub centalt: c_double,
    pub centaz: c_double,
    pub sitelat: c_double,
    pub sitelong: c_double,
    pub sitelat_str: [c_char; 71],
    pub sitelong_str: [c_char; 71],
    pub siteelev: c_double,
    pub bayer_pattern: [c_char; 71],
    pub bayer_xoffset: c_int,
    pub bayer_yoffset: c_int,
    pub airmass: c_double,
    pub focal_length: c_double,
    pub flength: c_double,
    pub iso_speed: c_double,
    pub exposure: c_double,
    pub aperture: c_double,
    pub ccd_temp: c_double,
    pub set_temp: c_double,
    pub livetime: c_double,
    pub stackcnt: c_uint,
    pub cvf: c_double,
    pub key_gain: c_int,
    pub key_offset: c_int,
    pub focname: [c_char; 71],
    pub focuspos: c_int,
    pub focussz: c_int,
    pub foctemp: c_double,
    // WCS data
    pub wcsdata: WcsInfo,
    pub wcslib: *mut c_void, // wcsprm pointer
    // DFT info - simplified
    pub dft_norm: [c_double; 3],
    pub dft_type: [c_char; 71],
    pub dft_ord: [c_char; 71],
}

/// Main FITS image structure
///
/// This corresponds to the `fits` struct in siril.h
#[repr(C)]
pub struct Fits {
    pub rx: c_uint,           // Image width
    pub ry: c_uint,           // Image height
    pub bitpix: c_int,        // Current bitpix
    pub orig_bitpix: c_int,   // Original bitpix
    pub naxis: c_int,         // Number of axes
    pub naxes: [c_long; 3],   // Dimensions [width, height, channels]
    pub keywords: FitsKeywords,
    pub checksum: gboolean,
    pub header: *mut c_char,  // Entire FITS header
    pub unknown_keys: *mut c_char,
    pub stats: *mut *mut ImStats, // Per-layer statistics
    pub mini: c_double,
    pub maxi: c_double,
    pub neg_ratio: c_float,
    pub fptr: *mut c_void,    // fitsfile pointer
    pub data_type: DataType,
    pub data: *mut WORD,      // 16-bit pixel data
    pub pdata: [*mut WORD; 3], // Per-layer pointers
    pub fdata: *mut c_float,  // 32-bit float data
    pub fpdata: [*mut c_float; 3], // Per-layer float pointers
    pub top_down: gboolean,
    pub debayer_checked: gboolean,
    pub orig_ry: c_uint,
    pub x_offset: c_int,
    pub y_offset: c_int,
    pub focalkey: gboolean,
    pub pixelkey: gboolean,
    pub history: *mut c_void, // GSList pointer
    pub color_managed: gboolean,
    pub icc_profile: *mut c_void, // cmsHPROFILE
}

impl Fits {
    /// Create a null/empty fits structure
    pub fn null() -> Self {
        unsafe { std::mem::zeroed() }
    }

    /// Check if this fits has 32-bit float data
    pub fn is_float(&self) -> bool {
        self.data_type == DataType::DataFloat
    }

    /// Get number of channels (1 for mono, 3 for RGB)
    pub fn channels(&self) -> u32 {
        if self.naxis == 2 {
            1
        } else {
            self.naxes[2] as u32
        }
    }
}

/// Per-image data in a sequence
#[repr(C)]
pub struct ImgData {
    pub filenum: c_int,
    pub incl: gboolean,
    pub date_obs: *mut c_void, // GDateTime
    pub airmass: c_double,
    pub rx: c_int,
    pub ry: c_int,
}

/// Registration data per image per layer
#[repr(C)]
pub struct RegData {
    pub fwhm_data: *mut PsfStar,
    pub fwhm: c_float,
    pub weighted_fwhm: c_float,
    pub roundness: c_float,
    pub quality: c_double,
    pub background_lvl: c_float,
    pub number_of_stars: c_int,
    pub h: Homography,
}

/// PSF/Star measurement result (fwhm_struct in Siril)
///
/// This is the complete structure for a fitted star with all measurement data.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PsfStar {
    pub star_name: *mut c_char,   // Star name/identifier
    pub b: c_double,              // Average sky background value
    pub a: c_double,              // Amplitude
    pub x0: c_double,             // X coordinate of peak
    pub y0: c_double,             // Y coordinate of peak
    pub sx: c_double,             // Size in X (PSF coordinates)
    pub sy: c_double,             // Size in Y (PSF coordinates)
    pub fwhmx: c_double,          // FWHM in X axis
    pub fwhmy: c_double,          // FWHM in Y axis
    pub fwhmx_arcsec: c_double,   // FWHM X in arcseconds
    pub fwhmy_arcsec: c_double,   // FWHM Y in arcseconds
    pub angle: c_double,          // Rotation angle
    pub rmse: c_double,           // RMSE of minimization
    pub sat: c_double,            // Saturation level
    pub r: c_int,                 // Optimized box size
    pub has_saturated: gboolean,  // Has saturated pixels

    // Moffat parameters
    pub beta: c_double,           // Moffat beta parameter
    pub profile: StarProfile,     // Profile type (Gaussian/Moffat)

    pub xpos: c_double,           // Position X in image
    pub ypos: c_double,           // Position Y in image

    // Photometry data
    pub mag: c_double,            // V magnitude
    pub b_mag: c_double,          // B magnitude
    pub s_mag: c_double,          // Magnitude error
    pub s_b_mag: c_double,        // B magnitude error
    pub snr: c_double,            // Signal to noise ratio
    pub phot: *mut c_void,        // Photometry data pointer
    pub phot_is_valid: gboolean,  // Photometry validity flag
    pub bv: c_double,             // B-V color index

    // Uncertainties
    pub b_err: c_double,
    pub a_err: c_double,
    pub x_err: c_double,
    pub y_err: c_double,
    pub sx_err: c_double,
    pub sy_err: c_double,
    pub ang_err: c_double,
    pub beta_err: c_double,

    pub layer: c_int,             // Image layer
    pub units: *mut c_char,       // Units string

    pub ra: c_double,             // Right ascension
    pub dec: c_double,            // Declination
}

impl PsfStar {
    /// Get average FWHM (geometric mean of X and Y)
    pub fn fwhm(&self) -> f64 {
        (self.fwhmx * self.fwhmy).sqrt()
    }

    /// Get roundness (ratio of FWHM Y to FWHM X)
    pub fn roundness(&self) -> f64 {
        if self.fwhmx > 0.0 {
            self.fwhmy / self.fwhmx
        } else {
            1.0
        }
    }
}

/// Star finder error codes
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfError {
    Ok = 0,
    FwhmTooLarge = 2,
    RmseTooLarge = 3,
    CenterOff = 10,
    NoFwhm = 11,
    NoPos = 12,
    NoMag = 13,
    FwhmTooSmall = 14,
    FwhmNeg = 15,
    RoundnessBelowCrit = 16,
    MoffatBetaTooSmall = 17,
    AmplitudeOutsideRange = 18,
}

/// PSF fitting error codes
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsfError {
    NoErr = 0,
    ErrAlloc = 3,
    ErrUnsupported = 4,
    ErrDiverged = 5,
    ErrOutOfWindow = 6,
    ErrInnerTooSmall = 7,
    ErrApertureTooSmall = 8,
    ErrTooFewBgPix = 9,
    ErrMeanFailed = 10,
    ErrInvalidStdError = 11,
    ErrInvalidPixValue = 12,
    ErrWindowTooSmall = 13,
    ErrInvalidImage = 14,
    ErrFluxRatio = 15,
    ErrMaxValue = 16,
}

/// Plate solver type
#[repr(C)]
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

/// Plate solve error codes
#[repr(C)]
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
#[repr(C)]
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

/// Polynomial order for astrometric distortion correction
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistortionOrder {
    /// No distortion correction (linear WCS)
    OrderNone = 0,
    /// SIP polynomial order 1
    Order1 = 1,
    /// SIP polynomial order 2
    Order2 = 2,
    /// SIP polynomial order 3
    Order3 = 3,
    /// SIP polynomial order 4
    Order4 = 4,
    /// SIP polynomial order 5
    Order5 = 5,
}

impl Default for DistortionOrder {
    fn default() -> Self {
        DistortionOrder::OrderNone
    }
}

/// Astrometry solver configuration and state
///
/// This structure holds the configuration for plate solving and
/// the results after solving.
#[repr(C)]
pub struct AstrometryData {
    /// Input FITS image to solve
    pub fit: *mut Fits,
    /// Camera pixel size in micrometers
    pub pixel_size: c_double,
    /// Telescope focal length in millimeters
    pub focal_length: c_double,
    /// Calculated image scale (arcsec/pixel)
    pub scale: c_double,
    /// Scale tolerance for matching (percent)
    pub scale_tolerance: c_double,
    /// Solver to use
    pub solver: PlateSolveSolver,
    /// Star catalogue to use
    pub catalogue: CatalogueType,
    /// Reference star catalogue data (opaque)
    pub ref_stars: *mut c_void,
    /// Initial center coordinates (opaque SirilWorldCS)
    pub cat_center: *mut c_void,
    /// Initial RA guess (degrees)
    pub ra: c_double,
    /// Initial Dec guess (degrees)
    pub dec: c_double,
    /// Search radius (degrees)
    pub search_radius: c_double,
    /// Magnitude limit for star matching
    pub mag_limit: c_double,
    /// Use automatic magnitude limit
    pub auto_mag: gboolean,
    /// SIP distortion polynomial order
    pub trans_order: c_int,
    /// Maximum number of stars to use for solving
    pub max_stars: c_int,
    /// Downsample image before solving
    pub downsample: gboolean,
    /// Image needs flipping
    pub flip_image: gboolean,
    /// Return error code
    pub ret: SirilSolveError,
    /// Whether image was flipped during solve
    pub image_flipped: gboolean,
    /// Output WCS solution (wcsprm pointer)
    pub wcsdata: *mut c_void,
    /// Number of matched stars
    pub matched_stars: c_int,
    /// RMS of solution in arcseconds
    pub rms_arcsec: c_double,
    /// Solved center RA (degrees)
    pub solved_ra: c_double,
    /// Solved center Dec (degrees)
    pub solved_dec: c_double,
    /// Solved rotation angle (degrees)
    pub solved_rotation: c_double,
    /// Solved plate scale (arcsec/pixel)
    pub solved_scale: c_double,
}

impl AstrometryData {
    /// Create a new zeroed astrometry data structure
    pub fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }

    /// Check if the solve was successful
    pub fn is_solved(&self) -> bool {
        self.ret == SirilSolveError::Ok && !self.wcsdata.is_null()
    }
}

impl Default for AstrometryData {
    fn default() -> Self {
        Self::new()
    }
}

/// World coordinate system data (simplified view)
///
/// This holds the key WCS parameters that can be extracted from
/// a successful plate solve.
#[derive(Debug, Clone)]
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

/// Star finder parameters
#[repr(C)]
pub struct StarFinderParams {
    pub sigma: c_double,
    pub roundness_limit: c_double,
    pub max_stars: c_int,
    pub min_beta: c_double,
    pub max_beta: c_double,
    pub min_a: c_double,
    pub max_a: c_double,
    pub profile: StarProfile,
    // Additional fields handled internally
}

impl Default for StarFinderParams {
    fn default() -> Self {
        StarFinderParams {
            sigma: 3.0,
            roundness_limit: 0.5,
            max_stars: 500,
            min_beta: 1.5,
            max_beta: 10.0,
            min_a: 0.0,
            max_a: 1.0,
            profile: StarProfile::Gaussian,
        }
    }
}

/// Sequence structure
#[repr(C)]
pub struct Sequence {
    pub seqname: *mut c_char,
    pub number: c_int,
    pub selnum: c_int,
    pub fixed: c_int,
    pub nb_layers: c_int,
    pub rx: c_uint,
    pub ry: c_uint,
    pub is_variable: gboolean,
    pub is_drizzle: gboolean,
    pub bitpix: c_int,
    pub reference_image: c_int,
    pub imgparam: *mut ImgData,
    pub regparam: *mut *mut RegData,
    pub stats: *mut *mut *mut ImStats,
    // Many more fields omitted - we'll add as needed
    pub seq_type: SequenceType,
    pub current: c_int,
}

/// Normalization coefficients
#[repr(C)]
pub struct NormCoeff {
    pub offset: *mut c_double,
    pub mul: *mut c_double,
    pub scale: *mut c_double,
    pub poffset: [*mut c_double; 3],
    pub pmul: [*mut c_double; 3],
    pub pscale: [*mut c_double; 3],
}

/// Stacking arguments structure
#[repr(C)]
pub struct StackingArgs {
    pub method: *const c_void, // stack_method function pointer
    pub seq: *mut Sequence,
    pub ref_image: c_int,
    pub filtering_criterion: *const c_void,
    pub filtering_parameter: c_double,
    pub nb_images_to_stack: c_int,
    pub image_indices: *mut c_int,
    pub description: *mut c_char,
    pub output_filename: *const c_char,
    pub output_parsed_filename: *mut c_char,
    pub output_overwrite: gboolean,
    pub lite_norm: gboolean,
    pub normalize: Normalization,
    pub coeff: NormCoeff,
    pub force_norm: gboolean,
    pub output_norm: gboolean,
    pub use_32bit_output: gboolean,
    pub feather_dist: c_int,
    pub overlap_norm: gboolean,
    pub reglayer: c_int,
    pub equalize_rgb: gboolean,
    pub maximize_framing: gboolean,
    pub offset: [c_int; 2],
    pub upscale_at_stacking: gboolean,
    pub drizzle: gboolean,
    pub type_of_rejection: Rejection,
    pub sig: [c_float; 2],
    pub critical_value: *mut c_float,
    pub create_rejmaps: gboolean,
    pub merge_lowhigh_rejmaps: gboolean,
    pub rejmap_low: *mut Fits,
    pub rejmap_high: *mut Fits,
    pub weighting_type: WeightingType,
    pub weights: *mut c_double,
    pub sd_calculator: *const c_void,
    pub mad_calculator: *const c_void,
    // timeval t_start - opaque
    pub t_start_sec: c_long,
    pub t_start_usec: c_long,
    pub retval: c_int,
    pub result: Fits,
}

impl StackingArgs {
    /// Create a zeroed stacking args structure
    pub fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Default for StackingArgs {
    fn default() -> Self {
        Self::new()
    }
}

/// Preprocessing data structure
#[repr(C)]
pub struct PreprocessingData {
    pub use_bias: gboolean,
    pub use_dark: gboolean,
    pub use_flat: gboolean,
    pub bias: *mut Fits,
    pub dark: *mut Fits,
    pub flat: *mut Fits,
    pub bias_level: c_float,
    pub use_dark_optim: gboolean,
    pub autolevel: gboolean,
    pub normalisation: c_float,
    pub equalize_cfa: gboolean,
    pub fix_xtrans: gboolean,
    pub use_exposure: gboolean,
    pub is_sequence: gboolean,
    pub seq: *mut Sequence,
    pub output_seqtype: SequenceType,
    pub allow_32bit_output: gboolean,
    pub ppprefix: *mut c_char,
    pub debayer: gboolean,
    pub history: *mut c_void, // GSList
    pub use_cosmetic_correction: gboolean,
    pub cc_from_dark: gboolean,
    pub bad_pixel_map_file: *mut c_void, // GFile
    pub sigma: [c_double; 2],
    pub icold: c_long,
    pub ihot: c_long,
    pub dev: *mut c_void, // deviant_pixel
    pub is_cfa: gboolean,
    pub ignore_exclusion: gboolean,
    pub retval: c_int,
}

impl PreprocessingData {
    pub fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Default for PreprocessingData {
    fn default() -> Self {
        Self::new()
    }
}

/// Registration arguments
#[repr(C)]
pub struct RegistrationArgs {
    pub func: *const c_void, // registration_function
    pub seq: *mut Sequence,
    pub bayer: gboolean,
    pub reference_image: c_int,
    // seq_filter_config filters - complex, omitted
    pub layer: c_int,
    pub retval: c_int,
    pub run_in_thread: gboolean,
    pub follow_star: gboolean,
    pub match_selection: gboolean,
    pub selection: Rectangle,
    pub sfargs: *mut c_void, // starfinder_data
    pub min_pairs: c_int,
    pub max_stars_candidates: c_int,
    pub transformation_type: TransformationType,
    pub percent_moved: c_float,
    pub velocity: Pointf,
    pub reference_date: *mut c_void, // GDateTime
    pub two_pass: gboolean,
    pub filtering_criterion: *const c_void,
    pub filtering_parameter: c_double,
    pub no_starlist: gboolean,
    pub output_scale: c_float,
    // disto fields omitted
    pub framing: FramingType,
    pub interpolation: OpencvInterpolation,
    pub clamp: gboolean,
    pub clamping_factor: c_double,
    pub no_output: gboolean,
    pub new_total: c_int,
    pub imgparam: *mut ImgData,
    pub regparam: *mut RegData,
    pub prefix: *mut c_char,
    pub load_new_sequence: gboolean,
    pub wcsref: *mut c_void, // wcsprm
    pub new_seq_name: *mut c_char,
}

impl RegistrationArgs {
    pub fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Default for RegistrationArgs {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Copy flags for copyfits
// =============================================================================

pub const CP_INIT: c_int = 0x01;   // Initialize data array with 0s
pub const CP_ALLOC: c_int = 0x02;  // Realloc data array
pub const CP_COPYA: c_int = 0x04;  // Copy data array content
pub const CP_FORMAT: c_int = 0x08; // Copy metadata
pub const CP_EXPAND: c_int = 0x20; // Expand 1-channel to 3-channel

// =============================================================================
// Stacking return codes
// =============================================================================

pub const ST_ALLOC_ERROR: c_int = -10;
pub const ST_CANCEL: c_int = -9;
pub const ST_SEQUENCE_ERROR: c_int = -2;
pub const ST_GENERIC_ERROR: c_int = -1;
pub const ST_OK: c_int = 0;
