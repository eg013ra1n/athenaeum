import { useState, useEffect, useCallback, useRef } from 'react';
import { api } from '../api';
import type {
  ExportMode,
  ExportSummary,
  WbppExportConfig,
} from '../types/export';
import type { ExportLightCalPrefs } from './useExportProgress';

interface UseExportSummaryResult {
  summary: ExportSummary | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch enhanced export summary for the new UI
 * Returns comprehensive data with equipment info, filter breakdowns, and detailed warnings
 *
 * The summary is drawn for `exportMode` — the backend shapes the data the way
 * that mode would export it before building the tree and totals, so a mode
 * change re-fetches. `lightCal` matters only to `calibratedLights` (the
 * debayer toggle decides the `c_*` names in the tree); pass it only then so
 * the other modes don't re-fetch on a toggle. `exportMode` omitted → the
 * backend's persisted config decides, as for the export itself.
 */
export function useExportSummary(
  frameSetId: number | null,
  exportMode?: ExportMode,
  lightCal?: ExportLightCalPrefs,
): UseExportSummaryResult {
  const [summary, setSummary] = useState<ExportSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A mode flip mid-flight must not let the older answer land last.
  const requestSeq = useRef(0);

  const loadData = useCallback(async () => {
    const seq = ++requestSeq.current;
    if (frameSetId === null) {
      setSummary(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<ExportSummary>('get_export_summary', {
        frameSetId,
        ...(exportMode ? { exportMode } : {}),
        ...(lightCal
          ? {
              flatNorm: lightCal.flatNorm,
              flatNormMode: lightCal.flatNormMode,
              params: lightCal.params,
              hotPixel: lightCal.hotPixel,
              debayer: lightCal.debayer,
            }
          : {}),
      });
      if (seq !== requestSeq.current) return;
      setSummary(result);
    } catch (err) {
      if (seq !== requestSeq.current) return;
      console.error('[useExportSummary] get_export_summary failed:', err);
      setError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
      setSummary(null);
    } finally {
      if (seq === requestSeq.current) setLoading(false);
    }
  }, [frameSetId, exportMode, lightCal]);

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
