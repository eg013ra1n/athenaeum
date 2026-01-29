//! FFI function declarations for Siril C library
//!
//! These are extern "C" declarations for Siril functions.
//! All functions are unsafe by nature of FFI.
//!
//! # Note
//!
//! These bindings are currently stubs. They will be linked when the
//! build system is configured to compile and link the Siril C library.

use super::types::*;
use std::ffi::{c_char, c_double, c_float, c_int, c_void};

// =============================================================================
// FITS I/O Functions (io/image_format_fits.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Read a FITS file into a fits structure
    ///
    /// # Arguments
    /// * `filename` - Path to the FITS file
    /// * `fit` - Output fits structure (must be pre-allocated)
    /// * `realname` - Buffer for resolved filename (can be null)
    /// * `force_float` - Convert to float data
    ///
    /// # Returns
    /// 0 on success, non-zero on error
    pub fn readfits(
        filename: *const c_char,
        fit: *mut Fits,
        realname: *mut c_char,
        force_float: gboolean,
    ) -> c_int;

    /// Save a fits structure to a FITS file
    ///
    /// # Arguments
    /// * `name` - Output filename
    /// * `fit` - The fits structure to save
    ///
    /// # Returns
    /// 0 on success, non-zero on error
    pub fn savefits(name: *const c_char, fit: *mut Fits) -> c_int;

    /// Copy fits structure
    ///
    /// # Arguments
    /// * `from` - Source fits
    /// * `to` - Destination fits
    /// * `oper` - Copy operation flags (CP_*)
    /// * `layer` - Layer to copy (-1 for all)
    ///
    /// # Returns
    /// 0 on success
    pub fn copyfits(from: *mut Fits, to: *mut Fits, oper: c_int, layer: c_int) -> c_int;

    /// Free fits structure memory
    pub fn clearfits(fit: *mut Fits);

    /// Create a new fits image
    ///
    /// # Arguments
    /// * `fit` - Pointer to pointer for new fits
    /// * `width` - Image width
    /// * `height` - Image height
    /// * `nblayer` - Number of layers
    /// * `type_` - Data type
    ///
    /// # Returns
    /// 0 on success
    pub fn new_fit_image(
        fit: *mut *mut Fits,
        width: c_int,
        height: c_int,
        nblayer: c_int,
        type_: DataType,
    ) -> c_int;

    /// Replace buffer in fits structure
    pub fn fit_replace_buffer(fit: *mut Fits, newbuf: *mut c_void, newtype: DataType);
}

// =============================================================================
// Arithmetic Functions (core/arithm.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Scalar operation: a = a OP scalar
    ///
    /// # Arguments
    /// * `a` - Input/output image
    /// * `scalar` - Scalar value
    /// * `oper` - Operation (add, sub, mul, div)
    /// * `conv_to_float` - Convert result to float
    ///
    /// # Returns
    /// 0 on success
    pub fn soper(
        a: *mut Fits,
        scalar: c_float,
        oper: ImageOperator,
        conv_to_float: gboolean,
    ) -> c_int;

    /// Image operation: a = a OP b
    ///
    /// # Arguments
    /// * `a` - Input/output image
    /// * `b` - Second input image
    /// * `oper` - Operation
    /// * `allow_32bits` - Allow 32-bit output
    ///
    /// # Returns
    /// 0 on success
    pub fn imoper(
        a: *mut Fits,
        b: *mut Fits,
        oper: ImageOperator,
        allow_32bits: gboolean,
    ) -> c_int;

    /// Flat division with normalization
    ///
    /// # Arguments
    /// * `a` - Light frame
    /// * `b` - Flat frame
    /// * `scalar` - Normalization factor
    /// * `allow_32bits` - Allow 32-bit output
    ///
    /// # Returns
    /// 0 on success
    pub fn siril_fdiv(
        a: *mut Fits,
        b: *mut Fits,
        scalar: c_float,
        allow_32bits: gboolean,
    ) -> c_int;
}

