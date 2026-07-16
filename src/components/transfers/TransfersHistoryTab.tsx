import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ArrowUp, ArrowDown, ChevronDown, ChevronRight, Search, Users } from 'lucide-react';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import type { Direction, HistoryRow, ProjectCard, SyncHistoryQuery } from '../../types/models';

const HISTORY_LIMIT = 500;
const POLL_MS = 5_000;

type DirFilter = 'all' | Direction;

function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

function shortProject(id: string): string {
  const t = id.trim();
  return t.length > 8 ? t.slice(0, 8) : t;
}

function outcomeTone(outcome: string): string {
  if (outcome.startsWith('failed') || outcome.startsWith('rejected')) return 'text-error';
  if (outcome === 'cancelled') return 'text-content-muted';
  return 'text-success'; // sent / ingested / duplicate / confirmed / replayed
}

/** A batch of `HistoryRow`s sharing the same `(direction, packageId)` key —
 * rows with `packageId: null` (legacy, pre-Task-14) all fall into one
 * "earlier" bucket per direction. Rows arrive newest-first from the backend,
 * so bucket order (first-seen) is already a reasonable recency sort. */
interface HistoryGroup {
  groupKey: string;
  packageId: string | null;
  direction: Direction;
  peerDevice: string;
  project: string | null;
  startedAt: string;
  /** `null` while any row in the group is still in flight (`finishedAt == null`). */
  finishedAt: string | null;
  totalBytes: number;
  outcomeCounts: Record<string, number>;
  rows: HistoryRow[];
}

function groupHistory(rows: HistoryRow[]): HistoryGroup[] {
  const byKey = new Map<string, HistoryRow[]>();
  const order: string[] = [];
  for (const r of rows) {
    const key = `${r.direction}:${r.packageId ?? '__earlier__'}`;
    if (!byKey.has(key)) {
      byKey.set(key, []);
      order.push(key);
    }
    byKey.get(key)!.push(r);
  }
  return order.map((key) => {
    const grouped = byKey.get(key)!;
    const first = grouped[0];
    const totalBytes = grouped.reduce((sum, r) => sum + r.bytes, 0);
    const anyInFlight = grouped.some((r) => !r.finishedAt);
    const finishedAt = anyInFlight
      ? null
      : grouped.reduce<string>((max, r) => (r.finishedAt! > max ? r.finishedAt! : max), grouped[0].finishedAt!);
    const startedAt = grouped.reduce<string>((min, r) => (r.startedAt < min ? r.startedAt : min), first.startedAt);
    const outcomeCounts: Record<string, number> = {};
    for (const r of grouped) outcomeCounts[r.outcome] = (outcomeCounts[r.outcome] ?? 0) + 1;
    return {
      groupKey: key,
      packageId: first.packageId,
      direction: first.direction,
      peerDevice: first.peerDevice,
      project: first.project,
      startedAt,
      finishedAt,
      totalBytes,
      outcomeCounts,
      rows: grouped,
    };
  });
}

/**
 * Grouped transfer History tab (Task 15) — the full-page counterpart of
 * `TransfersPanel`'s `HistoryTab`, batching rows by `packageId` into
 * collapsible groups. Owns its own fetch/poll/filter state (mirrors the
 * panel's HistoryTab internals) rather than sharing them, since the panel
 * stays mounted independently as a quick-glance surface.
 */
