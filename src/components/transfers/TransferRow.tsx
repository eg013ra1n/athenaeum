import { ArrowUp, ArrowDown, Loader2, RotateCw, Send, Trash2, X, Clock, Users, Telescope } from 'lucide-react';
import { formatTimestamp } from '../../utils/dateFormatting';
import {
  displayStateChip,
  displayStateSubline,
  formatBytes,
  formatCountdown,
  formatEta,
  formatSpeed,
  isSenderRevokeReason,
  isTransientTransferError,
  plainTransferError,
  shortPeer,
  shortProject,
} from './presentation';
import { summarizeOutcomeChips } from './historyGrouping';
import type { DeleteKey, UnifiedRow } from './types';
import type { Direction } from '../../types/models';
import type { TransferRow as TransferRowModel } from '../../hooks/useTransferQueue';

interface TransferRowProps {
  item: UnifiedRow;
  selected: boolean;
  onSelect: () => void;
  /** Shared 1s tick (ms) so every visible countdown ticks together, no per-row timer. */
  now: number;
  busy: Set<string>;
  onSendNow: (id: number) => void;
  onCancelOutbound: (id: number) => void;
  onCancelInbound: (packageId: string) => void;
  onResend: (id: number) => void;
  /** Remove a settled batch from history (trash icon). */
  onDelete: (direction: Direction, packageKey: string) => void;
}

/**
 * One row of the unified master-detail Transfers list (§D8). Renders a LIVE
 * in-flight/ledger row or a merged-in completed HISTORY group behind one
 * selectable shell: direction icon, batch NAME (never the raw handle), device
 * NAME (never hex), a `displayState` chip (benign neutral `waiting`, never a
 * sticky red error), progress "N of M files · X / Y", speed + ETA while moving,
 * and the row actions. The raw ids/hex live only in the detail pane's Details tab.
 */
export function TransferRow({
  item,
  selected,
  onSelect,
  now,
  busy,
  onSendNow,
  onCancelOutbound,
  onCancelInbound,
  onResend,
  onDelete,
}: TransferRowProps) {
  const selectedRing = selected ? 'border-l-accent bg-surface-hover' : 'border-l-transparent';
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSelect();
        }
      }}
      className={`cursor-pointer border-b border-l-2 border-border ${selectedRing} transition-colors hover:bg-surface-hover`}
    >
      {item.kind === 'live' ? (
        <LiveRowBody
          row={item.row}
          deleteKey={item.deleteKey}
          now={now}
          busy={busy}
          onSendNow={onSendNow}
          onCancelOutbound={onCancelOutbound}
          onCancelInbound={onCancelInbound}
          onResend={onResend}
          onDelete={onDelete}
        />
      ) : (
        <HistoryRowBody item={item} busy={busy} onDelete={onDelete} />
      )}
    </div>
  );
}

/** The trash affordance shared by settled live rows and history-only groups.
 *  Stops propagation so it never toggles row selection. */
function DeleteButton({
  deleteKey,
  busy,
  onDelete,
}: {
  deleteKey: DeleteKey;
  busy: Set<string>;
  onDelete: (direction: Direction, packageKey: string) => void;
}) {
  const deleteBusy = busy.has(`delete:${deleteKey.direction}:${deleteKey.packageKey}`);
  return (
    <button
      type="button"
      disabled={deleteBusy}
      onClick={(e) => {
        e.stopPropagation();
        onDelete(deleteKey.direction, deleteKey.packageKey);
      }}
      title="Remove from history — files on disk are not affected"
      aria-label="Remove from history"
      className="inline-flex items-center rounded border border-border px-1.5 py-1 text-content-muted transition-colors hover:border-error/40 hover:bg-error/10 hover:text-error disabled:cursor-not-allowed disabled:opacity-50"
    >
      {deleteBusy ? <Loader2 size={12} className="animate-spin" /> : <Trash2 size={12} />}
    </button>
  );
}

