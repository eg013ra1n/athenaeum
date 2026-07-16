import { useCallback, useEffect, useRef, useState } from 'react';
import { ArrowUp, ArrowDown, ChevronDown, ChevronRight, Loader2, RotateCw, Send, X } from 'lucide-react';
import { api } from '../../api';
import type { TransferRow } from '../../hooks/useTransferQueue';
import type { Direction, TransferFileEntry } from '../../types/models';

/** Local byte formatter — mirrors `src/components/collab/format.ts`'s
 * `formatBytes`, kept local rather than cross-imported from the collab
 * feature directory (same micro-util-duplication convention as `shortPeer`
 * across `useSyncStatus.ts` / `TransfersPanel.tsx`). */
function formatBytes(n: number): string {
  if (!isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatSpeed(bps: number | null): string | null {
  if (bps == null || !isFinite(bps) || bps <= 0) return null;
  return `${formatBytes(bps)}/s`;
}

function formatCountdown(nextRetryAt: string, now: number): string {
  const deadline = new Date(nextRetryAt).getTime();
  const remainingMs = Math.max(0, deadline - now);
  const totalSec = Math.ceil(remainingMs / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${s.toString().padStart(2, '0')}`;
}

/** State-staged progress fallback (0..1) for a row with no byte-level data yet. */
function stageProgress(state: string, bytesDone: number, byteSize: number): number {
  if (byteSize > 0 && bytesDone > 0) return Math.min(1, bytesDone / byteSize);
  switch (state) {
    case 'queued':
      return 0.02;
    case 'announced':
      return 0.08;
    case 'transferring':
    case 'fetching':
      return 0.5;
    case 'delivered':
    case 'ingesting':
      return 0.95;
    case 'confirmed':
    case 'done':
      return 1;
    default:
      return 0;
  }
}

function stateTone(state: string): string {
  switch (state) {
    case 'confirmed':
    case 'done':
      return 'text-success';
    case 'failed':
      return 'text-error';
    case 'cancelled':
      return 'text-content-muted';
    case 'transferring':
    case 'delivered':
    case 'fetching':
    case 'ingesting':
      return 'text-accent';
    default: // queued / announced
      return 'text-content-muted';
  }
}

function outcomeTone(outcome: string): string {
  if (outcome === 'ingested') return 'bg-success/15 text-success';
  if (outcome === 'duplicate') return 'bg-warning/15 text-warning';
  if (outcome === 'rejected' || outcome === 'cancelled') return 'bg-error/15 text-error';
  return 'bg-surface-hover text-content-muted';
}

interface ActiveTransferRowProps {
  row: TransferRow;
  busy: Set<string>;
  liveFiles: Map<string, Map<string, { bytesDone: number; bytesTotal: number }>>;
  onSendNow: (id: number) => void;
  onCancelOutbound: (id: number) => void;
  onCancelInbound: (packageId: string) => void;
  onResend: (id: number) => void;
}

export function ActiveTransferRow({
  row,
  busy,
  liveFiles,
  onSendNow,
  onCancelOutbound,
  onCancelInbound,
  onResend,
}: ActiveTransferRowProps) {
  const [expanded, setExpanded] = useState(false);
  const [files, setFiles] = useState<TransferFileEntry[] | null>(null);
  const [filesLoading, setFilesLoading] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // 1s local countdown tick, only while this row is actually waiting out a
  // backoff window (nextRetryAt set) — no timer otherwise.
  useEffect(() => {
    if (!row.nextRetryAt) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [row.nextRetryAt]);

  const loadFiles = useCallback(() => {
    setFilesLoading(true);
    const direction: Direction = row.kind === 'outbound' ? 'sent' : 'received';
    api
      .invoke<TransferFileEntry[]>('list_transfer_files', { direction, id: row.id })
      .then(setFiles)
      .catch((err) => {
        console.error('[ActiveTransferRow] list_transfer_files failed:', err);
        setFiles([]);
      })
      .finally(() => setFilesLoading(false));
  }, [row.kind, row.id]);

  const toggleExpand = useCallback(() => {
    setExpanded((prev) => {
      const next = !prev;
      if (next) {
        loadFiles();
      } else {
        // Collapsed — drop the cached detail so a later re-expand re-fetches
        // rather than showing whatever was true at the last expand.
        setFiles(null);
      }
      return next;
    });
  }, [loadFiles]);

  // The row's `key` (and so this component instance) is stable across the
  // active→terminal transition (same outbound id / inbound packageId), so an
  // already-expanded row survives a package finishing. `finishNonce` bumps
  // exactly once per `sync-finished` for this package — re-fetch the cached
  // detail then (settled outcome chips, or a cancel/failure) instead of
  // leaving the pre-finish snapshot on screen. Skipped on the initial mount
  // (no transition happened yet) via the "did the value actually change"
  // check against the previous render's nonce.
  const prevFinishNonceRef = useRef(row.finishNonce);
  useEffect(() => {
    if (row.finishNonce !== prevFinishNonceRef.current) {
      prevFinishNonceRef.current = row.finishNonce;
      if (expanded) loadFiles();
    }
  }, [row.finishNonce, expanded, loadFiles]);

  const pending = !row.terminal && (row.state === 'queued' || row.state === 'announced');
  const stalled = row.kind === 'outbound' && pending && row.attempts > 0;
  const canSendNow = row.kind === 'outbound' && pending;
  const canCancel =
    (row.kind === 'outbound' && pending) ||
    (row.kind === 'inbound' && (row.state === 'announced' || row.state === 'fetching'));
  const canResend = row.kind === 'outbound' && row.terminal;

  const progress = stageProgress(row.state, row.bytesDone, row.byteSize);
  const speedLabel = row.isTransferring ? formatSpeed(row.speedBps) : null;

  const sendBusy = busy.has(`send:${row.id}`);
  const cancelBusy =
    row.kind === 'outbound' ? busy.has(`cancel:${row.id}`) : busy.has(`cancelin:${row.packageId ?? ''}`);
  const resendBusy = busy.has(`resend:${row.id}`);

  const liveFilesForPkg = row.packageId ? liveFiles.get(row.packageId) : undefined;

  return (
    <>
      <tr
        className="cursor-pointer border-b border-border transition-colors hover:bg-surface-hover"
        onClick={toggleExpand}
      >
        <td className="w-6 px-2 py-2 text-content-muted">
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </td>
        <td className="w-6 px-1 py-2" title={row.kind === 'outbound' ? 'Sending' : 'Receiving'}>
          {row.kind === 'outbound' ? (
            <ArrowUp size={14} className="text-accent" />
          ) : (
            <ArrowDown size={14} className="text-success" />
          )}
        </td>
        <td className="px-2 py-2 font-mono text-xs text-content-secondary" title={row.peerShort}>
          {row.peerShort}
        </td>
        <td className="px-2 py-2">
          <span className="font-mono text-xs text-content-secondary" title={row.packageShort}>
            {row.packageShort}
          </span>
          <span className="ml-2 text-[11px] text-content-muted">{row.fileCount} files</span>
        </td>
        <td className="px-2 py-2">
          <span className={`text-xs font-medium ${stateTone(row.state)}`}>{row.state}</span>
          {stalled && (
            <span className="ml-1.5 rounded bg-warning/20 px-1 py-0.5 text-[10px] font-medium text-warning">
              stalled
            </span>
          )}
        </td>
        <td className="px-2 py-2">
          <div className="h-1.5 w-40 overflow-hidden rounded-full bg-surface-hover">
            <div
              className={`h-full rounded-full transition-all ${row.terminal ? 'bg-content-muted' : 'bg-accent'}`}
              style={{ width: `${Math.round(progress * 100)}%` }}
            />
          </div>
        </td>
        <td className="px-2 py-2 text-right text-xs text-content-muted tabular-nums">
          {formatBytes(row.byteSize)}
        </td>
        <td className="px-2 py-2 text-right text-xs text-content-muted tabular-nums">
          {speedLabel ?? '—'}
        </td>
        <td className="px-2 py-2 text-xs text-content-muted tabular-nums">
          {row.attempts > 0 ? (
            <span>
              attempt {row.attempts + 1}
              {row.nextRetryAt && <span className="ml-1">· {formatCountdown(row.nextRetryAt, now)}</span>}
            </span>
          ) : (
            '—'
          )}
        </td>
        <td className="px-2 py-2 text-right" onClick={(e) => e.stopPropagation()}>
          <div className="flex items-center justify-end gap-1">
            {canSendNow && (
              <button
                type="button"
                disabled={sendBusy}
                onClick={() => onSendNow(row.id)}
                title="Send now"
                className="inline-flex items-center gap-1 rounded border border-border px-2 py-1 text-[11px] text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {sendBusy ? <Loader2 size={11} className="animate-spin" /> : <Send size={11} />}
                Send now
              </button>
            )}
            {canCancel && (
              <button
                type="button"
                disabled={cancelBusy}
                onClick={() =>
                  row.kind === 'outbound' ? onCancelOutbound(row.id) : onCancelInbound(row.packageId ?? '')
                }
                title="Cancel"
                className="inline-flex items-center gap-1 rounded border border-error/40 px-2 py-1 text-[11px] text-error transition-colors hover:bg-error/10 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {cancelBusy ? <Loader2 size={11} className="animate-spin" /> : <X size={11} />}
                Cancel
              </button>
            )}
            {canResend && (
              <button
                type="button"
                disabled={resendBusy}
                onClick={() => onResend(row.id)}
                title="Resend"
                className="inline-flex items-center gap-1 rounded bg-accent px-2 py-1 text-[11px] text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
              >
                {resendBusy ? <Loader2 size={11} className="animate-spin" /> : <RotateCw size={11} />}
                Resend
              </button>
            )}
          </div>
        </td>
      </tr>
      {expanded && (
        <tr className="border-b border-border bg-surface">
          <td colSpan={9} className="px-8 py-2">
            {filesLoading || files === null ? (
              <p className="py-2 text-xs text-content-muted">Loading files…</p>
            ) : files.length === 0 ? (
              <p className="py-2 text-xs text-content-muted">No file detail yet.</p>
            ) : (
              <ul className="space-y-1 py-1">
                {files.map((f) => {
                  const live = liveFilesForPkg?.get(f.name);
                  const doneBytes = live?.bytesDone ?? f.bytesDone ?? 0;
                  const totalBytes = live?.bytesTotal ?? f.bytesTotal;
                  const fileProgress = totalBytes > 0 ? Math.min(1, doneBytes / totalBytes) : 0;
                  const showBar = row.kind === 'inbound' && f.outcome == null;
                  return (
                    <li key={f.name} className="flex items-center gap-2 text-xs">
                      <span className="min-w-0 flex-1 truncate text-content-secondary" title={f.name}>
                        {f.name}
                      </span>
                      <span className="shrink-0 text-content-muted tabular-nums">
                        {formatBytes(f.bytesTotal)}
                      </span>
                      {showBar && (
                        <div className="h-1 w-24 shrink-0 overflow-hidden rounded-full bg-surface-hover">
                          <div
                            className="h-full rounded-full bg-accent transition-all"
                            style={{ width: `${Math.round(fileProgress * 100)}%` }}
                          />
                        </div>
                      )}
                      {f.outcome && (
                        <span
                          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${outcomeTone(f.outcome)}`}
                        >
                          {f.outcome}
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </td>
        </tr>
      )}
    </>
  );
}
