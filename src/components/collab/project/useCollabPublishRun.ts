import { useCallback, useEffect, useState } from 'react';
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

/**
 * The project's publish-family run: the snapshot on mount (a page opened
 * mid-run shows it at once), then `collab-publish-progress` /
 * `collab-publish-finished` for this project. One source for the My frames
 * run panel and the Project settings status.
 */
export function useCollabPublishRun(projectId: string | undefined): PublishRunState {
  const { notify } = useNotifications();
  const [running, setRunning] = useState<CollabPublishProgress | null>(null);
  const [last, setLast] = useState<CollabPublishFinished | null>(null);
  const [cancelBusy, setCancelBusy] = useState(false);

  // Run id and reached step live in one state so the "same run" comparison
  // never reads a stale closure and no updater has side effects.
  const [reach, setReach] = useState<{ runId: string | null; step: number }>({ runId: null, step: -1 });

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
    api
      .invoke<CollabPublishRunView>('get_collab_publish_run', { projectId })
      .then((v) => {
        if (cancelled || !v) return;
        if (v.running) applyProgress(v.running);
        setLast(v.last);
      })
      .catch((err) => console.error('[publish-run] snapshot failed:', err));
    return () => {
      cancelled = true;
    };
  }, [projectId, applyProgress]);

  useEffect(() => {
    if (!projectId) return undefined;
    let cancelled = false;
    const offs: Array<() => void> = [];
    const sub = <T,>(name: string, handle: (p: T) => void) =>
      api
        .listen<T>(name, (p) => {
          if (!cancelled) handle(p);
        })
        .then((fn) => {
          if (cancelled) fn();
          else offs.push(fn);
        })
        .catch((err) => console.error(`[publish-run] listen ${name} failed:`, err));
    void sub<CollabPublishProgress>('collab-publish-progress', (p) => {
      if (p.projectId === projectId) applyProgress(p);
    });
    void sub<CollabPublishFinished>('collab-publish-finished', (f) => {
      if (f.projectId !== projectId) return;
      setRunning(null);
      setReach({ runId: null, step: -1 });
      setLast(f);
    });
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
