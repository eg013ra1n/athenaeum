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
  const { groups, deviceNames, projectNames, refetch, removeLocal } = useTransferHistory();

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

  // Merge live rows + history groups into one list.
  //
  // 1. Live rows split into ACTIVE (non-terminal — never collapsed, they own the
  //    live actions) and TERMINAL (settled). Terminal rows sharing a batch key
  //    collapse into ONE visible row (UX wave 2, §problem 1): outbound attempts
  //    share the package-dir basename → the same (truncated) `packageShort`;
  //    inbound rows share the wire `packageId`. The NEWEST attempt (max id) wins
  //    its state/chips/detail; `attemptCount` drives the "· N attempts" hint.
  // 2. History groups fold in behind the live rows, de-duping any group already
  //    represented by a live row (a lingering failed/cancelled ledger row wins —
  //    it carries live detail + a Resend action). The dedup keys off the same set
  //    of live `packageShort`s the collapse produced, so no resurrected duplicate
  //    appears below a collapsed row. Live keys: an outbound row's `packageShort`
  //    is the truncated (≤10-char) prefix of the package-dir basename that a
  //    `sent` history row stamps as its full `packageId`; an inbound row's full
  //    `packageId` equals a `received` history row's `packageId`.
  const unified = useMemo<UnifiedRow[]>(() => {
    const activeRows = rows.filter((r) => !r.terminal);
    const terminalRows = rows.filter((r) => r.terminal);

    // The full sent package-dir basenames live only in history (a live outbound
    // row carries just the truncated `packageShort`); a collapsed terminal sent
    // row resolves its delete key by prefix-matching against these.
    const sentHistoryFullKeys = groups
      .filter((g) => g.direction === 'sent' && g.packageId != null)
      .map((g) => g.packageId as string);

    const resolveDeleteKey = (r: TransferRowModel): DeleteKey | null => {
      if (r.kind === 'inbound') {
        return r.packageId ? { direction: 'received', packageKey: r.packageId } : null;
      }
      const full = sentHistoryFullKeys.find((k) => k.startsWith(r.packageShort));
      return full ? { direction: 'sent', packageKey: full } : null;
    };

    // Package keys of the VISIBLE active (non-terminal) live rows. An active row
    // owns the live Cancel action and already carries `deleteKey: null`; we also
    // suppress the trash on any collapsed-terminal row or history group that
    // shares its key, so the visible-active case never surfaces the backend's
    // "cancel it first" refusal — the user cancels via the live row instead. (A
    // dead-peer orphan has NO visible active row, so its collapsed/history row
    // keeps its trash and the backend now cancels+deletes it.)
    const activeSentPrefixes: string[] = [];
    const activeRecvIds = new Set<string>();
    for (const r of activeRows) {
      if (r.kind === 'outbound') activeSentPrefixes.push(r.packageShort);
      else if (r.packageId) activeRecvIds.add(r.packageId);
    }
    const hasActiveSibling = (dk: DeleteKey | null): boolean => {
      if (!dk) return false;
      return dk.direction === 'sent'
        ? activeSentPrefixes.some((p) => dk.packageKey.startsWith(p))
        : activeRecvIds.has(dk.packageKey);
    };
    const deleteKeyUnlessActive = (dk: DeleteKey | null): DeleteKey | null =>
      hasActiveSibling(dk) ? null : dk;

    // Collapse terminal rows by batch key, preserving first-seen order.
    const collapseKey = (r: TransferRowModel) =>
      r.kind === 'outbound' ? `out:${r.packageShort}` : `in:${r.packageId ?? r.id}`;
    const byBatch = new Map<string, TransferRowModel[]>();
    const batchOrder: string[] = [];
    for (const r of terminalRows) {
      const k = collapseKey(r);
      if (!byBatch.has(k)) {
        byBatch.set(k, []);
        batchOrder.push(k);
      }
      byBatch.get(k)!.push(r);
    }

    const liveUnified: UnifiedRow[] = [];
    // Active rows first (never collapsed, no delete affordance).
    for (const r of activeRows) {
      liveUnified.push({ kind: 'live', selKey: r.key, row: r, attemptCount: 1, deleteKey: null });
    }
    // Then one collapsed row per terminal batch (newest attempt wins).
    for (const k of batchOrder) {
      const attempts = byBatch.get(k)!;
      const newest = attempts.reduce((a, b) => (b.id > a.id ? b : a));
      liveUnified.push({
        kind: 'live',
        selKey: newest.key,
        row: newest,
        attemptCount: attempts.length,
        deleteKey: deleteKeyUnlessActive(resolveDeleteKey(newest)),
      });
    }

    // Dedup history groups against every live row's batch key (pre-collapse set
    // is identical — collapse never adds/removes a `packageShort`/`packageId`).
    const liveSentPrefixes: string[] = [];
    const liveRecvIds = new Set<string>();
    for (const r of rows) {
      if (r.kind === 'outbound') liveSentPrefixes.push(r.packageShort);
      else if (r.packageId) liveRecvIds.add(r.packageId);
    }

    const historyUnified: UnifiedRow[] = [];
    for (const g of groups) {
      const dup =
        g.packageId != null &&
        (g.direction === 'sent'
          ? liveSentPrefixes.some((p) => g.packageId!.startsWith(p))
          : liveRecvIds.has(g.packageId));
      if (dup) continue;
      historyUnified.push({
        kind: 'history',
        selKey: g.groupKey,
        group: g,
        deviceName: deviceNames[g.peerDevice] ?? null,
        projectName: g.project ? projectNames[g.project] ?? null : null,
        deleteKey: deleteKeyUnlessActive(
          g.packageId ? { direction: g.direction, packageKey: g.packageId } : null,
        ),
      });
    }
    return [...liveUnified, ...historyUnified];
  }, [rows, groups, deviceNames, projectNames]);

  // Filter membership (§D8). A row can match more than one bucket (a mixed
  // history group is both Completed and Failed); counts reflect that honestly.
  // A settled `cancelled` row and a purely-cancelled history group land in the
  // new Cancelled bucket (UX wave 2, §problem 4) — out of Completed. A group
  // mixing success + cancel stays in Completed; Failed is unchanged.
  const bucketsFor = (u: UnifiedRow): Set<TransferFilter> => {
    const s = new Set<TransferFilter>(['all']);
    if (u.kind === 'live') {
      const r = u.row;
      if (r.displayState === 'waiting') s.add('waiting');
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
  // unmount. No per-row timers, no leak.
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
