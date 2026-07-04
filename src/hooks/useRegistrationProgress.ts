import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  StackingPrepProgressEvent,
  StackingPrepCompleteEvent,
} from '../types/models';
import type { FrameRegistrationStatus } from '../types/helpers';

// ── Per-frame live status ────────────────────────────────────────────────────

export interface FrameRegStatus {
  status: FrameRegistrationStatus;
  matchedStars?: number;
  /** RMS residual value carried in the progress event (displayed directly). */
  rmsArcsec?: number;
  error?: string;
  filename?: string;
}

// ── Overall progress counters for the active run ─────────────────────────────

export interface RegistrationProgress {
  current: number;
  total: number;
}

// ── Per-frame-set state held in the active map ───────────────────────────────

export interface ActiveRegistration {
  framesSetId: number;
  frameSetName?: string;
  /** The reference frame to pass to the backend; undefined = backend picks. */
  referenceFrameId?: number;
  progress: RegistrationProgress | null;
  /** Live per-frame status map; keyed by frameId. */
  frameStatuses: Map<number, FrameRegStatus>;
  isComplete: boolean;
  isCancelling: boolean;
}

// ── Queue item ────────────────────────────────────────────────────────────────

export interface RegistrationQueueItem {
  framesSetId: number;
  frameSetName?: string;
  referenceFrameId?: number;
}

// ── Hook ─────────────────────────────────────────────────────────────────────

/**
 * Global FIFO queue for stacking-prep (frame registration) jobs. Mirrors
 * `useAnalysisProgress` in structure: one job runs at a time, progress events
 * are consumed here (not inside StackingPrepTab), and `notify()` fires on
 * completion. Exposed through `RegistrationProgressProvider` so components
 * anywhere in the tree share the same queue state.
 */
