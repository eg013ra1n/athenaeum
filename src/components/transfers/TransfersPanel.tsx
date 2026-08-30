import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { X, Search, ArrowUp, ArrowDown, ArrowUpRight, Inbox, Users } from 'lucide-react';
import { api } from '../../api';
import { useTransfers } from '../../contexts/TransfersContext';
import { plainTransferError, displayStateChip, displayStateSubline, formatBytes } from './presentation';
import { transportHealthView } from './transportHealth';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  Direction,
  HistoryRow,
  InboundSummary,
  OutboundSummary,
  ProjectCard,
  QueuedInboundSummary,
  SyncFinishedEvent,
  SyncHistoryQuery,
  SyncProgressEvent,
} from '../../types/models';

type Tab = 'active' | 'history';
type DirFilter = 'all' | Direction;

/** One outbound package's live PRE-TRANSFER byte counter (transfer-prepare spec
 *  §7.1), harvested from `sync-progress` by the panel itself. The polled
 *  `OutboundSummary` carries no `bytesDone` — only the event stream does — and
 *  without it a staging row's mini-line would sit frozen at "0 of 340" for the
 *  whole preparation, which is the exact "is it doing anything?" gap this cycle
 *  closes. Entries exist ONLY while a package ticks `preparing`/`indexing`. */
interface PreparingBytes {
  bytesDone: number;
  /** The tick's own view of the package total. Kept for parity with
   *  `useTransferQueue`'s `LiveBytes`, but NOT what the mini-row divides by —
   *  see the render site: the denominator is the row's §D4-adjusted manifest
   *  total so both Transfers surfaces quote the same "/ Y". */
  bytesTotal: number;
}

const HISTORY_LIMIT = 200;
const POLL_MS = 5_000;

/** Tone class for a history-row outcome tag. */
function outcomeTone(outcome: string): string {
  if (outcome.startsWith('failed') || outcome.startsWith('rejected')) return 'text-error';
  if (outcome === 'cancelled') return 'text-content-muted';
  // A `sent` row is only a transport-handoff start-marker, NOT proof of
  // delivery — amber "awaiting confirmation". Success is reserved for the
  // settled verdicts (see `isDelivered`).
  if (outcome === 'sent') return 'text-warning';
  return 'text-success'; // ingested / duplicate / confirmed / replayed
}

function shortPeer(hex: string): string {
  const t = hex.trim();
  return t.length > 10 ? t.slice(0, 10) : t;
}

/** Fallback label for a project chip when the id → title map has no entry. */
function shortProject(id: string): string {
  const t = id.trim();
  return t.length > 8 ? t.slice(0, 8) : t;
}

/**
 * Duration + throughput for a finished history row, e.g. `4.2s · 18.7 MB/s`.
 * Returns `null` for in-flight rows (`finishedAt` null) or unparseable/negative
 * spans, so the caller can omit the line entirely.
 */
