import { useState, useEffect, useCallback } from 'react';
import { flushSync } from 'react-dom';
import { api, type UnlistenFn } from '../api';
import type { ScanProgressEvent, ScanCompleteEvent, ScanResult, OrphanedFile } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

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
  const { notify } = useNotifications();

  // Listen for progress events
  useEffect(() => {
    let cancelled = false;
    let progressUnlisten: UnlistenFn | undefined;
    let completeUnlisten: UnlistenFn | undefined;

    api
      .listen<ScanProgressEvent>('scan-progress', (payload) => {
        if (cancelled) return;
        const { root_id } = payload;
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(root_id);
          if (existing) {
            updated.set(root_id, {
              ...existing,
              progress: payload,
            });
          }
          return updated;
        });
      })
      .then((fn) => { if (cancelled) fn(); else progressUnlisten = fn; })
      .catch((err) => console.error('[useScanProgress] listen failed:', err));

    api
      .listen<ScanCompleteEvent>('scan-complete', (payload) => {
        if (cancelled) return;
        const { root_id } = payload;
        setActiveScans((prev) => {
          const updated = new Map(prev);
          const existing = updated.get(root_id);
          if (existing) {
            updated.set(root_id, {
              ...existing,
              isComplete: true,
              result: {
                files_found: payload.files_found,
                files_processed: payload.files_processed,
                files_skipped: payload.files_skipped,
                errors: payload.errors,
                lights_count: payload.lights_count,
                darks_count: payload.darks_count,
                flats_count: payload.flats_count,
                bias_count: payload.bias_count,
                darkflats_count: payload.darkflats_count,
                calibration_sets_created: payload.calibration_sets_created,
                frames_renamed: payload.frames_renamed ?? 0,
                calibration_sets_deleted: payload.calibration_sets_deleted ?? 0,
                sessions_updated: payload.sessions_updated ?? 0,
                cancelled: payload.cancelled,
              },
              progress: null,
            });
          }
          return updated;
        });

        const scanErrs = payload.errors?.length ?? 0;
        const nothingNew =
          !payload.cancelled && scanErrs === 0 && payload.files_processed === 0;
        notify({
          title: payload.cancelled
            ? 'Scan cancelled'
            : nothingNew
              ? 'No new files found'
              : `Scan finished — ${payload.files_processed} new or updated`,
          detail: nothingNew
            ? `Already up to date — ${payload.files_found} files`
            : `${payload.files_found} on disk, ${payload.files_skipped} unchanged${scanErrs ? `, ${scanErrs} error${scanErrs === 1 ? '' : 's'}` : ''}`,
          kind: 'scan',
          hasErrors: scanErrs > 0,
          tone: scanErrs > 0
            ? 'warning'
            : payload.cancelled || nothingNew
              ? 'info'
              : 'success',
        });
      })
      .then((fn) => { if (cancelled) fn(); else completeUnlisten = fn; })
      .catch((err) => console.error('[useScanProgress] listen failed:', err));

    return () => {
      cancelled = true;
      progressUnlisten?.();
      completeUnlisten?.();
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
        const result = await api.invoke<ScanResult>('start_scan_with_progress', { rootId });

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
        const missingFiles = await api.invoke<OrphanedFile[]>('check_missing_files_in_scan_root', { rootId });

        // Step 2: Persist missing files to database for persistent tracking
        if (missingFiles.length > 0) {
          await api.invoke('sync_missing_files', {
            rootId,
            fileIds: missingFiles.map(f => f.id),
          });
        } else {
          // Clear any previously tracked missing files that are now found
          await api.invoke('sync_missing_files', {
            rootId,
            fileIds: [],
          });
        }

        // Step 3: Run scan (backend emits discovery/processing/etc phases)
        const scanResult = await api.invoke<ScanResult>('start_scan_with_progress', { rootId });

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
    await api.invoke('cancel_scan', { rootId });
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