// =============================================================================
// Preprocessing Functions (core/preprocess.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Preprocess a single image (uses gfit global)
    ///
    /// # Arguments
    /// * `args` - Preprocessing configuration
    ///
    /// # Returns
    /// 0 on success
    pub fn preprocess_single_image(args: *mut PreprocessingData) -> c_int;

    /// Preprocess a given image file
    ///
    /// # Arguments
    /// * `file` - Path to image file
    /// * `args` - Preprocessing configuration
    ///
    /// # Returns
    /// 0 on success
    pub fn preprocess_given_image(file: *mut c_char, args: *mut PreprocessingData) -> c_int;

    /// Calibrate a single image (batch calibration entry point)
    ///
    /// # Arguments
    /// * `args` - Preprocessing configuration with image loaded
    ///
    /// # Returns
    /// 0 on success
    pub fn calibrate_single_image(args: *mut PreprocessingData) -> c_int;

    /// Start sequence preprocessing
    pub fn start_sequence_preprocessing(prepro: *mut PreprocessingData);

    /// Clear preprocessing data
    pub fn clear_preprocessing_data(prepro: *mut PreprocessingData);

    /// Compute flat normalization value (mean of center)
    ///
    /// # Arguments
    /// * `flat` - Flat frame
    ///
    /// # Returns
    /// Mean value of center region for normalization
    pub fn compute_flat_mean_from_rgb(flat: *mut Fits) -> c_float;
}

// =============================================================================
// Stacking Functions (stacking/stacking.c, stacking/median_and_mean.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Median stacking
    ///
    /// # Arguments
    /// * `args` - Stacking configuration
    ///
    /// # Returns
    /// 0 on success, negative on error
    pub fn stack_median(args: *mut StackingArgs) -> c_int;

    /// Mean stacking with rejection
    ///
    /// # Arguments
    /// * `args` - Stacking configuration
    ///
    /// # Returns
    /// 0 on success, negative on error
    pub fn stack_mean_with_rejection(args: *mut StackingArgs) -> c_int;

    /// Max stacking
    pub fn stack_addmax(args: *mut StackingArgs) -> c_int;

    /// Min stacking
    pub fn stack_addmin(args: *mut StackingArgs) -> c_int;

    /// Main stacking entry point
    pub fn main_stack(args: *mut StackingArgs);

    /// Clean up after stacking
    pub fn clean_end_stacking(args: *mut StackingArgs);

    /// Initialize stacking args with defaults
    pub fn init_stacking_args(args: *mut StackingArgs);

    /// Deep copy stacking args
    pub fn stacking_args_deep_copy(from: *mut StackingArgs, to: *mut StackingArgs);

    /// Deep free stacking args
    pub fn stacking_args_deep_free(args: *mut StackingArgs);
}

// =============================================================================
// Normalization Functions (stacking/normalization.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Compute normalization coefficients
    ///
    /// # Arguments
    /// * `args` - Stacking args with normalization config
    ///
    /// # Returns
    /// 0 on success
    pub fn do_normalization(args: *mut StackingArgs) -> c_int;
}

