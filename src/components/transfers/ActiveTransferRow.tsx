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

/** State-staged progress fallback (0..1) for a row with no byte-level data yet.
 *
 * `capNonTerminalOutbound` caps the byte-derived fraction at 0.95 (Task 2.1):
 * an in-flight outbound row must never read 100% off upload bytes alone —
 * bytes == byteSize means "uploaded — awaiting confirmation", not delivered.
 * Only a terminal `confirmed`/`done` state reaches 1.0. `stageProgress` can't
 * see `row.kind`, so the caller passes the flag. */
function stageProgress(
  state: string,
  bytesDone: number,
  byteSize: number,
  capNonTerminalOutbound: boolean,
): number {
  if (byteSize > 0 && bytesDone > 0) {
    return Math.min(capNonTerminalOutbound ? 0.95 : 1, bytesDone / byteSize);
  }
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

/**
 * Map a raw sync `last_error` string to a short, human-readable reason (audit
 * UX-2). The known strings are matched as prefixes (case-insensitive) so any
 * trailing context still resolves; anything unrecognized falls through verbatim
 * so an error is never hidden. Exported for reuse by the Transfers slide-over
 * mini-rows. Keep the raw string on a `title=` hover at the call site.
 */
export function plainTransferError(raw: string): string {
  const s = raw.trim().toLowerCase();
  if (s.startsWith('no ack from peer within timeout'))
    return "Peer didn't respond — will keep retrying";
  if (s.startsWith('package payload missing on disk')) return 'Local package data is missing';
  if (s.startsWith('cancelled by receiver')) return 'Cancelled by the receiving device';
  // Class-prefixed dial failures the sync engine persists as `<class>: <raw>`
  // (Task 3.1). A retryable class is a warning, not a failure — delivery-forever
  // keeps trying — so the copy says so. `other:` sheds its machine prefix (the
  // raw reason is already the most specific text we have); any unknown prefix
  // falls through verbatim; the raw string still rides the `title=` hover.
  if (s.startsWith('no_route:')) return 'No route to peer — will keep retrying';
  if (s.startsWith('relay_unreachable:')) return 'Peer unreachable via relay — will keep retrying';
  if (s.startsWith('refused:')) return 'Peer refused the connection';
  if (s.startsWith('timeout:')) return "Peer didn't answer — will keep retrying";
  if (s.startsWith('not_started:')) return 'Peer app not running — will keep retrying';
  if (s.startsWith('other:')) return raw.slice(raw.indexOf(':') + 1).trim() || raw;
  return raw;
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
  const canSendNow = row.kind === 'outbound' && pending;
  const canCancel =
    (row.kind === 'outbound' && pending) ||
    (row.kind === 'inbound' && (row.state === 'announced' || row.state === 'fetching'));
  const canResend = row.kind === 'outbound' && row.terminal;

  // A row waiting out a backoff window is retrying, not failed. State is never
  // demoted across retries (a package can sit in `transferring` forever), so
  // `nextRetryAt` — not the state — is the truth signal that a retry is pending.
  const retrying = !!row.nextRetryAt && !row.terminal;

  // Non-terminal outbound rows cap their byte-derived progress at 95% — a fully
  // uploaded package that hasn't been acked is "awaiting confirmation", not done.
  const progress = stageProgress(
    row.state,
    row.bytesDone,
    row.byteSize,
    row.kind === 'outbound' && !row.terminal,
  );
  const speedLabel = row.isTransferring ? formatSpeed(row.speedBps) : null;

  // The honest post-upload, pre-ack window (Task 2.1): the provider finished
  // serving what the peer asked for, but the ack hasn't landed. Never shown on a
  // terminal row, and a later `transferring` tick (a resume) clears it.
  const awaitingConfirmation =
    row.liveStage === 'uploaded' && row.state === 'transferring' && !row.terminal;

  // Surface the last failed-attempt reason on a retrying row (any state, e.g. a
  // `transferring` row mid-backoff) or a terminal failed/cancelled row — the row
  // otherwise shows only its state with no "why" (audit UX-2). Plain-mapped
  // text, raw string on hover.
  const showReason = !!row.lastError && (row.terminal || row.attempts > 0);

  const sendBusy = busy.has(`send:${row.id}`);
  const cancelBusy =
    row.kind === 'outbound' ? busy.has(`cancel:${row.id}`) : busy.has(`cancelin:${row.packageId ?? ''}`);
  const resendBusy = busy.has(`resend:${row.id}`);

  // Live per-file bars are keyed by the packageId the transport event carries: the
  // wire package id for an inbound fetch, but the outbound ROW id (as a string) for
  // an outbound serve (Task 2.2) — an outbound row has no wire packageId, and the
  // send-side `sync-file-progress` is keyed by the row id.
  const liveKey = row.kind === 'outbound' ? String(row.id) : row.packageId;
  const liveFilesForPkg = liveKey ? liveFiles.get(liveKey) : undefined;

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
          {retrying && (
            <span className="ml-1.5 rounded bg-warning/20 px-1 py-0.5 text-[10px] font-medium text-warning">
              retrying
            </span>
          )}
          {awaitingConfirmation && (
            <p className="mt-0.5 text-[10px] font-medium leading-tight text-accent">
              uploaded — awaiting confirmation
            </p>
          )}
          {showReason && (
            <p
              className={`mt-0.5 max-w-[16rem] whitespace-normal break-words text-[10px] leading-tight ${
                row.terminal ? 'text-error/70' : 'text-warning'
              }`}
              title={row.lastError ?? undefined}
            >
              {plainTransferError(row.lastError as string)}
            </p>
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
                  // A live per-file bar shows for BOTH directions while the file has
                  // no settled outcome and the row is still active — an outbound file
                  // flips to its outcome chip only after the ack (Task 2.2).
                  const showBar = f.outcome == null && !row.terminal;
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
