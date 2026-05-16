import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  PlateSolveProgressEvent,
  PlateSolveCompleteEvent,
  QuadIndexStatus,
} from '../types/plate-solve';

export type FrameSolveStatus =
  | { kind: 'pending' }
  | { kind: 'solving' }
  | { kind: 'solved'; matched_stars: number; rms_arcsec: number }
  | { kind: 'failed'; error: string };

export interface PlateSolveSummary {
  solved: number;
  failed: number;
  total: number;
  total_time_ms: number;
}

export interface QueueItem {
  batchId: number;
  label: string;
  frameIds: number[];
}

export interface ActivePlateSolveBatch {
  batchId: number;
  label: string;
  frameIds: number[];
  progress: { current: number; total: number } | null;
  currentFrameId: number | null;
  frameStatuses: Map<number, FrameSolveStatus>;
  isComplete: boolean;
  isCancelling: boolean;
  summary: PlateSolveSummary | null;
  /** Set when the backend rejects the whole batch (e.g. quad index missing) so the
   *  per-batch banner can explain the failure instead of just showing "0/N solved". */
  errorMessage: string | null;
}

/** Reason the queue refused to invoke the backend. `index_missing` triggers the
 *  dedicated "Build the index from Settings" modal; `unknown` is a generic fallback. */
export type PlateSolvePrecheckError =
  | { kind: 'index_missing'; message: string }
  | { kind: 'unknown'; message: string };

let nextBatchId = 1;

