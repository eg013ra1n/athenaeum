// TypeScript interfaces for stacking module

/**
 * Status of a stacking job
 */
export type StackingJobStatus =
  | 'pending'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'cancelled';

/**
 * Status of a stacking step
 */
export type StackingStepStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped';

/**
 * Type of stacking step
 */
export type StackingStepType =
  | 'create_master_bias'
  | 'create_master_dark'
  | 'create_master_flat'
  | 'calibrate_lights'
  | 'measure_quality'
  | 'platesolve_frames'
  | 'register_frames'
  | 'stack_frames';

// ============================================================================
// Configuration Types
// ============================================================================

/**
 * Stacking method
 */
export type StackingMethod = 'mean' | 'median' | 'sum';

/**
 * Pixel rejection algorithm
 */
export type RejectionAlgorithm =
  | 'none'
  | 'sigma'
  | 'mad'
  | 'winsorized'
  | 'percentile'
  | 'linear_fit'
  | 'gesdt';

/**
 * Normalization method
 */
export type NormalizationMethod =
  | 'none'
  | 'additive'
  | 'multiplicative'
  | 'additive_scaling';

/**
 * Image weighting method
 */
export type WeightingMethod = 'none' | 'fwhm' | 'star_count' | 'noise';

/**
 * Dark optimization mode
 */
export type DarkOptimizationMode =
  | 'off'
  | 'exposure_based'
  | 'noise_minimization';

/**
 * Flat normalization mode
 */
export type FlatNormalizationMode = 'auto' | 'manual';

/**
 * Debayer method
 */
export type DebayerMethod = 'bilinear' | 'vng' | 'auto';

/**
 * Framing option for registration
 */
export type FramingOption = 'current' | 'max' | 'min' | 'center_of_gravity';

/**
 * Interpolation option
 */
export type InterpolationOption = 'nearest' | 'bilinear' | 'bicubic' | 'lanczos4';

// ============================================================================
// Configuration Structures
// ============================================================================

/**
 * Master creation configuration
 */
export interface MasterConfig {
  method: StackingMethod;
  rejection?: RejectionConfig;
  output32bit: boolean;
}

/**
 * Rejection configuration
 */
export interface RejectionConfig {
  algorithm: RejectionAlgorithm;
  sigmaLow: number;
  sigmaHigh: number;
}

/**
 * Calibration settings
 */
export interface CalibrationSettings {
  useBias: boolean;
  useDark: boolean;
  useFlat: boolean;
  darkOptimization: DarkOptimizationMode;
  flatNormalization: FlatNormalizationMode;
  debayerMethod: DebayerMethod;
  output32bit: boolean;
}

/**
 * Registration settings
 */
export interface RegistrationSettings {
  usePlateSolving: boolean;
  focalLengthMm: number;
  pixelSizeUm: number;
  searchRadiusDeg: number;
  framing: FramingOption;
  interpolation: InterpolationOption;
}

/**
 * Stacking settings
 */
export interface StackingSettings {
  method: StackingMethod;
  rejection: RejectionAlgorithm;
  sigmaLow: number;
  sigmaHigh: number;
  normalization: NormalizationMethod;
  weighting: WeightingMethod;
  output32bit: boolean;
}

/**
 * Complete stacking configuration
 */
export interface StackingConfig {
  output32bit: boolean;
  master: MasterConfig;
  calibration: CalibrationSettings;
  registration: RegistrationSettings;
  stacking: StackingSettings;
}

// ============================================================================
// Job Types
// ============================================================================

/**
 * Step statistics
 */
export interface StepStats {
  framesProcessed: number;
  framesRejected: number;
  totalExposureTime: number;
  averageFwhm?: number;
  processingTimeMs: number;
}

/**
 * A single step in a stacking job
 */
export interface StackingStep {
  id: number;
  jobId: number;
  stepOrder: number;
  stepType: StackingStepType;
  stepName: string;
  status: StackingStepStatus;
  startedAt: string | null;
  completedAt: string | null;
  errorMessage: string | null;
  outputFiles: string[];
  stats: StepStats | null;
}

/**
 * A stacking job with all its steps
 */
export interface StackingJob {
  id: number;
  frameSetId: number;
  outputDir: string;
  config: StackingConfig;
  status: StackingJobStatus;
  createdAt: string;
  startedAt: string | null;
  completedAt: string | null;
  errorMessage: string | null;
  steps: StackingStep[];
}