// =============================================================================
// Star Finding Functions (algos/star_finder.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Find stars in an image (main star detection function)
    ///
    /// # Arguments
    /// * `image` - Input image struct pointer
    /// * `layer` - Channel to analyze (0 for mono/R, 1 for G, 2 for B)
    /// * `sf` - Star finder parameters
    /// * `nb_stars` - Output: number of stars found
    /// * `area` - Search area (null for whole image)
    /// * `showtime` - Print timing info to console
    /// * `limit_nbstars` - Whether to limit number of stars returned
    /// * `maxstars` - Max stars if limit_nbstars is true
    /// * `profile` - PSF profile type (Gaussian or Moffat)
    /// * `threads` - Threading mode (0 = multi, 1 = single)
    ///
    /// # Returns
    /// Array of PSF star pointers (must be freed with free_fitted_stars)
    pub fn peaker(
        image: *mut c_void, // image struct
        layer: c_int,
        sf: *mut StarFinderParams,
        nb_stars: *mut c_int,
        area: *mut Rectangle,
        showtime: gboolean,
        limit_nbstars: gboolean,
        maxstars: c_int,
        profile: StarProfile,
        threads: c_int,
    ) -> *mut *mut PsfStar;

    /// Free star array returned by peaker
    pub fn free_fitted_stars(stars: *mut *mut PsfStar);

    /// Allocate new array for fitted stars
    pub fn new_fitted_stars(n: usize) -> *mut *mut PsfStar;

    /// Measure average FWHM of an image (quick measurement)
    ///
    /// # Arguments
    /// * `fit` - Input FITS image
    /// * `channel` - Channel to analyze
    ///
    /// # Returns
    /// Average FWHM in pixels, or -1 on error
    pub fn measure_image_FWHM(fit: *mut Fits, channel: c_int) -> c_float;

    /// Get FWHM statistics from star array
    ///
    /// # Arguments
    /// * `stars` - Array of stars
    /// * `nb` - Number of stars
    /// * `bitpix` - Image bit depth
    /// * `fwhmx` - Output: average FWHM X
    /// * `fwhmy` - Output: average FWHM Y
    /// * `units` - Output: units string
    /// * `b` - Output: average background
    /// * `acut` - Output: amplitude cutoff
    /// * `acutp` - Amplitude cutoff percentile
    pub fn FWHM_stats(
        stars: *mut *mut PsfStar,
        nb: c_int,
        bitpix: c_int,
        fwhmx: *mut c_float,
        fwhmy: *mut c_float,
        units: *mut *mut c_char,
        b: *mut c_float,
        acut: *mut c_float,
        acutp: c_double,
    );

    /// Calculate filtered FWHM average (excludes outliers)
    pub fn filtered_FWHM_average(stars: *mut *mut PsfStar, nb: c_int) -> c_float;

    /// Sort stars by magnitude (brightest first)
    pub fn sort_stars_by_mag(stars: *mut *mut PsfStar, total: c_int);

    /// Filter stars by amplitude threshold
    pub fn filter_stars_by_amplitude(
        stars: *mut *mut PsfStar,
        threshold: c_float,
        nbfilteredstars: *mut c_int,
    ) -> *mut *mut PsfStar;
}

// =============================================================================
// PSF Fitting Functions (algos/PSF.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Get FWHM from a selection area
    ///
    /// # Arguments
    /// * `fit` - Input FITS image
    /// * `layer` - Channel to analyze
    /// * `selection` - Area to analyze
    /// * `roundness` - Output: star roundness
    ///
    /// # Returns
    /// FWHM value in pixels
    pub fn psf_get_fwhm(
        fit: *mut Fits,
        layer: c_int,
        selection: *mut Rectangle,
        roundness: *mut c_double,
    ) -> c_double;

    /// PSF minimization to fit star
    ///
    /// # Arguments
    /// * `fit` - Input FITS image
    /// * `layer` - Channel to analyze
    /// * `area` - Area containing the star
    /// * `for_photometry` - Enable photometry calculations
    /// * `init_from_center` - Initialize from area center
    /// * `phot_set` - Photometry config (can be null)
    /// * `verbose` - Print verbose output
    /// * `profile` - PSF profile type
    /// * `error` - Output: error code
    ///
    /// # Returns
    /// Fitted star data or null on error
    pub fn psf_get_minimisation(
        fit: *mut Fits,
        layer: c_int,
        area: *mut Rectangle,
        for_photometry: gboolean,
        init_from_center: gboolean,
        phot_set: *mut c_void, // phot_config
        verbose: gboolean,
        profile: StarProfile,
        error: *mut PsfError,
    ) -> *mut PsfStar;

    /// Create new PSF star structure
    pub fn new_psf_star() -> *mut PsfStar;

    /// Initialize PSF star with defaults
    pub fn psf_star_init(s: *mut PsfStar);

    /// Duplicate a PSF star
    pub fn duplicate_psf(psf: *mut PsfStar) -> *mut PsfStar;

    /// Free a PSF star
    pub fn free_psf(psf: *mut PsfStar);

    /// Convert FWHM to arcseconds if possible
    pub fn fwhm_to_arcsec_if_needed(fit: *mut Fits, star: *mut PsfStar);
}

