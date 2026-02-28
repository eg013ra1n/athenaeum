/** Star detection and analysis configuration */
export interface AnalysisConfig {
  detection_sigma: number;
  min_star_area: number;
  max_star_area: number;
  saturation_fraction: number;
  max_stars: number;
  max_eccentricity: number;
  trail_threshold: number;
  use_gaussian_fit: boolean;
  background_mesh_size: number | null;
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
  max_stars: 200,
  max_eccentricity: 0.9,
  trail_threshold: 0.6,
  use_gaussian_fit: true,
  background_mesh_size: null,
  scoring_weights: {
    fwhm: 0.35,
    eccentricity: 0.15,
    snr_weight: 0.40,
    star_count: 0.10,
  },
};
