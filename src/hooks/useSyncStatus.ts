// Personal-sync status hook (task M3): the single always-mounted consumer of
// `get_sync_status` + the `sync-progress`/`sync-finished` event stream.
//
// Mounted ONCE via `TransfersProvider` so it is the sole place discrete-outcome
// notifications fire (no double toasts) and the sole poller. Polling is gated on
// `visible` — a hidden indicator (no sender/receiver activity, no dev flag) does
// no periodic work; an inbound/outbound event can still flip visibility via the
// push listener, which starts the interval.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';
import type { OutboundSummary, ScanRoot, SyncFinishedEvent, SyncStatus } from '../types/models';

/** Status re-poll cadence while the indicator is visible. */
const POLL_MS = 10_000;

/** Leading chars of a node-id hex / device id, enough to disambiguate. */
function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

/**
 * Raise the right discrete-outcome notification for one `sync-finished` event.
 * `direction` routes send-side ("package delivered" / "transfer failed") vs
 * receive-side ("N frames arrived"). Dedupe keys are prefixed per direction so a
 * sender row id can never collide with a receiver package id.
 *
 * A project-tagged event (`projectId != null`, collab exchange slice 4/5) takes
 * the project branch below and returns before ANY personal-sync path — so the
 * sync-incoming nudge never fires for a project transfer, and personal sync
 * (projectId == null) is byte-identical to before this branch existed.
 */
export function notifyFinished(p: SyncFinishedEvent, notify: NotifyLike): void {
  if (p.projectId != null) {
    const link = `/projects/${p.projectId}`;
    if (p.direction === 'sent') {
      if (p.outcome === 'confirmed') {
        notify({
          title: 'Contribution replicated',
          detail: `${p.okCount} frame${p.okCount === 1 ? '' : 's'} delivered — safe to go offline`,
          kind: 'project',
          tone: 'success',
          link,
          dedupeKey: `collab-sent-${p.packageId}`,
        });
      } else if (p.outcome.startsWith('failed')) {
        // Reuse the personal-sync reason-extraction idiom (below): bare
        // `failed` gets a generic reason, `failed: <msg>` carries the hub's.
        const reason =
          p.outcome === 'failed'
            ? 'Retries exhausted or the peer was unreachable.'
            : p.outcome.replace(/^failed:\s*/, '');
        notify({
          title: 'Contribution transfer failed',
          detail: reason,
          kind: 'project',
          tone: 'warning',
          hasErrors: true,
          link,
          dedupeKey: `collab-sent-${p.packageId}`,
        });
      }
      // 'cancelled' is user-initiated — no notification (same as personal).
      return;
    }

    // Receive side (coordinator/member pulled a project package).
    if (p.okCount > 0) {
      const partial = p.outcome === 'partial';
      notify({
        title: 'Project package downloaded',
        detail: partial
          ? `${p.okCount} received, ${p.failed.length} rejected`
          : `${p.okCount} frame${p.okCount === 1 ? '' : 's'} received`,
        kind: 'project',
        tone: partial ? 'warning' : 'success',
        hasErrors: partial,
        link,
        dedupeKey: `collab-recv-${p.packageId}`,
      });
    } else if (p.outcome === 'failed') {
      notify({
        title: 'Project package download failed',
        detail: `${p.failed.length} frame${p.failed.length === 1 ? '' : 's'} failed the integrity check`,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
        link,
        dedupeKey: `collab-recv-${p.packageId}`,
      });
    }
    return;
  }

  if (p.direction === 'sent') {
    if (p.outcome === 'confirmed') {
      notify({
        title: 'Package delivered',
        detail: `${p.okCount} frame${p.okCount === 1 ? '' : 's'} delivered to ${shortPeer(p.peerDevice)}`,
        kind: 'sync',
        tone: 'success',
        dedupeKey: `sync-sent-${p.packageId}`,
      });
    } else if (p.outcome.startsWith('failed')) {
      const reason =
        p.outcome === 'failed'
          ? 'Retries exhausted or the peer was unreachable.'
          : p.outcome.replace(/^failed:\s*/, '');
      notify({
        title: 'Transfer failed',
        detail: reason,
        kind: 'sync',
        tone: 'warning',
        hasErrors: true,
        dedupeKey: `sync-sent-${p.packageId}`,
      });
    }
    // 'cancelled' is user-initiated — no notification.
    return;
  }

  // Receive side. `replayed` re-acks an already-received package; its dedupeKey
  // matches the original arrival, so the duplicate toast is suppressed.
  if (p.okCount > 0) {
    notify({
      title: `${p.okCount} frame${p.okCount === 1 ? '' : 's'} arrived from ${shortPeer(p.peerDevice)}`,
      detail:
        p.outcome === 'partial'
          ? `${p.failed.length} frame${p.failed.length === 1 ? '' : 's'} rejected`
          : 'Received from your paired device.',
      kind: 'sync',
      tone: p.outcome === 'partial' ? 'warning' : 'success',
      hasErrors: p.outcome === 'partial',
      dedupeKey: `sync-recv-${p.packageId}`,
    });

    // Unconfigured-landing hint (Stage 1.5, Task 6; audit UX-1): when no
    // sync-incoming folder is designated, received files fall back to the
    // app-data folder. Nudge PER received batch — the dedupeKey is keyed on the
    // package id so it fires again for each arrival (the old permanent
    // `sync-incoming-unconfigured` key fired once, ever), while still
    // suppressing duplicate/replay events for the same package. The standing
    // strip on `/transfers` is the always-visible counterpart. Fire-and-forget:
    // `notify` is stable, the scan-root read is cheap, and a failure here must
    // never derail the arrival notification above.
    if (p.outcome === 'ingested' || p.outcome === 'partial') {
      void (async () => {
        try {
          const roots = await api.invoke<ScanRoot[]>('get_scan_roots');
          if (!roots.some((r) => r.kind === 'sync_incoming')) {
            notify({
              title: 'Received files are landing in the app data folder',
              detail:
                'Designate a Sync Incoming Folder in File Manager to keep them with your image library.',
              kind: 'sync',
              tone: 'warning',
              link: '/files',
              dedupeKey: `sync-incoming-unconfigured-${p.packageId}`,
            });
          }
        } catch (err) {
          console.error('[useSyncStatus] sync-incoming hint scan-root check failed:', err);
        }
      })();
    }
  } else if (p.outcome === 'failed') {
    notify({
      title: `Frames rejected from ${shortPeer(p.peerDevice)}`,
      detail: `${p.failed.length} frame${p.failed.length === 1 ? '' : 's'} failed the integrity check`,
      kind: 'sync',
      tone: 'warning',
      hasErrors: true,
      dedupeKey: `sync-recv-${p.packageId}`,
    });
  }
}

