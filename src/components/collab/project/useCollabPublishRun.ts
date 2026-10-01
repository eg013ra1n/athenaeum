import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../../api';
import { useNotifications } from '../../../contexts/NotificationContext';
import type {
  CollabPublishFinished,
  CollabPublishProgress,
  CollabPublishRunView,
  PublishStage,
} from '../../../types/models';

/** Spec 2026-10-01 §5: the order a run's stages can reach. */
export const STEP_ORDER: PublishStage[] = ['queued', 'calibrating', 'seeding', 'announcing', 'versions'];
export function stepIndex(stage: PublishStage): number {
  return STEP_ORDER.indexOf(stage);
}

export interface PublishRunState {
  running: CollabPublishProgress | null;
  last: CollabPublishFinished | null;
  /** Highest step reached by the running run (plan F3); -1 when idle. */
  reached: number;
  cancel: () => Promise<void>;
  cancelBusy: boolean;
}

const IDLE_REACH = { runId: null, step: -1 } as const;

/** The newer of two finished runs by `finishedAt` (`a` on a tie). */
function newerFinished(a: CollabPublishFinished | null, b: CollabPublishFinished | null): CollabPublishFinished | null {
  if (!a || !b) return a ?? b;
  return Date.parse(b.finishedAt) > Date.parse(a.finishedAt) ? b : a;
}

/**
 * The project's publish-family run: `collab-publish-progress` /
 * `collab-publish-finished` for this project, then the snapshot (a page
 * opened mid-run shows it at once). One source for the My frames run panel
 * and the Project settings status.
 *
 * Both listeners are registered BEFORE the snapshot is read, so no event can
 * fall between the read and the subscription. The reply can still be older
 * than an event heard while it was in flight: a `running` whose run already
 * finished is ignored, a `running: null` keeps a run whose progress was heard
 * and has not finished (it was queued after the reply was computed), and
 * `last` keeps the newer of the two.
 */
export function useCollabPublishRun(projectId: string | undefined): PublishRunState {
  const { notify } = useNotifications();
  const [running, setRunning] = useState<CollabPublishProgress | null>(null);
  const [last, setLast] = useState<CollabPublishFinished | null>(null);
  const [cancelBusy, setCancelBusy] = useState(false);

  // Run id and reached step live in one state so the "same run" comparison
  // never reads a stale closure and no updater has side effects.
  const [reach, setReach] = useState<{ runId: string | null; step: number }>(IDLE_REACH);

  // A different project starts from nothing, before its own snapshot: the
  // previous project's run never shows under the new one.
  const [shownFor, setShownFor] = useState(projectId);
  if (shownFor !== projectId) {
    setShownFor(projectId);
    setRunning(null);
    setLast(null);
    setReach(IDLE_REACH);
  }

  // The last run id heard in a `collab-publish-finished` (this subscription).
  const finishedRunRef = useRef<string | null>(null);
  // The last run id heard in a `collab-publish-progress` (this subscription).
  const progressRunRef = useRef<string | null>(null);

  const applyProgress = useCallback((p: CollabPublishProgress) => {
    setRunning(p);
    setReach((prev) => ({
      runId: p.publishRunId,
      step: prev.runId === p.publishRunId ? Math.max(prev.step, stepIndex(p.stage)) : stepIndex(p.stage),
    }));
  }, []);

  useEffect(() => {
    if (!projectId) return undefined;
    let cancelled = false;
    finishedRunRef.current = null;
    progressRunRef.current = null;
    const offs: Array<() => void> = [];
    // Never rejects: a failed listen is logged and the snapshot still reads.
    const sub = <T,>(name: string, handle: (p: T) => void): Promise<void> =>
      api
        .listen<T>(name, (p) => {
          if (!cancelled) handle(p);
        })
        .then((fn) => {
          if (cancelled) fn();
          else offs.push(fn);
        })
        .catch((err) => console.error(`[publish-run] listen ${name} failed:`, err));
    const subs = [
      sub<CollabPublishProgress>('collab-publish-progress', (p) => {
        if (p.projectId !== projectId) return;
        progressRunRef.current = p.publishRunId;
        applyProgress(p);
      }),
      sub<CollabPublishFinished>('collab-publish-finished', (f) => {
        if (f.projectId !== projectId) return;
        finishedRunRef.current = f.publishRunId;
        // Core drops a run from its list before emitting its finished event,
        // so a newer run's progress can arrive first: clear only our own.
        setRunning((cur) => (cur === null || cur.publishRunId === f.publishRunId ? null : cur));
        setReach((cur) => (cur.runId === f.publishRunId || cur.runId === null ? IDLE_REACH : cur));
        setLast(f);
      }),
    ];
    void Promise.all(subs)
      .then(() => {
        if (cancelled) return undefined;
        return api.invoke<CollabPublishRunView>('get_collab_publish_run', { projectId }).then((v) => {
          if (cancelled || !v) return;
          if (!v.running) {
            // A run heard while the read was in flight and not finished since
            // started after the reply was computed: it stays.
            const heard = progressRunRef.current;
            if (heard === null || heard === finishedRunRef.current) {
              setRunning(null);
              setReach(IDLE_REACH);
            }
          } else if (v.running.publishRunId !== finishedRunRef.current) {
            applyProgress(v.running);
          }
          setLast((prev) => newerFinished(prev, v.last));
        });
      })
      .catch((err) => console.error('[publish-run] snapshot failed:', err));
    return () => {
      cancelled = true;
      offs.forEach((fn) => fn());
    };
  }, [projectId, applyProgress]);

  const cancel = useCallback(async () => {
    if (!projectId) return;
    setCancelBusy(true);
    try {
      await api.invoke('cancel_collab_publish', { projectId });
    } catch (err) {
      console.error('[publish-run] cancel failed:', err);
      notify({
        title: 'Could not cancel the run',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setCancelBusy(false);
    }
  }, [projectId, notify]);

  return { running, last, reached: reach.step, cancel, cancelBusy };
}
