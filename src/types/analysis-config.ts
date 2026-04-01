/** Star detection and analysis configuration */
export interface AnalysisConfig {
  detection_sigma: number;
  min_star_area: number;
  max_star_area: number;
  saturation_fraction: number;
  max_stars: number;
  trail_threshold: number;
  mrs_layers: number;
  measure_cap: number;
  fit_max_iter: number;
  fit_tolerance: number;
  fit_max_rejects: number;
  batch_concurrency: number;
}

/** Default analysis configuration values */
export const DEFAULT_ANALYSIS_CONFIG: AnalysisConfig = {
  detection_sigma: 5.0,
  min_star_area: 5,
  max_star_area: 2000,
  saturation_fraction: 0.95,
  max_stars: 500,
  trail_threshold: 0.5,
  mrs_layers: 0,
  measure_cap: 2000,
  fit_max_iter: 25,
  fit_tolerance: 1e-4,
  fit_max_rejects: 5,
  batch_concurrency: 3,
};

/** Star annotation display settings */
export interface AnnotationSettings {
  color_scheme: string; // "eccentricity" | "fwhm" | "uniform"
  show_direction_tick: boolean;
  min_radius: number;
  max_radius: number;
  line_width: number; // 1-3
  ecc_good: number;
  ecc_warn: number;
  fwhm_good: number;
  fwhm_warn: number;
}

/** Default annotation settings matching rustafits defaults */
export const DEFAULT_ANNOTATION_SETTINGS: AnnotationSettings = {
  color_scheme: 'eccentricity',
  show_direction_tick: true,
  min_radius: 6.0,
  max_radius: 60.0,
  line_width: 2,
  ecc_good: 0.5,
  ecc_warn: 0.6,
  fwhm_good: 1.3,
  fwhm_warn: 2.0,
};
