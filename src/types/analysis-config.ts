// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type AnalysisPsfModel = "moffat4" | "auto";

export type AnalysisConfig = { 
/**
 * Matched-filter detection threshold in sigma. Default: 6.0 (range 2.0 to 20.0)
 */
detection_threshold: number, 
/**
 * The PSF model of the fit. Default: moffat4
 */
psf_model: AnalysisPsfModel, 
/**
 * The measurement budget N: the brightest good stars kept. Default: 500
 */
max_stars: number, 
/**
 * Concurrent frames during batch analysis. Default: auto (cores/3, min 2).
 * Higher values increase throughput but use more memory (~200MB per concurrent frame).
 */
batch_concurrency: number, };

export type AnnotationSettings = { 
/**
 * Color scheme: "eccentricity", "fwhm", or "uniform"
 */
color_scheme: string, 
/**
 * Draw a direction tick along the elongation axis
 */
show_direction_tick: boolean, 
/**
 * Ellipse semi-axis scale in units of FWHM (semi-major = fwhm_x × scale).
 * The historic hardcoded 2.5 drew ~50px lassos on oversampled frames,
 * making clean single stars read as blends. Absent in stored JSON from
 * older versions → serde default.
 */
ellipse_scale: number, 
/**
 * Minimum ellipse semi-axis radius in output pixels
 */
min_radius: number, 
/**
 * Maximum ellipse semi-axis radius in output pixels
 */
max_radius: number, 
/**
 * Line thickness: 1 = single pixel, 2 = 3px cross, 3 = 5px diamond
 */
line_width: number, 
/**
 * Eccentricity threshold: below this is green (good)
 */
ecc_good: number, 
/**
 * Eccentricity threshold: above this is red (problem)
 */
ecc_warn: number, 
/**
 * FWHM ratio threshold: below this is green (good)
 */
fwhm_good: number, 
/**
 * FWHM ratio threshold: above this is red (problem)
 */
fwhm_warn: number, };

