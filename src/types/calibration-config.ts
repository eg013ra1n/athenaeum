// TypeScript interfaces for Calibration Matching Configuration
// Matches the Rust types in src-tauri/src/calibration/config.rs

/** Match mode for parameter comparison */
export enum MatchMode {
  /** Must match exactly (with small tolerance for floats) */
  Exact = "exact",
  /** Match but warn if threshold exceeded, reject if matching threshold exceeded */
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
  /** For Warning mode: threshold that triggers warning display (must be <= matching_threshold) */
  warning_threshold?: number;
  /** For Warning mode: threshold that rejects match if exceeded */
  matching_threshold?: number;
  /** Whether this parameter can be changed by user (false = locked to Exact mode) */
  locked: boolean;
  /** Whether Warning mode is available for this parameter */
  supports_warning: boolean;
}

/** Configuration for matching a specific calibration type */
export interface CalibrationTypeConfig {
  /** Camera/instrument name - exact by default, can be set to ignore */
  instrume: ParameterConfig;
  /** Binning mode (e.g., "1x1", "2x2") - exact by default, can be set to ignore */
  binning: ParameterConfig;
  /** Sensor gain value - exact by default, can be set to ignore */
  gain: ParameterConfig;
  /** Sensor offset value - exact by default, can be set to ignore */
  offset: ParameterConfig;
  /** Telescope name - exact or disabled (no warning mode) */
  telescop: ParameterConfig;
  /** Exposure time in seconds - supports warning mode with thresholds */
  exptime: ParameterConfig;
  /** Focal length in mm - supports warning mode with thresholds */
  focallen: ParameterConfig;
  /** Filter name - exact or disabled (no warning mode) */
  filter: ParameterConfig;
  /** CCD temperature in Celsius - supports warning mode with thresholds */
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
  /** Weight for exposure time proximity in scoring (0.0-1.0) */
  exposure_match_weight: number;
  /** Exposure time scaling factor for scoring formula (default 1.0s) */
  exposure_scale: number;
}

/** Warning thresholds for calibration matching */
export interface WarningConfig {
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
  options: {
    required?: boolean;
    warning_threshold?: number;
    matching_threshold?: number;
    locked?: boolean;
    supports_warning?: boolean;
  } = {}
): ParameterConfig {
  return {
    mode,
    required: options.required ?? false,
    warning_threshold: options.warning_threshold,
    matching_threshold: options.matching_threshold,
    locked: options.locked ?? false,
    supports_warning: options.supports_warning ?? false,
  };
}

/** Helper to create exact match config (can be changed to ignore) */
export function exactMatch(required: boolean = true): ParameterConfig {
  return {
    mode: MatchMode.Exact,
    required,
    locked: false,
    supports_warning: false,
  };
}

/** Helper to create warning match config with dual thresholds */
export function warningMatch(
  warningThreshold: number,
  matchingThreshold: number
): ParameterConfig {
  return {
    mode: MatchMode.Warning,
    required: false,
    warning_threshold: warningThreshold,
    matching_threshold: matchingThreshold,
    locked: false,
    supports_warning: true,
  };
}

/** Helper to create ignore config */
export function ignoreParam(): ParameterConfig {
  return {
    mode: MatchMode.Ignore,
    required: false,
    locked: false,
    supports_warning: false,
  };
}

/** Helper to create ignore config that supports warning mode */
export function ignoreWithWarningSupport(): ParameterConfig {
  return {
    mode: MatchMode.Ignore,
    required: false,
    locked: false,
    supports_warning: true,
  };
}

/** Validate that warning_threshold <= matching_threshold */
export function validateThresholds(config: ParameterConfig): string | null {
  if (config.mode === MatchMode.Warning) {
    const warn = config.warning_threshold ?? 0;
    const match = config.matching_threshold ?? Infinity;
    if (warn > match) {
      return `Warning threshold (${warn}) cannot be greater than matching threshold (${match})`;
    }
  }
  return null;
}

/** Get display label for a parameter */
export function getParameterLabel(param: string): string {
  const labels: Record<string, string> = {
    instrume: "Camera",
    binning: "Binning",
    gain: "Gain",
    offset: "Offset",
    telescop: "Telescope",
    exptime: "Exposure",
    focallen: "Focal Length",
    filter: "Filter",
    ccd_temp: "CCD Temp",
  };
  return labels[param] || param;
}

/** Parameters that can be exact or disabled (no warning mode) */
export const EXACT_OR_DISABLED_PARAMETERS = [
  "instrume",
  "binning",
  "gain",
  "offset",
  "telescop",
  "filter",
] as const;

/** Parameters that support warning mode with dual thresholds */
export const WARNING_CAPABLE_PARAMETERS = [
  "exptime",
  "focallen",
  "ccd_temp",
] as const;

/** All configurable parameters */
export const CONFIGURABLE_PARAMETERS = [
  "instrume",
  "binning",
  "gain",
  "offset",
  "telescop",
  "exptime",
  "focallen",
  "filter",
  "ccd_temp",
] as const;

export type ConfigurableParameter = (typeof CONFIGURABLE_PARAMETERS)[number];
export type ExactOrDisabledParameter =
  (typeof EXACT_OR_DISABLED_PARAMETERS)[number];
export type WarningCapableParameter =
  (typeof WARNING_CAPABLE_PARAMETERS)[number];

/** Check if a parameter supports warning mode */
export function supportsWarningMode(
  param: string
): param is WarningCapableParameter {
  return (WARNING_CAPABLE_PARAMETERS as readonly string[]).includes(param);
}

/** Check if a parameter is exact-or-disabled (no warning mode) */
export function isExactOrDisabled(
  param: string
): param is ExactOrDisabledParameter {
  return (EXACT_OR_DISABLED_PARAMETERS as readonly string[]).includes(param);
}
