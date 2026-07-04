import { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type {
  ExportData,
  CalibrationRoute,
  ExportSummary,
  WbppExportConfig,
} from '../types/export';
import type { ExportableFrameSet } from '../types/helpers';

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
      const result = await api.invoke<ExportData>('get_export_preview', {
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
      const result = await api.invoke<ExportableFrameSet[]>('get_exportable_frame_sets');
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

interface UseCalibrationRouteResult {
  route: CalibrationRoute | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch the calibration route for UI display
 * Shows complete calibration hierarchy
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
      const result = await api.invoke<CalibrationRoute>('get_calibration_route', {
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

interface UseExportSummaryResult {
  summary: ExportSummary | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch enhanced export summary for the new UI
 * Returns comprehensive data with equipment info, filter breakdowns, and detailed warnings
 */
export function useExportSummary(frameSetId: number | null): UseExportSummaryResult {
  const [summary, setSummary] = useState<ExportSummary | null>(null);
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
      const result = await api.invoke<ExportSummary>('get_export_summary', {
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

interface UseWbppConfigResult {
  config: WbppExportConfig | null;
  loading: boolean;
  error: string | null;
  save: (config: WbppExportConfig) => Promise<void>;
  reset: () => Promise<void>;
  refresh: () => void;
}

/**
 * Hook to manage WBPP export configuration
 */
export function useWbppConfig(): UseWbppConfigResult {
  const [config, setConfig] = useState<WbppExportConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<WbppExportConfig>('get_wbpp_export_config');
      setConfig(result);
    } catch (err) {
      setError(err as string);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
  }, [loadData]);

  const save = useCallback(async (newConfig: WbppExportConfig) => {
    try {
      setError(null);
      const result = await api.invoke<WbppExportConfig>('set_wbpp_export_config', { config: newConfig });
      setConfig(result);
    } catch (err) {
      setError(err as string);
    }
  }, []);

  const reset = useCallback(async () => {
    try {
      setError(null);
      const result = await api.invoke<WbppExportConfig>('reset_wbpp_export_config');
      setConfig(result);
    } catch (err) {
      setError(err as string);
    }
  }, []);

  return { config, loading, error, save, reset, refresh: loadData };
}
