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
