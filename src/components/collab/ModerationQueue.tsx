import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Loader2, X } from 'lucide-react';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import { formatBytes } from './format';
import type { ModerationItem } from '../../types/models';

const POLL_MS = 4_000;
const REASON_MAX = 500;

/**
 * Coordinator review queue (visible only when the parent decides
 * `coordinator && requireApproval`). Every pending package with its landed
 * review copy: a per-frame metrics table once the copy is complete, otherwise
 * an honest "receiving review copy…" line. Approve / reject both call
 * `decide_collab_announcement`; reject requires a reason (≤500). Errors surface
 * inline (S6) and the list re-fetches on any decision.
 */
export default function ModerationQueue({
  projectId,
  onDecided,
}: {
  projectId: string;
  onDecided: () => void;
}) {
  const [items, setItems] = useState<ModerationItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [rejectFor, setRejectFor] = useState<ModerationItem | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const next = await api.invoke<ModerationItem[]>('list_collab_moderation', { projectId });
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

  // Poll while any review copy is still landing so its metrics table appears
  // without a manual refresh.
  const anyIncomplete = (items ?? []).some((i) => !i.reviewCopyComplete);
  useEffect(() => {
    if (!anyIncomplete) return;
    const timer = setInterval(() => void load(), POLL_MS);
    return () => clearInterval(timer);
  }, [anyIncomplete, load]);

  const decide = useCallback(
    async (announcementId: string, approve: boolean, reason?: string) => {
      setError(null);
      setBusy((prev) => new Set(prev).add(announcementId));
      try {
        await api.invoke('decide_collab_announcement', { announcementId, approve, reason });
        setRejectFor(null);
        await load();
        onDecided();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.error('[moderation] decide_collab_announcement failed:', err);
        if (mounted.current) setError(msg);
      } finally {
        if (mounted.current)
          setBusy((prev) => {
            const next = new Set(prev);
            next.delete(announcementId);
            return next;
          });
      }
    },
    [load, onDecided],
  );

  return (
    <div className="space-y-3">
      {error && <p className="text-sm text-error">{error}</p>}

      {items === null ? (
        <p className="text-sm text-content-muted">Loading…</p>
      ) : items.length === 0 ? (
        <p className="text-sm text-content-muted">Nothing waiting for review.</p>
      ) : (
        <ul className="space-y-3">
          {items.map((item) => {
            const itemBusy = busy.has(item.announcementId);
            return (
              <li key={item.announcementId} className="rounded border border-border p-3 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium text-content">{item.publisher}</span>
                  <span className="text-xs text-content-muted">
                    {item.frameCount} frames · {formatBytes(item.byteSize)} ·{' '}
                    {formatTimestamp(item.createdAt)}
                  </span>
                  <span className="ml-auto flex items-center gap-2">
                    <button
                      type="button"
                      onClick={() => void decide(item.announcementId, true)}
                      disabled={itemBusy}
                      className="inline-flex items-center gap-1 rounded bg-accent px-2.5 py-1 text-xs text-surface transition-colors hover:bg-accent-hover disabled:opacity-50"
                    >
                      {itemBusy ? (
                        <Loader2 size={12} className="animate-spin" />
                      ) : (
                        <Check size={12} />
                      )}{' '}
                      Approve
                    </button>
                    <button
                      type="button"
                      onClick={() => setRejectFor(item)}
                      disabled={itemBusy}
                      className="inline-flex items-center gap-1 rounded border border-error/50 px-2.5 py-1 text-xs text-error transition-colors hover:bg-error/10 disabled:opacity-50"
                    >
                      <X size={12} /> Reject
                    </button>
                  </span>
                </div>

                {item.reviewCopyComplete ? (
                  <ReviewFrames item={item} />
                ) : (
                  <p className="mt-2 inline-flex items-center gap-1 text-xs text-content-muted">
                    <Loader2 size={12} className="animate-spin" /> receiving review copy…
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {rejectFor && (
        <RejectDialog
          item={rejectFor}
          busy={busy.has(rejectFor.announcementId)}
          onCancel={() => setRejectFor(null)}
          onReject={(reason) => void decide(rejectFor.announcementId, false, reason)}
        />
      )}
    </div>
  );
}

/** Per-frame metrics table for a fully-landed review copy. */
function ReviewFrames({ item }: { item: ModerationItem }) {
  if (item.frames.length === 0)
    return <p className="mt-2 text-xs text-content-muted">No frame metrics available.</p>;
  return (
    <div className="mt-2 overflow-x-auto">
      <table className="w-full text-left text-xs">
        <thead className="text-content-muted">
          <tr>
            <th className="py-1 pr-3 font-normal">Frame</th>
            <th className="pr-3 font-normal">FWHM</th>
            <th className="pr-3 font-normal">Ecc</th>
            <th className="pr-3 font-normal">Stars</th>
            <th className="font-normal">SNR</th>
          </tr>
        </thead>
        <tbody>
          {item.frames.map((f) => (
            <tr key={f.frameUuid} className="border-t border-border/50">
              <td className="max-w-[16rem] truncate py-1 pr-3 text-content" title={f.relPath}>
                {f.relPath}
              </td>
              <td className="pr-3 text-content-secondary">
                {f.fwhm != null ? f.fwhm.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">
                {f.eccentricity != null ? f.eccentricity.toFixed(2) : '—'}
              </td>
              <td className="pr-3 text-content-secondary">{f.stars ?? '—'}</td>
              <td className="text-content-secondary">{f.snr != null ? f.snr.toFixed(1) : '—'}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Required-reason reject dialog (≤500 chars). */
function RejectDialog({
  item,
  busy,
  onCancel,
  onReject,
}: {
  item: ModerationItem;
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
          <h2 className="font-medium text-content">Reject contribution</h2>
          <button
            onClick={onCancel}
            className="ml-auto text-content-muted transition-colors hover:text-content"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-2 text-xs text-content-muted">
          {item.publisher} · {item.frameCount} frames. The reason is sent to the publisher.
        </p>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          rows={4}
          autoFocus
          placeholder="Why is this contribution rejected?"
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
