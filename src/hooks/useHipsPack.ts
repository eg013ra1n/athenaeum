// Offline HiPS pack for one survey: the selection, its debounced estimate,
// and the live run (status on mount + `hips-download-progress` events).
import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { HipsPackEstimate, PackProgress, PackSelection, PackState } from '../types/models';
import { useNotifications } from '../contexts/NotificationContext';

const ESTIMATE_DEBOUNCE_MS = 250;
const TERMINAL: PackState[] = ['done', 'cancelled', 'failed', 'interrupted'];

export function defaultPackSelection(surveyId: string): PackSelection {
  return { surveyId, skyOrder: 4, aroundLocations: true, aroundMessier: true, aroundNgcIc: false };
}

export interface HipsPack {
  selection: PackSelection;
  setSelection: (next: PackSelection) => void;
  estimate: HipsPackEstimate | null;
  progress: PackProgress | null;
  start: () => Promise<void>;
  cancel: () => Promise<void>;
}

export function useHipsPack(surveyId: string, onFinished?: () => void): HipsPack {
  const { notify } = useNotifications();
  const [selection, setSelection] = useState<PackSelection>(() => defaultPackSelection(surveyId));
  const [estimate, setEstimate] = useState<HipsPackEstimate | null>(null);
  const [progress, setProgress] = useState<PackProgress | null>(null);
  // The status has been read, so the selection is final enough to estimate.
  const [ready, setReady] = useState(false);
  // Bumped when a run ends so the estimate is read again.
  const [estimateTick, setEstimateTick] = useState(0);
  // Cancel was asked for; no `cancelling` event exists, so show it locally
  // until the terminal event or status arrives.
  const cancellingRef = useRef(false);
  // An event for this survey arrived since a status/start request was sent.
  // Such a response was computed before that event and must not overwrite it.
  const eventSeenRef = useRef(false);
  const onFinishedRef = useRef(onFinished);
  onFinishedRef.current = onFinished;
  const notifyRef = useRef(notify);
  notifyRef.current = notify;

  // Status on mount.
  useEffect(() => {
    let cancelled = false;
    setReady(false);
    eventSeenRef.current = false;
    api.invoke<PackProgress>('get_hips_pack_status', { surveyId })
      .then((status) => {
        if (cancelled) return;
        if (!eventSeenRef.current) setProgress(status);
        setSelection(status.selection ?? defaultPackSelection(surveyId));
        setReady(true);
      })
      .catch((err) => {
        console.error('[HipsPack] get_hips_pack_status failed:', err);
        if (cancelled) return;
        setSelection(defaultPackSelection(surveyId));
        setReady(true);
      });
    return () => { cancelled = true; };
  }, [surveyId]);

  // Debounced estimate; never touches the network on the backend.
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      api.invoke<HipsPackEstimate>('estimate_hips_pack', { selection })
        .then((e) => { if (!cancelled) setEstimate(e); })
        .catch((err) => console.error('[HipsPack] estimate_hips_pack failed:', err));
    }, ESTIMATE_DEBOUNCE_MS);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [ready, selection, estimateTick]);

  // Progress events (StrictMode-safe listener pattern). The payload has no
  // survey id of its own: it is read from the run's selection.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<PackProgress>('hips-download-progress', (payload) => {
      if (cancelled) return;
      if (payload.selection?.surveyId !== surveyId) return;
      eventSeenRef.current = true;
      const terminal = TERMINAL.includes(payload.state);
      if (terminal) cancellingRef.current = false;
      setProgress(cancellingRef.current && payload.state === 'running'
        ? { ...payload, state: 'cancelling' }
        : payload);
      if (terminal) {
        setEstimateTick((t) => t + 1);
        onFinishedRef.current?.();
        if (payload.state === 'failed') {
          notifyRef.current({
            kind: 'generic', tone: 'warning', title: 'Sky pack stopped', detail: payload.error ?? 'Unknown error',
          });
        }
      }
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[HipsPack] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [surveyId]);

  const start = useCallback(async () => {
    try {
      cancellingRef.current = false;
      eventSeenRef.current = false;
      const initial = await api.invoke<PackProgress>('start_hips_pack', { selection });
      // The run thread starts before this answer returns: a fast failure can
      // reach us first, and `initial` (running) would then rewind it.
      if (!eventSeenRef.current) setProgress(initial);
    } catch (err) {
      console.error('[HipsPack] start_hips_pack failed:', err);
      notifyRef.current({ kind: 'generic', tone: 'warning', title: 'Sky pack not started', detail: String(err) });
    }
  }, [selection]);

  const cancel = useCallback(async () => {
    cancellingRef.current = true;
    setProgress((p) => (p && p.state === 'running' ? { ...p, state: 'cancelling' } : p));
    try {
      await api.invoke<void>('cancel_hips_pack', {});
    } catch (err) {
      // The run already ended: its terminal event or the status says so.
      console.debug('[HipsPack] cancel_hips_pack after the run ended:', err);
      // The terminal event may have been missed: read where the run stands
      // so "Cancelling…" never outlives it.
      eventSeenRef.current = false;
      try {
        const status = await api.invoke<PackProgress>('get_hips_pack_status', { surveyId });
        if (!eventSeenRef.current) setProgress(status);
      } catch (statusErr) {
        console.error('[HipsPack] get_hips_pack_status after cancel failed:', statusErr);
        setProgress((p) => (p && p.state === 'cancelling' ? { ...p, state: 'running' } : p));
      }
      cancellingRef.current = false;
      setEstimateTick((t) => t + 1);
    }
  }, [surveyId]);

  return { selection, setSelection, estimate, progress, start, cancel };
}
