// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type MatchMode = "exact" | "warning" | "ignore";

export type ParameterConfig = { 
/**
 * How to match this parameter
 */
mode: MatchMode, 
/**
 * If true and frame's metadata is NULL, skip matching entirely
 */
required: boolean, 
/**
 * For Warning mode: threshold that triggers warning display (must be <= matching_threshold)
 */
warning_threshold?: number, 
/**
 * For Warning mode: threshold that rejects match if exceeded
 * If value difference > matching_threshold, the calibration set is rejected.
 * If value difference > warning_threshold but <= matching_threshold, match is accepted with warning.
 */
matching_threshold?: number, 
/**
 * Whether this parameter can be changed by user (false = locked to Exact mode)
 */
locked: boolean, 
/**
 * Whether Warning mode is available for this parameter
 */
supports_warning: boolean, };

export type CalibrationTypeConfig = { 
/**
 * Camera/instrument name - exact by default, can be set to ignore
 */
instrume: ParameterConfig, 
/**
 * Binning mode (e.g., "1x1", "2x2") - exact by default, can be set to ignore
 */
binning: ParameterConfig, 
/**
 * Sensor gain value - exact by default, can be set to ignore
 */
gain: ParameterConfig, 
/**
 * Sensor offset value - exact by default, can be set to ignore
 */
offset: ParameterConfig, 
/**
 * Telescope name - exact or disabled (no warning mode)
 */
telescop: ParameterConfig, 
/**
 * Exposure time in seconds - supports warning mode with thresholds
 */
exptime: ParameterConfig, 
/**
 * Focal length in mm - supports warning mode with thresholds
 */
focallen: ParameterConfig, 
/**
 * Filter name - exact or disabled (no warning mode)
 */
filter: ParameterConfig, 
/**
 * CCD temperature in Celsius - supports warning mode with thresholds
 */
ccd_temp: ParameterConfig, };

export type SourceTypeConfig = { 
/**
 * Flat calibration rules (for Lights)
 */
flat?: CalibrationTypeConfig, 
/**
 * DarkFlat calibration rules (for Flats)
 */
darkflat?: CalibrationTypeConfig, 
/**
 * Dark calibration rules
 */
dark?: CalibrationTypeConfig, 
/**
 * Bias calibration rules
 */
bias?: CalibrationTypeConfig, };

export type BehavioralOptions = { 
/**
 * Link Bias sets as sub-calibration to Dark sets
 */
use_bias_for_dark_optimization: boolean, 
/**
 * Fallback to Bias if Dark not found (for Flats)
 */
use_bias_if_no_darks: boolean, 
/**
 * Fallback chain order (e.g., ["darkflat", "dark", "bias"])
 */
fallback_chain: Array<string>, };

export type MasterPreference = "prefer_master" | "prefer_frameset" | "no_preference";

export type ClusteringConfig = { 
/**
 * Maximum age of frames to consider valid (in days)
 */
max_age_days: number, 
/**
 * Time threshold for clustering frames (in minutes)
 */
time_cluster_minutes: number, 
/**
 * Temperature threshold for clustering frames (in degrees Celsius)
 */
temp_threshold_celsius: number, };

export type ScoringConfig = { 
/**
 * Weight for temperature proximity in scoring (0.0-1.0)
 */
temperature_match_weight: number, 
/**
 * Temperature scaling factor for scoring formula (default 2.0)
 */
temperature_scale: number, 
/**
 * Weight for exposure time proximity in scoring (0.0-1.0)
 */
exposure_match_weight: number, 
/**
 * Exposure time scaling factor for scoring formula (default 1.0s)
 */
exposure_scale: number, };

export type WarningConfig = { 
/**
 * Flat calibration date warning threshold in days
 */
flat_date_warning_days: number, 
/**
 * Dark calibration date warning threshold in days
 */
dark_date_warning_days: number, 
/**
 * DarkFlat calibration date warning threshold in days
 */
darkflat_date_warning_days: number, };

export type CalibrationMatchingConfig = { 
/**
 * Schema version for migration support
 */
version: number, 
/**
 * Configuration for Light frames (→ Flat, Dark, Bias)
 */
lights: SourceTypeConfig, 
/**
 * Configuration for Flat frames (→ DarkFlat, Dark, Bias with fallback chain)
 */
flats: SourceTypeConfig, 
/**
 * Configuration for Dark frames (→ Bias)
 */
darks: SourceTypeConfig, 
/**
 * Behavioral options per source type
 */
behavioral_options: { [key in string]?: BehavioralOptions }, 
/**
 * Master preference per calibration type
 */
master_preferences: { [key in string]?: MasterPreference }, 
/**
 * Clustering configuration per calibration type
 */
clustering: { [key in string]?: ClusteringConfig }, 
/**
 * Scoring configuration
 */
scoring: ScoringConfig, 
/**
 * Warning thresholds
 */
warnings: WarningConfig, };

