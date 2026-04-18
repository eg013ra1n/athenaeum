import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import type {
  PlateSolveProgressEvent,
  PlateSolveCompleteEvent,
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
}

let nextBatchId = 1;

export function usePlateSolveQueue() {
  const [queue, setQueue] = useState<QueueItem[]>([]);
  const [activeBatches, setActiveBatches] = useState<Map<number, ActivePlateSolveBatch>>(
    new Map(),
  );
  const processingRef = useRef(false);
  const queueRef = useRef(queue);
  queueRef.current = queue;

  // Backend only runs one plate-solve batch at a time (cancel handle key = 0),
  // so every incoming progress event belongs to the currently-running batch.
  const currentBatchIdRef = useRef<number | null>(null);

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

    try {
      await api.invoke<void>('plate_solve_batch', { frameIds: next.frameIds });
    } catch (err) {
      console.error(`Plate solve batch ${next.batchId} failed:`, err);
      setActiveBatches(prev => {
        const updated = new Map(prev);
        const entry = updated.get(next.batchId);
        if (entry) {
          updated.set(next.batchId, {
            ...entry,
            isComplete: true,
            isCancelling: false,
            summary: { solved: 0, failed: entry.frameIds.length, total: entry.frameIds.length, total_time_ms: 0 },
          });
        }
        return updated;
      });
    }

    processingRef.current = false;
    currentBatchIdRef.current = null;
    setQueue(q => q.slice(1));
  }, []);

  // Kick the processor whenever the queue has pending items.
  useEffect(() => {
    if (queue.length > 0 && !processingRef.current) {
      runNext();
    }
  }, [queue, runNext]);

  // Listen for backend progress + complete events once on mount.
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;

    (async () => {
      unlistenProgress = await api.listen<PlateSolveProgressEvent>(
        'plate-solve-progress',
        (payload) => {
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
      );

      unlistenComplete = await api.listen<PlateSolveCompleteEvent>(
        'plate-solve-complete',
        (payload) => {
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
        },
      );
    })();

    return () => {
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
    // state
    activeBatches,
    currentBatch,
    queueLength,
    hasActiveBatches,
    // helpers
    getFrameStatus,
  };
}
