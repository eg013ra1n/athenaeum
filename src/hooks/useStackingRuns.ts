import { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type {
  MasterLightKind,
  Stage,
  StackingConfig,
  StackingCompleteEvent,
  StackingMasterRef,
  StackingProgressEvent,
  StartedStacking,
} from '../types/stacking';
import { useNotifications } from '../contexts/NotificationContext';

export interface RunProgress {
  runId: number;
  setId: number;
  stage: Stage;
  groupKey: string | null;
  current: number;
  total: number;
  percent: number;
  bytesDone: number;
  bytesTotal: number;
  frameId: number | null;
  message: string | null;
  /** `Date.now()` at the first `stacking-progress` event seen for this run. */
  startedAt: number;
}

export interface RunOutcome {
  runId: number;
  setId: number;
  success: boolean;
  cancelled: boolean;
  error: string | null;
  warnings: string[];
  masters: StackingMasterRef[];
  finishedAt: number;
}

/** Local `basename()` — kept duplicated rather than shared for one line,
 *  same convention as `CreateMasterDialog.tsx`/`components/folders/format.ts`. */
function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/**
 * Tracks in-flight stacking runs by frame-set id. The backend (ComputeQueue,
 * Plan 5a) owns admission and progress — this hook holds no frontend FIFO,
 * mirroring `useMasterBuilds`'s listener discipline exactly: fire-and-forget
 * `startRun`/`cancelRun`, all state transitions arrive via
 * `stacking-progress` / `stacking-complete` events.
 *
 * `progress` holds the LAST progress event per set and is cleared the
 * instant that set's run finishes (success, cancel, or failure) — so
 * `isRunning(setId)` (`progress.has(setId)`) and `stageSummary.ts`'s
 * `rowState` both read "an entry in `progress`" as "a run is active right
 * now" and fall back to `lastOutcome` once it is gone.
 */
export function useStackingRuns() {
  const [progress, setProgress] = useState<Map<number, RunProgress>>(new Map());
  const [lastOutcome, setLastOutcome] = useState<Map<number, RunOutcome>>(new Map());
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenComplete: (() => void) | undefined;

    api
      .listen<StackingProgressEvent>('stacking-progress', (payload) => {
        if (cancelled) return;
        setProgress((prev) => {
          const next = new Map(prev);
          const existing = prev.get(payload.setId);
          const startedAt =
            existing && existing.runId === payload.runId ? existing.startedAt : Date.now();
          next.set(payload.setId, {
            runId: payload.runId,
            setId: payload.setId,
            stage: payload.stage,
            groupKey: payload.groupKey,
            current: payload.current,
            total: payload.total,
            percent: payload.percent,
            bytesDone: payload.bytesDone,
            bytesTotal: payload.bytesTotal,
            frameId: payload.frameId,
            message: payload.message,
            startedAt,
          });
          return next;
        });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[useStackingRuns] listen failed:', err));

    api
      .listen<StackingCompleteEvent>('stacking-complete', (payload) => {
        if (cancelled) return;
        const finishedAt = Date.now();

        setLastOutcome((prev) => {
          const next = new Map(prev);
          next.set(payload.setId, {
            runId: payload.runId,
            setId: payload.setId,
            success: payload.success,
            cancelled: payload.cancelled,
            error: payload.error,
            warnings: payload.warnings,
            masters: payload.masters,
            finishedAt,
          });
          return next;
        });

        // Only clear the progress entry this completion actually belongs to
        // — a stale/duplicate event for a superseded run must not erase a
        // newer run's live progress.
        setProgress((prev) => {
          const existing = prev.get(payload.setId);
          if (!existing || existing.runId !== payload.runId) return prev;
          const next = new Map(prev);
          next.delete(payload.setId);
          return next;
        });

        // M3 Task 6: append " · drizzled" when any master this run produced
        // has a drizzle output — `StackingMasterRef.drizzlePath`, filled by
        // M3 Task 5.
        const anyDrizzled = payload.masters.some((m) => m.drizzlePath != null);
        notify({
          title: payload.success
            ? `Stacking finished — ${payload.masters.length} master(s)${anyDrizzled ? ' · drizzled' : ''}`
            : payload.cancelled
              ? 'Stacking cancelled'
              : 'Stacking failed',
          detail: payload.error ?? payload.masters.map((m) => basename(m.path)).join(', '),
          kind: 'stacking',
          hasErrors: !payload.success && !payload.cancelled,
          tone: payload.success ? 'success' : payload.cancelled ? 'info' : 'warning',
          dedupeKey: `stack-${payload.runId}`,
          link: `/objects/${payload.setId}?tab=stacking`,
        });

        if (payload.success) {
          window.dispatchEvent(new Event('library-updated'));
        }
      })
      .then((fn) => { if (cancelled) fn(); else unlistenComplete = fn; })
      .catch((err) => console.error('[useStackingRuns] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
    };
  }, [notify]);

  // Fix round 1, Minor #4: catch + log + notify + rethrow here (not just at
  // each call site) so every caller — including Task 3's Measure-panel
  // "Re-measure" button, which calls `startRun` through `StackingTab`'s
  // `handleRerunFrom` with no error handling of its own — gets a console
  // trace and a user-visible notification on a failed start/cancel for
  // free. Callers still see the rejection (for their own local state
  // cleanup, e.g. clearing an optimistic "starting" flag) but must not
  // ALSO notify — that would double the toast.
  const startRun = useCallback(
    async (setId: number, config?: StackingConfig, rerunFrom?: Stage): Promise<number> => {
      try {
        const result = await api.invoke<StartedStacking>('start_stacking', {
          setId,
          config,
          rerunFrom,
        });
        return result.runId;
      } catch (err) {
        console.error('[useStackingRuns] start_stacking failed:', err);
        notify({
          title: 'Failed to start stacking',
          detail: String(err),
          kind: 'stacking',
          hasErrors: true,
          tone: 'warning',
        });
        throw err;
      }
    },
    [notify],
  );

  const cancelRun = useCallback(async (runId: number): Promise<void> => {
    try {
      await api.invoke('cancel_stacking', { runId });
    } catch (err) {
      console.error('[useStackingRuns] cancel_stacking failed:', err);
      notify({
        title: 'Failed to cancel stacking',
        detail: String(err),
        kind: 'stacking',
        hasErrors: true,
        tone: 'warning',
      });
      throw err;
    }
  }, [notify]);

  const isRunning = useCallback((setId: number): boolean => progress.has(setId), [progress]);

  return { progress, lastOutcome, startRun, cancelRun, isRunning };
}

/** Normalize a JPEG payload from either backend — desktop's
 *  `tauri::ipc::Response` crosses the IPC boundary as an `ArrayBuffer`,
 *  `httpApi.invoke` already decodes an `image/*` response into a
 *  `Uint8Array`, and the `number[]` arm is what a JSON-serialized `Vec<u8>`
 *  looks like (an older backend). Same three-arm shape `BlinkViewer` uses
 *  for its own previews. */
function toJpegBytes(
  payload: Uint8Array<ArrayBuffer> | ArrayBuffer | number[],
): Uint8Array<ArrayBuffer> {
  if (payload instanceof Uint8Array) return payload;
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  return new Uint8Array(payload);
}

export interface MasterPreview {
  /** A blob URL, revoked when the hook unmounts or its arguments change. */
  url: string | null;
  loading: boolean;
  /** The backend's message — a run/output/file that is gone reads as a
   *  404-class `NotFound` and is shown, never swallowed. */
  error: string | null;
}

/** JPEG thumbnail for one master light a run wrote (M4d Task 3, ruling
 *  R-M4d-5). Fetches through `get_master_light_preview` on both targets
 *  (the web host answers the same command name with `image/jpeg`), turns
 *  the bytes into a blob URL and revokes it on unmount or when the
 *  arguments change.
 *
 *  StrictMode-safe by the cancelled-flag pattern: the object URL is created
 *  only if the effect is still live when the bytes arrive, so a
 *  double-mounted dev render never leaks one. */
export function useMasterPreview(
  runId: number | null,
  groupKey: string | null,
  kind: MasterLightKind,
  maxPx = 512,
): MasterPreview {
  const [url, setUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (runId == null || !groupKey) {
      setUrl(null);
      setError(null);
      setLoading(false);
      return;
    }

    let cancelled = false;
    let objectUrl: string | null = null;
    setLoading(true);
    setError(null);

    api
      .invoke<Uint8Array<ArrayBuffer> | ArrayBuffer | number[]>('get_master_light_preview', {
        runId,
        groupKey,
        kind,
        maxPx,
      })
      .then((payload) => {
        if (cancelled) return;
        const blob = new Blob([toJpegBytes(payload)], { type: 'image/jpeg' });
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('[useMasterPreview] get_master_light_preview failed:', err);
        setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
      setUrl(null);
    };
  }, [runId, groupKey, kind, maxPx]);

  return { url, loading, error };
}