export function useRegistrationProgress() {
  const [queue, setQueue] = useState<RegistrationQueueItem[]>([]);
  const [activeRegistrations, setActiveRegistrations] = useState<
    Map<number, ActiveRegistration>
  >(new Map());
  const { notify } = useNotifications();

  const processingRef = useRef(false);
  const queueRef = useRef(queue);
  queueRef.current = queue;

  // Ref that always points at the framesSetId currently being processed so the
  // event listeners (mounted once) know which active-map entry to update.
  const currentSetIdRef = useRef<number | null>(null);

  // ── Queue runner ──────────────────────────────────────────────────────────

  const runNext = useCallback(async () => {
    if (processingRef.current) return;
    const next = queueRef.current[0];
    if (!next) return;

    processingRef.current = true;
    currentSetIdRef.current = next.framesSetId;

    try {
      await api.invoke<void>('register_frame_set', {
        framesSetId: next.framesSetId,
        ...(next.referenceFrameId != null
          ? { referenceFrameId: next.referenceFrameId }
          : {}),
      });
      // `stacking-prep-complete` drives the isComplete transition; the invoke
      // resolving only means the backend command returned (it fires the event
      // before returning). Mark complete here as a safety net for cases where
      // the event arrives before the invoke resolves or gets lost.
      setActiveRegistrations((prev) => {
        const entry = prev.get(next.framesSetId);
        if (!entry || entry.isComplete) return prev;
        const updated = new Map(prev);
        updated.set(next.framesSetId, { ...entry, isComplete: true, isCancelling: false });
        return updated;
      });
    } catch (err) {
      console.error(
        `[useRegistrationProgress] register_frame_set failed for set ${next.framesSetId}:`,
        err,
      );
      setActiveRegistrations((prev) => {
        const entry = prev.get(next.framesSetId);
        if (!entry) return prev;
        const updated = new Map(prev);
        updated.set(next.framesSetId, { ...entry, isComplete: true, isCancelling: false });
        return updated;
      });
    }

    processingRef.current = false;
    currentSetIdRef.current = null;
    setQueue((q) => q.slice(1));
  }, []);

  // Kick the processor whenever the queue gains an item.
  useEffect(() => {
    if (queue.length > 0 && !processingRef.current) {
      runNext();
    }
  }, [queue, runNext]);

  // ── Backend event listeners (mounted once, StrictMode-safe) ──────────────

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenComplete: (() => void) | undefined;

    api
      .listen<StackingPrepProgressEvent>('stacking-prep-progress', (payload) => {
        if (cancelled) return;
        const setId = currentSetIdRef.current;
        if (setId == null) return;

        setActiveRegistrations((prev) => {
          const entry = prev.get(setId);
          if (!entry) return prev;
          const frameStatuses = new Map(entry.frameStatuses);
          frameStatuses.set(payload.frameId, {
            // Generated StackingPrepProgressEvent.status is a plain `string`
            // (Rust field has no enum, just a doc-commented value set —
            // see registration/service.rs); narrowed here to the documented
            // FrameRegistrationStatus values.
            status: payload.status as FrameRegistrationStatus,
            // Generated StackingPrepProgressEvent fields are `T | null`
            // (plain Option<T>, no #[ts(optional)] override); FrameRegStatus
            // keeps its local `T | undefined` shape, so normalize here.
            matchedStars: payload.matchedStars ?? undefined,
            rmsArcsec: payload.rmsPx ?? undefined,
            error: payload.error ?? undefined,
            filename: payload.filename ?? undefined,
          });
          const updated = new Map(prev);
          updated.set(setId, {
            ...entry,
            progress: { current: payload.current, total: payload.total },
            frameStatuses,
          });
          return updated;
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenProgress = fn;
      })
      .catch((err) =>
        console.error('[useRegistrationProgress] listen stacking-prep-progress failed:', err),
      );

    api
      .listen<StackingPrepCompleteEvent>('stacking-prep-complete', (payload) => {
        if (cancelled) return;
        const setId = currentSetIdRef.current;
        if (setId == null) return;

        setActiveRegistrations((prev) => {
          const entry = prev.get(setId);
          if (!entry) return prev;
          const updated = new Map(prev);
          updated.set(setId, { ...entry, isComplete: true, isCancelling: false });
          return updated;
        });

        notify({
          title: `Registration finished — ${payload.aligned}/${payload.total} aligned`,
          detail:
            payload.failed > 0
              ? `${payload.failed} frame${payload.failed === 1 ? '' : 's'} failed`
              : 'All frames aligned',
          kind: 'registration',
          tone: payload.failed > 0 ? 'warning' : 'success',
          hasErrors: payload.failed > 0,
          dedupeKey: `reg-${setId}`,
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenComplete = fn;
      })
      .catch((err) =>
        console.error('[useRegistrationProgress] listen stacking-prep-complete failed:', err),
      );

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Public actions ────────────────────────────────────────────────────────

  /** Add a frame set to the registration queue. Skips if it is already queued
   *  and not yet complete. Returns immediately; execution is serialized. */
  const enqueueRegistration = useCallback(
    (framesSetId: number, frameSetName?: string, referenceFrameId?: number) => {
      setActiveRegistrations((prev) => {
        // If already queued and in-progress, don't reset the entry.
        if (prev.has(framesSetId) && !prev.get(framesSetId)!.isComplete) return prev;
        const updated = new Map(prev);
        updated.set(framesSetId, {
          framesSetId,
          frameSetName,
          referenceFrameId,
          progress: null,
          frameStatuses: new Map(),
          isComplete: false,
          isCancelling: false,
        });
        return updated;
      });
      setQueue((q) => {
        if (q.some((item) => item.framesSetId === framesSetId)) return q;
        return [...q, { framesSetId, frameSetName, referenceFrameId }];
      });
    },
    [],
  );

  /** Cancel the registration for a given frame set. If it is currently running
   *  the backend command is called; if queued-but-not-started it is removed. */
  const cancelRegistration = useCallback(async (framesSetId: number) => {
    setQueue((q) => q.filter((item) => item.framesSetId !== framesSetId));
    setActiveRegistrations((prev) => {
      const entry = prev.get(framesSetId);
      if (!entry || entry.isComplete) return prev;
      const updated = new Map(prev);
      updated.set(framesSetId, { ...entry, isCancelling: true });
      return updated;
    });
    if (currentSetIdRef.current === framesSetId) {
      try {
        await api.invoke<void>('cancel_frame_set_registration', { framesSetId });
      } catch (err) {
        // May fail if the run already finished — safe to ignore.
        console.error('[useRegistrationProgress] cancel failed:', err);
      }
    }
  }, []);

  /** Cancel all pending and active registrations. */
  const cancelAll = useCallback(async () => {
    const running = currentSetIdRef.current;
    setQueue([]);
    if (running != null) {
      try {
        await api.invoke<void>('cancel_frame_set_registration', { framesSetId: running });
      } catch {
        // ignore
      }
    }
    setActiveRegistrations((prev) => {
      const updated = new Map(prev);
      for (const [, entry] of updated) {
        if (!entry.isComplete) {
          updated.set(entry.framesSetId, { ...entry, isCancelling: true });
        }
      }
      return updated;
    });
  }, []);

  /** Dismiss a completed registration entry from the active map. */
  const dismissCompleted = useCallback((framesSetId: number) => {
    setActiveRegistrations((prev) => {
      const updated = new Map(prev);
      updated.delete(framesSetId);
      return updated;
    });
  }, []);

  /** Get the live run state for a specific frame set (null if not in the map). */
  const getRegistrationState = useCallback(
    (framesSetId: number): ActiveRegistration | null => {
      return activeRegistrations.get(framesSetId) ?? null;
    },
    [activeRegistrations],
  );

  // ── Derived state for the indicator ──────────────────────────────────────

  const currentRegistration =
    queue.length > 0 ? activeRegistrations.get(queue[0].framesSetId) ?? null : null;
  const queueLength = queue.length;
  const hasActiveRegistrations = Array.from(activeRegistrations.values()).some(
    (r) => !r.isComplete,
  );

  return {
    // actions
    enqueueRegistration,
    cancelRegistration,
    cancelAll,
    dismissCompleted,
    // state selectors
    getRegistrationState,
    activeRegistrations,
    currentRegistration,
    queueLength,
    hasActiveRegistrations,
  };
}
