// `/transfers` page hook (Task 15) — merges the shared `useSyncStatus` poll
// snapshot (via `TransfersContext`) with two page-scoped live event streams
// (`sync-progress` for row-level bytes/speed, `sync-file-progress` for the
// expanded-row per-file overlay) into one unified, torrent-style Active list,
// plus the four row actions.
//
// Design note (terminal outbound rows / Resend): `SyncSenderStatus.active`
// NEVER includes a terminal (confirmed/failed/cancelled) row by construction
// (see `OutboundSummary`'s doc comment) — terminal packages are rolled into
// counts and land in history. But `retry_sync_package` needs the outbound
// row's durable i64 id, and history rows carry only a `packageId` STRING (the
// package dir basename) with no lookup back to that id. So a torrent-style
// "row stays visible with a Resend button after it fails/gets cancelled" view
// is only buildable from data this page already held: this hook snapshots
// every `OutboundSummary` it sees in `active` (`outboundSeenRef`), and on a
// `sync-finished` event with a non-confirmed outcome, promotes that snapshot
// (or a degraded fallback built from the event alone, if the row was never
// seen active — e.g. a near-instant failure) into a page-local terminal
// ledger. This ledger is intentionally session/page-scoped: it resets on
// unmount, same as any other purely-client bookkeeping. The full durable
// audit trail is the History tab (`list_sync_history`), which needs no such
// ledger since it has no retry affordance.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api';
import { useTransfers } from '../contexts/TransfersContext';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  OutboundSummary,
  SyncFileProgressEvent,
  SyncFinishedEvent,
  SyncProgressEvent,
} from '../types/models';

/** Leading chars of a node-id hex, enough to disambiguate (mirrors useSyncStatus/TransfersPanel). */
function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