function transferStats(r: HistoryRow): string | null {
  if (!r.finishedAt) return null;
  const ms = new Date(r.finishedAt).getTime() - new Date(r.startedAt).getTime();
  if (!isFinite(ms) || ms < 0) return null;
  const secs = ms / 1000;
  // Round total seconds BEFORE splitting into m/s, else e.g. 119.7s renders as
  // "1m 60s" (Math.round(59.7) = 60). Sub-minute keeps the one-decimal form.
  const s = Math.round(secs);
  const dur = s >= 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${secs.toFixed(1)}s`;
  const mbs = r.bytes > 0 && secs > 0 ? ` · ${(r.bytes / 1048576 / secs).toFixed(1)} MB/s` : '';
  return `${dur}${mbs}`;
}

/**
 * A sender history row the peer confirmed it received — `ingested` (newly
 * stored) or `duplicate` (already had it). Either way the local copy is safe to
 * delete. Written only by the sender's `append_confirmed_history`; failed /
 * cancelled sends carry other outcomes and never qualify. Kept in lockstep with
 * the Perseus web page's "safe to delete" chip predicate.
 */
function isDelivered(r: HistoryRow): boolean {
  return r.direction === 'sent' && (r.outcome === 'ingested' || r.outcome === 'duplicate');
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
  const navigate = useNavigate();
  const incoming = status?.receiver.active ?? [];
  // Variant B: announces parked in a sending peer's lane with no `sync_inbound`
  // row yet. They belong in the same Active list as the rows — the batch has
  // arrived, and the panel is the at-a-glance answer to "is anything coming in?".
  const incomingQueued = status?.receiver.queued ?? [];
  const [tab, setTab] = useState<Tab>('active');
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [deviceNames, setDeviceNames] = useState<Record<string, string>>({});
  const [projectNames, setProjectNames] = useState<Record<string, string>>({});
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [search, setSearch] = useState('');
  const [dirFilter, setDirFilter] = useState<DirFilter>('all');
  const [preparingBytes, setPreparingBytes] = useState<Map<number, PreparingBytes>>(new Map());
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
      // No dedicated project-scoped SQL filter here — the project dimension is
      // shown as a per-row chip and matched by the free-text filter below.
      project: null,
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

  // Load the hub's node-id → device-name map once per panel open (NOT per poll
  // tick). Degrades to shortPeer hex on any failure / empty map (signed out or
  // hub unreachable) — the render falls back automatically.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    api
      .invoke<Record<string, string>>('get_sync_device_names')
      .then((names) => {
        if (!cancelled && mounted.current) setDeviceNames(names ?? {});
      })
      .catch((err) => console.error('[TransfersPanel] get_sync_device_names failed:', err));
    return () => {
      cancelled = true;
    };
  }, [open]);

  // Project id → title map (cache-only) so a collab history row's project chip
  // renders a name instead of the raw id. Degrades to a short id on any failure.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    api
      .invoke<ProjectCard[]>('list_collab_projects')
      .then((cards) => {
        if (cancelled || !mounted.current) return;
        setProjectNames(Object.fromEntries(cards.map((c) => [c.projectId, c.title])));
      })
      .catch((err) => console.error('[TransfersPanel] list_collab_projects failed:', err));
    return () => {
      cancelled = true;
    };
  }, [open]);

  // Live pre-transfer bytes for the outbound mini-rows (spec §7.1). Mount-scoped,
  // NOT gated on `open`: the panel is rendered unconditionally at app root, so
  // the counter is already warm when the user opens it mid-preparation.
  //
  // The map is self-bounding — an id enters only on a `preparing`/`indexing`
  // tick and is dropped again on ANY other sender stage (the `transferring`
  // counter restarts from zero and the row leaves the preparing-family branch)
  // or on `sync-finished`, so a cancelled/failed preparation leaves nothing
  // stale behind. Cancelled-flag listener form per CLAUDE.md (StrictMode-safe).
  //
  // The `sync-finished` arm is deliberately NOT folded into the panel's existing
  // one below: that listener is gated on `open` (it drives the re-poll), so a
  // preparation that ends while the slide-over is closed would leave its entry
  // behind. This one lives for the panel's whole mount, like the counter it clears.
  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenFinished: (() => void) | undefined;

    const dropEntry = (id: number) =>
      setPreparingBytes((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Map(prev);
        next.delete(id);
        return next;
      });

    api
      .listen<SyncProgressEvent>('sync-progress', (p) => {
        if (cancelled || p.direction !== 'sent') return;
        const id = Number(p.packageId);
        if (!Number.isFinite(id)) return;
        if (p.stage !== 'preparing' && p.stage !== 'indexing') {
          dropEntry(id);
          return;
        }
        if (p.bytesDone == null || p.bytesTotal == null) return;
        const entry: PreparingBytes = { bytesDone: p.bytesDone, bytesTotal: p.bytesTotal };
        setPreparingBytes((prev) => new Map(prev).set(id, entry));
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenProgress = fn;
      })
      .catch((err) => console.error('[TransfersPanel] sync-progress listen failed:', err));

    api
      .listen<SyncFinishedEvent>('sync-finished', (p) => {
        if (cancelled || p.direction !== 'sent') return;
        const id = Number(p.packageId);
        if (!Number.isFinite(id)) return;
        dropEntry(id);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenFinished = fn;
      })
      .catch((err) =>
        console.error('[TransfersPanel] preparing-bytes sync-finished listen failed:', err),
      );

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, []);

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
        r.peerDevice.toLowerCase().includes(q) ||
        (deviceNames[r.peerDevice] ?? '').toLowerCase().includes(q) ||
        (r.project ?? '').toLowerCase().includes(q) ||
        (r.project ? (projectNames[r.project] ?? '').toLowerCase().includes(q) : false),
    );
  }, [history, search, deviceNames, projectNames]);

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
          <div className="flex items-center gap-3">
            <h2 className="text-sm font-semibold">Transfers</h2>
            <button
              type="button"
              onClick={() => {
                closePanel();
                navigate('/transfers');
              }}
              className="flex items-center gap-0.5 text-xs text-content-muted transition-colors hover:text-accent"
            >
              Open full screen <ArrowUpRight size={12} />
            </button>
          </div>
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
          <div className="flex flex-col gap-1.5 border-b border-border px-4 py-2 text-xs text-content-muted">
            <div className="flex items-center gap-4">
              <span className="flex items-center gap-1" title="Queued + transferring out">
                <ArrowUp size={12} className="text-accent" />
                {status.sender.queued + status.sender.transferring} sending
              </span>
              <span className="flex items-center gap-1" title="Frames received">
                <ArrowDown size={12} />
                {status.receiver.receivedTotal} received
              </span>
            </div>
            {/* Transport-health line (Task 3.3): a coloured dot + human detail
                (relay url / NAT / no-relay-map warning). */}
            {(() => {
              const health = transportHealthView(status.transport);
              return (
                <span className="flex items-center gap-1.5" title={health.detail}>
                  <span className={`h-2 w-2 shrink-0 rounded-full ${health.dot}`} aria-hidden="true" />
                  <span className="truncate">{health.detail}</span>
                </span>
              );
            })()}
          </div>
        )}

        <div className="flex border-b border-border">
          {(['active', 'history'] as const).map((t) => {
            const activeCount = active.length + incoming.length + incomingQueued.length;
            return (
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
                {t === 'active' ? `Active${activeCount ? ` (${activeCount})` : ''}` : 'History'}
              </button>
            );
          })}
        </div>

        <div className="flex-1 overflow-y-auto">
          {tab === 'active' ? (
            <ActiveTab
              active={active}
              incoming={incoming}
              incomingQueued={incomingQueued}
              preparingBytes={preparingBytes}
            />
          ) : (
            <HistoryTab
              rows={filteredHistory}
              deviceNames={deviceNames}
              projectNames={projectNames}
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

function ActiveTab({
  active,
  incoming,
  incomingQueued,
  preparingBytes,
}: {
  active: OutboundSummary[];
  incoming: InboundSummary[];
  /** Variant-B lane-queue ghosts — announced, no `sync_inbound` row yet. */
  incomingQueued: QueuedInboundSummary[];
  /** Live pre-transfer bytes per outbound row id (spec §7.1), empty for every
   *  package that is not currently staging. */
  preparingBytes: Map<number, PreparingBytes>;
}) {
  if (active.length === 0 && incoming.length === 0 && incomingQueued.length === 0) {
    return (
      <p className="px-4 py-10 text-center text-sm text-content-muted">No active transfers</p>
    );
  }
  return (
    <ul className="divide-y divide-border">
      {/* Outbound mini-rows — same presentation model as the full page (batch
          name, device name, `displayState` chip incl. neutral waiting). The full
          unified view with actions + per-file detail lives on /transfers. */}
      {active.map((row) => {
        const chip = displayStateChip(row.displayState);
        const subline = displayStateSubline(row.displayState, 'outbound');
        const total = row.fileCounts.total || row.fileCount;
        // Same §D4 split as the page row (`TransferRow`): count what travels,
        // not the manifest — the peer's duplicates are settled `done` up front
        // and would otherwise read as progress. Tooltip carries the remainder;
        // the 10px line has no room for the suffix.
        const dup = row.fileCounts.duplicate;
        // Transfer-prepare spec §7.1: before the announce goes out no file has
        // moved, so "0 of 340" would sit frozen for the whole preparation. Show
        // the live byte fraction instead — the one thing actually advancing.
        // Denominator is the row's own §D4-adjusted manifest total (NOT the
        // tick's `bytesTotal`), so the mini-row and the full /transfers row
        // always quote the same "/ Y".
        const preparingLike = row.displayState === 'preparing' || row.displayState === 'indexing';
        const travelFiles = Math.max(0, total - dup);
        const travelBytes = Math.max(0, row.byteSize - row.fileCounts.duplicateBytes);
        const prepared = preparingBytes.get(row.id);
        return (
          <li key={`out-${row.id}`} className="flex items-start justify-between gap-2 px-4 py-3">
            <div className="min-w-0">
              <p className="truncate text-xs font-medium text-content" title={row.displayName ?? row.packageShort}>
                {row.displayName ?? row.packageShort}
              </p>
              <p className="mt-0.5 flex items-center gap-1 text-[10px] text-content-muted" title={row.deviceName ?? row.peerShort}>
                <ArrowUp size={9} className="shrink-0 text-accent" />
                <span className="truncate">{row.deviceName ?? row.peerShort}</span>
                <span aria-hidden="true">·</span>
                <span
                  className="shrink-0 tabular-nums"
                  title={dup > 0 ? `${dup} already on peer` : undefined}
                >
                  {preparingLike
                    ? `${formatBytes(prepared?.bytesDone ?? 0)} / ${formatBytes(travelBytes)}`
                    : `${Math.max(0, row.fileCounts.done - dup)} of ${travelFiles}`}
                </span>
              </p>
              {row.displayState === 'waiting' && (
                <p className="mt-0.5 text-[10px] text-content-secondary">retry soon</p>
              )}
              {/* D1: no "retry soon" here — a peer-absent row is not on a clock.
                  The subline below says what it IS waiting for. */}
              {subline && <p className="mt-0.5 text-[10px] text-content-muted">{subline}</p>}
              {/* Reason shows only while a retry is genuinely pending — never
                  gated on the monotonic attempts counter. */}
              {/* Suppressed under `queued_at_receiver` (variant A, review fix —
                  the page row already does this): the ack-ladder's stale "Peer
                  didn't respond" under a chip that SAYS the receiver is alive and
                  busy is the exact misreading the state exists to remove. */}
              {row.retrying && row.lastError && row.displayState !== 'queued_at_receiver' && (
                <p className="mt-0.5 truncate text-[10px] text-warning" title={row.lastError}>
                  {plainTransferError(row.lastError)}
                </p>
              )}
            </div>
            <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${chip.className}`}>
              {chip.label}
            </span>
          </li>
        );
      })}
      {incoming.map((row) => {
        const chip = displayStateChip(row.displayState);
        // D2: received rows can now be parked too (`waiting_peer`), so this branch
        // needs the same subline the outbound one has — otherwise the panel shows
        // a chip with no explanation of why nothing is moving. Variant C adds the
        // slot-parked `queued`, whose subline exists ONLY for the receive side —
        // hence the explicit kind.
        const subline = displayStateSubline(row.displayState, 'inbound');
        const total = row.fileCounts.total || row.frameCount;
        return (
          <li key={`in-${row.id}`} className="flex items-start justify-between gap-2 px-4 py-3">
            <div className="min-w-0">
              <p className="truncate text-xs font-medium text-content" title={row.displayName ?? row.packageShort}>
                {row.displayName ?? row.packageShort}
              </p>
              <p className="mt-0.5 flex items-center gap-1 text-[10px] text-content-muted" title={row.deviceName ?? row.peerShort}>
                <ArrowDown size={9} className="shrink-0 text-success" />
                <span className="truncate">{row.deviceName ?? row.peerShort}</span>
                <span aria-hidden="true">·</span>
                <span className="shrink-0 tabular-nums">
                  {row.fileCounts.done} of {total}
                </span>
              </p>
              {subline && <p className="mt-0.5 text-[10px] text-content-muted">{subline}</p>}
            </div>
            <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${chip.className}`}>
              {chip.label}
            </span>
          </li>
        );
      })}
      {/* Variant-B ghosts, last: a compact line per announce still sitting in its
          sender's lane queue. Same chip + subline as a slot-parked real row (they
          are one fact to the user), just without the per-file counter — there is
          no row to count against yet, only the announce's manifest totals. */}
      {incomingQueued.map((row) => {
        const chip = displayStateChip('queued');
        const subline = displayStateSubline('queued', 'inbound');
        return (
          <li
            key={`inq-${row.batchUuid}`}
            className="flex items-start justify-between gap-2 px-4 py-3"
          >
            <div className="min-w-0">
              <p
                className="truncate text-xs font-medium text-content"
                title={row.batchName ?? row.batchUuid}
              >
                {row.batchName ?? shortPeer(row.batchUuid)}
              </p>
              <p
                className="mt-0.5 flex items-center gap-1 text-[10px] text-content-muted"
                title={row.deviceName ?? row.peerShort}
              >
                <ArrowDown size={9} className="shrink-0 text-success" />
                <span className="truncate">{row.deviceName ?? row.peerShort}</span>
                <span aria-hidden="true">·</span>
                <span className="shrink-0 tabular-nums">
                  {row.frameCount} file{row.frameCount === 1 ? '' : 's'}
                </span>
              </p>
              {subline && <p className="mt-0.5 text-[10px] text-content-muted">{subline}</p>}
            </div>
            <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${chip.className}`}>
              {chip.label}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

interface HistoryTabProps {
  rows: HistoryRow[];
  deviceNames: Record<string, string>;
  projectNames: Record<string, string>;
  loading: boolean;
  search: string;
  onSearch: (v: string) => void;
  dirFilter: DirFilter;
  onDirFilter: (d: DirFilter) => void;
}

function HistoryTab({
  rows,
  deviceNames,
  projectNames,
  loading,
  search,
  onSearch,
  dirFilter,
  onDirFilter,
}: HistoryTabProps) {
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
          {rows.map((r, i) => {
            const peerLabel = deviceNames[r.peerDevice] ?? shortPeer(r.peerDevice);
            const stats = transferStats(r);
            const delivered = isDelivered(r);
            return (
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
                  <span
                    className={`shrink-0 text-[11px] font-medium ${outcomeTone(r.outcome)}`}
                    title={r.outcome}
                  >
                    {r.outcome === 'sent' ? 'sent — awaiting confirmation' : r.outcome}
                  </span>
                </div>
                <p className="mt-0.5 flex items-center gap-2 pl-5 text-[10px] text-content-muted">
                  {r.batchName && (
                    <span className="truncate font-medium text-content-secondary" title={r.batchName}>
                      {r.batchName}
                    </span>
                  )}
                  {r.object && <span className="truncate">{r.object}</span>}
                  {r.project && (
                    <span
                      className="inline-flex shrink-0 items-center gap-0.5 rounded bg-accent/15 px-1 py-0.5 text-[9px] text-accent"
                      title={r.project}
                    >
                      <Users size={9} />
                      <span className="max-w-[8rem] truncate">
                        {projectNames[r.project] ?? shortProject(r.project)}
                      </span>
                    </span>
                  )}
                  <span title={r.peerDevice}>{peerLabel}</span>
                  <span className="ml-auto shrink-0">{formatTimestamp(r.startedAt)}</span>
                </p>
                {(delivered || stats) && (
                  <p className="mt-0.5 flex items-center gap-2 pl-5 text-[10px] text-content-muted">
                    {delivered && (
                      <span className="font-medium text-success">✓ delivered — safe to delete</span>
                    )}
                    {stats && <span className="ml-auto shrink-0 tabular-nums">{stats}</span>}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