export function TransfersHistoryTab() {
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [deviceNames, setDeviceNames] = useState<Record<string, string>>({});
  const [projectNames, setProjectNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [search, setSearch] = useState('');
  const [dirFilter, setDirFilter] = useState<DirFilter>('all');
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const fetchHistory = useCallback(() => {
    setLoading(true);
    const query: SyncHistoryQuery = {
      filename: null,
      object: null,
      direction: dirFilter === 'all' ? null : dirFilter,
      peer: null,
      project: null,
      limit: HISTORY_LIMIT,
    };
    api
      .invoke<HistoryRow[]>('list_sync_history', { query })
      .then((rows) => {
        if (mounted.current) setHistory(rows);
      })
      .catch((err) => console.error('[TransfersHistoryTab] list_sync_history failed:', err))
      .finally(() => {
        if (mounted.current) setLoading(false);
      });
  }, [dirFilter]);

  useEffect(() => {
    fetchHistory();
  }, [fetchHistory]);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<Record<string, string>>('get_sync_device_names')
      .then((names) => {
        if (!cancelled && mounted.current) setDeviceNames(names ?? {});
      })
      .catch((err) => console.error('[TransfersHistoryTab] get_sync_device_names failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<ProjectCard[]>('list_collab_projects')
      .then((cards) => {
        if (cancelled || !mounted.current) return;
        setProjectNames(Object.fromEntries(cards.map((c) => [c.projectId, c.title])));
      })
      .catch((err) => console.error('[TransfersHistoryTab] list_collab_projects failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Periodic re-poll — a full page has no natural "closed" state to gate on,
  // so this tab polls unconditionally while mounted (mirrors the panel's 5s
  // cadence); `sync-finished` isn't separately listened to here since the
  // shared `useTransferQueue`/`useSyncStatus` machinery already reacts to it
  // for the Active tab and this tab's own interval keeps history fresh.
  useEffect(() => {
    const id = setInterval(fetchHistory, POLL_MS);
    return () => clearInterval(id);
  }, [fetchHistory]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return history;
    return history.filter(
      (r) =>
        r.filename.toLowerCase().includes(q) ||
        (r.object ?? '').toLowerCase().includes(q) ||
        r.peerDevice.toLowerCase().includes(q) ||
        (deviceNames[r.peerDevice] ?? '').toLowerCase().includes(q) ||
        (r.project ?? '').toLowerCase().includes(q) ||
        (r.project ? (projectNames[r.project] ?? '').toLowerCase().includes(q) : false),
    );
  }, [history, search, deviceNames, projectNames]);

  const groups = useMemo(() => groupHistory(filtered), [filtered]);

  const toggleGroup = useCallback((key: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const chips: DirFilter[] = ['all', 'sent', 'received'];

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2">
        <div className="relative min-w-[16rem] flex-1">
          <Search
            size={13}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-content-muted"
          />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Filter by filename, object, or peer"
            className="w-full rounded border border-border bg-surface py-1.5 pl-7 pr-2 text-xs text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
          />
        </div>
        <div className="flex gap-1">
          {chips.map((c) => (
            <button
              key={c}
              type="button"
              onClick={() => setDirFilter(c)}
              className={`rounded px-2 py-1 text-[11px] capitalize transition-colors ${
                dirFilter === c
                  ? 'bg-accent text-surface'
                  : 'bg-surface text-content-muted hover:bg-surface-hover hover:text-content'
              }`}
            >
              {c}
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading && groups.length === 0 ? (
          <p className="px-4 py-10 text-center text-sm text-content-muted">Loading…</p>
        ) : groups.length === 0 ? (
          <p className="px-4 py-10 text-center text-sm text-content-muted">No transfer history</p>
        ) : (
          <ul className="divide-y divide-border">
            {groups.map((g) => {
              const isOpen = expandedGroups.has(g.groupKey);
              const peerLabel = deviceNames[g.peerDevice] ?? shortPeer(g.peerDevice);
              return (
                <li key={g.groupKey}>
                  <button
                    type="button"
                    onClick={() => toggleGroup(g.groupKey)}
                    className="flex w-full items-center gap-2 px-4 py-2.5 text-left transition-colors hover:bg-surface-hover"
                  >
                    {isOpen ? (
                      <ChevronDown size={14} className="shrink-0 text-content-muted" />
                    ) : (
                      <ChevronRight size={14} className="shrink-0 text-content-muted" />
                    )}
                    {g.direction === 'sent' ? (
                      <ArrowUp size={13} className="shrink-0 text-accent" />
                    ) : (
                      <ArrowDown size={13} className="shrink-0 text-success" />
                    )}
                    <span className="text-xs text-content-secondary" title={g.peerDevice}>
                      {peerLabel}
                    </span>
                    {g.project && (
                      <span
                        className="inline-flex shrink-0 items-center gap-0.5 rounded bg-accent/15 px-1 py-0.5 text-[9px] text-accent"
                        title={g.project}
                      >
                        <Users size={9} />
                        <span className="max-w-[8rem] truncate">
                          {projectNames[g.project] ?? shortProject(g.project)}
                        </span>
                      </span>
                    )}
                    <span className="text-[11px] text-content-muted">
                      {g.rows.length} file{g.rows.length === 1 ? '' : 's'} · {formatBytesShort(g.totalBytes)}
                    </span>
                    <span className="ml-auto flex shrink-0 items-center gap-1">
                      {Object.entries(g.outcomeCounts).map(([outcome, count]) => (
                        <span key={outcome} className={`text-[10px] font-medium ${outcomeTone(outcome)}`}>
                          {count} {outcome}
                        </span>
                      ))}
                    </span>
                    <span className="shrink-0 text-[10px] text-content-muted">
                      {formatTimestamp(g.finishedAt ?? g.startedAt)}
                    </span>
                  </button>
                  {isOpen && (
                    <ul className="divide-y divide-border bg-surface px-4 pb-2">
                      {g.rows.map((r, i) => (
                        <li key={`${r.frameUuid}-${r.startedAt}-${i}`} className="py-2 pl-6">
                          <div className="flex items-center gap-2">
                            <span
                              className="min-w-0 flex-1 truncate text-xs text-content-secondary"
                              title={r.filename}
                            >
                              {r.filename}
                            </span>
                            <span className={`shrink-0 text-[11px] font-medium ${outcomeTone(r.outcome)}`}>
                              {r.outcome}
                            </span>
                          </div>
                          <p className="mt-0.5 flex items-center gap-2 text-[10px] text-content-muted">
                            {r.object && <span className="truncate">{r.object}</span>}
                            <span className="ml-auto shrink-0">{formatTimestamp(r.startedAt)}</span>
                          </p>
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

function formatBytesShort(n: number): string {
  if (!isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
