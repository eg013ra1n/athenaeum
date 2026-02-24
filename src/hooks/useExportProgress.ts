import { useState, useEffect, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { api, type UnlistenFn } from '../api';
import type { ExportProgressEvent, ExportCompleteEvent, ExportResult } from '../types/export';

export interface ActiveExport {
  frameSetId: number;
  progress: ExportProgressEvent | null;
  isComplete: boolean;
  isCancelling: boolean;
  result: ExportResult | null;
}

export function useExportProgress() {
  const [activeExports, setActiveExports] = useState<Map<number, ActiveExport>>(new Map());

  // Listen for progress/complete events
  useEffect(() => {
    let progressUnlisten: UnlistenFn;
    let completeUnlisten: UnlistenFn;

    const setupListeners = async () => {
      progressUnlisten = await api.listen<ExportProgressEvent>('export-progress', (payload) => {
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
      });

      completeUnlisten = await api.listen<ExportCompleteEvent>('export-complete', (payload) => {
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
      });
    };

    setupListeners();

    return () => {
      if (progressUnlisten) progressUnlisten();
      if (completeUnlisten) completeUnlisten();
    };
  }, []);

  const startExport = useCallback(
    async (frameSetId: number, outputDir: string, useSymlinks: boolean): Promise<ExportResult> => {
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