// =============================================================================
// Registration Functions (registration/registration.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Global star alignment registration
    ///
    /// Performs star-based registration by matching stars between frames.
    ///
    /// # Arguments
    /// * `args` - Registration arguments with sequence and configuration
    ///
    /// # Returns
    /// 0 on success, non-zero on error
    pub fn register_star_alignment(args: *mut RegistrationArgs) -> c_int;

    /// Two-pass registration
    ///
    /// First pass: rough alignment using few stars
    /// Second pass: refined alignment using many stars
    pub fn register_multi_step_global(args: *mut RegistrationArgs) -> c_int;

    /// Apply registration transforms to create aligned images
    ///
    /// Takes the computed homographies and applies them to create
    /// transformed/aligned output images.
    pub fn register_apply_reg(args: *mut RegistrationArgs) -> c_int;

    /// Initialize registration args with default values
    pub fn init_registration_args(args: *mut RegistrationArgs);

    /// Free resources in registration args
    pub fn clear_registration_args(args: *mut RegistrationArgs);

    /// Compute output dimensions for given framing type
    pub fn compute_framing(
        seq: *mut Sequence,
        args: *mut RegistrationArgs,
        new_rx: *mut c_int,
        new_ry: *mut c_int,
    ) -> c_int;
}

// =============================================================================
// Image Transformation Functions (registration/opencv_functions.cpp)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Transform image using homography matrix
    ///
    /// # Arguments
    /// * `src` - Source image
    /// * `dst` - Destination image (pre-allocated with correct size)
    /// * `h` - Homography transformation matrix
    /// * `interpolation` - Interpolation method
    /// * `clamp` - Enable clamping
    /// * `clamping_factor` - Clamping factor (typically 3.0)
    ///
    /// # Returns
    /// 0 on success
    pub fn cvTransformImage(
        src: *mut Fits,
        dst: *mut Fits,
        h: Homography,
        interpolation: OpencvInterpolation,
        clamp: gboolean,
        clamping_factor: c_double,
    ) -> c_int;

    /// Remap image using coordinate maps
    pub fn cvRemap(
        src: *mut Fits,
        dst: *mut Fits,
        mapx: *mut c_void,
        mapy: *mut c_void,
        interpolation: OpencvInterpolation,
    ) -> c_int;

    /// Compute coordinate maps from homography
    pub fn cvComputeMaps(
        width: c_int,
        height: c_int,
        h: Homography,
        mapx: *mut *mut c_void,
        mapy: *mut *mut c_void,
    ) -> c_int;

    /// Free coordinate maps
    pub fn cvFreeMaps(mapx: *mut c_void, mapy: *mut c_void);

    /// Calculate inverse homography
    pub fn cvInvertH(h: Homography, h_inv: *mut Homography) -> c_int;
}

// =============================================================================
// Drizzle Functions (registration/drizzle.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Apply drizzle algorithm
    ///
    /// Drizzle is used for upscaling during stacking to recover
    /// sub-pixel detail.
    ///
    /// # Arguments
    /// * `args` - Stacking args with drizzle configuration
    ///
    /// # Returns
    /// 0 on success
    pub fn drizzle_image(args: *mut StackingArgs) -> c_int;
}

// =============================================================================
// WCS Functions (algos/siril_wcs.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Convert pixel to world coordinates
    pub fn pix2wcs(
        wcs: *mut c_void,
        px: c_double,
        py: c_double,
        ra: *mut c_double,
        dec: *mut c_double,
    ) -> c_int;

    /// Convert world to pixel coordinates
    pub fn wcs2pix(
        wcs: *mut c_void,
        ra: c_double,
        dec: c_double,
        px: *mut c_double,
        py: *mut c_double,
    ) -> c_int;

    /// Check if FITS has valid WCS data
    pub fn has_wcs(fit: *mut Fits) -> gboolean;

    /// Free WCS data structure
    pub fn free_wcs(wcs: *mut c_void);

    /// Duplicate WCS data structure
    pub fn wcs_dup(wcs: *mut c_void) -> *mut c_void;

    /// Load WCS from FITS header
    pub fn load_WCS_from_fits(fit: *mut Fits) -> c_int;

    /// Write WCS to FITS header
    pub fn save_wcs_to_file(fit: *mut Fits) -> c_int;
}

