/** Star detection and analysis configuration */
export interface AnalysisConfig {
  detection_sigma: number;
  min_star_area: number;
  max_star_area: number;
  saturation_fraction: number;
  max_stars: number;
  trail_threshold: number;
  use_gaussian_fit: boolean;
  background_mesh_size: number | null;
  use_moffat_fit: boolean;
  iterative_background: number;
  mrs_noise: number;
  moffat_beta: number | null;
  max_distortion: number | null;
  scoring_weights: ScoringWeights;
}

/** Weights for composite quality score calculation */
export interface ScoringWeights {
  fwhm: number;
  eccentricity: number;
  snr_weight: number;
  star_count: number;
}

/** Default analysis configuration values */
export const DEFAULT_ANALYSIS_CONFIG: AnalysisConfig = {
  detection_sigma: 5.0,
  min_star_area: 5,
  max_star_area: 2000,
  saturation_fraction: 0.95,
  max_stars: 500,
  trail_threshold: 0.5,
  use_gaussian_fit: true,
  background_mesh_size: 64,
  use_moffat_fit: true,
  iterative_background: 1,
  mrs_noise: 0,
  moffat_beta: null,
  max_distortion: null,
  scoring_weights: {
    fwhm: 0.35,
    eccentricity: 0.15,
    snr_weight: 0.40,
    star_count: 0.10,
  },
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
