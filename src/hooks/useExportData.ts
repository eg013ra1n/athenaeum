import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  ExportData,
  ExportResult,
  ExportableFrameSet,
  ExportMode,
  ExportTarget,
  SirilWorkflow,
  CalibrationRoute,
  ExportGroupsSummary,
  ReferenceFrameMode,
  RejectionAlgorithm,
  ImageWeightingMethod,
  DrizzleScale,
  ExptimeToleranceMode,
  ExportProgress,
  ExportStage,
} from '../types/export';

interface UseExportDataResult {
  data: ExportData | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch export preview data for a frame set
 */
export function useExportData(frameSetId: number | null): UseExportDataResult {
  const [data, setData] = useState<ExportData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    if (frameSetId === null) {
      setData(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportData>('get_export_preview', {
        frameSetId,
      });
      setData(result);
    } catch (err) {
      setError(err as string);
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [frameSetId]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  return { data, loading, error, refresh: loadData };
}

interface UseExportableFrameSetsResult {
  frameSets: ExportableFrameSet[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch list of frame sets available for export
 */
export function useExportableFrameSets(): UseExportableFrameSetsResult {
  const [frameSets, setFrameSets] = useState<ExportableFrameSet[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportableFrameSet[]>('get_exportable_frame_sets');
      setFrameSets(result);
    } catch (err) {
      setError(err as string);
      setFrameSets([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
  }, [loadData]);

  return { frameSets, loading, error, refresh: loadData };
}

interface UseExportResult {
  execute: (config: ExportExecuteConfig) => Promise<ExportResult>;
  loading: boolean;
  error: string | null;
  result: ExportResult | null;
}

interface ExportExecuteConfig {
  frameSetId: number;
  outputDir: string;
  mode: ExportMode;
  workflow: SirilWorkflow;
  rejectionLow?: number;
  rejectionHigh?: number;
  useSymlinks?: boolean;
}

/**
 * Hook to execute an export operation
 */
export function useExport(): UseExportResult {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);

  const execute = useCallback(async (config: ExportExecuteConfig): Promise<ExportResult> => {
    try {
      setLoading(true);
      setError(null);
      setResult(null);

      const exportResult = await invoke<ExportResult>('export_frame_set', {
        frameSetId: config.frameSetId,
        outputDir: config.outputDir,
        mode: config.mode,
        workflow: config.workflow,
        rejectionLow: config.rejectionLow ?? 3.0,
        rejectionHigh: config.rejectionHigh ?? 3.0,
        useSymlinks: config.useSymlinks ?? false,
      });

      setResult(exportResult);
      return exportResult;
    } catch (err) {
      const errorMessage = err as string;
      setError(errorMessage);
      throw new Error(errorMessage);
    } finally {
      setLoading(false);
    }
  }, []);

  return { execute, loading, error, result };
}

interface UseSirilPathResult {
  path: string | null;
  loading: boolean;
  error: string | null;
  setPath: (path: string) => Promise<void>;
  refresh: () => void;
}

/**
 * Hook to manage Siril CLI path setting
 */
export function useSirilPath(): UseSirilPathResult {
  const [path, setPathState] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadPath = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<string | null>('get_siril_path');
      setPathState(result);
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  }, []);

  const setPath = useCallback(async (newPath: string) => {
    try {
      setError(null);
      await invoke('set_siril_path', { path: newPath });
      setPathState(newPath);
    } catch (err) {
      setError(err as string);
      throw err;
    }
  }, []);

  useEffect(() => {
    loadPath();
  }, [loadPath]);

  return { path, loading, error, setPath, refresh: loadPath };
}

// ============================================================================
// V3 Export Hooks (Nested Folder Hierarchy)
// ============================================================================

interface UseCalibrationRouteResult {
  route: CalibrationRoute | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch the calibration route for UI display
 * Shows complete calibration hierarchy and script preview
 */
export function useCalibrationRoute(frameSetId: number | null): UseCalibrationRouteResult {
  const [route, setRoute] = useState<CalibrationRoute | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    if (frameSetId === null) {
      setRoute(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<CalibrationRoute>('get_calibration_route', {
        frameSetId,
      });
      setRoute(result);
    } catch (err) {
      setError(err as string);
      setRoute(null);
    } finally {
      setLoading(false);
    }
  }, [frameSetId]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  return { route, loading, error, refresh: loadData };
}

interface UseExportGroupsSummaryResult {
  summary: ExportGroupsSummary | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch lightweight export groups summary
 * Good for quick preview without full calibration details
 */
export function useExportGroupsSummary(frameSetId: number | null): UseExportGroupsSummaryResult {
  const [summary, setSummary] = useState<ExportGroupsSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    if (frameSetId === null) {
      setSummary(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportGroupsSummary>('get_export_groups_summary', {
        frameSetId,
      });
      setSummary(result);
    } catch (err) {
      setError(err as string);
      setSummary(null);
    } finally {
      setLoading(false);
    }
  }, [frameSetId]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  return { summary, loading, error, refresh: loadData };
}

interface UseExportV2Result {
  execute: (config: ExportV2Config) => Promise<ExportResult>;
  loading: boolean;
  error: string | null;
  result: ExportResult | null;
}

interface ExportV2Config {
  frameSetId: number;
  outputDir: string;
  mode: ExportMode;
  target?: ExportTarget;
  createMasters?: boolean;
  rejectionLow?: number;
  rejectionHigh?: number;
  useSymlinks?: boolean;
  // Advanced Siril options
  referenceFrameMode?: ReferenceFrameMode;
  rejectionAlgorithm?: RejectionAlgorithm;
  imageWeighting?: ImageWeightingMethod;
  drizzleEnabled?: boolean;
  drizzleScale?: DrizzleScale;
  // Exposure time grouping
  exptimeToleranceMode?: ExptimeToleranceMode;
  exptimeToleranceValue?: number;
}

/**
 * Hook to execute a V3 export operation
 * Uses nested folder hierarchy: camera → bias → darks → flats → fdarks → lights
 */
export function useExportV2(): UseExportV2Result {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);

  const execute = useCallback(async (config: ExportV2Config): Promise<ExportResult> => {
    try {
      setLoading(true);
      setError(null);
      setResult(null);

      const exportResult = await invoke<ExportResult>('export_frame_set_v3', {
        frameSetId: config.frameSetId,
        outputDir: config.outputDir,
        mode: config.mode,
        target: config.target ?? 'siril',
        createMasters: config.createMasters ?? true,
        rejectionLow: config.rejectionLow ?? 2.5,
        rejectionHigh: config.rejectionHigh ?? 2.5,
        useSymlinks: config.useSymlinks ?? false,
        // Advanced Siril options
        referenceFrameMode: config.referenceFrameMode ?? 'siril_auto',
        rejectionAlgorithm: config.rejectionAlgorithm ?? 'sigma',
        imageWeighting: config.imageWeighting ?? 'wfwhm',
        drizzleEnabled: config.drizzleEnabled ?? false,
        drizzleScale: config.drizzleScale ?? 'x2',
        // Exposure time grouping
        exptimeToleranceMode: config.exptimeToleranceMode ?? 'disabled',
        exptimeToleranceValue: config.exptimeToleranceValue ?? 30.0,
      });

      setResult(exportResult);
      return exportResult;
    } catch (err) {
      const errorMessage = err as string;
      setError(errorMessage);
      throw new Error(errorMessage);
    } finally {
      setLoading(false);
    }
  }, []);

  return { execute, loading, error, result };
}

// ============================================================================
// Pipeline Execution Hook
// ============================================================================

interface UseExportExecutionResult {
  /** Current progress state */
  progress: ExportProgress | null;
  /** Whether the pipeline is currently running */
  isRunning: boolean;
  /** Error message if pipeline failed */
  error: string | null;
  /** Start the export pipeline for a given export directory */
  runPipeline: (exportDir: string) => Promise<void>;
  /** Clear the progress state */
  reset: () => void;
}

/**
 * Hook to execute the Siril export pipeline and track progress
 *
 * This hook:
 * - Listens for `export-progress` events from the backend
 * - Provides a `runPipeline` function to start automatic execution
 * - Tracks progress through all stages (masters → calibrate → collect → register/stack)
 *
 * Usage:
 * ```typescript
 * const { progress, isRunning, runPipeline, error } = useExportExecution();
 *
 * // Start the pipeline
 * await runPipeline('/path/to/export');
 *
 * // Display progress
 * if (progress) {
 *   console.log(`${progress.stage}: ${progress.message} (${progress.progress}%)`);
 * }
 * ```
 */
export function useExportExecution(): UseExportExecutionResult {
  const [progress, setProgress] = useState<ExportProgress | null>(null);
  const [isRunning, setIsRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Listen for progress events
  useEffect(() => {
    let unlisten: UnlistenFn;

    const setupListener = async () => {
      unlisten = await listen<ExportProgress>('export-progress', (event) => {
        setProgress(event.payload);

        // Auto-detect completion or failure
        if (event.payload.stage === 'complete') {
          setIsRunning(false);
        } else if (event.payload.stage === 'failed') {
          setIsRunning(false);
          setError(event.payload.message);
        }
      });
    };

    setupListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const runPipeline = useCallback(async (exportDir: string) => {
    setIsRunning(true);
    setError(null);
    setProgress(null);

    try {
      await invoke<string>('run_siril_export_pipeline', { exportDir });
    } catch (err) {
      const errorMessage = err as string;
      setError(errorMessage);
      setIsRunning(false);
      throw new Error(errorMessage);
    }
  }, []);

  const reset = useCallback(() => {
    setProgress(null);
    setError(null);
    setIsRunning(false);
  }, []);

  return { progress, isRunning, error, runPipeline, reset };
}

/**
 * Get a human-readable label for an export stage
 */
export function getStageLabel(stage: ExportStage): string {
  switch (stage) {
    case 'collecting':
      return 'Collecting frames';
    case 'organizing':
      return 'Organizing files';
    case 'generating_scripts':
      return 'Generating scripts';
    case 'siril_creating_masters':
      return 'Creating calibration masters';
    case 'siril_calibrating':
      return 'Calibrating light frames';
    case 'collecting_calibrated_frames':
      return 'Collecting calibrated frames';
    case 'siril_registering':
      return 'Registering frames';
    case 'siril_stacking':
      return 'Stacking frames';
    case 'complete':
      return 'Complete';
    case 'failed':
      return 'Failed';
    default:
      return stage;
  }
}