function shortId(s: string): string {
  const t = s.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

/** EMA smoothing factor tuned for "~3 samples" per the approved design. */
const SPEED_EMA_ALPHA = 2 / (3 + 1);

interface LiveBytes {
  bytesDone: number;
  bytesTotal: number;
  /** Smoothed bytes/sec, `null` until at least two increasing samples arrive. */
  speedBps: number | null;
}

interface SpeedTrackerEntry {
  lastTs: number;
  lastBytes: number;
  ema: number | null;
}

/** Push one (time, bytes) sample into `store[key]`'s tracker and return the
 * live-bytes reading (merging the raw bytes with the freshly computed EMA). */
function trackBytes(
  store: Map<string, SpeedTrackerEntry>,
  key: string,
  bytesDone: number,
  bytesTotal: number,
): LiveBytes {
  const now = Date.now();
  const prev = store.get(key);
  let ema = prev?.ema ?? null;
  if (prev && bytesDone > prev.lastBytes && now > prev.lastTs) {
    const rate = ((bytesDone - prev.lastBytes) / (now - prev.lastTs)) * 1000;
    ema = ema == null ? rate : SPEED_EMA_ALPHA * rate + (1 - SPEED_EMA_ALPHA) * ema;
  }
  store.set(key, { lastTs: now, lastBytes: bytesDone, ema });
  return { bytesDone, bytesTotal, speedBps: ema };
}

export type TransferRowKind = 'outbound' | 'inbound';

/** One unified Active-tab row — an in-flight outbound/inbound package, or a
 * page-session-lingering terminal outbound package kept around for Resend. */
export interface TransferRow {
  key: string;
  kind: TransferRowKind;
  /** Outbound: `sync_outbound.id`. Inbound: `sync_inbound.id`. */
  id: number;
  /** Full wire package id — inbound only (needed by `cancel_incoming_package`); `null` for outbound. */
  packageId: string | null;
  packageShort: string;
  peerShort: string;
  fileCount: number;
  byteSize: number;
  bytesDone: number;
  state: string;
  /** Outbound-only; `0` for inbound rows (the field doesn't exist there). */
  attempts: number;
  lastError: string | null;
  nextRetryAt: string | null;
  /** `true` for a lingering failed/cancelled outbound row (Resend, not Cancel/Send-now). */
  terminal: boolean;
  /** Latest `sync-progress` `stage` seen for this outbound id (`sent` ticks only),
   * cleared on `sync-finished`. Outbound-only; `null` for inbound/terminal rows.
   * Drives the "uploaded — awaiting confirmation" post-upload/pre-ack label. */
  liveStage: string | null;
  speedBps: number | null;
  isTransferring: boolean;
  /** Bumped on every `sync-finished` for this package — an expanded row watches
   * this to know its cached `list_transfer_files` detail needs a re-fetch. */
  finishNonce: number;
}

export interface UseTransferQueue {
  rows: TransferRow[];
  activeCount: number;
  /** Live per-file bytes for the expanded-row overlay, keyed by full package id → file name. */
  liveFiles: Map<string, Map<string, { bytesDone: number; bytesTotal: number }>>;
  refresh: () => void;
  sendNow: (id: number) => void;
  cancelOutbound: (id: number) => void;
  cancelInbound: (packageId: string) => void;
  resend: (id: number) => void;
  /** Action keys currently in flight (`send:<id>` / `cancel:<id>` / `cancelin:<packageId>` / `resend:<id>`) — disable the triggering button while present. */
  busy: Set<string>;
}

export function useTransferQueue(): UseTransferQueue {
  const { status, refresh } = useTransfers();
  const { notify } = useNotifications();

  const outboundSeenRef = useRef<Map<number, OutboundSummary>>(new Map());
  const [terminalOutbound, setTerminalOutbound] = useState<
    Map<number, { summary: OutboundSummary; outcome: 'failed' | 'cancelled' }>
  >(new Map());

  const [liveOutboundBytes, setLiveOutboundBytes] = useState<Map<number, LiveBytes>>(new Map());
  const [liveInboundBytes, setLiveInboundBytes] = useState<Map<string, LiveBytes>>(new Map());
  // Latest send-side `sync-progress` stage per outbound id (Task 2.1). Drives the
  // "uploaded — awaiting confirmation" label; cleared on `sync-finished`.
  const [liveOutboundStage, setLiveOutboundStage] = useState<Map<number, string>>(new Map());
  const [liveFiles, setLiveFiles] = useState<
    Map<string, Map<string, { bytesDone: number; bytesTotal: number }>>
  >(new Map());
  const outSpeedRef = useRef<Map<string, SpeedTrackerEntry>>(new Map());
  const inSpeedRef = useRef<Map<string, SpeedTrackerEntry>>(new Map());

  // Bumped per row `key` (`out:<id>` / `in:<packageId>`) on every `sync-finished`
  // for that package — lets an already-expanded row (same component instance,
  // same `key`, across the active→terminal transition) know its cached
  // `list_transfer_files` detail is stale and re-fetch settled outcome chips.
  const [finishNonce, setFinishNonce] = useState<Map<string, number>>(new Map());
  const bumpFinishNonce = useCallback((rowKey: string) => {
    setFinishNonce((prev) => {
      const next = new Map(prev);
      next.set(rowKey, (next.get(rowKey) ?? 0) + 1);
      return next;
    });
  }, []);

  const [busy, setBusy] = useState<Set<string>>(new Set());
  const withBusy = useCallback((busyKey: string, fn: () => Promise<void>) => {
    setBusy((prev) => new Set(prev).add(busyKey));
    fn().finally(() => {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(busyKey);
        return next;
      });
    });
  }, []);

  // Snapshot every outbound row we ever see `active` so a later terminal
  // transition (sync-finished) can still render a full row for it. Pruned to
  // ids that are either still active or pinned in the terminal ledger, so
  // this never grows unbounded over a long session.
  useEffect(() => {
    if (!status) return;
    const activeIds = new Set<number>();
    for (const row of status.sender.active) {
      outboundSeenRef.current.set(row.id, row);
      activeIds.add(row.id);
    }
    for (const id of Array.from(outboundSeenRef.current.keys())) {
      if (!activeIds.has(id) && !terminalOutbound.has(id)) {
        outboundSeenRef.current.delete(id);
      }
    }
  }, [status, terminalOutbound]);

  // Page-scoped live listeners (mounted-only), per CLAUDE.md's cancelled-flag
  // pattern. `sync-progress` feeds row-level bytes + speed; `sync-file-progress`
  // feeds the expanded-row per-file overlay; `sync-finished` promotes a
  // terminal outbound row into the Resend ledger and clears live state for the
  // finished package. Status re-poll on these events is already handled by the
  // shared `useSyncStatus` inside `TransfersContext` — this hook does its own
  // bookkeeping only, no redundant refresh() calls from the progress tick.
  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenFile: (() => void) | undefined;
    let unlistenFinished: (() => void) | undefined;

    api
      .listen<SyncProgressEvent>('sync-progress', (p) => {
        if (cancelled) return;
        if (p.bytesDone == null || p.bytesTotal == null) return;
        if (p.direction === 'sent') {
          const id = Number(p.packageId);
          if (!Number.isFinite(id)) return;
          const live = trackBytes(outSpeedRef.current, `out:${id}`, p.bytesDone, p.bytesTotal);
          setLiveOutboundBytes((prev) => new Map(prev).set(id, live));
          // Record the stage so ActiveTransferRow can show "uploaded — awaiting
          // confirmation"; a later `transferring` tick (a resume) flips it back.
          setLiveOutboundStage((prev) => new Map(prev).set(id, p.stage));
        } else {
          const live = trackBytes(inSpeedRef.current, `in:${p.packageId}`, p.bytesDone, p.bytesTotal);
          setLiveInboundBytes((prev) => new Map(prev).set(p.packageId, live));
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenProgress = fn;
      })
      .catch((err) => console.error('[useTransferQueue] sync-progress listen failed:', err));

    api
      .listen<SyncFileProgressEvent>('sync-file-progress', (p) => {
        if (cancelled) return;
        setLiveFiles((prev) => {
          const next = new Map(prev);
          const forPkg = new Map(next.get(p.packageId) ?? []);
          forPkg.set(p.file, { bytesDone: p.bytesDone, bytesTotal: p.bytesTotal });
          next.set(p.packageId, forPkg);
          return next;
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFile = fn;
      })
      .catch((err) => console.error('[useTransferQueue] sync-file-progress listen failed:', err));

    api
      .listen<SyncFinishedEvent>('sync-finished', (p) => {
        if (cancelled) return;
        if (p.direction === 'sent') {
          const id = Number(p.packageId);
          if (!Number.isFinite(id)) return;
          bumpFinishNonce(`out:${id}`);
          outSpeedRef.current.delete(`out:${id}`);
          setLiveOutboundBytes((prev) => {
            if (!prev.has(id)) return prev;
            const next = new Map(prev);
            next.delete(id);
            return next;
          });
          setLiveOutboundStage((prev) => {
            if (!prev.has(id)) return prev;
            const next = new Map(prev);
            next.delete(id);
            return next;
          });
          if (p.outcome === 'cancelled' || p.outcome.startsWith('failed')) {
            const outcome: 'failed' | 'cancelled' = p.outcome === 'cancelled' ? 'cancelled' : 'failed';
            const cached = outboundSeenRef.current.get(id);
            const summary: OutboundSummary = cached
              ? { ...cached, state: outcome }
              : {
                  id,
                  packageShort: shortId(String(id)),
                  peerShort: shortPeer(p.peerDevice),
                  state: outcome,
                  attempts: 0,
                  createdAt: new Date().toISOString(),
                  lastError: null,
                  nextRetryAt: null,
                  byteSize: 0,
                  fileCount: p.okCount + p.failed.length,
                };
            setTerminalOutbound((prev) => new Map(prev).set(id, { summary, outcome }));
          }
        } else {
          bumpFinishNonce(`in:${p.packageId}`);
          inSpeedRef.current.delete(`in:${p.packageId}`);
          setLiveInboundBytes((prev) => {
            if (!prev.has(p.packageId)) return prev;
            const next = new Map(prev);
            next.delete(p.packageId);
            return next;
          });
          setLiveFiles((prev) => {
            if (!prev.has(p.packageId)) return prev;
            const next = new Map(prev);
            next.delete(p.packageId);
            return next;
          });
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFinished = fn;
      })
      .catch((err) => console.error('[useTransferQueue] sync-finished listen failed:', err));

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenFile?.();
      unlistenFinished?.();
    };
  }, []);

  const rows = useMemo<TransferRow[]>(() => {
    const out: TransferRow[] = [];
    const activeOutboundIds = new Set<number>();
    for (const s of status?.sender.active ?? []) {
      activeOutboundIds.add(s.id);
      const live = liveOutboundBytes.get(s.id);
      out.push({
        key: `out:${s.id}`,
        kind: 'outbound',
        id: s.id,
        packageId: null,
        packageShort: s.packageShort,
        peerShort: s.peerShort,
        fileCount: s.fileCount,
        byteSize: s.byteSize,
        bytesDone: live?.bytesDone ?? 0,
        state: s.state,
        attempts: s.attempts,
        lastError: s.lastError,
        nextRetryAt: s.nextRetryAt,
        terminal: false,
        liveStage: liveOutboundStage.get(s.id) ?? null,
        speedBps: s.state === 'transferring' ? (live?.speedBps ?? null) : null,
        isTransferring: s.state === 'transferring',
        finishNonce: finishNonce.get(`out:${s.id}`) ?? 0,
      });
    }
    // A `sync-finished` outcome lands in `terminalOutbound` synchronously, but
    // the polled `status.sender.active` snapshot only drops the same id on its
    // NEXT resolve — for that transient window both sources agree on the same
    // id. The active-side entry above is the live, authoritative one, so a
    // ledger entry for an id still (momentarily) `active` is skipped here
    // rather than rendered as a second `out:<id>` row; once the next poll
    // catches up, the id naturally leaves `active` and the ledger row takes
    // over unmasked. No separate ledger pruning needed (ids are never reused —
    // `retry_sync_package` always mints a new id).
    for (const { summary, outcome } of terminalOutbound.values()) {
      if (activeOutboundIds.has(summary.id)) continue;
      out.push({
        key: `out:${summary.id}`,
        kind: 'outbound',
        id: summary.id,
        packageId: null,
        packageShort: summary.packageShort,
        peerShort: summary.peerShort,
        fileCount: summary.fileCount,
        byteSize: summary.byteSize,
        bytesDone: 0,
        state: outcome,
        attempts: summary.attempts,
        lastError: summary.lastError,
        nextRetryAt: null,
        terminal: true,
        liveStage: null,
        speedBps: null,
        isTransferring: false,
        finishNonce: finishNonce.get(`out:${summary.id}`) ?? 0,
      });
    }
    for (const s of status?.receiver.active ?? []) {
      const live = liveInboundBytes.get(s.packageId);
      out.push({
        key: `in:${s.id}`,
        kind: 'inbound',
        id: s.id,
        packageId: s.packageId,
        packageShort: s.packageShort,
        peerShort: s.peerShort,
        fileCount: s.frameCount,
        byteSize: s.byteSize,
        bytesDone: Math.max(s.bytesDone, live?.bytesDone ?? 0),
        state: s.state,
        attempts: 0,
        lastError: null,
        nextRetryAt: null,
        terminal: false,
        liveStage: null,
        speedBps: s.state === 'fetching' ? (live?.speedBps ?? null) : null,
        isTransferring: s.state === 'fetching',
        finishNonce: finishNonce.get(`in:${s.packageId}`) ?? 0,
      });
    }
    return out;
  }, [status, terminalOutbound, liveOutboundBytes, liveInboundBytes, liveOutboundStage, finishNonce]);

  const activeCount = (status?.sender.queued ?? 0) + (status?.sender.transferring ?? 0) + rows.filter((r) => r.kind === 'inbound').length;

  const sendNow = useCallback(
    (id: number) =>
      withBusy(`send:${id}`, async () => {
        try {
          await api.invoke('send_now_sync_package', { id });
          refresh();
        } catch (err) {
          console.error('[useTransferQueue] send_now_sync_package failed:', err);
          notify({
            title: 'Send now failed',
            detail: String(err),
            kind: 'generic',
            tone: 'warning',
          });
        }
      }),
    [withBusy, refresh, notify],
  );

  const cancelOutbound = useCallback(
    (id: number) =>
      withBusy(`cancel:${id}`, async () => {
        try {
          await api.invoke('cancel_sync_package', { id });
          refresh();
        } catch (err) {
          console.error('[useTransferQueue] cancel_sync_package failed:', err);
          notify({
            title: 'Cancel failed',
            detail: String(err),
            kind: 'generic',
            tone: 'warning',
          });
        }
      }),
    [withBusy, refresh, notify],
  );

  const cancelInbound = useCallback(
    (packageId: string) =>
      withBusy(`cancelin:${packageId}`, async () => {
        try {
          await api.invoke('cancel_incoming_package', { packageId });
          refresh();
        } catch (err) {
          console.error('[useTransferQueue] cancel_incoming_package failed:', err);
          notify({
            title: 'Cancel failed',
            detail: String(err),
            kind: 'generic',
            tone: 'warning',
          });
        }
      }),
    [withBusy, refresh, notify],
  );

  const resend = useCallback(
    (id: number) =>
      withBusy(`resend:${id}`, async () => {
        try {
          await api.invoke<number>('retry_sync_package', { id });
          setTerminalOutbound((prev) => {
            if (!prev.has(id)) return prev;
            const next = new Map(prev);
            next.delete(id);
            return next;
          });
          refresh();
        } catch (err) {
          console.error('[useTransferQueue] retry_sync_package failed:', err);
          notify({
            title: 'Resend failed',
            detail: String(err),
            kind: 'generic',
            tone: 'warning',
          });
        }
      }),
    [withBusy, refresh, notify],
  );

  return { rows, activeCount, liveFiles, refresh, sendNow, cancelOutbound, cancelInbound, resend, busy };
}
