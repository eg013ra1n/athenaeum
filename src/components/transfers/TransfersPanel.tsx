import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { X, Search, ArrowUp, ArrowDown, Inbox } from 'lucide-react';
import { api } from '../../api';
import { useTransfers } from '../../contexts/TransfersContext';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  Direction,
  HistoryRow,
  OutboundState,
  OutboundSummary,
  SyncHistoryQuery,
} from '../../types/models';

type Tab = 'active' | 'history';
type DirFilter = 'all' | Direction;

const HISTORY_LIMIT = 200;
const POLL_MS = 5_000;

/** Tone class for an in-flight outbound package state. */
function stateTone(state: OutboundState): string {
  switch (state) {
    case 'confirmed':
      return 'text-success';
    case 'failed':
      return 'text-error';
    case 'transferring':
    case 'delivered':
      return 'text-accent';
    default: // queued / announced
      return 'text-content-muted';
  }
}

/** Tone class for a history-row outcome tag. */
function outcomeTone(outcome: string): string {
  if (outcome.startsWith('failed') || outcome.startsWith('rejected')) return 'text-error';
  if (outcome === 'cancelled') return 'text-content-muted';
  return 'text-success'; // sent / ingested / duplicate / confirmed / replayed
}

function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

/**
 * Transfers slide-over (task M3) — mounted at app root (NotificationPanel
 * pattern), opened from `TransferIndicator`. Active tab lists the shared
 * `useSyncStatus` in-flight rows; History tab queries `list_sync_history`
 * (direction filter SQL-side, free-text search filtered client-side over the
 * fetched page). While open it re-polls every 5s and on every `sync-finished`.
 */
export function TransfersPanel() {
  const { open, closePanel, status, active, refresh } = useTransfers();
  const [tab, setTab] = useState<Tab>('active');
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [search, setSearch] = useState('');
  const [dirFilter, setDirFilter] = useState<DirFilter>('all');
  const mounted = useRef(true);
  const closeBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const fetchHistory = useCallback(() => {
    setLoadingHistory(true);
    const query: SyncHistoryQuery = {
      filename: null,
      object: null,
      direction: dirFilter === 'all' ? null : dirFilter,
      peer: null,
      limit: HISTORY_LIMIT,
    };
    api
      .invoke<HistoryRow[]>('list_sync_history', { query })
      .then((rows) => {
        if (mounted.current) setHistory(rows);
      })
      .catch((err) => console.error('[TransfersPanel] list_sync_history failed:', err))
      .finally(() => {
        if (mounted.current) setLoadingHistory(false);
      });
  }, [dirFilter]);

  // Immediate history (re)fetch when the tab/filter opens or changes.
  useEffect(() => {
    if (!open || tab !== 'history') return;
    fetchHistory();
  }, [open, tab, fetchHistory]);

  // Escape closes the panel (mirrors NotificationPanel), focus the close button.
  useEffect(() => {
    if (!open) return;
    closeBtnRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closePanel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, closePanel]);

  // While open: 5s re-poll + refresh on `sync-finished`. Stable via a ref so the
  // Tauri listener is not re-subscribed on every keystroke (StrictMode-safe).
  const tickRef = useRef<() => void>(() => {});
  useEffect(() => {
    tickRef.current = () => {
      refresh();
      if (tab === 'history') fetchHistory();
    };
  }, [refresh, tab, fetchHistory]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const id = setInterval(() => tickRef.current(), POLL_MS);
    api
      .listen<unknown>('sync-finished', () => {
        if (!cancelled) tickRef.current();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[TransfersPanel] sync-finished listen failed:', err));
    return () => {
      cancelled = true;
      clearInterval(id);
      unlisten?.();
    };
  }, [open]);

  const filteredHistory = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return history;
    return history.filter(
      (r) =>
        r.filename.toLowerCase().includes(q) ||
        (r.object ?? '').toLowerCase().includes(q) ||
        r.peerDevice.toLowerCase().includes(q),
    );
  }, [history, search]);

  return (
    <>
      {open && (
        <div className="fixed inset-0 z-[55] bg-black/50" onClick={closePanel} aria-hidden="true" />
      )}
      <aside
        role="dialog"
        aria-label="Transfers"
        aria-hidden={!open}
        className={`fixed inset-y-0 right-0 z-[60] flex w-96 max-w-[90vw] flex-col border-l border-border bg-surface-elevated shadow-xl transition-transform duration-200 ${
          open ? 'translate-x-0' : 'translate-x-full pointer-events-none'
        }`}
      >
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <h2 className="text-sm font-semibold">Transfers</h2>
          <button
            ref={closeBtnRef}
            type="button"
            onClick={closePanel}
            aria-label="Close transfers"
            className="rounded p-1 text-content-muted transition-colors hover:bg-surface-hover hover:text-content"
          >
            <X size={16} />
          </button>
        </div>

        {status && (
          <div className="flex items-center gap-4 border-b border-border px-4 py-2 text-xs text-content-muted">
            <span className="flex items-center gap-1" title="Queued + transferring out">
              <ArrowUp size={12} className="text-accent" />
              {status.sender.queued + status.sender.transferring} sending
            </span>
            <span className="flex items-center gap-1" title="Frames received">
              <ArrowDown size={12} />
              {status.receiver.receivedTotal} received
            </span>
            <span className="ml-auto truncate" title="Pairing status">
              {status.pairing.kind === 'paired'
                ? `→ ${status.pairing.peerShort ?? ''}`
                : status.pairing.kind === 'disabled'
                  ? 'not sending'
                  : status.pairing.kind === 'devTicket'
                    ? 'dev pairing'
                    : 'signed out'}
            </span>
          </div>
        )}

        <div className="flex border-b border-border">
          {(['active', 'history'] as const).map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setTab(t)}
              className={`flex-1 px-4 py-2 text-xs font-medium capitalize transition-colors ${
                tab === t
                  ? 'border-b-2 border-accent text-content'
                  : 'text-content-muted hover:text-content'
              }`}
            >
              {t === 'active' ? `Active${active.length ? ` (${active.length})` : ''}` : 'History'}
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto">
          {tab === 'active' ? (
            <ActiveTab active={active} />
          ) : (
            <HistoryTab
              rows={filteredHistory}
              loading={loadingHistory}
              search={search}
              onSearch={setSearch}
              dirFilter={dirFilter}
              onDirFilter={setDirFilter}
            />
          )}
        </div>
      </aside>
    </>
  );
}