// =============================================================================
// Plate Solving Functions (algos/astrometry_solver.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Main plate solver entry point (run as thread function)
    ///
    /// # Arguments
    /// * `p` - Pointer to AstrometryData structure
    ///
    /// # Returns
    /// GINT_TO_POINTER of error code (0 on success)
    pub fn plate_solver(p: gpointer) -> gpointer;

    /// Initialize astrometry data with defaults
    pub fn init_astrometry_data(data: *mut AstrometryData);

    /// Free astrometry data internal resources
    pub fn free_astrometry_data(data: *mut AstrometryData);

    /// Plate solve a single image (synchronous)
    ///
    /// # Arguments
    /// * `fit` - Input FITS image
    /// * `data` - Astrometry configuration (filled with results)
    ///
    /// # Returns
    /// 0 on success, error code on failure
    pub fn siril_platesolve(fit: *mut Fits, data: *mut AstrometryData) -> c_int;

    /// Calculate image scale from camera and telescope parameters
    ///
    /// # Arguments
    /// * `pixel_size_um` - Camera pixel size in micrometers
    /// * `focal_length_mm` - Telescope focal length in millimeters
    ///
    /// # Returns
    /// Image scale in arcseconds per pixel
    pub fn compute_image_scale(pixel_size_um: c_double, focal_length_mm: c_double) -> c_double;

    /// Extract center coordinates from FITS header
    pub fn get_center_from_wcs(
        fit: *mut Fits,
        ra: *mut c_double,
        dec: *mut c_double,
    ) -> c_int;

    /// Get image dimensions from FITS
    pub fn get_image_dimensions(
        fit: *mut Fits,
        width: *mut c_int,
        height: *mut c_int,
    );
}

// =============================================================================
// Astrometric Registration Functions (registration/astrometric.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Compute homographies from astrometric (WCS) solutions
    ///
    /// This computes the transformation matrices to align images
    /// based on their WCS solutions (plate-solved coordinates).
    ///
    /// # Arguments
    /// * `seq` - Sequence of images
    /// * `included` - Array of included image flags
    /// * `ref_index` - Reference image index
    /// * `wcsdata` - Reference WCS data
    /// * `framing` - Output framing type
    /// * `layer` - Channel to use
    /// * `h_out` - Output array of homographies
    /// * `prm_out` - Output WCS for result
    ///
    /// # Returns
    /// 0 on success
    pub fn compute_Hs_from_astrometry(
        seq: *mut Sequence,
        included: *mut c_int,
        ref_index: c_int,
        wcsdata: *mut c_void,
        framing: FramingType,
        layer: c_int,
        h_out: *mut Homography,
        prm_out: *mut *mut c_void,
    ) -> c_int;

    /// Collect WCS solutions from all images in sequence
    ///
    /// # Arguments
    /// * `seq` - Sequence of plate-solved images
    /// * `wcs_array` - Output array of WCS pointers
    ///
    /// # Returns
    /// Number of successful WCS extractions
    pub fn collect_sequence_astrometry(
        seq: *mut Sequence,
        wcs_array: *mut *mut c_void,
    ) -> c_int;

    /// Check if sequence has astrometric data
    pub fn sequence_has_astrometry(seq: *mut Sequence) -> gboolean;
}

