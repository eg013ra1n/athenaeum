import { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  PlateSolveProgressEvent,
  PlateSolveCompleteEvent,
  CatalogStatusInfo,
} from '../types/helpers';
import type { PlateSolveActivity, PlateSolveOrigin } from '../types/plate-solve';

export type FrameSolveStatus =
  | { kind: 'pending' }
  | { kind: 'solving' }
  | { kind: 'solved'; matched_stars: number; rms_arcsec: number }
  | { kind: 'failed'; error: string; code?: string; filename?: string };

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
  /** File name of the frame a worker most recently picked up. */
  currentFilename: string | null;
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

/** A plate solve the assistant started (a plan the user applied), as the panel and the sidebar show it. */
export interface AssistantSolveRun {
  runId: string | null;
  done: number;
  total: number;
  currentFile: string | null;
  isCancelling: boolean;
}

/** How often a batch held back by a running solve asks the activity again, in case the complete event was lost. */
const BUSY_REREAD_MS = 3000;

/** The refusal's words, display only: the decision is the activity's `running`, never this text. */
function busyDetail(a: PlateSolveActivity): string {
  const counts = `${a.done} of ${a.total} frames`;
  return a.origin === 'assistant'
    ? `A plate solve started from the assistant is running (${counts}). Wait for it or cancel it.`
    : `A plate solve batch is already running (${counts}). Wait for it or cancel it.`;
}

function assistantRunOf(a: PlateSolveActivity | null): AssistantSolveRun | null {
  if (!a || !a.running || a.origin !== 'assistant') return null;
  return { runId: a.runId, done: a.done, total: a.total, currentFile: a.currentFile, isCancelling: false };
}

