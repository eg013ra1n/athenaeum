// TypeScript interfaces for export module

/**
 * Export operation mode
 */
export type ExportMode =
  | 'direct_execution'
  | 'organize_and_script';

/**
 * Siril processing workflow type
 */
export type SirilWorkflow =
  | 'mono_preprocessing'
  | 'osc_preprocessing'
  | 'lrgb_processing';

/**
 * Export target application
 */
export type ExportTarget = 'siril' | 'pixinsight_wbpp';

/**
 * Camera type based on Bayer pattern presence
 */
export type CameraType = 'osc' | 'mono';

/**
 * Reference frame selection mode for registration
 */
export type ReferenceFrameMode =
  | 'siril_auto' // Use Siril's -2pass auto-selection (recommended)
  | 'athenaeum_scoring' // Pre-select using Athenaeum quality metrics
  | 'manual'; // User manually specifies reference frame

/**
 * Pixel rejection algorithm for stacking
 */
export type RejectionAlgorithm =
  | 'percentile' // Good for small datasets (<20 frames)
  | 'sigma' // General purpose (default)
  | 'linear_fit' // Good for large sets with gradients
  | 'gesd' // Best for 50+ images
  | 'mad'; // Good for drizzled CFA data

/**
 * Image weighting method for stacking
 */
export type ImageWeightingMethod =
  | 'none' // No weighting
  | 'stars' // Weight by number of stars detected
  | 'wfwhm' // Weight by weighted FWHM (recommended)
  | 'noise' // Weight by noise level
  | 'exposure_time'; // Weight by integration time

/**
 * Drizzle scale factor for super-resolution
 */
export type DrizzleScale =
  | 'none' // No drizzle (1x, disabled)
  | 'x2' // 2x super-resolution
  | 'x3'; // 3x super-resolution

/**
 * Exposure time tolerance mode for stacking grouping
 *
 * Controls how frames with different exposure times are grouped for stacking.
 */
export type ExptimeToleranceMode =
  | 'disabled' // Stack all frames with same filter together (ignores exposure time)
  | 'absolute' // Group frames if within X seconds of each other
  | 'relative'; // Group frames if within X percent of each other

/**
 * Configuration for an export operation
 */
export interface ExportConfig {
  frameSetId: number;
  outputDir: string;
  target: ExportTarget;
  mode: ExportMode;
  workflow: SirilWorkflow;
  createMasters: boolean;
  rejectionLow: number;
  rejectionHigh: number;
  useSymlinks: boolean;

  // === Advanced Siril Options ===

  /** Reference frame selection mode for registration */
  referenceFrameMode: ReferenceFrameMode;
  /** Manual reference frame ID (only used when referenceFrameMode is 'manual') */
  manualReferenceFrameId?: number;
  /** Pixel rejection algorithm for stacking */
  rejectionAlgorithm: RejectionAlgorithm;
  /** Image weighting method for stacking */
  imageWeighting: ImageWeightingMethod;
  /** Enable drizzle for super-resolution */
  drizzleEnabled: boolean;
  /** Drizzle scale factor (only used when drizzleEnabled is true) */
  drizzleScale: DrizzleScale;

  // === Exposure Time Grouping ===

  /** Exposure time tolerance mode for stacking grouping */
  exptimeToleranceMode: ExptimeToleranceMode;
  /** Exposure time tolerance value (seconds for absolute, percentage for relative) */
  exptimeToleranceValue: number;
}

/**
 * A single frame for export
 */
export interface ExportFrame {
  frameId: number;
  fileId: number;
  filePath: string;
  filename: string;
  exptime: number | null;
  filter: string | null;
  ccdTemp: number | null;
  gain: number | null;
  offset: number | null;
  binning: string | null;
  dateObs: string | null;
  /** Focal length in mm */
  focallen: number | null;
  /** Bayer pattern for OSC detection (e.g., "RGGB") */
  bayerpat: string | null;
  /** Camera/instrument name */
  instrume: string | null;
}

/**
 * A calibration set with its frames
 */
export interface ExportCalibrationSet {
  setId: number;
  imagetyp: string;
  frames: ExportFrame[];
  subCalibrations: ExportCalibrationSet[];
  matchScore: number | null;
  warnings: string[];
}

/**
 * Group of light frames by filter with their calibrations
 */
export interface FilterExportGroup {
  filter: string | null;
  lightFrames: ExportFrame[];
  flatSets: ExportCalibrationSet[];
  darkSets: ExportCalibrationSet[];
  biasSets: ExportCalibrationSet[];
}

/**
 * Summary of calibration availability
 */
export interface CalibrationSummary {
  flatCount: number;
  darkCount: number;
  biasCount: number;
  darkFlatCount: number;
  flatsComplete: boolean;
  darksComplete: boolean;
  biasComplete: boolean;
  warnings: string[];
}

/**
 * Complete export data for a frame set
 */
export interface ExportData {
  frameSetId: number;
  frameSetName: string;
  objectName: string | null;
  /** New export groups structure (grouped by filter + camera type) */
  groups: ExportGroup[];
  /** Master creation plan with dependency ordering */
  masterPlan: MasterCreationPlan;
  /** Legacy filter groups (kept for compatibility) */
  filters: FilterExportGroup[];
  calibrationSummary: CalibrationSummary;
  totalLightFrames: number;
  totalExposureSeconds: number;
}

