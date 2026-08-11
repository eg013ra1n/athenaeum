import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  ContentIndexFinished,
  ContentIndexProgress,
  ContentIndexStatus,
} from '../types/models';

/**
 * Dedupe key for one terminal event.
 *
 * The key must survive a duplicated delivery of the SAME run (a leaked second
 * listener) without muting a later run that happens to have identical counts:
 * the dedupe set is persisted in localStorage, so a key built from the counts
 * alone would silence every future pass with the same outcome — including
 * every future failure, which is the one thing that must always be heard.
 * Second-granularity receipt time gives both: a duplicate delivery is
 * milliseconds apart and shares the bucket, a later run never does.
 */
function terminalDedupeKey(suffix: string): string {
  return `content-index-${suffix}-${new Date().toISOString().slice(0, 19)}`;
}

/** Status + manual start for the content index (Settings card). */
export function useContentIndex() {
  const [status, setStatus] = useState<ContentIndexStatus | null>(null);
  const [starting, setStarting] = useState(false);
  /** The last terminal event seen while this hook was mounted — the only
   * source for "the run left work behind, and repeating it will not help". */
  const [lastFinished, setLastFinished] = useState<ContentIndexFinished | null>(null);
  /** A pass that started elsewhere (boot autostart, post-scan re-arm) while the
   * card was already open. The progress events are used ONLY to flip this flag:
   * nothing renders their numbers today — the sidebar compute-queue card shows
   * a label, running/queued and a cancel button, and this card shows a running
   * state. A percentage or a bar is a deliberate follow-up, not an oversight. */
  const [runningLive, setRunningLive] = useState(false);

  const refresh = useCallback(() => {
    api.invoke<ContentIndexStatus>('get_content_index_status')
      .then(setStatus)
      .catch((err) => console.error('[useContentIndex] status failed:', err));
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<ContentIndexFinished>('content-index-finished', (payload) => {
      if (cancelled) return;
      setLastFinished(payload);
      setRunningLive(false);
      refresh();
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useContentIndex] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [refresh]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<ContentIndexProgress>('content-index-progress', () => {
      if (cancelled) return;
      setRunningLive(true);
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useContentIndex] progress listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  const start = useCallback(async () => {
    setStarting(true);
    try {
      const started = await api.invoke<boolean>('start_content_index');
      if (!started) {
        console.warn('[useContentIndex] a pass is already running; start ignored');
      }
      refresh();
    } catch (err) {
      console.error('[useContentIndex] start failed:', err);
    } finally {
      setStarting(false);
    }
  }, [refresh]);

  const running = (status?.running ?? false) || runningLive;

  return { status, lastFinished, running, refresh, start, starting };
}

/**
 * App-root listener that turns the terminal event into one notification.
 * Mounted once in Layout so it fires whatever page the user is on. Progress is
 * deliberately NOT notified — it is high-frequency UI data, and the sidebar
 * compute-queue card already shows the job running.
 */
export function useContentIndexNotifications() {
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<ContentIndexFinished>('content-index-finished', (payload) => {
      if (cancelled) return;
      // A failed pass must never read as a finished one — it is the only
      // terminal state the user cannot infer from the counts.
      if (payload.failed) {
        notify({
          title: 'Content index failed',
          detail: 'Could not read the catalog. See the log for details.',
          kind: 'sync',
          tone: 'warning',
          hasErrors: true,
          dedupeKey: terminalDedupeKey('failed'),
        });
        return;
      }
      if (payload.updated === 0 && !payload.cancelled) return; // nothing-to-do pass: stay quiet
      notify({
        title: payload.cancelled
          ? `Content index cancelled — ${payload.updated} indexed`
          : `Content index finished — ${payload.updated} files indexed`,
        // Same explanation as the Settings card's skipped-count note, in the
        // same order — one number explained two different ways is worse than
        // either explanation on its own.
        detail:
          payload.skipped > 0
            ? `${payload.skipped} skipped — offline storage, changed since the last scan, or archived into a ZIP`
            : '',
        kind: 'sync',
        tone: payload.cancelled ? 'warning' : 'success',
        dedupeKey: terminalDedupeKey(
          `${payload.updated}-${payload.skipped}-${payload.cancelled}`,
        ),
      });
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useContentIndexNotifications] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, [notify]);
}