// =============================================================================
// Demosaicing Functions (algos/demosaicing.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Debayer a CFA image
    ///
    /// # Arguments
    /// * `fit` - Input/output image
    /// * `interpolation` - Debayer method
    /// * `pattern` - Bayer pattern
    ///
    /// # Returns
    /// 0 on success
    pub fn debayer(
        fit: *mut Fits,
        interpolation: InterpolationMethod,
        pattern: SensorPattern,
    ) -> c_int;

    /// Debayer if the image needs it
    ///
    /// # Arguments
    /// * `imagetype` - Type of image (LIGHT, FLAT, etc.)
    /// * `fit` - Input/output image
    /// * `force_debayer` - Force debayering even if not detected
    ///
    /// # Returns
    /// 0 on success
    pub fn debayer_if_needed(
        imagetype: c_int, // image type enum
        fit: *mut Fits,
        force_debayer: gboolean,
    ) -> c_int;

    /// Get validated CFA pattern from FITS
    ///
    /// # Arguments
    /// * `fit` - Input image
    /// * `force_debayer` - Force pattern detection
    /// * `verbose` - Print verbose output
    ///
    /// # Returns
    /// Detected sensor pattern
    pub fn get_validated_cfa_pattern(
        fit: *mut Fits,
        force_debayer: gboolean,
        verbose: gboolean,
    ) -> SensorPattern;

    /// Check if FITS image is CFA (has Bayer pattern)
    pub fn fit_is_cfa(fit: *mut Fits) -> gboolean;

    /// Get CFA pattern index from string
    pub fn get_cfa_pattern_index_from_string(bayer: *const c_char) -> SensorPattern;
}

// =============================================================================
// Statistics Functions (algos/statistics.c)
// =============================================================================

#[cfg(feature = "siril-ffi")]
extern "C" {
    /// Compute image statistics
    pub fn compute_all_channels_statistics_single_image(
        fit: *mut Fits,
        option: c_int,
        threading: ThreadingType,
    );
}

// =============================================================================
// Stub implementations when FFI is not linked
// These allow the code to compile without the C library
// =============================================================================

#[cfg(not(feature = "siril-ffi"))]
pub mod stubs {
    use super::*;

