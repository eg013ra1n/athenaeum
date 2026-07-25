import { useCallback, useEffect, useMemo, useState } from 'react';
import { ArrowLeftRight } from 'lucide-react';
import { api } from '../api';
import { AppDataWarningStrip } from '../components/transfers/AppDataWarningStrip';
import { TransferRow } from '../components/transfers/TransferRow';
import { TransferDetail } from '../components/transfers/TransferDetail';
import { groupHasCancel, groupHasFailure, groupHasRealSuccess } from '../components/transfers/historyGrouping';
import type { DeleteKey, TransferFilter, UnifiedRow } from '../components/transfers/types';
import type { Direction } from '../types/models';
import type { TransferRow as TransferRowModel } from '../hooks/useTransferQueue';
import { useTransferQueue } from '../hooks/useTransferQueue';
import { useTransferHistory } from '../hooks/useTransferHistory';

/**
 * `/transfers` — the torrent-style master-detail Transfers view (Transfers
 * Status Model v2, §D8). ONE unified list of every transfer — live in-flight
 * rows AND merged-in completed/failed history — filtered by chips (Completed is
 * a filter, not a separate screen; nothing disappears on completion). Selecting
 * a row opens the bottom detail pane (Files / Log / Details). Batch names and
 * device names everywhere; hex only in the Details tab; `waiting` is benign.
 */