/**
 * Progress event for stacking operations
 */
export interface StackingProgress {
  jobId: number;
  stepOrder: number;
  totalSteps: number;
  stepType: StackingStepType;
  stepProgress: number;
  overallProgress: number;
  message: string;
  currentFile?: string;
  framesProcessed?: number;
  framesTotal?: number;
}

// ============================================================================
// Default Configuration
// ============================================================================

/**
 * Get the default stacking configuration
 */
export function getDefaultStackingConfig(): StackingConfig {
  return {
    output32bit: true,
    master: {
      method: 'median',
      output32bit: true,
    },
    calibration: {
      useBias: true,
      useDark: true,
      useFlat: true,
      darkOptimization: 'off',
      flatNormalization: 'auto',
      debayerMethod: 'vng',
      output32bit: true,
    },
    registration: {
      usePlateSolving: true,
      focalLengthMm: 500,
      pixelSizeUm: 3.76,
      searchRadiusDeg: 10,
      framing: 'max',
      interpolation: 'lanczos4',
    },
    stacking: {
      method: 'mean',
      rejection: 'sigma',
      sigmaLow: 2.5,
      sigmaHigh: 2.5,
      normalization: 'additive_scaling',
      weighting: 'fwhm',
      output32bit: true,
    },
  };
}

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Get human-readable label for stacking job status
 */
export function getStackingJobStatusLabel(status: StackingJobStatus): string {
  switch (status) {
    case 'pending':
      return 'Pending';
    case 'running':
      return 'Running';
    case 'paused':
      return 'Paused';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'cancelled':
      return 'Cancelled';
    default:
      return status;
  }
}

/**
 * Get human-readable label for stacking step status
 */
export function getStackingStepStatusLabel(status: StackingStepStatus): string {
  switch (status) {
    case 'pending':
      return 'Pending';
    case 'running':
      return 'Running';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'skipped':
      return 'Skipped';
    default:
      return status;
  }
}

/**
 * Get human-readable label for stacking step type
 */
export function getStackingStepTypeLabel(stepType: StackingStepType): string {
  switch (stepType) {
    case 'create_master_bias':
      return 'Create Master Bias';
    case 'create_master_dark':
      return 'Create Master Dark';
    case 'create_master_flat':
      return 'Create Master Flat';
    case 'calibrate_lights':
      return 'Calibrate Light Frames';
    case 'measure_quality':
      return 'Measure Frame Quality';
    case 'platesolve_frames':
      return 'Plate Solve Frames';
    case 'register_frames':
      return 'Register Frames';
    case 'stack_frames':
      return 'Stack Frames';
    default:
      return stepType;
  }
}

/**
 * Get icon name for stacking step type (for lucide-react)
 */
export function getStackingStepTypeIcon(stepType: StackingStepType): string {
  switch (stepType) {
    case 'create_master_bias':
    case 'create_master_dark':
    case 'create_master_flat':
      return 'Layers';
    case 'calibrate_lights':
      return 'Wand2';
    case 'measure_quality':
      return 'Gauge';
    case 'platesolve_frames':
      return 'Compass';
    case 'register_frames':
      return 'Target';
    case 'stack_frames':
      return 'LayersIcon';
    default:
      return 'Circle';
  }
}

/**
 * Get status color class for tailwind
 */
export function getStackingStatusColor(status: StackingJobStatus | StackingStepStatus): string {
  switch (status) {
    case 'pending':
      return 'text-gray-400';
    case 'running':
      return 'text-blue-400';
    case 'completed':
      return 'text-green-400';
    case 'failed':
      return 'text-red-400';
    case 'paused':
      return 'text-yellow-400';
    case 'cancelled':
      return 'text-gray-500';
    case 'skipped':
      return 'text-gray-400';
    default:
      return 'text-gray-400';
  }
}

/**
 * Get status background color class for tailwind
 */
export function getStackingStatusBgColor(status: StackingJobStatus | StackingStepStatus): string {
  switch (status) {
    case 'pending':
      return 'bg-gray-700';
    case 'running':
      return 'bg-blue-900/50';
    case 'completed':
      return 'bg-green-900/50';
    case 'failed':
      return 'bg-red-900/50';
    case 'paused':
      return 'bg-yellow-900/50';
    case 'cancelled':
      return 'bg-gray-800';
    case 'skipped':
      return 'bg-gray-700';
    default:
      return 'bg-gray-700';
  }
}
