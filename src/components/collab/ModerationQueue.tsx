import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Loader2, X } from 'lucide-react';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import type { ModerationFrameView } from '../../types/models';

const REASON_MAX = 500;

/**
 * Coordinator review queue (visible only when the parent decides
 * `coordinator && requireApproval`). Every pending frame, cache-only
 * (`list_collab_moderation`) — the per-frame model has no batch/review-copy
 * step any more. Approve calls `approve_collab_frame` with a "Trust this
 * publisher" checkbox (on by default, spec §9); reject calls
 * `reject_collab_frame` with a reason (≤500, required). Errors surface
 * inline (S6) and the list re-fetches on any decision.
 */
export default function ModerationQueue({
  projectId,
  onDecided,
}: {
  projectId: string;
  onDecided: () => void;
}) {
  const [items, setItems] = useState<ModerationFrameView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [rejectFor, setRejectFor] = useState<ModerationFrameView | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const next = await api.invoke<ModerationFrameView[]>('list_collab_moderation', { projectId });
      if (mounted.current) setItems(next);
    } catch (err) {
      // S6 — surface a failed load rather than showing a stale/empty queue silently.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[moderation] list_collab_moderation failed:', err);
      if (mounted.current) setError(msg);
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const withBusy = useCallback(
    async (frameUuid: string, fn: () => Promise<void>) => {
      setError(null);
      setBusy((prev) => new Set(prev).add(frameUuid));
      try {
        await fn();
        await load();
        onDecided();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.error('[moderation] decision failed:', err);
        if (mounted.current) setError(msg);
      } finally {
        if (mounted.current)
          setBusy((prev) => {
            const next = new Set(prev);
            next.delete(frameUuid);
            return next;
          });
      }
    },
    [load, onDecided],
  );

  const approve = useCallback(
    (frameUuid: string, trust: boolean) =>
      void withBusy(frameUuid, () =>
        api.invoke('approve_collab_frame', { projectId, frameUuid, trust }),
      ),
    [projectId, withBusy],
  );

  const reject = useCallback(
    (frameUuid: string, reason: string) =>
      void withBusy(frameUuid, async () => {
        await api.invoke('reject_collab_frame', { projectId, frameUuid, reason });
        setRejectFor(null);
      }),
    [projectId, withBusy],
  );

  return (
    <div className="space-y-3">
      {error && <p className="text-sm text-error">{error}</p>}

      {items === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : items.length === 0 ? (
        <p className="text-sm text-content-muted">Nothing waiting for review.</p>
      ) : (
        <ul className="space-y-2">
          {items.map((item) => (
            <ModerationRow
              key={item.frameUuid}
              item={item}
              busy={busy.has(item.frameUuid)}
              onApprove={(trust) => approve(item.frameUuid, trust)}
              onRejectRequested={() => setRejectFor(item)}
            />
          ))}
        </ul>
      )}

      {rejectFor && (
        <RejectDialog
          item={rejectFor}
          busy={busy.has(rejectFor.frameUuid)}
          onCancel={() => setRejectFor(null)}
          onReject={(reason) => reject(rejectFor.frameUuid, reason)}
        />
      )}
    </div>
  );
}

/** One pending frame with its own "trust this publisher" checkbox (default on)
 *  and approve/reject actions. */
function ModerationRow({
  item,
  busy,
  onApprove,
  onRejectRequested,
}: {
  item: ModerationFrameView;
  busy: boolean;
  onApprove: (trust: boolean) => void;
  onRejectRequested: () => void;
}) {
  const [trust, setTrust] = useState(true);
  return (
    <li className="rounded border border-border p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="max-w-[16rem] truncate font-medium text-content" title={item.fileName}>
          {item.fileName}
        </span>
        <span className="text-xs text-content-muted">
          {item.publisher} · {item.filter} · {item.exptimeSec.toFixed(1)}s
          {item.fwhmArcsec != null ? ` · FWHM ${item.fwhmArcsec.toFixed(2)}″` : ''} ·{' '}
          {formatTimestamp(item.createdAt)}
        </span>
        <span className="ml-auto flex items-center gap-2">
          <button
            type="button"
            onClick={() => onApprove(trust)}
            disabled={busy}
            className="inline-flex items-center gap-1 rounded bg-accent px-2.5 py-1 text-xs text-surface transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {busy ? <Loader2 size={12} className="animate-spin" /> : <Check size={12} />} Approve
          </button>
          <button
            type="button"
            onClick={onRejectRequested}
            disabled={busy}
            className="inline-flex items-center gap-1 rounded border border-error/50 px-2.5 py-1 text-xs text-error transition-colors hover:bg-error/10 disabled:opacity-50"
          >
            <X size={12} /> Reject
          </button>
        </span>
      </div>
      <label className="mt-1.5 flex items-center gap-1.5 text-xs text-content-muted">
        <input
          type="checkbox"
          checked={trust}
          onChange={(e) => setTrust(e.target.checked)}
          className="h-3.5 w-3.5 rounded border-border bg-surface-hover text-accent focus:ring-accent"
        />
        Trust this publisher
      </label>
    </li>
  );
}

/** Required-reason reject dialog (≤500 chars). */
function RejectDialog({
  item,
  busy,
  onCancel,
  onReject,
}: {
  item: ModerationFrameView;
  busy: boolean;
  onCancel: () => void;
  onReject: (reason: string) => void;
}) {
  const [reason, setReason] = useState('');
  const trimmed = reason.trim();
  const tooLong = reason.length > REASON_MAX;
  const valid = trimmed.length > 0 && !tooLong;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40" onClick={onCancel}>
      <div
        className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center gap-2">
          <X size={16} className="text-error" />
          <h2 className="font-medium text-content">Reject frame</h2>
          <button
            onClick={onCancel}
            className="ml-auto text-content-muted transition-colors hover:text-content"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-2 text-xs text-content-muted">
          {item.fileName} · {item.publisher}. The reason is sent to the publisher.
        </p>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          rows={4}
          autoFocus
          placeholder="Why is this frame rejected?"
          className="w-full resize-none rounded border border-border bg-surface-elevated p-2 text-sm text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
        />
        <div className="mt-1 flex items-center justify-between text-xs">
          <span className={tooLong ? 'text-error' : 'text-content-muted'}>
            {reason.length}/{REASON_MAX}
          </span>
        </div>
        <div className="mt-3 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => onReject(trimmed)}
            disabled={!valid || busy}
            className="inline-flex items-center gap-1 rounded bg-error px-3 py-1.5 text-sm text-surface transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy && <Loader2 size={12} className="animate-spin" />} Reject
          </button>
        </div>
      </div>
    </div>
  );
}