export default function Transfers() {
  const { rows, liveFiles, sendNow, cancelOutbound, cancelInbound, resend, deleteTransfer, busy } =
    useTransferQueue();
  const { groups, deviceNames, deviceKinds, projectNames, refetch, removeLocal } =
    useTransferHistory();

  const [filter, setFilter] = useState<TransferFilter>('all');
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  // Re-read history the moment a transfer finishes (cheap; the hook also polls).
  // Live rows already react via the shared `useSyncStatus` snapshot.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<unknown>('sync-finished', () => {
        if (!cancelled) refetch();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[Transfers] sync-finished listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refetch]);

  // Merge live rows + history groups into one unified list (§D8). The batch model
  // guarantees ONE durable row per transfer per direction — the sender reuses its
  // row across resends (state flips in place, `generation++`), the receiver keeps
  // one row per (peer, batch) — so `rows` already carries exactly one entry per
  // transfer. No attempt-collapse, no supersession, no sibling-scan: a live and a
  // settled transfer are the SAME row id, deduped by id upstream in the hook.
  //
  // History groups fold in BEHIND the live rows, hiding any group already on screen
  // as a live/terminal row. `list_terminal_transfers` is a recent (~100) window, so
  // the surviving history groups are OLDER transfers beyond that window — the one
  // thing history still contributes. Dedup + delete keys are now direct field
  // reads, and — since B5b — symmetric across BOTH directions: a row's `batchUuid`
  // == its history group's `packageId` (the received writer + delete now key on the
  // durable `batch_uuid`, not the rotating wire id; this also kills the old-attempt
  // phantom history group a receiver re-attempt used to leave behind, since every
  // attempt of one batch now writes history under the SAME key).
  const unified = useMemo<UnifiedRow[]>(() => {
    // Delete key (item 3): BOTH directions delete by `batchUuid` now (B5b fully
    // unified the received side onto it, symmetric with sent — the backend's
    // NULL-edge fallback to the wire id covers only a legacy pre-batch-model row,
    // which the frontend never sees since the summary mapper already resolves that
    // fallback into `batchUuid` itself). Trash shows only on a settled (terminal)
    // row; a live row IS the same row, so there is no sibling to suppress it against.
    const liveDeleteKey = (r: TransferRowModel): DeleteKey | null => {
      if (!r.terminal || !r.batchUuid) return null;
      return { direction: r.kind === 'inbound' ? 'received' : 'sent', packageKey: r.batchUuid };
    };

    const liveUnified: UnifiedRow[] = rows.map(
      (r): UnifiedRow => ({
        kind: 'live',
        selKey: r.key,
        row: r,
        deleteKey: liveDeleteKey(r),
      }),
    );

    // Batch keys already on screen as a live/terminal row, so their history groups
    // don't double — both directions by `batchUuid` (B5b). A variant-B ghost counts
    // here too: it carries the batch's real `batchUuid` and IS on screen, so an
    // older history group for the same batch must still stay hidden behind it.
    const liveSentKeys = new Set<string>();
    const liveRecvKeys = new Set<string>();
    for (const r of rows) {
      if (!r.batchUuid) continue;
      if (r.kind === 'outbound') liveSentKeys.add(r.batchUuid);
      else liveRecvKeys.add(r.batchUuid);
    }

    const historyUnified: UnifiedRow[] = [];
    for (const g of groups) {
      const dup =
        g.packageId != null &&
        (g.direction === 'sent' ? liveSentKeys.has(g.packageId) : liveRecvKeys.has(g.packageId));
      if (dup) continue;
      historyUnified.push({
        kind: 'history',
        selKey: g.groupKey,
        group: g,
        deviceName: deviceNames[g.peerDevice] ?? null,
        // Sender-kind badge (Perseus UI v2) — a RECEIVED group's origin, by peer
        // hex. `null` for a sent group (the peer is the recipient, not a sender)
        // so no badge shows there.
        deviceKind: g.direction === 'received' ? (deviceKinds[g.peerDevice] ?? null) : null,
        projectName: g.project ? projectNames[g.project] ?? null : null,
        // A history group's `packageId` IS its delete key — `batchUuid` in both
        // directions since B5b. `null` for a legacy "Earlier transfers" bucket with
        // no single package key.
        deleteKey: g.packageId ? { direction: g.direction, packageKey: g.packageId } : null,
      });
    }
    return [...liveUnified, ...historyUnified];
  }, [rows, groups, deviceNames, deviceKinds, projectNames]);

  // Filter membership (§D8). A row can match more than one bucket (a mixed
  // history group is both Completed and Failed); counts reflect that honestly.
  // A settled `cancelled` row and a purely-cancelled history group land in the
  // new Cancelled bucket (UX wave 2, §problem 4) — out of Completed. A group
  // mixing success + cancel stays in Completed; Failed is unchanged.
  const bucketsFor = (u: UnifiedRow): Set<TransferFilter> => {
    const s = new Set<TransferFilter>(['all']);
    if (u.kind === 'live') {
      const r = u.row;
      // D1's own rule, taken at its word: EVERY parked shape belongs in the
      // Waiting chip — the countdown one, the peer-absent one, and (owner review)
      // the queued family: `queued_at_receiver` (parked on the RECEIVER's queue),
      // an inbound `queued` (parked on a receive slot or a busy lane — the ghost
      // rows carry the same displayState). All of them are "not moving right now,
      // resumes by itself", which is exactly what a user filtering Waiting is
      // asking to see. The ONE queued shape that stays out is the OUTBOUND local
      // `queued` — a millisecond pipeline stage between enqueue and announce, not
      // a park on anything external; it stays in Sending.
      const parkedQueued =
        r.displayState === 'queued_at_receiver' ||
        (r.kind === 'inbound' && r.displayState === 'queued');
      if (r.displayState === 'waiting' || r.displayState === 'waiting_peer' || parkedQueued)
        s.add('waiting');
      else if (r.terminal) {
        if (r.displayState === 'failed') s.add('failed');
        else if (r.displayState === 'cancelled') s.add('cancelled');
        else s.add('completed');
      } else s.add(r.kind === 'outbound' ? 'sending' : 'receiving');
    } else {
      const success = groupHasRealSuccess(u.group);
      const failure = groupHasFailure(u.group);
      const cancel = groupHasCancel(u.group);
      if (success) s.add('completed');
      if (failure) s.add('failed');
      if (cancel && !success && !failure) s.add('cancelled');
      // A group with no recognized outcome at all still surfaces (as Completed).
      if (!success && !failure && !cancel) s.add('completed');
    }
    return s;
  };

  const counts = useMemo(() => {
    const c: Record<TransferFilter, number> = {
      all: 0,
      sending: 0,
      receiving: 0,
      waiting: 0,
      completed: 0,
      cancelled: 0,
      failed: 0,
    };
    for (const u of unified) for (const b of bucketsFor(u)) c[b] += 1;
    return c;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [unified]);

  const filtered = useMemo(
    () => unified.filter((u) => bucketsFor(u).has(filter)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [unified, filter],
  );

  // Resolve the selected row from the FILTERED list, not the full one: a row the
  // active filter no longer includes must drop out of the detail pane (it comes
  // back if the filter widens again — `selectedKey` is preserved, not cleared).
  const selected = selectedKey ? (filtered.find((u) => u.selKey === selectedKey) ?? null) : null;

  // Trash a settled batch (UX wave 2, §problem 5): fire the durable delete, then
  // — only on success (the backend refuses `Invalid` if any attempt is active) —
  // optimistically clear the batch's history rows and reconcile.
  const handleDelete = useCallback(
    (direction: Direction, packageKey: string) => {
      void deleteTransfer(direction, packageKey).then((ok) => {
        if (!ok) return;
        removeLocal(direction, packageKey);
        refetch();
      });
    },
    [deleteTransfer, removeLocal, refetch],
  );

  // Shared 1s countdown tick — runs ONLY while a `waiting` row with a live
  // deadline is actually on screen (filtered view), and stops on filter change /
  // unmount. No per-row timers, no leak. A `waiting_peer` row never has a
  // `stalledUntil` (D1: it waits for a signal, not an instant), so it can never
  // start this ticker.
  const hasVisibleCountdown = filtered.some(
    (u) => u.kind === 'live' && u.row.displayState === 'waiting' && !!u.row.stalledUntil,
  );
  useEffect(() => {
    if (!hasVisibleCountdown) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [hasVisibleCountdown]);

  const chips: Array<{ key: TransferFilter; label: string }> = [
    { key: 'all', label: 'All' },
    { key: 'sending', label: 'Sending' },
    { key: 'receiving', label: 'Receiving' },
    { key: 'waiting', label: 'Waiting' },
    { key: 'completed', label: 'Completed' },
    { key: 'cancelled', label: 'Cancelled' },
    { key: 'failed', label: 'Failed' },
  ];

  return (
    <div className="flex h-full flex-col p-4 pt-3">
      <div className="mb-3 flex shrink-0 items-center gap-2">
        <ArrowLeftRight size={22} className="text-accent" />
        <h2 className="text-2xl font-bold">Transfers</h2>
      </div>

      <AppDataWarningStrip />

      <div className="mb-3 flex shrink-0 flex-wrap gap-1.5">
        {chips.map((chip) => {
          const isActive = filter === chip.key;
          const count = counts[chip.key];
          return (
            <button
              key={chip.key}
              type="button"
              onClick={() => setFilter(chip.key)}
              className={`inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium transition-colors ${
                isActive
                  ? 'border-accent bg-accent/15 text-content'
                  : 'border-border bg-surface text-content-muted hover:bg-surface-hover hover:text-content'
              }`}
            >
              {chip.label}
              <span
                className={`rounded-full px-1.5 text-[10px] tabular-nums ${
                  isActive ? 'bg-accent/20 text-content' : 'bg-surface-hover text-content-muted'
                }`}
              >
                {count}
              </span>
            </button>
          );
        })}
      </div>

      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border border-border">
        <div className="min-h-0 flex-1 overflow-y-auto">
          {unified.length === 0 ? (
            <p className="px-4 py-10 text-center text-sm text-content-muted">No transfers yet</p>
          ) : filtered.length === 0 ? (
            <p className="px-4 py-10 text-center text-sm text-content-muted">
              No transfers match this filter
            </p>
          ) : (
            filtered.map((u) => (
              <TransferRow
                key={u.selKey}
                item={u}
                selected={selectedKey === u.selKey}
                onSelect={() => setSelectedKey((prev) => (prev === u.selKey ? null : u.selKey))}
                now={now}
                busy={busy}
                onSendNow={sendNow}
                onCancelOutbound={cancelOutbound}
                onCancelInbound={cancelInbound}
                onResend={resend}
                onDelete={handleDelete}
              />
            ))
          )}
        </div>

        {selected && (
          <div className="h-72 shrink-0 border-t border-border bg-surface">
            <TransferDetail
              key={selected.selKey}
              item={selected}
              liveFiles={liveFiles}
              onClose={() => setSelectedKey(null)}
            />
          </div>
        )}
      </div>
    </div>
  );
}
