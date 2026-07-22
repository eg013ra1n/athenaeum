// `/transfers` page hook (Task 15) — merges the shared `useSyncStatus` poll
// snapshot (via `TransfersContext`) with two page-scoped live event streams
// (`sync-progress` for row-level bytes/speed, `sync-file-progress` for the
// expanded-row per-file overlay) into one unified, torrent-style Active list,
// plus the four row actions.
//
// Design note (terminal rows / Resend, tv2 follow-up): `SyncSenderStatus.active`
// NEVER includes a terminal (confirmed/failed/cancelled) row by construction
// (see `OutboundSummary`'s doc comment) — the cheap `get_sync_status` poll
// returns only non-terminal rows. Two sources reunite the terminal rows:
//
//   1. The DURABLE read (`list_terminal_transfers`): the recent window of
//      settled sends + receives, straight from `sync_outbound`/`sync_inbound`.
//      Fetched on mount and re-fetched on every `sync-finished` (NOT on the 10s
//      poll). These survive a RESTART — the bug this follow-up fixes: before
//      it, a settled row (and its Resend button + detail) vanished on relaunch.
//   2. The in-memory LEDGER (`terminalOutbound`): the same-session immediacy
//      path — on a `sync-finished` with a non-confirmed outcome the hook
//      promotes the last-seen `OutboundSummary` snapshot (or a degraded
//      event-only fallback) into a page-local ledger, so the row flips to
//      "settled + Resend" instantly, before the next durable fetch resolves.
//
// The DB rows SUPERSEDE the ledger for the same id (deduped by id in `rows`),
// so there is never a row+row double. Inbound terminal rows come only from the
// durable read (there is no inbound ledger) and carry NO actions. The full
// audit trail remains the History tab (`list_sync_history`).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api';
import { useTransfers } from '../contexts/TransfersContext';
import { useNotifications } from '../contexts/NotificationContext';
import type {
  DeletedTransferRecord,
  Direction,
  InboundSummary,
  OutboundSummary,
  SyncFileProgressEvent,
  SyncFinishedEvent,
  SyncProgressEvent,
  TerminalTransfers,
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
 * page-session-lingering terminal outbound package kept around for Resend.
 *
 * Transfers Status Model v2: presentation keys off `displayState` /
 * `retrying` / `stalledUntil` / `fileCounts` (never the raw `attempts` gate),
 * and shows `displayName` / `deviceName` instead of the raw handle + hex. The
 * raw `state`, `attempts`, `packageShort`, `peerShort` survive for the Details
 * tab (the ONLY place ids/hex/raw state appear). */
export interface TransferRow {
  key: string;
  kind: TransferRowKind;
  /** Outbound: `sync_outbound.id`. Inbound: `sync_inbound.id`. */
  id: number;
  /** Full wire package id — inbound only (needed by `cancel_incoming_package`); `null` for outbound. */
  packageId: string | null;
  packageShort: string;
  peerShort: string;
  /** Human batch name (§D1), or `null` for a legacy/unnamed batch. */
  displayName: string | null;
  /** Friendly peer device name (§D5), or `null` when the peer isn't in the cache. */
  deviceName: string | null;
  /** Backend-derived presentation state (§D5): outbound
   *  `queued|preparing|transferring|uploaded|waiting|confirmed|cancelled|failed`,
   *  inbound `announced|fetching|ingesting|done|failed|cancelled`. */
  displayState: string;
  /** RFC3339 retry deadline while `displayState === 'waiting'`, else `null` — the countdown target. */
  stalledUntil: string | null;
  /** Whether a retry is armed (`next_retry_at` set) — gates the error-reason line. NOT `attempts`. */
  retrying: boolean;
  /** Per-file rollup for the "N of M files" progress line. */
  fileCounts: { total: number; done: number; failed: number };
  fileCount: number;
  byteSize: number;
  bytesDone: number;
  state: string;
  createdAt: string;
  /** Outbound-only; `0` for inbound rows (the field doesn't exist there). Details-tab only.
   *  Includes the engine's internal announce-retries — NOT the user-facing counter. */
  attempts: number;
  /** User-facing "attempt N" counter (Transfers Batch Model §D5) — bumped ONLY by a
   *  resend, never by the engine's internal announce-retries (`attempts`). Rendered
   *  as "attempt N" when `> 1`, on active AND terminal rows. */
  generation: number;
  /** The durable per-transfer batch identity (§D1): outbound == the package-dir
   *  basename (== the sent `sync_history.package_id`); inbound == `sync_inbound
   *  .batch_uuid` (== the received `sync_history.package_id`, B5b). `delete_transfer
   *  _history`'s `package_key` is THIS field in BOTH directions (B5b unified) — the
   *  wire `packageId` still rotates per attempt but is no longer the delete key. */
  batchUuid: string;
  /** The terminal reason: outbound engine failure/cancel text, or (B5b) an inbound
   *  sender-revoke reason — `"by sender"` / `"sender failed"` / a superseded detail.
   *  See `plainTransferError`/`isSenderRevokeReason` for the inbound mapping. */
  lastError: string | null;
  nextRetryAt: string | null;
  /** `true` for a lingering failed/cancelled outbound row (Resend, not Cancel/Send-now). */
  terminal: boolean;
  /** Whether Resend would currently succeed (terminal failed/cancelled AND the
   *  package payload is still on disk). `false` for every non-terminal/inbound
   *  row. Gates the Resend button — see `TransferRow.tsx`'s `canResend`. */
  resendable: boolean;
  /** Latest `sync-progress` `stage` seen for this outbound id (`sent` ticks only),
   * cleared on `sync-finished`. Outbound-only; `null` for inbound/terminal rows. */
  liveStage: string | null;
  speedBps: number | null;
  isTransferring: boolean;
  /** Bumped on every `sync-finished` for this package — a selected/expanded row
   * watches this to know its cached `list_transfer_files` detail needs a re-fetch. */
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
  /** Remove one settled batch's durable records (UX wave 2 trash action).
   *  Optimistically drops the matching terminal rows on success and re-reads the
   *  durable window; resolves `true` on success, `false` on a refusal (an active
   *  attempt — a warning toast is raised) so the caller can also clear history. */
  deleteTransfer: (direction: Direction, packageKey: string) => Promise<boolean>;
  /** Action keys currently in flight (`send:<id>` / `cancel:<id>` / `cancelin:<packageId>` / `resend:<id>` / `delete:<direction>:<packageKey>`) — disable the triggering button while present. */
  busy: Set<string>;
}

export function useTransferQueue(): UseTransferQueue {
  const { status, refresh } = useTransfers();
  const { notify } = useNotifications();

  const outboundSeenRef = useRef<Map<number, OutboundSummary>>(new Map());
  const [terminalOutbound, setTerminalOutbound] = useState<
    Map<number, { summary: OutboundSummary; outcome: 'failed' | 'cancelled' }>
  >(new Map());

  // Durable terminal rows (tv2 follow-up) — the recent window of settled sends +
  // receives from `list_terminal_transfers`, which survive a restart. Fetched on
  // mount and re-fetched on every `sync-finished` (see the dedicated effect
  // below), never on the 10s status poll.
  const [dbTerminalSent, setDbTerminalSent] = useState<OutboundSummary[]>([]);
  const [dbTerminalReceived, setDbTerminalReceived] = useState<InboundSummary[]>([]);

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

  // Guards state-writes from a durable fetch that resolves after unmount (the
  // listener is torn down on unmount, but an in-flight `list_terminal_transfers`
  // promise can still settle). Mirrors `useTransferHistory`'s pattern.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // The durable terminal read, hoisted so both the mount/`sync-finished` effect
  // and the delete action can re-run it.
  const fetchTerminal = useCallback(() => {
    api
      .invoke<TerminalTransfers>('list_terminal_transfers', {})
      .then((t) => {
        if (!mountedRef.current) return;
        setDbTerminalSent(t.sent);
        setDbTerminalReceived(t.received);
      })
      .catch((err) => console.error('[useTransferQueue] list_terminal_transfers failed:', err));
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

  // Durable terminal rows: fetch once on mount and re-fetch on every
  // `sync-finished` (a discrete outcome — NOT the high-frequency poll). This is
  // what makes a settled transfer's row + Resend button survive a relaunch, when
  // the in-memory ledger has been wiped. Cancelled-flag listener pattern
  // (StrictMode-safe): await the unlisten into a flag-guarded variable so a
  // double-mount can't leak a second listener.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    fetchTerminal();
    api
      .listen<SyncFinishedEvent>('sync-finished', () => {
        if (!cancelled) fetchTerminal();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) =>
        console.error('[useTransferQueue] terminal sync-finished listen failed:', err),
      );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [fetchTerminal]);

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
          // Record the stage for the row's post-upload/pre-ack label; a later
          // `transferring` tick (a resume) flips it back.
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
            // `resendable: true` unconditionally here (owner follow-up): the
            // package just finished failing/cancelling THIS session, so its
            // payload provably existed moments ago — unlike a DB terminal row
            // (which may be an OLD batch retention has since swept), a
            // same-session ledger row is safe to always offer Resend on.
            const summary: OutboundSummary = cached
              ? { ...cached, state: outcome, displayState: outcome, resendable: true }
              : {
                  id,
                  packageShort: shortId(String(id)),
                  peerShort: shortPeer(p.peerDevice),
                  state: outcome,
                  attempts: 0,
                  // Transfers Batch Model §D5 additive fields (B6 consumes them
                  // fully): a from-scratch synthesized terminal row has no batch
                  // identity to key on, so fall back to the row id and attempt 1.
                  generation: 1,
                  batchUuid: String(id),
                  createdAt: new Date().toISOString(),
                  lastError: null,
                  nextRetryAt: null,
                  byteSize: 0,
                  fileCount: p.okCount + p.failed.length,
                  // Transfers Status Model v2 additive fields (T7 will consume
                  // them fully); a synthesized terminal row has no batch/device
                  // name and is not retrying.
                  displayName: null,
                  deviceName: null,
                  displayState: outcome,
                  stalledUntil: null,
                  fileCounts: {
                    total: p.okCount + p.failed.length,
                    done: p.okCount,
                    failed: p.failed.length,
                  },
                  retrying: false,
                  resendable: true,
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
        displayName: s.displayName,
        deviceName: s.deviceName,
        displayState: s.displayState,
        stalledUntil: s.stalledUntil,
        retrying: s.retrying,
        fileCounts: s.fileCounts,
        fileCount: s.fileCount,
        byteSize: s.byteSize,
        bytesDone: live?.bytesDone ?? 0,
        state: s.state,
        createdAt: s.createdAt,
        attempts: s.attempts,
        generation: s.generation,
        batchUuid: s.batchUuid,
        lastError: s.lastError,
        nextRetryAt: s.nextRetryAt,
        terminal: false,
        resendable: false,
        liveStage: liveOutboundStage.get(s.id) ?? null,
        speedBps: s.state === 'transferring' ? (live?.speedBps ?? null) : null,
        isTransferring: s.state === 'transferring',
        finishNonce: finishNonce.get(`out:${s.id}`) ?? 0,
      });
    }
    // Ids already covered by the durable read supersede the in-memory ledger:
    // the DB summary is authoritative (correct display_name/device_name/counts),
    // so the ledger entry for the same id is dropped to avoid a row+row double.
    const dbTerminalSentIds = new Set(dbTerminalSent.map((s) => s.id));

    // A `sync-finished` outcome lands in `terminalOutbound` synchronously, but
    // the polled `status.sender.active` snapshot only drops the same id on its
    // NEXT resolve — for that transient window both sources agree on the same
    // id. The active-side entry above is the live, authoritative one, so a
    // ledger entry for an id still (momentarily) `active` is skipped here
    // rather than rendered as a second `out:<id>` row; once the next poll
    // catches up, the id naturally leaves `active` and the ledger row takes
    // over unmasked. Batch model: a resend reuses the same id (resets the row in
    // place, generation++), so a ledger entry for an id that has gone active
    // again is correctly masked here until the resend settles.
    for (const { summary, outcome } of terminalOutbound.values()) {
      if (activeOutboundIds.has(summary.id)) continue;
      if (dbTerminalSentIds.has(summary.id)) continue;
      out.push({
        key: `out:${summary.id}`,
        kind: 'outbound',
        id: summary.id,
        packageId: null,
        packageShort: summary.packageShort,
        peerShort: summary.peerShort,
        displayName: summary.displayName,
        deviceName: summary.deviceName,
        // A terminal ledger row is settled — its display state IS the outcome,
        // never a stale `transferring`/`waiting` from before it finished.
        displayState: outcome,
        stalledUntil: null,
        retrying: false,
        fileCounts: summary.fileCounts,
        fileCount: summary.fileCount,
        byteSize: summary.byteSize,
        bytesDone: 0,
        state: outcome,
        createdAt: summary.createdAt,
        attempts: summary.attempts,
        generation: summary.generation,
        batchUuid: summary.batchUuid,
        lastError: summary.lastError,
        nextRetryAt: null,
        terminal: true,
        resendable: summary.resendable,
        liveStage: null,
        speedBps: null,
        isTransferring: false,
        finishNonce: finishNonce.get(`out:${summary.id}`) ?? 0,
      });
    }
    // Durable terminal sent rows (survive restart). Skip an id still (momentarily)
    // `active` — the live row above is authoritative during that transient window.
    // A confirmed row is INCLUDED here (unlike the ledger, which only ever held
    // failed/cancelled); `canResend` gates it out of the Resend affordance.
    for (const s of dbTerminalSent) {
      if (activeOutboundIds.has(s.id)) continue;
      out.push({
        key: `out:${s.id}`,
        kind: 'outbound',
        id: s.id,
        packageId: null,
        packageShort: s.packageShort,
        peerShort: s.peerShort,
        displayName: s.displayName,
        deviceName: s.deviceName,
        // The persisted display state IS the settled outcome (`confirmed` /
        // `failed` / `cancelled`).
        displayState: s.displayState,
        stalledUntil: null,
        retrying: false,
        fileCounts: s.fileCounts,
        fileCount: s.fileCount,
        byteSize: s.byteSize,
        bytesDone: 0,
        state: s.state,
        createdAt: s.createdAt,
        attempts: s.attempts,
        generation: s.generation,
        batchUuid: s.batchUuid,
        lastError: s.lastError,
        nextRetryAt: null,
        terminal: true,
        // Backend-computed (tv2 owner follow-up): terminal failed/cancelled AND
        // the package dir still has its payload on disk — the exact guard
        // `retry_sync_package` enforces. `false` for confirmed (never offered)
        // and for a failed/cancelled row whose payload retention already swept.
        resendable: s.resendable,
        liveStage: null,
        speedBps: null,
        isTransferring: false,
        finishNonce: finishNonce.get(`out:${s.id}`) ?? 0,
      });
    }
    const activeInboundIds = new Set<number>();
    for (const s of status?.receiver.active ?? []) {
      activeInboundIds.add(s.id);
      const live = liveInboundBytes.get(s.packageId);
      out.push({
        key: `in:${s.id}`,
        kind: 'inbound',
        id: s.id,
        packageId: s.packageId,
        packageShort: s.packageShort,
        peerShort: s.peerShort,
        displayName: s.displayName,
        deviceName: s.deviceName,
        displayState: s.displayState,
        stalledUntil: s.stalledUntil,
        retrying: false,
        fileCounts: s.fileCounts,
        fileCount: s.frameCount,
        byteSize: s.byteSize,
        bytesDone: Math.max(s.bytesDone, live?.bytesDone ?? 0),
        state: s.state,
        createdAt: s.createdAt,
        attempts: 0,
        generation: s.generation,
        batchUuid: s.batchUuid,
        // B5b: `InboundSummary.lastError` — a sender-revoke reason (rare on an
        // active row; only meaningful once terminal, but read honestly either way).
        lastError: s.lastError,
        nextRetryAt: null,
        terminal: false,
        resendable: false,
        liveStage: null,
        speedBps: s.state === 'fetching' ? (live?.speedBps ?? null) : null,
        isTransferring: s.state === 'fetching',
        finishNonce: finishNonce.get(`in:${s.packageId}`) ?? 0,
      });
    }
    // Durable terminal received rows (survive restart) — detail symmetry with
    // sent (Files/Log across relaunches). NO actions. Skip an id still active.
    for (const s of dbTerminalReceived) {
      if (activeInboundIds.has(s.id)) continue;
      out.push({
        key: `in:${s.id}`,
        kind: 'inbound',
        id: s.id,
        packageId: s.packageId,
        packageShort: s.packageShort,
        peerShort: s.peerShort,
        displayName: s.displayName,
        deviceName: s.deviceName,
        // `done` / `failed` / `cancelled`.
        displayState: s.displayState,
        stalledUntil: null,
        retrying: false,
        fileCounts: s.fileCounts,
        fileCount: s.frameCount,
        byteSize: s.byteSize,
        bytesDone: s.bytesDone,
        state: s.state,
        createdAt: s.createdAt,
        attempts: 0,
        generation: s.generation,
        batchUuid: s.batchUuid,
        // B5b: the terminal reason (e.g. a sender revoke's "by sender"/"sender
        // failed"/superseded) — see `plainTransferError`/`isSenderRevokeReason`.
        lastError: s.lastError,
        nextRetryAt: null,
        terminal: true,
        // Inbound rows carry NO actions (no Resend concept for a receive).
        resendable: false,
        liveStage: null,
        speedBps: null,
        isTransferring: false,
        finishNonce: finishNonce.get(`in:${s.packageId}`) ?? 0,
      });
    }
    return out;
  }, [
    status,
    terminalOutbound,
    dbTerminalSent,
    dbTerminalReceived,
    liveOutboundBytes,
    liveInboundBytes,
    liveOutboundStage,
    finishNonce,
  ]);

  // Live-only count (terminal rows excluded) — an inbound row still moving.
  const activeCount =
    (status?.sender.queued ?? 0) +
    (status?.sender.transferring ?? 0) +
    rows.filter((r) => r.kind === 'inbound' && !r.terminal).length;

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
          const newId = await api.invoke<number>('retry_sync_package', { id });
          // Batch model (§D1): resend USUALLY REUSES the same row —
          // `retry_sync_package` resets `sync_outbound` id `id` in place (state →
          // queued, generation++) and returns the SAME id. Drop it from both
          // terminal sources so it stops rendering as settled; the next
          // `get_sync_status` poll returns the same id in `sender.active` and the row
          // flips back to live IN PLACE (one row, "attempt N").
          //
          // Decline exception (Task D): a transfer the RECEIVER declined is final per
          // its `batch_uuid`, so a deliberate re-ask mints a NEW transfer (new dir
          // basename ⇒ new batch identity) and returns a DIFFERENT id. Keep the old
          // declined row as history — just flip its Resend affordance off — and let
          // the new live row arrive via the normal status poll.
          if (newId === id) {
            setTerminalOutbound((prev) => {
              if (!prev.has(id)) return prev;
              const next = new Map(prev);
              next.delete(id);
              return next;
            });
            setDbTerminalSent((prev) => prev.filter((s) => s.id !== id));
          } else {
            // Do NOT remove the old id from the terminal ledgers (it stays as a
            // declined-history row); flip its session-ledger entry's `resendable` to
            // false so the button disappears, and re-read the durable terminal window
            // so the persisted old row reflects its now-dead Resend affordance.
            setTerminalOutbound((prev) => {
              const entry = prev.get(id);
              if (!entry) return prev;
              const next = new Map(prev);
              next.set(id, {
                ...entry,
                summary: { ...entry.summary, resendable: false },
              });
              return next;
            });
            fetchTerminal();
          }
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
    [withBusy, refresh, fetchTerminal, notify],
  );

  // Trash a settled batch's durable records (UX wave 2). Terminal-only by
  // construction — the trash affordance never appears on an active row — but the
  // backend also refuses (`Invalid`) if any attempt is momentarily active, in
  // which case we notify and remove nothing. On success we optimistically drop the
  // batch's terminal rows from both durable sources by EXACT key (batch model, B5b:
  // ONE row per batch — BOTH directions now match on `batchUuid`, symmetric with the
  // sent side; the received wire `packageId` rotates per attempt and is no longer
  // the delete key) and re-read the durable window to reconcile. Returns success so
  // the page can also clear the batch's history rows.
  const deleteTransfer = useCallback(
    (direction: Direction, packageKey: string): Promise<boolean> => {
      const busyKey = `delete:${direction}:${packageKey}`;
      setBusy((prev) => new Set(prev).add(busyKey));
      return api
        .invoke<DeletedTransferRecord>('delete_transfer_history', { direction, packageKey })
        .then(() => {
          if (direction === 'received') {
            // B5b: received batch key IS `batchUuid` (the durable id; falls back to
            // the wire id server-side only for a legacy NULL-batch_uuid row, which
            // the frontend never sees since the summary mapper already resolves
            // that fallback into `batchUuid` itself).
            setDbTerminalReceived((prev) => prev.filter((s) => s.batchUuid !== packageKey));
          } else {
            // Sent batch key IS `batchUuid` (== the package-dir basename == the sent
            // history `packageId`) — an exact match, no prefix scan.
            setDbTerminalSent((prev) => prev.filter((s) => s.batchUuid !== packageKey));
            setTerminalOutbound((prev) => {
              let changed = false;
              const next = new Map(prev);
              for (const [id, entry] of prev) {
                if (entry.summary.batchUuid === packageKey) {
                  next.delete(id);
                  changed = true;
                }
              }
              return changed ? next : prev;
            });
          }
          fetchTerminal();
          return true;
        })
        .catch((err) => {
          console.error('[useTransferQueue] delete_transfer_history failed:', err);
          notify({
            title: 'Could not remove from history',
            detail: String(err),
            kind: 'generic',
            tone: 'warning',
          });
          return false;
        })
        .finally(() => {
          setBusy((prev) => {
            const next = new Set(prev);
            next.delete(busyKey);
            return next;
          });
        });
    },
    [fetchTerminal, notify],
  );

  return {
    rows,
    activeCount,
    liveFiles,
    refresh,
    sendNow,
    cancelOutbound,
    cancelInbound,
    resend,
    deleteTransfer,
    busy,
  };
}
