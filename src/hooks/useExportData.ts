import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type {
  ExportData,
  ExportResult,
  ExportableFrameSet,
  ExportMode,
  ExportTarget,
  SirilWorkflow,
  CalibrationRoute,
  ExportGroupsSummary,
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
