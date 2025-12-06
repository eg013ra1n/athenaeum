// TypeScript interfaces for Calibration Matching Configuration
// Matches the Rust types in src-tauri/src/calibration/config.rs

/** Match mode for parameter comparison */
export enum MatchMode {
  /** Must match exactly (with small tolerance for floats) */
  Exact = "exact",
  /** Match but warn if threshold exceeded (e.g., temperature delta > 2°C) */
  Warning = "warning",
  /** Don't check this parameter */
  Ignore = "ignore",
}

/** Configuration for a single parameter matching rule */
export interface ParameterConfig {
  /** How to match this parameter */
  mode: MatchMode;
  /** If true and frame's metadata is NULL, skip matching entirely */
  required: boolean;
  /** For Warning mode: threshold value (e.g., temperature delta in °C) */
  warning_threshold?: number;
}

/** Configuration for matching a specific calibration type */
export interface CalibrationTypeConfig {
  /** Camera/instrument name */
  instrume: ParameterConfig;
  /** Binning mode (e.g., "1x1", "2x2") */
  binning: ParameterConfig;
  /** Sensor gain value */
  gain: ParameterConfig;
  /** Sensor offset value */
  offset: ParameterConfig;
  /** Exposure time in seconds */
  exptime: ParameterConfig;
  /** Focal length in mm */
  focallen: ParameterConfig;
  /** Filter name */
  filter: ParameterConfig;
  /** CCD temperature in Celsius */
  ccd_temp: ParameterConfig;
}

/** Configuration for a source type (what calibrations it can link to) */
export interface SourceTypeConfig {
  /** Flat calibration rules (for Lights) */
  flat?: CalibrationTypeConfig;
  /** DarkFlat calibration rules (for Flats) */
  darkflat?: CalibrationTypeConfig;
  /** Dark calibration rules */
  dark?: CalibrationTypeConfig;
  /** Bias calibration rules */
  bias?: CalibrationTypeConfig;
}

/** Behavioral options for a source type */
export interface BehavioralOptions {
  /** Link Bias sets as sub-calibration to Dark sets */
  use_bias_for_dark_optimization: boolean;
  /** Fallback to Bias if Dark not found (for Flats) */
  use_bias_if_no_darks: boolean;
  /** Fallback chain order (e.g., ["darkflat", "dark", "bias"]) */
  fallback_chain: string[];
}

/** Preference for Master frames vs frame sets */
export enum MasterPreference {
  PreferMaster = "prefer_master",
  PreferFrameset = "prefer_frameset",
  NoPreference = "no_preference",
}

/** Clustering configuration for a calibration type */
export interface ClusteringConfig {
  /** Maximum age of frames to consider valid (in days) */
  max_age_days: number;
  /** Time threshold for clustering frames (in minutes) */
  time_cluster_minutes: number;
  /** Temperature threshold for clustering frames (in degrees Celsius) */
  temp_threshold_celsius: number;
}

/** Scoring configuration for calibration matching */
export interface ScoringConfig {
  /** Weight for temperature proximity in scoring (0.0-1.0) */
  temperature_match_weight: number;
  /** Temperature scaling factor for scoring formula (default 2.0) */
  temperature_scale: number;
}

/** Warning thresholds for calibration matching */
export interface WarningConfig {
  /** Temperature delta tolerance in Celsius */
  temp_delta_celsius: number;
  /** Flat calibration date warning threshold in days */
  flat_date_warning_days: number;
  /** Dark calibration date warning threshold in days */
  dark_date_warning_days: number;
  /** DarkFlat calibration date warning threshold in days */
  darkflat_date_warning_days: number;
}

/** Complete calibration matching configuration */
export interface CalibrationMatchingConfig {
  /** Schema version for migration support */
  version: number;
  /** Configuration for Light frames (→ Flat, Dark, Bias) */
  lights: SourceTypeConfig;
  /** Configuration for Flat frames (→ DarkFlat, Dark, Bias with fallback chain) */
  flats: SourceTypeConfig;
  /** Configuration for Dark frames (→ Bias) */
  darks: SourceTypeConfig;
  /** Behavioral options per source type */
  behavioral_options: Record<string, BehavioralOptions>;
  /** Master preference per calibration type */
  master_preferences: Record<string, MasterPreference>;
  /** Clustering configuration per calibration type */
  clustering: Record<string, ClusteringConfig>;
  /** Scoring configuration */
  scoring: ScoringConfig;
  /** Warning thresholds */
  warnings: WarningConfig;
}

/** Helper to create a default ParameterConfig */
export function createParameterConfig(
  mode: MatchMode = MatchMode.Ignore,
  required: boolean = false,
  warning_threshold?: number
): ParameterConfig {
  return { mode, required, warning_threshold };
}

/** Helper to create exact match config */
export function exactMatch(required: boolean = true): ParameterConfig {
  return { mode: MatchMode.Exact, required };
}

/** Helper to create warning match config */
export function warningMatch(threshold: number): ParameterConfig {
  return { mode: MatchMode.Warning, required: false, warning_threshold: threshold };
}

/** Helper to create ignore config */
export function ignoreParam(): ParameterConfig {
  return { mode: MatchMode.Ignore, required: false };
}

/** Get display label for a parameter */
export function getParameterLabel(param: string): string {
  const labels: Record<string, string> = {
    instrume: "Camera",
    binning: "Binning",
    gain: "Gain",
    offset: "Offset",
    exptime: "Exposure",
    focallen: "Focal Length",
    filter: "Filter",
    ccd_temp: "CCD Temp",
  };
  return labels[param] || param;
}

/** All configurable parameters */
export const CONFIGURABLE_PARAMETERS = [
  "instrume",
  "binning",
  "gain",
  "offset",
  "exptime",
  "focallen",
  "filter",
  "ccd_temp",
] as const;

export type ConfigurableParameter = (typeof CONFIGURABLE_PARAMETERS)[number];