export function usePlateSolveQueue() {
  const [queue, setQueue] = useState<QueueItem[]>([]);
  const { notify } = useNotifications();
  const [activeBatches, setActiveBatches] = useState<Map<number, ActivePlateSolveBatch>>(
    new Map(),
  );
  const [precheckError, setPrecheckError] = useState<PlateSolvePrecheckError | null>(null);
  const processingRef = useRef(false);
  const queueRef = useRef(queue);
  queueRef.current = queue;

  // Once we've confirmed the quad index is built, skip the precheck for
  // subsequent batches in the same session — the file-existence check is
  // cheap but pointless to repeat once we know it's there.
  const quadIndexVerifiedRef = useRef(false);

  // Backend only runs one plate-solve batch at a time (cancel handle key = 0),
  // so every incoming progress event belongs to the currently-running batch.
  const currentBatchIdRef = useRef<number | null>(null);

  /** Mark a batch as completed-with-failure and stash the reason so the panel
   *  banner can show *why* every frame failed. */
  const failBatch = useCallback((batchId: number, frameCount: number, message: string) => {
    setActiveBatches(prev => {
      const updated = new Map(prev);
      const entry = updated.get(batchId);
      if (entry) {
        updated.set(batchId, {
          ...entry,
          isComplete: true,
          isCancelling: false,
          summary: { solved: 0, failed: frameCount, total: frameCount, total_time_ms: 0 },
          errorMessage: message,
        });
      }
      return updated;
    });
  }, []);

  const runNext = useCallback(async () => {
    if (processingRef.current) return;
    const next = queueRef.current[0];
    if (!next) return;

    processingRef.current = true;
    currentBatchIdRef.current = next.batchId;

    // Pre-populate per-frame statuses so the UI shows pending rows immediately.
    setActiveBatches(prev => {
      const updated = new Map(prev);
      const entry = updated.get(next.batchId);
      if (entry) {
        const frameStatuses = new Map(entry.frameStatuses);
        for (const id of next.frameIds) {
          if (!frameStatuses.has(id)) frameStatuses.set(id, { kind: 'pending' });
        }
        updated.set(next.batchId, { ...entry, frameStatuses });
      }
      return updated;
    });

    // Precheck: bail out before invoking the backend if the quad index isn't
    // built yet. The backend would reject with the same error string, but
    // detecting it here lets us show a dedicated modal that links to the
    // build-it-from-Settings UI instead of just an inline error.
    if (!quadIndexVerifiedRef.current) {
      try {
        const status = await api.invoke<QuadIndexStatus>('get_quad_index_status');
        if (!status.built) {
          const message =
            'Plate-solve indexes have not been built yet. Open Settings → Plate Solving to download the Tycho-2 catalog and build the quad index.';
          failBatch(next.batchId, next.frameIds.length, message);
          setPrecheckError({ kind: 'index_missing', message });
          processingRef.current = false;
          currentBatchIdRef.current = null;
          setQueue(q => q.slice(1));
          return;
        }
        quadIndexVerifiedRef.current = true;
      } catch (err) {
        // Status call itself failed — surface generically and skip the batch.
        const message = `Could not verify plate-solve index status: ${String(err)}`;
        console.error('Plate solve precheck failed:', err);
        failBatch(next.batchId, next.frameIds.length, message);
        setPrecheckError({ kind: 'unknown', message });
        processingRef.current = false;
        currentBatchIdRef.current = null;
        setQueue(q => q.slice(1));
        return;
      }
    }

    try {
      await api.invoke<void>('plate_solve_batch', { frameIds: next.frameIds });
    } catch (err) {
      console.error(`Plate solve batch ${next.batchId} failed:`, err);
      const message = String(err);
      // Some failures only show up once the backend tries to load the index
      // (e.g. file disappeared between the precheck and the call). Re-trigger
      // the index-missing modal in that case so the user gets the right CTA.
      if (/quad index/i.test(message) && /not found|missing/i.test(message)) {
        quadIndexVerifiedRef.current = false;
        setPrecheckError({ kind: 'index_missing', message });
      }
      failBatch(next.batchId, next.frameIds.length, message);
    }

    processingRef.current = false;
    currentBatchIdRef.current = null;
    setQueue(q => q.slice(1));
  }, [failBatch]);

  // Kick the processor whenever the queue has pending items.
  useEffect(() => {
    if (queue.length > 0 && !processingRef.current) {
      runNext();
    }
  }, [queue, runNext]);

  // Listen for backend progress + complete events once on mount.
  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;

    api
      .listen<PlateSolveProgressEvent>(
        'plate-solve-progress',
        (payload) => {
          if (cancelled) return;
          const batchId = currentBatchIdRef.current;
          if (batchId == null) return;

          setActiveBatches(prev => {
            const entry = prev.get(batchId);
            if (!entry) return prev;
            const frameStatuses = new Map(entry.frameStatuses);
            if (payload.status === 'solving') {
              frameStatuses.set(payload.frame_id, { kind: 'solving' });
            } else if (payload.status === 'solved') {
              frameStatuses.set(payload.frame_id, {
                kind: 'solved',
                matched_stars: payload.matched_stars ?? 0,
                rms_arcsec: payload.rms_arcsec ?? 0,
              });
            } else if (payload.status === 'failed') {
              frameStatuses.set(payload.frame_id, {
                kind: 'failed',
                error: payload.error ?? 'Solve failed',
              });
            }
            const updated = new Map(prev);
            updated.set(batchId, {
              ...entry,
              progress: { current: payload.current, total: payload.total },
              currentFrameId: payload.frame_id,
              frameStatuses,
            });
            return updated;
          });
        },
      )
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[usePlateSolveQueue] listen failed:', err));

    api
      .listen<PlateSolveCompleteEvent>(
        'plate-solve-complete',
        (payload) => {
          if (cancelled) return;
          const batchId = currentBatchIdRef.current;
          if (batchId == null) return;
          setActiveBatches(prev => {
            const entry = prev.get(batchId);
            if (!entry) return prev;
            const updated = new Map(prev);
            updated.set(batchId, {
              ...entry,
              isComplete: true,
              isCancelling: false,
              summary: {
                solved: payload.solved,
                failed: payload.failed,
                total: payload.total,
                total_time_ms: payload.total_time_ms,
              },
            });
            return updated;
          });

          notify({
            title: `Plate-solve finished — ${payload.solved}/${payload.total} solved`,
            detail: payload.failed
              ? `${payload.failed} failed`
              : 'All frames solved',
            kind: 'platesolve',
            hasErrors: payload.failed > 0,
            tone: payload.failed > 0 ? 'warning' : 'success',
          });
        },
      )
      .then((fn) => { if (cancelled) fn(); else unlistenComplete = fn; })
      .catch((err) => console.error('[usePlateSolveQueue] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
  }, []);

  const enqueuePlateSolve = useCallback(
    (frameIds: number[], label?: string): number => {
      if (frameIds.length === 0) return -1;
      const batchId = nextBatchId++;
      const resolvedLabel = label ?? `${frameIds.length} frame${frameIds.length === 1 ? '' : 's'}`;

      setActiveBatches(prev => {
        const updated = new Map(prev);
        const frameStatuses = new Map<number, FrameSolveStatus>();
        for (const id of frameIds) frameStatuses.set(id, { kind: 'pending' });
        updated.set(batchId, {
          batchId,
          label: resolvedLabel,
          frameIds,
          progress: null,
          currentFrameId: null,
          frameStatuses,
          isComplete: false,
          isCancelling: false,
          summary: null,
          errorMessage: null,
        });
        return updated;
      });
      setQueue(q => [...q, { batchId, label: resolvedLabel, frameIds }]);
      return batchId;
    },
    [],
  );

  const cancelBatch = useCallback(async (batchId: number) => {
    setQueue(q => q.filter(item => item.batchId !== batchId));
    setActiveBatches(prev => {
      const entry = prev.get(batchId);
      if (!entry || entry.isComplete) return prev;
      const updated = new Map(prev);
      updated.set(batchId, { ...entry, isCancelling: true });
      return updated;
    });
    if (currentBatchIdRef.current === batchId) {
      try {
        await api.invoke('cancel_plate_solve');
      } catch {
        // May fail if the batch already finished — safe to ignore.
      }
    }
  }, []);

  const cancelAll = useCallback(async () => {
    setQueue([]);
    if (currentBatchIdRef.current != null) {
      try {
        await api.invoke('cancel_plate_solve');
      } catch {
        // ignore
      }
    }
    setActiveBatches(prev => {
      const updated = new Map(prev);
      for (const [, entry] of updated) {
        if (!entry.isComplete) {
          updated.set(entry.batchId, { ...entry, isCancelling: true });
        }
      }
      return updated;
    });
  }, []);

  const dismissCompleted = useCallback((batchId: number) => {
    setActiveBatches(prev => {
      const updated = new Map(prev);
      updated.delete(batchId);
      return updated;
    });
  }, []);

  const dismissPrecheckError = useCallback(() => setPrecheckError(null), []);

  const getFrameStatus = useCallback(
    (frameId: number): FrameSolveStatus | null => {
      for (const batch of activeBatches.values()) {
        const status = batch.frameStatuses.get(frameId);
        if (status) return status;
      }
      return null;
    },
    [activeBatches],
  );

  const currentBatch = queue.length > 0 ? activeBatches.get(queue[0].batchId) ?? null : null;
  const queueLength = queue.length;
  const hasActiveBatches = Array.from(activeBatches.values()).some(b => !b.isComplete);

  return {
    // actions
    enqueuePlateSolve,
    cancelBatch,
    cancelAll,
    dismissCompleted,
    dismissPrecheckError,
    // state
    activeBatches,
    currentBatch,
    queueLength,
    hasActiveBatches,
    precheckError,
    // helpers
    getFrameStatus,
  };
}
