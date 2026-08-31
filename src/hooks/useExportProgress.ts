import { useState, useEffect, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { api, type UnlistenFn } from '../api';
import type { ExportProgressEvent, ExportCompleteEvent, ExportResult, ExportMode } from '../types/export';
import type { FlatNormMode, LightCalParams } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

/** Generation options forwarded to `export_to_wbpp`. In `calibratedLights` mode
 *  the backend calibrates every light from its linked masters as it places it,
 *  and these choose how; every other mode ignores them. Optional so
 *  pre-mode-selector callers keep working (the backend defaults each field). */
export interface ExportLightCalPrefs {
  flatNorm: boolean;
  flatNormMode: FlatNormMode;
  params: LightCalParams;
  /** Replace the master dark's hot pixels with a neighbourhood median. */
  hotPixel: boolean;
  /** Debayer CFA lights to full-resolution RGB. */
  debayer: boolean;
}

export interface ActiveExport {
  frameSetId: number;
  progress: ExportProgressEvent | null;
  isComplete: boolean;
  isCancelling: boolean;
  result: ExportResult | null;
}

export function useExportProgress() {
  const [activeExports, setActiveExports] = useState<Map<number, ActiveExport>>(new Map());
  const { notify } = useNotifications();

  // Listen for progress/complete events
  useEffect(() => {
    let cancelled = false;
    let progressUnlisten: UnlistenFn | undefined;
    let completeUnlisten: UnlistenFn | undefined;

    api
      .listen<ExportProgressEvent>('export-progress', (payload) => {
        if (cancelled) return;
        const { frameSetId } = payload;
        setActiveExports((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(frameSetId);
          if (existing) {
            updated.set(frameSetId, {
              ...existing,
              progress: payload,
            });
          }
          return updated;
        });
      })
      .then((fn) => { if (cancelled) fn(); else progressUnlisten = fn; })
      .catch((err) => console.error('[useExportProgress] listen failed:', err));

    api
      .listen<ExportCompleteEvent>('export-complete', (payload) => {
        if (cancelled) return;
        const { frameSetId } = payload;
        setActiveExports((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(frameSetId);
          if (existing) {
            updated.set(frameSetId, {
              ...existing,
              isComplete: true,
              isCancelling: false,
              result: {
                success: payload.success,
                outputDir: payload.outputDir,
                filesOrganized: payload.filesOrganized,
                scriptsGenerated: [],
                warnings: payload.warnings,
                error: payload.error,
              },
              progress: null,
            });
          }
          return updated;
        });

        const exportWarnings = payload.warnings?.length ?? 0;
        const exportFailed = !payload.success || !!payload.error;
        notify({
          title: exportFailed
            ? 'Export failed'
            : `Export complete — ${payload.filesOrganized} files`,
          detail: payload.error
            ? String(payload.error)
            : `${payload.outputDir}${exportWarnings ? ` · ${exportWarnings} warning${exportWarnings === 1 ? '' : 's'}` : ''}`,
          kind: 'export',
          hasErrors: exportFailed,
          tone: exportFailed || exportWarnings > 0 ? 'warning' : 'success',
        });
      })
      .then((fn) => { if (cancelled) fn(); else completeUnlisten = fn; })
      .catch((err) => console.error('[useExportProgress] listen failed:', err));

    return () => {
      cancelled = true;
      progressUnlisten?.();
      completeUnlisten?.();
    };
  }, []);

  const startExport = useCallback(
    async (
      frameSetId: number,
      outputDir: string,
      useSymlinks: boolean,
      lightCalPrefs?: ExportLightCalPrefs,
      // Explicit export mode. Correctness-bearing: the backend uses this over
      // the persisted WbppExportConfig's mode when provided (the config sync is
      // now only best-effort persistence). Omitted → backend config fallback.
      exportMode?: ExportMode,
    ): Promise<ExportResult> => {
      // Register export in state immediately (shows toast before api.invoke)
      flushSync(() => {
        setActiveExports((prev) => {
          const updated = new Map(prev);
          updated.delete(frameSetId);
          updated.set(frameSetId, {
            frameSetId,
            progress: null,
            isComplete: false,
            isCancelling: false,
            result: null,
          });
          return updated;
        });
      });

      // Small delay to ensure the progress toast is painted
      await new Promise(resolve => setTimeout(resolve, 50));

      try {
        const result = await api.invoke<ExportResult>('export_to_wbpp', {
          frameSetId,
          outputDir,
          useSymlinks,
          // Explicit export mode overrides the persisted config on the backend.
          // Omitted → backend falls back to the persisted WbppExportConfig.
          ...(exportMode ? { exportMode } : {}),
          // Generation options for `calibratedLights`; ignored by the backend in
          // the other three modes. Omitted keys → backend defaults.
          ...(lightCalPrefs
            ? {
                flatNorm: lightCalPrefs.flatNorm,
                flatNormMode: lightCalPrefs.flatNormMode,
                params: lightCalPrefs.params,
                hotPixel: lightCalPrefs.hotPixel,
                debayer: lightCalPrefs.debayer,
              }
            : {}),
        });

        // Update state with final result
        setActiveExports((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(frameSetId);
          if (existing) {
            updated.set(frameSetId, {
              ...existing,
              isComplete: true,
              isCancelling: false,
              result,
              progress: null,
            });
          }
          return updated;
        });

        return result;
      } catch (error) {
        // Remove from active exports on error
        setActiveExports((prev) => {
          const updated = new Map(prev);
          updated.delete(frameSetId);
          return updated;
        });
        throw error;
      }
    },
    []
  );

  const cancelExport = useCallback(async (frameSetId: number) => {
    // Mark as cancelling in local state (for UI feedback)
    setActiveExports((prev) => {
      const updated = new Map(prev);
      const existing = updated.get(frameSetId);
      if (existing) {
        updated.set(frameSetId, {
          ...existing,
          isCancelling: true,
        });
      }
      return updated;
    });
    // Send cancel request to backend
    await api.invoke('cancel_export', { frameSetId });
  }, []);

  const dismissCompletedExport = useCallback((frameSetId: number) => {
    setActiveExports((prev) => {
      const updated = new Map(prev);
      updated.delete(frameSetId);
      return updated;
    });
  }, []);

  const isExporting = useCallback(
    (frameSetId: number) => {
      const exp = activeExports.get(frameSetId);
      return exp ? !exp.isComplete : false;
    },
    [activeExports]
  );

  return {
    activeExports,
    startExport,
    cancelExport,
    dismissCompletedExport,
    isExporting,
    hasActiveExports: Array.from(activeExports.values()).some((e) => !e.isComplete),
  };
}
