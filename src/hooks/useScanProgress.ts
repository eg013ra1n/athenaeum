import { useState, useEffect, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import type { ScanProgressEvent, ScanCompleteEvent, ScanResult, OrphanedFile } from '../types/models';

// Extended result that includes missing files info
export interface RescanResult extends ScanResult {
  missingFilesCount: number;
  missingFiles: OrphanedFile[];
}

export interface ActiveScan {
  rootId: number;
  rootPath: string;
  progress: ScanProgressEvent | null;
  isComplete: boolean;
  isCancelling?: boolean;
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
                cancelled: event.payload.cancelled,
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
      // Register scan in local state (delete any existing entry first to clear stale completed scans)
      // Use flushSync to ensure the progress modal renders before the scan starts
      // This prevents a race condition where fast scans complete before the modal shows
      flushSync(() => {
        setActiveScans((prev) => {
          const updated = new Map(prev);
          updated.delete(rootId);
          updated.set(rootId, {
            rootId,
            rootPath,
            progress: null,
            isComplete: false,
            result: null,
          });
          return updated;
        });
      });

      // Small delay to ensure the progress modal is painted to screen before scan starts
      // Without this, fast scans (e.g., rescans with 0 new files) complete before the modal renders
      await new Promise(resolve => setTimeout(resolve, 50));

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

  // Unified rescan flow: check missing files + scan in one progress modal
  const startRescanWithProgress = useCallback(
    async (rootId: number, rootPath: string): Promise<RescanResult> => {
      // Register scan in state (shows modal immediately)
      flushSync(() => {
        setActiveScans((prev) => {
          const updated = new Map(prev);
          updated.delete(rootId);
          updated.set(rootId, {
            rootId,
            rootPath,
            progress: null,
            isComplete: false,
            result: null,
          });
          return updated;
        });
      });

      // Small delay to ensure the progress modal is painted
      await new Promise(resolve => setTimeout(resolve, 50));

      try {
        // Step 1: Check missing files (backend emits "verifying" phase)
        const missingFiles = await invoke<OrphanedFile[]>('check_missing_files_in_scan_root', { rootId });

        // Step 2: Persist missing files to database for persistent tracking
        if (missingFiles.length > 0) {
          await invoke('sync_missing_files', {
            rootId,
            fileIds: missingFiles.map(f => f.id),
          });
        } else {
          // Clear any previously tracked missing files that are now found
          await invoke('sync_missing_files', {
            rootId,
            fileIds: [],
          });
        }

        // Step 3: Run scan (backend emits discovery/processing/etc phases)
        const scanResult = await invoke<ScanResult>('start_scan_with_progress', { rootId });

        // Combine results
        const result: RescanResult = {
          ...scanResult,
          missingFilesCount: missingFiles.length,
          missingFiles,
        };

        // Update state with final result
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(rootId);
          if (existing) {
            updated.set(rootId, {
              ...existing,
              isComplete: true,
              result: scanResult,
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
    // Mark as cancelling in local state (for UI feedback)
    setActiveScans((prev) => {
      const updated = new Map(prev);
      const existing = updated.get(rootId);
      if (existing) {
        updated.set(rootId, {
          ...existing,
          isCancelling: true,
        });
      }
      return updated;
    });
    // Send cancel request to backend - scan will complete with cancelled flag
    await invoke('cancel_scan', { rootId });
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
    startRescanWithProgress,
    cancelScan,
    dismissCompletedScan,
    isScanning,
    hasActiveScans: Array.from(activeScans.values()).some((s) => !s.isComplete),
  };
}
