import { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type { MasterBuildProgressEvent, MasterBuildCompleteEvent } from '../types/helpers';
import type { MasterRecipe, BatchBuildReport } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

export type BuildState =
  | { phase: 'starting' }
  | { phase: 'building'; progress: MasterBuildProgressEvent }
  | { phase: 'done'; result: MasterBuildCompleteEvent };

/**
 * Tracks in-flight master builds by source calibration-set id. The backend
 * (ComputeQueue, Task 4/12) owns admission and ordering — this hook holds no
 * frontend FIFO. `startBuild`/`startBatch` fire-and-forget the invoke; all
 * state transitions arrive via `master-build-progress` / `master-build-complete`
 * events, mirroring `useAnalysisProgress`'s listener discipline exactly.
 */
export function useMasterBuilds() {
  const [buildStates, setBuildStates] = useState<Map<number, BuildState>>(new Map());
  const { notify } = useNotifications();

  // Listen for progress and completion events (once on mount)
  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;

    api
      .listen<MasterBuildProgressEvent>('master-build-progress', (payload) => {
        if (cancelled) return;
        setBuildStates(prev => new Map(prev).set(payload.set_id, { phase: 'building', progress: payload }));
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[useMasterBuilds] listen failed:', err));

    api
      .listen<MasterBuildCompleteEvent>('master-build-complete', (payload) => {
        if (cancelled) return;
        setBuildStates(prev => new Map(prev).set(payload.set_id, { phase: 'done', result: payload }));

        notify({
          title: payload.cancelled
            ? 'Master build cancelled'
            : payload.success
              ? 'Master created'
              : 'Master build failed',
          detail: payload.success
            ? `Set #${payload.set_id} → master set #${payload.master_set_id}`
            : (payload.error ?? ''),
          kind: 'masterbuild',
          hasErrors: !payload.success && !payload.cancelled,
          tone: payload.success ? 'success' : payload.cancelled ? 'info' : 'warning',
        });

        // The set list changed shape (raw set superseded + new master row).
        window.dispatchEvent(new Event('library-updated'));
      })
      .then((fn) => { if (cancelled) fn(); else unlistenComplete = fn; })
      .catch((err) => console.error('[useMasterBuilds] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
  }, [notify]);

  const startBuild = useCallback(async (setId: number, recipe: MasterRecipe) => {
    setBuildStates(prev => new Map(prev).set(setId, { phase: 'starting' }));
    try {
      await api.invoke('start_master_build', { setId, recipe });
    } catch (err) {
      setBuildStates(prev => new Map(prev).set(setId, {
        phase: 'done',
        result: { set_id: setId, master_set_id: null, success: false, cancelled: false, error: String(err) },
      }));
      throw err;
    }
  }, []);

  const startBatch = useCallback(async (setIds: number[], recipe: MasterRecipe): Promise<BatchBuildReport> => {
    setBuildStates(prev => {
      const next = new Map(prev);
      for (const id of setIds) next.set(id, { phase: 'starting' });
      return next;
    });
    return api.invoke<BatchBuildReport>('start_master_builds_batch', { setIds, recipe });
  }, []);

  const cancelBuild = useCallback(async (setId: number) => {
    try {
      await api.invoke('cancel_master_build', { setId });
    } catch {
      // May fail if already finished — safe to ignore.
    }
  }, []);

  const isBuilding = useCallback((setId: number): boolean => {
    const s = buildStates.get(setId);
    return !!s && s.phase !== 'done';
  }, [buildStates]);

  return { buildStates, startBuild, startBatch, cancelBuild, isBuilding };
}