function ActiveTab({ active }: { active: OutboundSummary[] }) {
  if (active.length === 0) {
    return (
      <p className="px-4 py-10 text-center text-sm text-content-muted">No active transfers</p>
    );
  }
  return (
    <ul className="divide-y divide-border">
      {active.map((row) => (
        <li key={row.id} className="flex items-center justify-between gap-2 px-4 py-3">
          <div className="min-w-0">
            <p className="truncate font-mono text-xs text-content-secondary" title={row.packageShort}>
              {row.packageShort}
            </p>
            <p className="mt-0.5 text-[10px] text-content-muted" title={`to ${row.peerShort}`}>
              → {row.peerShort}
              {row.attempts > 0 ? ` · attempt ${row.attempts + 1}` : ''}
            </p>
          </div>
          <span className={`shrink-0 text-xs font-medium ${stateTone(row.state)}`}>{row.state}</span>
        </li>
      ))}
    </ul>
  );
}

interface HistoryTabProps {
  rows: HistoryRow[];
  loading: boolean;
  search: string;
  onSearch: (v: string) => void;
  dirFilter: DirFilter;
  onDirFilter: (d: DirFilter) => void;
}

function HistoryTab({ rows, loading, search, onSearch, dirFilter, onDirFilter }: HistoryTabProps) {
  const chips: DirFilter[] = ['all', 'sent', 'received'];
  return (
    <div>
      <div className="space-y-2 border-b border-border px-4 py-2">
        <div className="relative">
          <Search
            size={13}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-content-muted"
          />
          <input
            type="text"
            value={search}
            onChange={(e) => onSearch(e.target.value)}
            placeholder="Filter by filename, object, or peer"
            className="w-full rounded border border-border bg-surface py-1.5 pl-7 pr-2 text-xs text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
          />
        </div>
        <div className="flex gap-1">
          {chips.map((c) => (
            <button
              key={c}
              type="button"
              onClick={() => onDirFilter(c)}
              className={`rounded px-2 py-0.5 text-[11px] capitalize transition-colors ${
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

      {loading && rows.length === 0 ? (
        <p className="px-4 py-10 text-center text-sm text-content-muted">Loading…</p>
      ) : rows.length === 0 ? (
        <p className="px-4 py-10 text-center text-sm text-content-muted">No transfer history</p>
      ) : (
        <ul className="divide-y divide-border">
          {rows.map((r, i) => (
            <li key={`${r.frameUuid}-${r.startedAt}-${i}`} className="px-4 py-2.5">
              <div className="flex items-center gap-2">
                {r.direction === 'sent' ? (
                  <ArrowUp size={12} className="shrink-0 text-content-muted" />
                ) : (
                  <Inbox size={12} className="shrink-0 text-content-muted" />
                )}
                <span className="min-w-0 flex-1 truncate text-xs text-content-secondary" title={r.filename}>
                  {r.filename}
                </span>
                <span className={`shrink-0 text-[11px] font-medium ${outcomeTone(r.outcome)}`}>
                  {r.outcome}
                </span>
              </div>
              <p className="mt-0.5 flex items-center gap-2 pl-5 text-[10px] text-content-muted">
                {r.object && <span className="truncate">{r.object}</span>}
                <span title={r.peerDevice}>{shortPeer(r.peerDevice)}</span>
                <span className="ml-auto shrink-0">{formatTimestamp(r.startedAt)}</span>
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