/** Sender-kind origin badge (Perseus UI v2) — a small chip next to the device
 *  name that renders ONLY when a received transfer came from a Perseus capture
 *  agent. Athenaeum senders and unknown kinds render nothing, so the common
 *  case stays clean. Accent-muted via design tokens (never raw colors). */
function SenderKindBadge({ kind }: { kind: string | null }) {
  if (kind !== 'perseus') return null;
  return (
    <span
      className="inline-flex shrink-0 items-center gap-0.5 rounded border border-border bg-surface px-1 py-0.5 text-[9px] font-medium text-content-muted"
      title="Sent by a Perseus capture agent"
    >
      <Telescope size={9} />
      Perseus
    </span>
  );
}

/** State-staged progress fraction (0..1) when no byte figure is available yet.
 *  Non-terminal outbound caps at 0.95 — a fully-uploaded-but-unacked package is
 *  "awaiting confirmation", not delivered. */
function stageProgress(
  displayState: string,
  bytesDone: number,
  byteSize: number,
  capNonTerminalOutbound: boolean,
): number {
  if (byteSize > 0 && bytesDone > 0) {
    return Math.min(capNonTerminalOutbound ? 0.95 : 1, bytesDone / byteSize);
  }
  switch (displayState) {
    case 'queued':
      return 0.02;
    case 'preparing':
      return 0.05;
    case 'announced':
    case 'waiting':
      return 0.08;
    case 'transferring':
    case 'fetching':
      return 0.5;
    case 'uploaded':
    case 'ingesting':
      return 0.95;
    case 'confirmed':
    case 'done':
      return 1;
    default:
      return 0;
  }
}

interface LiveRowBodyProps {
  row: TransferRowModel;
  /** Batch delete key, or `null` when the row can't be deleted (non-terminal row). */
  deleteKey: DeleteKey | null;
  now: number;
  busy: Set<string>;
  onSendNow: (id: number) => void;
  onCancelOutbound: (id: number) => void;
  onCancelInbound: (packageId: string) => void;
  onResend: (id: number) => void;
  onDelete: (direction: Direction, packageKey: string) => void;
}