    /// Stub: Read FITS file
    ///
    /// Returns error since library not linked
    pub unsafe fn readfits(
        _filename: *const c_char,
        _fit: *mut Fits,
        _realname: *mut c_char,
        _force_float: gboolean,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Save FITS file
    pub unsafe fn savefits(_name: *const c_char, _fit: *mut Fits) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Copy fits
    pub unsafe fn copyfits(
        _from: *mut Fits,
        _to: *mut Fits,
        _oper: c_int,
        _layer: c_int,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Clear fits
    pub unsafe fn clearfits(_fit: *mut Fits) {}

    /// Stub: New fit image
    pub unsafe fn new_fit_image(
        _fit: *mut *mut Fits,
        _width: c_int,
        _height: c_int,
        _nblayer: c_int,
        _type_: DataType,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Median stacking
    pub unsafe fn stack_median(_args: *mut StackingArgs) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Mean stacking
    pub unsafe fn stack_mean_with_rejection(_args: *mut StackingArgs) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Debayer
    pub unsafe fn debayer(
        _fit: *mut Fits,
        _interpolation: InterpolationMethod,
        _pattern: SensorPattern,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Debayer if needed
    pub unsafe fn debayer_if_needed(
        _imagetype: c_int,
        _fit: *mut Fits,
        _force_debayer: gboolean,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Get validated CFA pattern
    pub unsafe fn get_validated_cfa_pattern(
        _fit: *mut Fits,
        _force_debayer: gboolean,
        _verbose: gboolean,
    ) -> SensorPattern {
        SensorPattern::BayerNone
    }

    /// Stub: Check if CFA
    pub unsafe fn fit_is_cfa(_fit: *mut Fits) -> gboolean {
        GFALSE
    }

    /// Stub: Preprocess single image
    pub unsafe fn preprocess_single_image(_args: *mut PreprocessingData) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Preprocess given image
    pub unsafe fn preprocess_given_image(
        _file: *mut c_char,
        _args: *mut PreprocessingData,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Calibrate single image
    pub unsafe fn calibrate_single_image(_args: *mut PreprocessingData) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Clear preprocessing data
    pub unsafe fn clear_preprocessing_data(_prepro: *mut PreprocessingData) {}

    /// Stub: Peaker (star detection)
    pub unsafe fn peaker(
        _image: *mut c_void,
        _layer: c_int,
        _sf: *mut StarFinderParams,
        nb_stars: *mut c_int,
        _area: *mut Rectangle,
        _showtime: gboolean,
        _limit_nbstars: gboolean,
        _maxstars: c_int,
        _profile: StarProfile,
        _threads: c_int,
    ) -> *mut *mut PsfStar {
        if !nb_stars.is_null() {
            *nb_stars = 0;
        }
        std::ptr::null_mut()
    }

    /// Stub: Free fitted stars
    pub unsafe fn free_fitted_stars(_stars: *mut *mut PsfStar) {}

    /// Stub: Measure image FWHM
    pub unsafe fn measure_image_FWHM(_fit: *mut Fits, _channel: c_int) -> c_float {
        -1.0
    }

    /// Stub: PSF get FWHM
    pub unsafe fn psf_get_fwhm(
        _fit: *mut Fits,
        _layer: c_int,
        _selection: *mut Rectangle,
        _roundness: *mut c_double,
    ) -> c_double {
        -1.0
    }

    /// Stub: PSF minimization
    pub unsafe fn psf_get_minimisation(
        _fit: *mut Fits,
        _layer: c_int,
        _area: *mut Rectangle,
        _for_photometry: gboolean,
        _init_from_center: gboolean,
        _phot_set: *mut c_void,
        _verbose: gboolean,
        _profile: StarProfile,
        _error: *mut PsfError,
    ) -> *mut PsfStar {
        std::ptr::null_mut()
    }

    /// Stub: New PSF star
    pub unsafe fn new_psf_star() -> *mut PsfStar {
        std::ptr::null_mut()
    }

    /// Stub: Free PSF
    pub unsafe fn free_psf(_psf: *mut PsfStar) {}

    /// Stub: Initialization check
    pub fn is_ffi_available() -> bool {
        false
    }

    // =========================================================================
    // WCS Stubs
    // =========================================================================

    /// Stub: Check if FITS has WCS
    pub unsafe fn has_wcs(_fit: *mut Fits) -> gboolean {
        GFALSE
    }

    /// Stub: Free WCS
    pub unsafe fn free_wcs(_wcs: *mut c_void) {}

    /// Stub: Duplicate WCS
    pub unsafe fn wcs_dup(_wcs: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// Stub: Load WCS from FITS
    pub unsafe fn load_WCS_from_fits(_fit: *mut Fits) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Save WCS to FITS
    pub unsafe fn save_wcs_to_file(_fit: *mut Fits) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Pixel to world coordinates
    pub unsafe fn pix2wcs(
        _wcs: *mut c_void,
        _px: c_double,
        _py: c_double,
        _ra: *mut c_double,
        _dec: *mut c_double,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: World to pixel coordinates
    pub unsafe fn wcs2pix(
        _wcs: *mut c_void,
        _ra: c_double,
        _dec: c_double,
        _px: *mut c_double,
        _py: *mut c_double,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    // =========================================================================
    // Plate Solving Stubs
    // =========================================================================

    /// Stub: Plate solver
    pub unsafe fn plate_solver(_p: gpointer) -> gpointer {
        std::ptr::null_mut()
    }

    /// Stub: Init astrometry data
    pub unsafe fn init_astrometry_data(_data: *mut AstrometryData) {}

    /// Stub: Free astrometry data
    pub unsafe fn free_astrometry_data(_data: *mut AstrometryData) {}

    /// Stub: Siril plate solve
    pub unsafe fn siril_platesolve(_fit: *mut Fits, data: *mut AstrometryData) -> c_int {
        if !data.is_null() {
            (*data).ret = SirilSolveError::InternalError;
        }
        ST_GENERIC_ERROR
    }

    /// Stub: Compute image scale
    pub unsafe fn compute_image_scale(pixel_size_um: c_double, focal_length_mm: c_double) -> c_double {
        if focal_length_mm > 0.0 {
            // Scale in arcsec/pixel = (pixel_size_um / focal_length_mm) * 206.265
            (pixel_size_um / focal_length_mm) * 206.265
        } else {
            0.0
        }
    }

    /// Stub: Get center from WCS
    pub unsafe fn get_center_from_wcs(
        _fit: *mut Fits,
        _ra: *mut c_double,
        _dec: *mut c_double,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Get image dimensions
    pub unsafe fn get_image_dimensions(
        fit: *mut Fits,
        width: *mut c_int,
        height: *mut c_int,
    ) {
        if !fit.is_null() && !width.is_null() && !height.is_null() {
            *width = (*fit).rx as c_int;
            *height = (*fit).ry as c_int;
        }
    }

    // =========================================================================
    // Astrometric Registration Stubs
    // =========================================================================

    /// Stub: Compute homographies from astrometry
    pub unsafe fn compute_Hs_from_astrometry(
        _seq: *mut Sequence,
        _included: *mut c_int,
        _ref_index: c_int,
        _wcsdata: *mut c_void,
        _framing: FramingType,
        _layer: c_int,
        _h_out: *mut Homography,
        _prm_out: *mut *mut c_void,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Collect sequence astrometry
    pub unsafe fn collect_sequence_astrometry(
        _seq: *mut Sequence,
        _wcs_array: *mut *mut c_void,
    ) -> c_int {
        0
    }

    /// Stub: Check sequence astrometry
    pub unsafe fn sequence_has_astrometry(_seq: *mut Sequence) -> gboolean {
        GFALSE
    }

    // =========================================================================
    // Registration Stubs
    // =========================================================================

    /// Stub: Star alignment registration
    pub unsafe fn register_star_alignment(_args: *mut RegistrationArgs) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Multi-step registration
    pub unsafe fn register_multi_step_global(_args: *mut RegistrationArgs) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Apply registration
    pub unsafe fn register_apply_reg(_args: *mut RegistrationArgs) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Init registration args
    pub unsafe fn init_registration_args(_args: *mut RegistrationArgs) {}

    /// Stub: Clear registration args
    pub unsafe fn clear_registration_args(_args: *mut RegistrationArgs) {}

    /// Stub: Compute framing
    pub unsafe fn compute_framing(
        _seq: *mut Sequence,
        _args: *mut RegistrationArgs,
        _new_rx: *mut c_int,
        _new_ry: *mut c_int,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    // =========================================================================
    // Image Transformation Stubs
    // =========================================================================

    /// Stub: Transform image with homography
    pub unsafe fn cvTransformImage(
        _src: *mut Fits,
        _dst: *mut Fits,
        _h: Homography,
        _interpolation: OpencvInterpolation,
        _clamp: gboolean,
        _clamping_factor: c_double,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Remap image
    pub unsafe fn cvRemap(
        _src: *mut Fits,
        _dst: *mut Fits,
        _mapx: *mut c_void,
        _mapy: *mut c_void,
        _interpolation: OpencvInterpolation,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Compute coordinate maps
    pub unsafe fn cvComputeMaps(
        _width: c_int,
        _height: c_int,
        _h: Homography,
        _mapx: *mut *mut c_void,
        _mapy: *mut *mut c_void,
    ) -> c_int {
        ST_GENERIC_ERROR
    }

    /// Stub: Free coordinate maps
    pub unsafe fn cvFreeMaps(_mapx: *mut c_void, _mapy: *mut c_void) {}

    /// Stub: Invert homography
    pub unsafe fn cvInvertH(h: Homography, h_inv: *mut Homography) -> c_int {
        if !h_inv.is_null() {
            // Return identity as stub
            *h_inv = Homography::default();
        }
        0
    }

    /// Stub: Drizzle
    pub unsafe fn drizzle_image(_args: *mut StackingArgs) -> c_int {
        ST_GENERIC_ERROR
    }
}

#[cfg(not(feature = "siril-ffi"))]
pub use stubs::*;

/// Check if FFI is available (library linked)
#[cfg(feature = "siril-ffi")]
pub fn is_ffi_available() -> bool {
    true
}

#[cfg(not(feature = "siril-ffi"))]
pub fn is_ffi_available() -> bool {
    false
}