export interface UseSyncStatus {
  /** Latest full snapshot, or `null` before the first poll resolves. */
  status: SyncStatus | null;
  /** In-flight outbound rows for the Active tab (`status.sender.active`). */
  active: OutboundSummary[];
  /** Whether the sidebar indicator should render (sender/receiver present OR dev flag). */
  visible: boolean;
  /** Re-poll `get_sync_status` now. */
  refresh: () => void;
}

export function useSyncStatus(): UseSyncStatus {
  const { notify } = useNotifications();
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback(() => {
    api
      .invoke<SyncStatus>('get_sync_status')
      .then((s) => {
        if (mounted.current) setStatus(s);
      })
      .catch((err) => console.error('[useSyncStatus] get_sync_status failed:', err));
  }, []);

  // Initial fetch + push subscription (always mounted, StrictMode-safe).
  useEffect(() => {
    mounted.current = true;
    refresh();
    let cancelled = false;
    let unlistenFinished: (() => void) | undefined;
    let unlistenProgress: (() => void) | undefined;

    api
      .listen<SyncFinishedEvent>('sync-finished', (p) => {
        if (cancelled) return;
        notifyFinished(p, notify);
        refresh();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFinished = fn;
      })
      .catch((err) => console.error('[useSyncStatus] sync-finished listen failed:', err));

    // Progress ticks only nudge a re-poll (the snapshot is the source of truth);
    // they never notify — that would be per-transition spam.
    api
      .listen<unknown>('sync-progress', () => {
        if (!cancelled) refresh();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenProgress = fn;
      })
      .catch((err) => console.error('[useSyncStatus] sync-progress listen failed:', err));

    return () => {
      cancelled = true;
      mounted.current = false;
      unlistenFinished?.();
      unlistenProgress?.();
    };
  }, [refresh, notify]);

  const visible =
    !!status &&
    (status.transportStarted || status.sender.started || status.devPairingEnabled);

  // Periodic re-poll ONLY while visible — a hidden indicator does no polling.
  useEffect(() => {
    if (!visible) return;
    const id = setInterval(refresh, POLL_MS);
    return () => clearInterval(id);
  }, [visible, refresh]);

  return { status, active: status?.sender.active ?? [], visible, refresh };
}
