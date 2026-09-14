import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  PlateSolveProgressEvent,
  PlateSolveCompleteEvent,
  CatalogStatusInfo,
} from '../types/helpers';

export type FrameSolveStatus =
  | { kind: 'pending' }
  | { kind: 'solving'; filename?: string; startedAt: number }
  | { kind: 'cancelled' }
  | { kind: 'solved'; matched_stars: number; rms_arcsec: number }
  | { kind: 'failed'; error: string; code?: string; filename?: string };

export interface PlateSolveSummary {
  solved: number;
  failed: number;
  total: number;
  total_time_ms: number;
  cancelled?: boolean;
  not_processed?: number;
}

export interface QueueItem {
  sequential?: boolean;
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
  startedAt: number | null;
  frameStatuses: Map<number, FrameSolveStatus>;
  isComplete: boolean;
  isCancelling: boolean;
  summary: PlateSolveSummary | null;
  /** Set when the backend rejects the whole batch (e.g. star catalog missing) so the
   *  per-batch banner can explain the failure instead of just showing "0/N solved". */
  errorMessage: string | null;
}

/** Reason the queue refused to invoke the backend. `catalog_missing` triggers the
 *  dedicated "Download the star catalog from Settings" modal; `unknown` is a generic fallback. */
export type PlateSolvePrecheckError =
  | { kind: 'catalog_missing'; message: string }
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
  const cancelRequested = useRef(new Set<number>());
  const backendStarted = useRef(false);
  const completeRef = useRef<(() => void) | null>(null);
  const queueRef = useRef(queue);
  queueRef.current = queue;

  // Once we've confirmed the star catalog is present, skip the precheck for
  // subsequent batches in the same session — the check is cheap but pointless
  // to repeat once we know it's there.
  const catalogVerifiedRef = useRef(false);

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
        updated.set(next.batchId, { ...entry, frameStatuses, startedAt: Date.now() });
      }
      return updated;
    });

    // Precheck: bail out before invoking the backend if the star catalog
    // (solvemyastro's stars.smac) hasn't been downloaded yet. The backend would
    // reject with an error string, but detecting it here lets us show a
    // dedicated modal that links to the download-from-Settings UI.
    if (!catalogVerifiedRef.current) {
      try {
        const statuses = await api.invoke<CatalogStatusInfo[]>('get_catalog_status');
        if (!statuses.some(s => s.installed)) {
          const message =
            'The star catalog has not been downloaded yet. Open Settings → Plate Solving to download the star catalog.';
          failBatch(next.batchId, next.frameIds.length, message);
          setPrecheckError({ kind: 'catalog_missing', message });
          processingRef.current = false;
          currentBatchIdRef.current = null;
          setQueue(q => q.filter(item => item.batchId !== next.batchId));
          return;
        }
        catalogVerifiedRef.current = true;
      } catch (err) {
        // Status call itself failed — surface generically and skip the batch.
        const message = `Could not verify star catalog status: ${String(err)}`;
        console.error('Plate solve precheck failed:', err);
        failBatch(next.batchId, next.frameIds.length, message);
        setPrecheckError({ kind: 'unknown', message });
        processingRef.current = false;
        currentBatchIdRef.current = null;
        setQueue(q => q.filter(item => item.batchId !== next.batchId));
        return;
      }
    }

    if (cancelRequested.current.delete(next.batchId)) {
      setActiveBatches(prev => {
        const updated = new Map(prev),
          entry = updated.get(next.batchId);
        if (entry)
          updated.set(next.batchId, {
            ...entry,
            isComplete: true,
            isCancelling: false,
            summary: {
              solved: 0,
              failed: 0,
              total: next.frameIds.length,
              total_time_ms: 0,
              cancelled: true,
              not_processed: next.frameIds.length,
            },
          });
        return updated;
      });
      processingRef.current = false;
      currentBatchIdRef.current = null;
      setQueue(q => q.filter(item => item.batchId !== next.batchId));
      return;
    }
    try {
      const completion = new Promise<void>(resolve => {
        completeRef.current = resolve;
      });
      await api.invoke<void>('plate_solve_batch', {
        frameIds: next.frameIds,
        sequential: next.sequential ?? false,
      });
      await completion;
    } catch (err) {
      console.error(`Plate solve batch ${next.batchId} failed:`, err);
      const message = String(err);
      // Some failures only show up once the backend tries to open the catalog
      // (e.g. file disappeared between the precheck and the call). Re-trigger
      // the catalog-missing modal in that case so the user gets the right CTA.
      if (/cache|catalog|smac/i.test(message) && /not found|missing|no such/i.test(message)) {
        catalogVerifiedRef.current = false;
        setPrecheckError({ kind: 'catalog_missing', message });
      }
      failBatch(next.batchId, next.frameIds.length, message);
    }

    completeRef.current = null;
    backendStarted.current = false;
    cancelRequested.current.delete(next.batchId);
    processingRef.current = false;
    currentBatchIdRef.current = null;
    setQueue(q => q.filter(item => item.batchId !== next.batchId));
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
      .listen<PlateSolveProgressEvent>('plate-solve-progress', payload => {
        if (cancelled) return;
        const batchId = currentBatchIdRef.current;
        if (batchId == null) return;

        if (!backendStarted.current) {
          backendStarted.current = true;
          if (cancelRequested.current.has(batchId)) {
            api.invoke('cancel_plate_solve').catch(error => {
              console.error('Cancel plate solve failed:', error);
              cancelRequested.current.delete(batchId);
              setActiveBatches(prev => {
                const next = new Map(prev),
                  entry = next.get(batchId);
                if (entry) next.set(batchId, { ...entry, isCancelling: false });
                return next;
              });
              notify({
                title: 'Could not cancel plate solving',
                detail: String(error),
                kind: 'platesolve',
                tone: 'warning',
                hasErrors: true,
              });
            });
          }
        }
        setActiveBatches(prev => {
          const entry = prev.get(batchId);
          if (!entry) return prev;
          const frameStatuses = new Map(entry.frameStatuses);
          if (payload.status === 'solving') {
            frameStatuses.set(payload.frameId, {
              kind: 'solving',
              filename: payload.filename ?? undefined,
              startedAt: Date.now(),
            });
          } else if (payload.status === 'solved') {
            frameStatuses.set(payload.frameId, {
              kind: 'solved',
              matched_stars: payload.matchedStars ?? 0,
              rms_arcsec: payload.rmsArcsec ?? 0,
            });
          } else if (payload.status === 'cancelled') {
            frameStatuses.set(payload.frameId, { kind: 'cancelled' });
          } else if (payload.status === 'failed') {
            frameStatuses.set(payload.frameId, {
              kind: 'failed',
              error: payload.error ?? 'Solve failed',
              code: payload.failureCode ?? undefined,
              filename: payload.filename ?? undefined,
            });
          }
          const updated = new Map(prev);
          updated.set(batchId, {
            ...entry,
            progress: {
              current: Math.max(entry.progress?.current ?? 0, payload.current),
              total: payload.total,
            },
            currentFrameId: payload.frameId,
            frameStatuses,
          });
          return updated;
        });
      })
      .then(fn => {
        if (cancelled) fn();
        else unlistenProgress = fn;
      })
      .catch(err => console.error('[usePlateSolveQueue] listen failed:', err));

    api
      .listen<PlateSolveCompleteEvent>('plate-solve-complete', payload => {
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
              total_time_ms: payload.totalTimeMs,
              cancelled: payload.cancelled,
              not_processed: payload.notProcessed,
            },
          });
          return updated;
        });

        completeRef.current?.();
        completeRef.current = null;
        notify({
          title: `Plate-solve ${payload.cancelled ? 'cancelled' : 'finished'} — ${payload.solved}/${payload.total} solved`,
          detail: payload.failed
            ? `${payload.failed} failed`
            : payload.cancelled
              ? `${payload.notProcessed ?? payload.total - payload.solved} not processed`
              : 'All frames solved',
          kind: 'platesolve',
          hasErrors: payload.failed > 0,
          tone: payload.failed > 0 ? 'warning' : 'success',
        });
      })
      .then(fn => {
        if (cancelled) fn();
        else unlistenComplete = fn;
      })
      .catch(err => console.error('[usePlateSolveQueue] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
  }, []);

  const enqueuePlateSolve = useCallback(
    (frameIds: number[], label?: string, sequential = false): number => {
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
          startedAt: null,
          frameStatuses,
          isComplete: false,
          isCancelling: false,
          summary: null,
          errorMessage: null,
        });
        return updated;
      });
      setQueue(q => [...q, { batchId, label: resolvedLabel, frameIds, sequential }]);
      return batchId;
    },
    [],
  );

  const cancelBatch = useCallback(
    async (batchId: number) => {
      const running = currentBatchIdRef.current === batchId;
      if (running) cancelRequested.current.add(batchId);
      setQueue(q => q.filter(item => item.batchId !== batchId || running));
      setActiveBatches(prev => {
        const entry = prev.get(batchId);
        if (!entry || entry.isComplete) return prev;
        const updated = new Map(prev);
        updated.set(
          batchId,
          running
            ? { ...entry, isCancelling: true }
            : {
                ...entry,
                isComplete: true,
                isCancelling: false,
                summary: {
                  solved: 0,
                  failed: 0,
                  total: entry.frameIds.length,
                  total_time_ms: 0,
                  cancelled: true,
                  not_processed: entry.frameIds.length,
                },
              },
        );
        return updated;
      });
      // Before the first progress event there may not be a backend cancel handle.
      // Keep the intent and cancel as soon as that event confirms the worker exists.
      if (running && backendStarted.current) {
        try {
          await api.invoke('cancel_plate_solve');
        } catch (error) {
          console.error('Cancel plate solve failed:', error);
          cancelRequested.current.delete(batchId);
          setActiveBatches(prev => {
            const next = new Map(prev),
              entry = next.get(batchId);
            if (entry) next.set(batchId, { ...entry, isCancelling: false });
            return next;
          });
          notify({
            title: 'Could not cancel plate solving',
            detail: String(error),
            kind: 'platesolve',
            tone: 'warning',
            hasErrors: true,
          });
        }
      }
    },
    [notify],
  );

  const cancelAll = useCallback(async () => {
    await Promise.all(queueRef.current.map(item => cancelBatch(item.batchId)));
  }, [cancelBatch]);

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

  const currentBatch =
    Array.from(activeBatches.values()).find(b => !b.isComplete && b.startedAt !== null) ??
    (queue.length > 0 ? (activeBatches.get(queue[0].batchId) ?? null) : null);
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