function LiveRowBody({
  row,
  deleteKey,
  now,
  busy,
  onSendNow,
  onCancelOutbound,
  onCancelInbound,
  onResend,
  onDelete,
}: LiveRowBodyProps) {
  const batchName = row.displayName ?? row.packageShort;
  const deviceLabel = row.deviceName ?? row.peerShort;
  const chip = displayStateChip(row.displayState);
  const subline = displayStateSubline(row.displayState);
  const waiting = row.displayState === 'waiting';

  const totalFiles = row.fileCounts.total || row.fileCount;
  const doneFiles = row.fileCounts.done;

  const cap = row.kind === 'outbound' && !row.terminal;
  const progress = stageProgress(row.displayState, row.bytesDone, row.byteSize, cap);
  const speedLabel = row.isTransferring ? formatSpeed(row.speedBps) : null;
  const remaining = Math.max(0, row.byteSize - row.bytesDone);
  const eta = row.isTransferring && remaining > 0 ? formatEta(remaining, row.speedBps) : null;

  // Progress-line shape (§problem 3): the detailed "N of M · X / Y" form is for
  // an ACTIVE row with real per-file counts. A legacy batch (no per-file rows) OR
  // any settled row (its `bytesDone` is 0 post-settle, so "X / Y" would read
  // "0 B / …") shows the honest compact "M files · total" instead — no "0 of",
  // no "0 B /".
  const compactCounts = row.terminal || row.fileCounts.total === 0;
  const displayCount = row.fileCounts.total || row.fileCount;

  // Reason text is honest, not sticky: it shows only while a retry is genuinely
  // pending (`retrying`) or on a terminal failure/cancel — NEVER gated on the
  // monotonic `attempts` counter (that was the bug that made errors stick after
  // recovery). On a terminal row a "will keep retrying" promise is void, so the
  // transient-family causes render WITHOUT the suffix and in a muted tone; an
  // explicit terminal cause (cancelled-by-peer, failure) keeps error styling. An
  // INBOUND row's `lastError` (B5b) is a sender-revoke reason ("by sender" /
  // "sender failed" / superseded) — purely explanatory (the receiver has no retry
  // to act on), so it renders muted regardless of the row's own chip color.
  const showReason = !!row.lastError && (row.retrying || row.terminal);
  const reasonMuted =
    row.terminal &&
    !!row.lastError &&
    (isTransientTransferError(row.lastError) ||
      (row.kind === 'inbound' && isSenderRevokeReason(row.lastError)));
  const reasonTone = !row.terminal
    ? 'text-warning'
    : reasonMuted
      ? 'text-content-muted'
      : 'text-error/80';

  const pending = !row.terminal && (row.state === 'queued' || row.state === 'announced');
  // Send-now: a pending row, or one sitting in the `waiting` backoff countdown — a
  // user staring at "retry in …" wants "try now", and the backend `kick` is exactly
  // that (collapse the backoff so the next worker pass re-announces).
  const canSendNow = row.kind === 'outbound' && (pending || waiting);
  // Cancel: ANY non-terminal outbound row. The backend `engine.cancel(id)` cancels
  // cooperatively at every stage (transferring/uploaded/waiting included), and after
  // a restart the resurrected engine holds the slot again — so the button must not be
  // gated to the narrow `pending` window. Inbound gate is unchanged.
  const canCancel =
    (row.kind === 'outbound' && !row.terminal) ||
    (row.kind === 'inbound' && (row.state === 'announced' || row.state === 'fetching'));
  // Resend re-announces the on-disk payload — but a package's payload can be gone
  // by the time this renders: a `confirmed` row's payload is always cleaned up
  // after confirm, AND retention can sweep an old failed/cancelled row's payload
  // too. `resendable` (backend-computed for a durable terminal row via the same
  // `package_has_payload` guard `retry_sync_package` itself enforces; always
  // `true` for a same-session ledger row, whose payload provably existed moments
  // ago) subsumes the old displayState check — a stale row no longer shows a
  // dead Resend button that fails "data missing on disk".
  const canResend = row.kind === 'outbound' && row.terminal && row.resendable;

  const sendBusy = busy.has(`send:${row.id}`);
  const cancelBusy =
    row.kind === 'outbound' ? busy.has(`cancel:${row.id}`) : busy.has(`cancelin:${row.packageId ?? ''}`);
  const resendBusy = busy.has(`resend:${row.id}`);

  return (
    <div className="px-3 py-2">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 shrink-0" title={row.kind === 'outbound' ? 'Sending' : 'Receiving'}>
          {row.kind === 'outbound' ? (
            <ArrowUp size={15} className="text-accent" />
          ) : (
            <ArrowDown size={15} className="text-success" />
          )}
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium text-content" title={batchName}>
              {batchName}
            </span>
            <span
              className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${chip.className}`}
            >
              {chip.label}
            </span>
            {subline && <span className="shrink-0 text-[10px] text-content-muted">{subline}</span>}
            {waiting && row.stalledUntil && (
              <span className="inline-flex shrink-0 items-center gap-1 text-[11px] text-content-secondary tabular-nums">
                <Clock size={11} />
                retry in {formatCountdown(row.stalledUntil, now)}
                {row.generation > 1 && ` · attempt ${row.generation}`}
              </span>
            )}
          </div>

          <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-content-muted">
            <span title={deviceLabel}>{deviceLabel}</span>
            {row.kind === 'inbound' && <SenderKindBadge kind={row.peerKind} />}
            {/* User-facing "attempt N" (§D5), from `generation` (bumped only by a
                resend — never the engine's internal announce-retries). Shown on
                active AND terminal rows; suppressed while `waiting`, where the
                countdown above already carries its own "· attempt N". */}
            {row.generation > 1 && !waiting && (
              <>
                <span aria-hidden="true">·</span>
                <span className="text-content-muted tabular-nums">attempt {row.generation}</span>
              </>
            )}
            <span aria-hidden="true">·</span>
            {compactCounts ? (
              <span className="tabular-nums">
                {displayCount} file{displayCount === 1 ? '' : 's'} · {formatBytes(row.byteSize)}
              </span>
            ) : (
              <>
                <span className="tabular-nums">
                  {doneFiles} of {totalFiles} file{totalFiles === 1 ? '' : 's'}
                </span>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">
                  {formatBytes(row.bytesDone)} / {formatBytes(row.byteSize)}
                </span>
              </>
            )}
            {speedLabel && (
              <>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums text-accent">{speedLabel}</span>
              </>
            )}
            {eta && (
              <>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">ETA {eta}</span>
              </>
            )}
          </div>

          {showReason && (
            <p
              className={`mt-0.5 max-w-[36rem] whitespace-normal break-words text-[11px] leading-tight ${reasonTone}`}
              title={row.lastError ?? undefined}
            >
              {plainTransferError(row.lastError as string, row.terminal)}
            </p>
          )}
        </div>

        {/* Actions — stop propagation so a button click doesn't also select the row. */}
        <div className="flex shrink-0 items-center gap-1" onClick={(e) => e.stopPropagation()}>
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
          {deleteKey && <DeleteButton deleteKey={deleteKey} busy={busy} onDelete={onDelete} />}
        </div>
      </div>

      {/* Thin progress bar — only for a still-moving live row. */}
      {!row.terminal && (
        <div className="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-surface-hover">
          <div
            className="h-full rounded-full bg-accent transition-all"
            style={{ width: `${Math.round(progress * 100)}%` }}
          />
        </div>
      )}
    </div>
  );
}

function HistoryRowBody({
  item,
  busy,
  onDelete,
}: {
  item: Extract<UnifiedRow, { kind: 'history' }>;
  busy: Set<string>;
  onDelete: (direction: Direction, packageKey: string) => void;
}) {
  const { group, deviceName, projectName, deleteKey } = item;
  const batchName =
    group.batchName ?? (group.packageId ? shortPeer(group.packageId) : 'Earlier transfers');
  const deviceLabel = deviceName ?? shortPeer(group.peerDevice);
  const chips = summarizeOutcomeChips(group.outcomeCounts);

  return (
    <div className="px-3 py-2">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 shrink-0" title={group.direction === 'sent' ? 'Sent' : 'Received'}>
          {group.direction === 'sent' ? (
            <ArrowUp size={15} className="text-content-muted" />
          ) : (
            <ArrowDown size={15} className="text-content-muted" />
          )}
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium text-content" title={batchName}>
              {batchName}
            </span>
            {group.project && (
              <span
                className="inline-flex shrink-0 items-center gap-0.5 rounded bg-accent/15 px-1 py-0.5 text-[9px] text-accent"
                title={group.project}
              >
                <Users size={9} />
                <span className="max-w-[8rem] truncate">
                  {projectName ?? shortProject(group.project)}
                </span>
              </span>
            )}
          </div>
          <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[11px] text-content-muted">
            <span title={deviceLabel}>{deviceLabel}</span>
            {group.direction === 'received' && <SenderKindBadge kind={item.deviceKind} />}
            <span aria-hidden="true">·</span>
            <span className="tabular-nums">
              {group.rows.length} file{group.rows.length === 1 ? '' : 's'} · {formatBytes(group.totalBytes)}
            </span>
          </div>
        </div>

        <div className="flex shrink-0 items-start gap-2">
          <div className="flex flex-col items-end gap-0.5">
            <span className="flex items-center gap-1.5">
              {chips.map((chip) => (
                <span key={chip.key} className={`text-[10px] font-medium ${chip.tone}`} title={chip.title}>
                  {chip.label}
                </span>
              ))}
            </span>
            <span className="text-[10px] text-content-muted tabular-nums">
              {formatTimestamp(group.finishedAt ?? group.startedAt)}
            </span>
          </div>
          {deleteKey && <DeleteButton deleteKey={deleteKey} busy={busy} onDelete={onDelete} />}
        </div>
      </div>
    </div>
  );
}
