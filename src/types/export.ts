// TypeScript interfaces for export module

/**
 * Camera type based on Bayer pattern presence
 */
export type CameraType = 'osc' | 'mono';

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
  focallen: number | null;
  bayerpat: string | null;
  instrume: string | null;
}

/**
 * Information about a calibration set and its sub-calibrations (recursive)
 */
export interface CalibrationSetInfo {
  setId: number;
  imagetyp: string;
  frames: ExportFrame[];
  frameCount: number;
  darkFlat: CalibrationSetInfo | null;
  dark: CalibrationSetInfo | null;
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
 * Plan for creating all required master calibration files
 */
export interface MasterCreationPlan {
  masters: MasterInfo[];
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

/**
 * Complete export data for a frame set
 */
export interface ExportData {
  frameSetId: number;
  frameSetName: string;
  objectName: string | null;
  groups: ExportGroup[];
  masterPlan: MasterCreationPlan;
  filters: FilterExportGroup[];
  calibrationSummary: CalibrationSummary;
  totalLightFrames: number;
  totalExposureSeconds: number;
}

/**
 * Legacy filter group structure
 */
export interface FilterExportGroup {
  filter: string | null;
  lightFrames: ExportFrame[];
  flatSets: ExportCalibrationSet[];
  darkSets: ExportCalibrationSet[];
  biasSets: ExportCalibrationSet[];
}

/**
 * Legacy calibration set structure
 */
export interface ExportCalibrationSet {
  setId: number;
  imagetyp: string;
  frames: ExportFrame[];
  subCalibrations: ExportCalibrationSet[];
  matchScore: number | null;
  warnings: string[];
}

// ============================================================================
// Calibration Route (UI Display)
// ============================================================================

/**
 * Calibration route for UI display
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
 * Preview of a Siril script (kept for type compatibility)
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

// ============================================================================
// Export Summary (Enhanced UI)
// ============================================================================

/**
 * Complete export summary for the enhanced UI
 */
export interface ExportSummary {
  frameSetId: number;
  frameSetName: string;
  objectName: string | null;
  /** Unique cameras used */
  cameras: string[];
  /** Unique telescopes used */
  telescopes: string[];
  /** Date range of sessions [start, end] */
  dateRange: [string, string] | null;
  /** Filter groups with full breakdown */
  filterGroups: FilterGroupSummary[];
  /** Folder structure preview */
  folderPreview: FolderPreview;
  /** Detailed warnings */
  warnings: DetailedWarning[];
  /** Total file count */
  totalFiles: number;
  /** Estimated total size in bytes */
  estimatedSizeBytes: number;
}

/**
 * Summary for a single filter group (filter + camera type combination)
 */
export interface FilterGroupSummary {
  /** Filter name (null for unfiltered/luminance) */
  filter: string | null;
  /** Camera type (OSC or Mono) */
  cameraType: CameraType;
  /** Camera/instrument name */
  camera: string | null;
  /** Telescope name */
  telescope: string | null;
  /** Gain setting */
  gain: number | null;
  /** Offset setting */
  offset: number | null;
  /** Binning mode (e.g., "1x1") */
  binning: string | null;
  /** Average CCD temperature */
  avgTemp: number | null;
  /** Exposure breakdown by exposure time */
  exposureGroups: ExposureGroup[];
  /** Total exposure time in seconds */
  totalExposure: number;
  /** Total frame count */
  frameCount: number;
  /** Linked flat calibration info */
  flatInfo: CalibrationDetail | null;
  /** Linked dark calibration info */
  darkInfo: CalibrationDetail | null;
  /** Linked bias calibration info */
  biasInfo: CalibrationDetail | null;
  /** All frames in this group (for expandable list) */
  frames: FrameDetail[];
}

/**
 * Exposure time group (frames with same exposure)
 */
export interface ExposureGroup {
  /** Exposure time in seconds */
  exptime: number;
  /** Number of frames with this exposure */
  count: number;
  /** Total exposure time for this group (exptime * count) */
  totalSeconds: number;
}

/**
 * Detailed calibration information
 */
export interface CalibrationDetail {
  /** Calibration set ID */
  setId: number;
  /** Calibration type (Flat, Dark, Bias, DarkFlat) */
  calibrationType: string;
  /** Number of calibration frames */
  frameCount: number;
  /** Average exposure time (for darks/flats) */
  avgExptime: number | null;
  /** Average CCD temperature */
  avgTemp: number | null;
  /** Match quality score (0.0 - 1.0) */
  matchScore: number;
  /** Date range of calibration frames [start, end] */
  dateRange: [string, string] | null;
  /** Specific warnings for this calibration */
  warnings: string[];
  /** Sub-calibrations (e.g., Flat -> Dark -> Bias chain) */
  subCalibrations: CalibrationDetail[];
}

/**
 * Individual frame detail for expandable list
 */
export interface FrameDetail {
  /** Frame ID */
  frameId: number;
  /** Filename */
  filename: string;
  /** Full file path */
  filePath: string;
  /** Date observed */
  dateObs: string | null;
  /** Exposure time in seconds */
  exptime: number | null;
  /** CCD temperature */
  temp: number | null;
  /** Gain setting */
  gain: number | null;
  /** Offset setting */
  offset: number | null;
  /** Calibration chain description (e.g., "Flat #12 → Dark #8 → Bias #3") */
  calibrationChain: string;
  /** File size in bytes (if known) */
  fileSize: number | null;
}

/**
 * Folder structure preview for export
 */
export interface FolderPreview {
  /** Root folder name */
  rootName: string;
  /** Folder structure tree */
  structure: FolderNode[];
  /** Total file count */
  totalFiles: number;
  /** Estimated total size (human readable) */
  estimatedSize: string;
}

/**
 * A node in the folder structure tree
 */
export interface FolderNode {
  /** Node name (folder or file name) */
  name: string;
  /** Node type */
  nodeType: FolderNodeType;
  /** File count (for folders) */
  fileCount: number | null;
  /** Description (e.g., "← 50 darks, 100 bias") */
  description: string | null;
  /** Child nodes */
  children: FolderNode[];
}

/**
 * Type of folder node
 */
export type FolderNodeType = 'folder' | 'file' | 'ellipsis';

/**
 * Detailed warning with full context
 */
export interface DetailedWarning {
  /** Warning type */
  warningType: WarningType;
  /** Warning severity */
  severity: WarningSeverity;
  /** Short title */
  title: string;
  /** Detailed description */
  description: string;
  /** Related calibration set ID (if applicable) */
  setId: number | null;
  /** Related filter (if applicable) */
  filter: string | null;
  /** Actual value (for mismatches) */
  actualValue: string | null;
  /** Expected value (for mismatches) */
  expectedValue: string | null;
  /** Delta/difference (for numeric comparisons) */
  delta: string | null;
  /** Recommendation text */
  recommendation: string | null;
}

/**
 * Warning type categories
 */
export type WarningType =
  | 'temperature_mismatch'
  | 'calibration_age'
  | 'missing_calibration'
  | 'gain_offset_mismatch'
  | 'binning_mismatch'
  | 'exposure_mismatch'
  | 'general';

/**
 * Warning severity levels
 */
export type WarningSeverity = 'info' | 'warning' | 'error';
