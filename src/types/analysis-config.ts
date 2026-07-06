// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type AnalysisConfig = { 
/**
 * Detection threshold in sigma above background. Default: 5.0
 */
detection_sigma: number, 
/**
 * Minimum connected-component area in pixels. Default: 5
 */
min_star_area: number, 
/**
 * Maximum connected-component area in pixels. Default: 2000
 */
max_star_area: number, 
/**
 * Reject stars with peak > this fraction of saturation. Default: 0.95
 */
saturation_fraction: number, 
/**
 * Keep only the brightest N stars. Default: 500
 */
max_stars: number, 
/**
 * R² threshold for trail detection. Default: 0.5 (range 0.0-1.0)
 */
trail_threshold: number, 
/**
 * MRS wavelet noise estimation layers. Default: 0 (disabled)
 */
mrs_layers: number, 
/**
 * Max stars to PSF-fit. 0 = measure all. Default: 2000
 */
measure_cap: number, 
/**
 * LM max iterations for measurement pass. Default: 25
 */
fit_max_iter: number, 
/**
 * LM convergence tolerance for measurement pass. Default: 1e-4
 */
fit_tolerance: number, 
/**
 * LM consecutive reject bailout. Default: 5
 */
fit_max_rejects: number, 
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

