import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  StackingPrepProgressEvent,
  StackingPrepCompleteEvent,
  FrameRegistrationStatus,
} from '../types/models';

/** Live status tracked per-frame while a registration run is active. */
export interface FrameRegStatus {
  status: FrameRegistrationStatus;
  matchedStars?: number;
  /** RMS residual in arcsec, when available from the progress event. */
  rmsArcsec?: number;
  error?: string;
  filename?: string;
}

/** Overall progress counters for the active run. */
export interface RegistrationProgress {
  current: number;
  total: number;
}

export interface RegistrationRunState {
  framesSetId: number;
  isRunning: boolean;
  isCancelling: boolean;
  progress: RegistrationProgress | null;
  /** Live per-frame status map; keyed by frameId. */
  frameStatuses: Map<number, FrameRegStatus>;
  isComplete: boolean;
}

/**
 * Hook for driving stacking-prep (frame registration) for a single frame set.
 *
 * Mirrors the structure of usePlateSolveQueue but simplified to one-frame-set-
 * at-a-time (the backend command is scoped to a framesSetId, not a global
 * batch queue). The hook subscribes to `stacking-prep-progress` and
 * `stacking-prep-complete` using the StrictMode-safe cancelled-flag pattern.
 */
export function useRegistrationQueue(framesSetId: number) {
  const { notify } = useNotifications();

  const [runState, setRunState] = useState<RegistrationRunState>({
    framesSetId,
    isRunning: false,
    isCancelling: false,
    progress: null,
    frameStatuses: new Map(),
    isComplete: false,
  });

  // Track whether a run is active so the event handlers know which events
  // belong to this hook instance's frame set.
  const isRunningRef = useRef(false);
  const framesSetIdRef = useRef(framesSetId);
  framesSetIdRef.current = framesSetId;

  // Subscribe to backend events once on mount; use cancelled-flag form for
  // React 18 StrictMode safety (see CLAUDE.md §Notifications).
  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenComplete: (() => void) | undefined;

    api
      .listen<StackingPrepProgressEvent>('stacking-prep-progress', (payload) => {
        if (cancelled) return;
        // Only process events for the frame set this hook instance manages.
        if (!isRunningRef.current) return;

        setRunState((prev) => {
          const frameStatuses = new Map(prev.frameStatuses);
          frameStatuses.set(payload.frameId, {
            status: payload.status,
            matchedStars: payload.matchedStars,
            // Progress event carries rmsPx; convert to arcsec if provided
            // (the backend may also include rmsPx directly for live display).
            rmsArcsec: payload.rmsPx,
            error: payload.error,
            filename: payload.filename,
          });
          return {
            ...prev,
            progress: { current: payload.current, total: payload.total },
            frameStatuses,
          };
        });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[useRegistrationQueue] listen progress failed:', err));

    api
      .listen<StackingPrepCompleteEvent>('stacking-prep-complete', (payload) => {
        if (cancelled) return;
        if (!isRunningRef.current) return;

        isRunningRef.current = false;

        setRunState((prev) => ({
          ...prev,
          isRunning: false,
          isCancelling: false,
          isComplete: true,
        }));

        notify({
          title: `Registration finished — ${payload.aligned}/${payload.total} aligned`,
          detail: payload.failed > 0
            ? `${payload.failed} frame${payload.failed === 1 ? '' : 's'} failed`
            : 'All frames aligned',
          kind: 'registration',
          tone: payload.failed > 0 ? 'warning' : 'success',
          hasErrors: payload.failed > 0,
          dedupeKey: `reg-${framesSetIdRef.current}`,
        });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenComplete = fn; })
      .catch((err) => console.error('[useRegistrationQueue] listen complete failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
  // Intentionally empty deps: subscribe once on mount, refs handle the
  // dynamic values (framesSetId, isRunning) without needing re-subscription.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Start a registration run for the frame set. Optionally supply a specific
   *  reference frame; if omitted the backend picks the best one. */
  const start = useCallback(
    async (referenceFrameId?: number) => {
      if (isRunningRef.current) return;

      isRunningRef.current = true;
      setRunState({
        framesSetId,
        isRunning: true,
        isCancelling: false,
        progress: null,
        frameStatuses: new Map(),
        isComplete: false,
      });

      try {
        await api.invoke<void>('register_frame_set', {
          framesSetId,
          ...(referenceFrameId != null ? { referenceFrameId } : {}),
        });
      } catch (err) {
        console.error('[useRegistrationQueue] register_frame_set failed:', err);
        isRunningRef.current = false;
        setRunState((prev) => ({
          ...prev,
          isRunning: false,
          isCancelling: false,
          isComplete: true,
        }));
      }
    },
    [framesSetId],
  );

  /** Cancel the in-progress registration run. */
  const cancel = useCallback(async () => {
    if (!isRunningRef.current) return;
    setRunState((prev) => ({ ...prev, isCancelling: true }));
    try {
      await api.invoke<void>('cancel_frame_set_registration', { framesSetId });
    } catch (err) {
      // May fail if the run already finished — safe to ignore.
      console.error('[useRegistrationQueue] cancel failed:', err);
    }
  }, [framesSetId]);

  return { runState, start, cancel };
}
