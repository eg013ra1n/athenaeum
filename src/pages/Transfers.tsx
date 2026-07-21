import { useEffect, useMemo, useState } from 'react';
import { ArrowLeftRight } from 'lucide-react';
import { api } from '../api';
import { AppDataWarningStrip } from '../components/transfers/AppDataWarningStrip';
import { TransferRow } from '../components/transfers/TransferRow';
import { TransferDetail } from '../components/transfers/TransferDetail';
import { groupHasFailure, groupHasSuccess } from '../components/transfers/historyGrouping';
import type { TransferFilter, UnifiedRow } from '../components/transfers/types';
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
  const { rows, liveFiles, sendNow, cancelOutbound, cancelInbound, resend, busy } = useTransferQueue();
  const { groups, deviceNames, projectNames, refetch } = useTransferHistory();

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

  // Merge live rows + history groups into one list, de-duping any history group
  // already represented by a live row (a lingering failed/cancelled ledger row
  // wins — it carries live detail + a Resend action). Live keys: an outbound
  // row's `packageShort` is the truncated (≤10-char) prefix of the package-dir
  // basename that a `sent` history row stamps as its full `packageId`; an
  // inbound row's full `packageId` equals a `received` history row's `packageId`.
  const unified = useMemo<UnifiedRow[]>(() => {
    const liveSentPrefixes: string[] = [];
    const liveRecvIds = new Set<string>();
    for (const r of rows) {
      if (r.kind === 'outbound') liveSentPrefixes.push(r.packageShort);
      else if (r.packageId) liveRecvIds.add(r.packageId);
    }

    const liveUnified: UnifiedRow[] = rows.map((r) => ({ kind: 'live', selKey: r.key, row: r }));

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
      });
    }
    return [...liveUnified, ...historyUnified];
  }, [rows, groups, deviceNames, projectNames]);

  // Filter membership (§D8). A row can match more than one bucket (a mixed
  // history group is both Completed and Failed); counts reflect that honestly.
  const bucketsFor = (u: UnifiedRow): Set<TransferFilter> => {
    const s = new Set<TransferFilter>(['all']);
    if (u.kind === 'live') {
      const r = u.row;
      if (r.displayState === 'waiting') s.add('waiting');
      else if (r.terminal) s.add(r.displayState === 'failed' ? 'failed' : 'completed');
      else s.add(r.kind === 'outbound' ? 'sending' : 'receiving');
    } else {
      if (groupHasSuccess(u.group)) s.add('completed');
      if (groupHasFailure(u.group)) s.add('failed');
      if (!groupHasSuccess(u.group) && !groupHasFailure(u.group)) s.add('completed');
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
