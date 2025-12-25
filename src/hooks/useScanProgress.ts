import { useState, useEffect, useCallback } from 'react';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import type { ScanProgressEvent, ScanCompleteEvent, ScanResult } from '../types/models';

export interface ActiveScan {
  rootId: number;
  rootPath: string;
  progress: ScanProgressEvent | null;
  isComplete: boolean;
  result: ScanResult | null;
}

export function useScanProgress() {
  const [activeScans, setActiveScans] = useState<Map<number, ActiveScan>>(new Map());

  // Listen for progress events
  useEffect(() => {
    let progressUnlisten: UnlistenFn;
    let completeUnlisten: UnlistenFn;

    const setupListeners = async () => {
      progressUnlisten = await listen<ScanProgressEvent>('scan-progress', (event) => {
        const { root_id } = event.payload;
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(root_id);
          if (existing) {
            updated.set(root_id, {
              ...existing,
              progress: event.payload,
            });
          }
          return updated;
        });
      });

      completeUnlisten = await listen<ScanCompleteEvent>('scan-complete', (event) => {
        const { root_id } = event.payload;
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(root_id);
          if (existing) {
            updated.set(root_id, {
              ...existing,
              isComplete: true,
              result: {
                files_found: event.payload.files_found,
                files_processed: event.payload.files_processed,
                files_skipped: event.payload.files_skipped,
                errors: event.payload.errors,
                lights_count: event.payload.lights_count,
                darks_count: event.payload.darks_count,
                flats_count: event.payload.flats_count,
                bias_count: event.payload.bias_count,
                darkflats_count: event.payload.darkflats_count,
                calibration_sets_created: event.payload.calibration_sets_created,
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

  const startScanWithProgress = useCallback(
    async (rootId: number, rootPath: string): Promise<ScanResult> => {
      // Register scan in local state
      setActiveScans((prev) => {
        const updated = new Map(prev);
        updated.set(rootId, {
          rootId,
          rootPath,
          progress: null,
          isComplete: false,
          result: null,
        });
        return updated;
      });

      try {
        // Start scan with progress - this will emit events and return result
        const result = await invoke<ScanResult>('start_scan_with_progress', { rootId });

        // Update state with final result
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(rootId);
          if (existing) {
            updated.set(rootId, {
              ...existing,
              isComplete: true,
              result,
              progress: null,
            });
          }
          return updated;
        });

        return result;
      } catch (error) {
        // Remove from active scans on error
        setActiveScans((prev) => {
          const updated = new Map(prev);
          updated.delete(rootId);
          return updated;
        });
        throw error;
      }
    },
    []
  );

  const cancelScan = useCallback(async (rootId: number) => {
    try {
      await invoke('cancel_scan', { rootId });
    } finally {
      setActiveScans((prev) => {
        const updated = new Map(prev);
        updated.delete(rootId);
        return updated;
      });
    }
  }, []);

  const dismissCompletedScan = useCallback((rootId: number) => {
    setActiveScans((prev) => {
      const updated = new Map(prev);
      updated.delete(rootId);
      return updated;
    });
  }, []);

  const isScanning = useCallback(
    (rootId: number) => {
      const scan = activeScans.get(rootId);
      return scan ? !scan.isComplete : false;
    },
    [activeScans]
  );

  return {
    activeScans,
    startScanWithProgress,
    cancelScan,
    dismissCompletedScan,
    isScanning,
    hasActiveScans: Array.from(activeScans.values()).some((s) => !s.isComplete),
  };
}