// ============================================================================
// New Export Models (Phase 2 Refactoring)
// ============================================================================

/**
 * Information about a calibration set and its sub-calibrations (recursive)
 */
export interface CalibrationSetInfo {
  setId: number;
  imagetyp: string;
  frames: ExportFrame[];
  frameCount: number;
  /** Sub-calibration: DarkFlat set (for Flats) */
  darkFlat: CalibrationSetInfo | null;
  /** Sub-calibration: Dark set (for Flats or Lights) */
  dark: CalibrationSetInfo | null;
  /** Sub-calibration: Bias set (for Flats, Darks, or Lights) */
  bias: CalibrationSetInfo | null;
  matchScore: number | null;
  warnings: string[];
}

/**
 * A subgroup of frames that share the same calibration set links
 */
export interface CalibrationSubgroup {
  subgroupKey: string;
  displayName: string;
  frames: ExportFrame[];
  flat: CalibrationSetInfo | null;
  dark: CalibrationSetInfo | null;
  bias: CalibrationSetInfo | null;
  warnings: string[];
}

/**
 * An export group - frames that will be stacked into one master light
 * Groups frames by filter AND camera type (OSC vs Mono)
 */
export interface ExportGroup {
  groupKey: string;
  filter: string | null;
  cameraType: CameraType;
  displayName: string;
  subgroups: CalibrationSubgroup[];
  totalFrames: number;
  totalExposure: number;
  warnings: string[];
}

// ============================================================================
// Master Creation Plan
// ============================================================================

/**
 * Plan for creating all required master calibration files
 */
export interface MasterCreationPlan {
  /** Ordered list of masters to create (respects dependencies) */
  masters: MasterInfo[];
  /** Map of set_id → master file path for reference */
  masterPaths: Record<number, string>;
}

/**
 * Information about a master calibration file to create
 */
export interface MasterInfo {
  setId: number;
  masterType: string;
  outputName: string;
  sourceFrames: ExportFrame[];
  dependsOn: number[];
  applyBias: number | null;
  applyDark: number | null;
}

// ============================================================================
// Calibration Route (UI Display)
// ============================================================================

/**
 * Calibration route for UI display - shows complete hierarchy and script preview
 */
export interface CalibrationRoute {
  groups: CalibrationRouteGroup[];
  scriptPreview: SirilScriptPreview[];
  summary: CalibrationRouteSummary;
}

/**
 * A group in the calibration route display
 */
export interface CalibrationRouteGroup {
  name: string;
  lightCount: number;
  totalExposure: number;
  subgroupCount: number;
  calibrationTree: CalibrationTreeNode[];
}

/**
 * A node in the calibration tree for UI display
 */
export interface CalibrationTreeNode {
  nodeType: 'Light' | 'Flat' | 'Dark' | 'Bias' | 'DarkFlat';
  label: string;
  setId: number | null;
  count: number;
  children: CalibrationTreeNode[];
  warnings: string[];
  isMissing: boolean;
  isShared: boolean;
}

/**
 * Preview of a Siril script
 */
export interface SirilScriptPreview {
  name: string;
  description: string;
  content: string;
}

/**
 * Summary of the calibration route
 */
export interface CalibrationRouteSummary {
  groupCount: number;
  totalLights: number;
  totalExposure: number;
  uniqueCalibrationSets: number;
  mastersToCreate: number;
  flatsComplete: boolean;
  darksComplete: boolean;
  biasComplete: boolean;
  warnings: string[];
}

// ============================================================================
// Export Groups Summary (Lightweight)
// ============================================================================

/**
 * Summary of export groups for quick preview
 */
export interface ExportGroupsSummary {
  frameSetId: number;
  totalGroups: number;
  totalFrames: number;
  totalExposure: number;
  mastersToCreate: number;
  groups: ExportGroupInfo[];
}

/**
 * Info about a single export group
 */
export interface ExportGroupInfo {
  groupKey: string;
  filter: string | null;
  cameraType: string;
  displayName: string;
  frameCount: number;
  totalExposure: number;
  subgroupCount: number;
  hasFlat: boolean;
  hasDark: boolean;
  hasBias: boolean;
  warnings: string[];
}

/**
 * Result of an export operation
 */
export interface ExportResult {
  success: boolean;
  outputDir: string;
  filesOrganized: number;
  scriptsGenerated: string[];
  warnings: string[];
  error: string | null;
}

/**
 * Progress update during export or Siril execution
 */
export interface ExportProgress {
  stage: ExportStage;
  progress: number;
  message: string;
  currentFile: string | null;
}

/**
 * Export operation stages
 */
export type ExportStage =
  | 'collecting'
  | 'organizing'
  | 'generating_scripts'
  | 'siril_creating_masters'
  | 'siril_calibrating'
  | 'collecting_calibrated_frames'
  | 'siril_registering'
  | 'siril_stacking'
  | 'complete'
  | 'failed';

/**
 * Frame set summary for export selection
 */
export interface ExportableFrameSet {
  id: number;
  name: string | null;
  totalExposureSeconds: number;
  frameCount: number;
  objectName: string | null;
  filters: string[];
}
