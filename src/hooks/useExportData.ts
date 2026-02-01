import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type {
  ExportData,
  ExportableFrameSet,
  CalibrationRoute,
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