/** A batch cancelled before it started: it ends now, solved nothing, and nothing waits for it. */
function endedUnstarted(entry: ActivePlateSolveBatch): ActivePlateSolveBatch {
  return {
    ...entry,
    isComplete: true,
    isCancelling: false,
    summary: { solved: 0, failed: 0, total: entry.frameIds.length, total_time_ms: 0 },
  };
}

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
  const notifyRef = useRef(notify);
  notifyRef.current = notify;

  // The plate solve the assistant started, if one runs (get_plate_solve_activity, then the events).
  const [assistantRun, setAssistantRunState] = useState<AssistantSolveRun | null>(null);
  // The same value, written at once: the event listeners (registered once) and the activity reads decide on it.
  const assistantRunRef = useRef<AssistantSolveRun | null>(null);
  const setAssistantRun = useCallback((update: (prev: AssistantSolveRun | null) => AssistantSolveRun | null) => {
    const next = update(assistantRunRef.current);
    assistantRunRef.current = next;
    setAssistantRunState(next);
  }, []);
  // Bumped when a run starts, so a read that began earlier cannot hide it.
  const assistantEpochRef = useRef(0);
  // Runs whose end was already told (an end comes from the event and from a re-read, never twice).
  const toldRunsRef = useRef<Set<string>>(new Set());
  // The head batch waits because another run holds the one slot; it stays queued and starts when the slot is free.
  const [held, setHeldState] = useState(false);
  const heldRef = useRef(false);
  const setHeld = useCallback((v: boolean) => {
    heldRef.current = v;
    setHeldState(v);
  }, []);
  const runNextRef = useRef<() => Promise<void>>(async () => {});

  // Once we've confirmed the star catalog is present, skip the precheck for
  // subsequent batches in the same session — the check is cheap but pointless
  // to repeat once we know it's there.
  const catalogVerifiedRef = useRef(false);

  // Backend only runs one plate-solve batch at a time (cancel handle key = 0),
  // so every incoming progress event belongs to the currently-running batch.
  const currentBatchIdRef = useRef<number | null>(null);
  // Resolves the running batch's wait for `plate-solve-complete`.
  const completeResolveRef = useRef<(() => void) | null>(null);

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

  /** The assistant's line ended: hide it and tell the outcome once (the event's counts, or the line's own when the event was lost). */
  const endAssistantRun = useCallback(
    (payload?: PlateSolveCompleteEvent) => {
      const run = assistantRunRef.current;
      if (!run) return;
      setAssistantRun(() => null);
      const key = `plate-solve-done-${run.runId ?? 'unknown'}`;
      if (toldRunsRef.current.has(key)) return;
      toldRunsRef.current.add(key);
      if (payload) {
        notifyRef.current({
          title: `Plate-solve finished — ${payload.solved}/${payload.total} solved`,
          detail: payload.failed ? `${payload.failed} failed` : 'All frames solved',
          kind: 'platesolve',
          hasErrors: payload.failed > 0,
          tone: payload.failed > 0 ? 'warning' : 'success',
          dedupeKey: key,
        });
      } else {
        notifyRef.current({
          title: 'Plate solve from the assistant ended',
          detail: `${run.done} of ${run.total} frames were done when it ended`,
          kind: 'platesolve',
          tone: 'info',
          dedupeKey: key,
        });
      }
    },
    [setAssistantRun],
  );

  /** The slot's holder, read from the backend; `null` when it could not be read (the backend then decides).
   *  Every read also moves the assistant's line: it follows the activity, whatever events arrived. */
  const readActivity = useCallback(async (): Promise<PlateSolveActivity | null> => {
    const epoch = assistantEpochRef.current;
    let a: PlateSolveActivity | null;
    try {
      a = await api.invoke<PlateSolveActivity | null>('get_plate_solve_activity');
    } catch (err) {
      console.error('[usePlateSolveQueue] get_plate_solve_activity failed:', err);
      return null;
    }
    const run = assistantRunOf(a);
    if (run) {
      // Another run than the one the line shows: the shown one ended (told), the new one takes the line.
      if (assistantRunRef.current && assistantRunRef.current.runId !== run.runId) endAssistantRun();
      setAssistantRun((prev) =>
        prev && prev.runId === run.runId
          ? {
              ...prev,
              // The counter never steps back; the file goes with the newer count.
              done: Math.max(prev.done, run.done),
              total: run.total,
              currentFile: run.done >= prev.done ? run.currentFile ?? prev.currentFile : prev.currentFile,
            }
          : run,
      );
    } else if (a && epoch === assistantEpochRef.current) {
      // Nothing of the assistant's runs now (a run that started after this read began is newer than it).
      endAssistantRun();
    }
    return a;
  }, [setAssistantRun, endAssistantRun]);

  /** Another run holds the slot: say so once per wait, keep the batch queued, and let a re-read or the complete event retry. */
  const holdBatch = useCallback(
    (activity: PlateSolveActivity) => {
      processingRef.current = false;
      currentBatchIdRef.current = null;
      completeResolveRef.current = null;
      if (!heldRef.current) {
        setHeld(true);
        notifyRef.current({
          title: 'Plate solve not started',
          detail: busyDetail(activity),
          tone: 'warning',
          kind: 'platesolve',
          // One refusal per holder run: the notification centre keeps a seen key for good.
          dedupeKey: `plate-solve-busy-${activity.runId ?? 'unknown'}`,
        });
      }
    },
    [setHeld],
  );

  const runNext = useCallback(async () => {
    if (processingRef.current) return;
    const next = queueRef.current[0];
    if (!next) {
      if (heldRef.current) setHeld(false);
      return;
    }

    processingRef.current = true;

    // The slot decides, not an error text: while any run holds it, the batch stays queued.
    const activity = await readActivity();
    if (activity?.running) {
      holdBatch(activity);
      return;
    }
    if (heldRef.current) setHeld(false);
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
          setQueue(q => q.slice(1));
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
        setQueue(q => q.slice(1));
        return;
      }
    }

    // The desktop command returns once the batch is done; the web route returns
    // at once and solves in the background. Both end the batch with
    // `plate-solve-complete`, so the queue advances on that event, not on the
    // response — advancing early would drop every later progress event.
    const completed = new Promise<void>((resolve) => {
      completeResolveRef.current = resolve;
    });
    try {
      await api.invoke<void>('plate_solve_batch', { frameIds: next.frameIds });
      await completed;
    } catch (err) {
      console.error(`Plate solve batch ${next.batchId} failed:`, err);
      // A rejection is read through the activity: a run that holds the slot is the reason, whatever the wording.
      const after = await readActivity();
      if (after?.running) {
        holdBatch(after);
        return;
      }
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

    completeResolveRef.current = null;
    processingRef.current = false;
    currentBatchIdRef.current = null;
    setQueue(q => q.slice(1));
  }, [failBatch, readActivity, holdBatch, setHeld]);
  runNextRef.current = runNext;

  // A held batch asks again now and then: the complete event of the run that holds the slot can be lost.
  useEffect(() => {
    if (!held) return;
    const timer = setInterval(() => void runNextRef.current(), BUSY_REREAD_MS);
    return () => clearInterval(timer);
  }, [held]);

  // The assistant's run, if one is already running when the app starts.
  useEffect(() => {
    void readActivity();
  }, [readActivity]);

  // While the assistant's line shows, the activity is read again now and then: the complete event can be lost (web).
  const assistantShown = assistantRun !== null;
  useEffect(() => {
    if (!assistantShown) return;
    const timer = setInterval(() => void readActivity(), BUSY_REREAD_MS);
    return () => clearInterval(timer);
  }, [assistantShown, readActivity]);

  // Kick the processor whenever the queue has pending items.
  useEffect(() => {
    if (queue.length > 0 && !processingRef.current) {
      runNext();
    } else if (queue.length === 0 && heldRef.current) {
      // The held batch was cancelled: nothing waits for the slot any more.
      setHeld(false);
    }
  }, [queue, runNext, setHeld]);

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
          if (batchId == null) {
            // Not the queue's batch: the assistant's run, when one is on screen. The counter never steps back.
            setAssistantRun(prev =>
              prev
                ? {
                    ...prev,
                    done: Math.max(prev.done, payload.current),
                    total: payload.total,
                    currentFile:
                      payload.status === 'solving' ? payload.filename ?? prev.currentFile : prev.currentFile,
                  }
                : prev,
            );
            return;
          }

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
                code: payload.failure_code,
                filename: payload.filename,
              });
            }
            // Workers run in parallel and a "solving" event carries the count
            // read before it started, so it can arrive after a newer "solved"
            // one: never let the counter step back.
            const current = Math.max(entry.progress?.current ?? 0, payload.current);
            const updated = new Map(prev);
            updated.set(batchId, {
              ...entry,
              progress: { current, total: payload.total },
              currentFilename:
                payload.status === 'solving'
                  ? payload.filename ?? entry.currentFilename
                  : entry.currentFilename,
              frameStatuses,
            });
            return updated;
          });
        },
      )
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[usePlateSolveQueue] listen failed:', err));

    let unlistenStarted: (() => void) | null = null;
    api
      .listen<{ origin: PlateSolveOrigin; runId: string; total: number }>(
        'plate-solve-started',
        (payload) => {
          if (cancelled) return;
          if (payload.origin === 'assistant') {
            assistantEpochRef.current += 1;
            setAssistantRun(() => ({
              runId: payload.runId,
              done: 0,
              total: payload.total,
              currentFile: null,
              isCancelling: false,
            }));
          }
        },
      )
      .then((fn) => { if (cancelled) fn(); else unlistenStarted = fn; })
      .catch((err) => console.error('[usePlateSolveQueue] listen failed:', err));

    api
      .listen<PlateSolveCompleteEvent>(
        'plate-solve-complete',
        (payload) => {
          if (cancelled) return;
          const batchId = currentBatchIdRef.current;
          if (batchId == null) {
            // Not the queue's batch: the assistant's run ended (told once), and the slot is free, so a held batch starts.
            endAssistantRun(payload);
            if (heldRef.current) void runNextRef.current();
            return;
          }
          // The queue's own batch ended. The event carries no run id, so a line the assistant's run still has
          // is left to the next activity read.
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

          notifyRef.current({
            title: `Plate-solve finished — ${payload.solved}/${payload.total} solved`,
            detail: payload.failed
              ? `${payload.failed} failed`
              : 'All frames solved',
            kind: 'platesolve',
            hasErrors: payload.failed > 0,
            tone: payload.failed > 0 ? 'warning' : 'success',
          });
          completeResolveRef.current?.();
        },
      )
      .then((fn) => { if (cancelled) fn(); else unlistenComplete = fn; })
      .catch((err) => console.error('[usePlateSolveQueue] listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenComplete?.();
      unlistenStarted?.();
    };
  }, [setAssistantRun, endAssistantRun]);

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
          currentFilename: null,
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
    const isCurrent = currentBatchIdRef.current === batchId;
    setQueue(q => q.filter(item => item.batchId !== batchId));
    setActiveBatches(prev => {
      const entry = prev.get(batchId);
      if (!entry || entry.isComplete) return prev;
      const updated = new Map(prev);
      // The running batch ends with its complete event; a held or queued one never started, so it ends now.
      updated.set(batchId, isCurrent ? { ...entry, isCancelling: true } : endedUnstarted(entry));
      return updated;
    });
    if (isCurrent) {
      try {
        await api.invoke('cancel_plate_solve');
      } catch {
        // May fail if the batch already finished — safe to ignore.
      }
    }
  }, []);

  /** Cancel the assistant's run (the same command as the queue's own cancel: one slot, one cancel handle).
   *  The slot is read first: only the run the line shows is cancelled, never a batch that took the slot since. */
  const cancelAssistantRun = useCallback(async () => {
    const shown = assistantRunRef.current;
    if (!shown) return;
    const a = await readActivity();
    if (a === null) {
      // Unreadable (already logged): cancelling blind could stop another run.
      notifyRef.current({
        title: 'Could not cancel the plate solve',
        detail: 'Athenaeum could not tell which plate solve is running.',
        tone: 'warning',
        hasErrors: true,
        kind: 'platesolve',
      });
      return;
    }
    if (!(a.running && a.origin === 'assistant' && a.runId === shown.runId)) return;
    setAssistantRun(prev => (prev ? { ...prev, isCancelling: true } : prev));
    try {
      await api.invoke('cancel_plate_solve');
    } catch (err) {
      console.error('[usePlateSolveQueue] cancel_plate_solve failed:', err);
      setAssistantRun(prev => (prev ? { ...prev, isCancelling: false } : prev));
      notifyRef.current({
        title: 'Could not cancel the plate solve',
        detail: String(err),
        tone: 'warning',
        hasErrors: true,
        kind: 'platesolve',
      });
    }
  }, [readActivity, setAssistantRun]);

  const cancelAll = useCallback(async () => {
    setQueue([]);
    if (assistantRunRef.current) void cancelAssistantRun();
    const currentId = currentBatchIdRef.current;
    if (currentId != null) {
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
          // The running batch ends with its complete event; a held or queued one never started.
          updated.set(entry.batchId, entry.batchId === currentId ? { ...entry, isCancelling: true } : endedUnstarted(entry));
        }
      }
      return updated;
    });
  }, [cancelAssistantRun]);

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
    cancelAssistantRun,
    dismissCompleted,
    dismissPrecheckError,
    // state
    activeBatches,
    assistantRun,
    currentBatch,
    queueLength,
    hasActiveBatches,
    precheckError,
    // helpers
    getFrameStatus,
  };
}
