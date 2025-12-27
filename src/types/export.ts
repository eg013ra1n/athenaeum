// TypeScript interfaces for export module

/**
 * Export operation mode
 */
export type ExportMode =
  | 'generate_scripts'
  | 'organize_files'
  | 'organize_and_script'
  | 'direct_execution';

/**
 * Siril processing workflow type
 */
export type SirilWorkflow =
  | 'mono_preprocessing'
  | 'osc_preprocessing'
  | 'lrgb_processing';

/**
 * Configuration for an export operation
 */
export interface ExportConfig {
  frameSetId: number;
  outputDir: string;
  mode: ExportMode;
  workflow: SirilWorkflow;
  createMasters: boolean;
  rejectionLow: number;
  rejectionHigh: number;
  useSymlinks: boolean;
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
  filters: FilterExportGroup[];
  calibrationSummary: CalibrationSummary;
  totalLightFrames: number;
  totalExposureSeconds: number;
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
  | 'siril_calibrating'
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
