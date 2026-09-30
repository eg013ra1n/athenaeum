import { useEffect, useState } from 'react';
import { Ban, Loader2, X } from 'lucide-react';
import { api } from '../../../api';
import type { FrameVM } from './frames';

const REASON_MAX = 500;

/**
 * Coordinator-only, reason-required exclusion of one or more frames
 * (spec 2026-09-30 Task 9). Calls `exclude_collab_frame` once per frame, in
 * order; a failure stops the loop, logs and reports how many succeeded
 * before it, and leaves the dialog open so the reason survives for a retry.
 * All frames succeeding closes the dialog. Reused by the drawer (single
 * frame) and by the Published/Library table actions (Tasks 10, 11).
 */
export default function ExcludeDialog({
  projectId,
  frames,
  onClose,
  onDone,
}: {
  projectId: string;
  frames: FrameVM[];
  onClose: () => void;
  onDone: (excluded: number) => void;
}) {
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const trimmed = reason.trim();
  // Unicode scalar values, not UTF-16 code units — matches the core's
  // `chars().count()` and the hub's count (an emoji is 1 char, not 2).
  const trimmedLength = [...trimmed].length;
  const valid = trimmedLength >= 1 && trimmedLength <= REASON_MAX;
  const tooLong = trimmedLength > REASON_MAX;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Owns Escape while open (see FrameDrawer's onKey, which defers to
      // this dialog) — but never while a request is in flight, so a
      // mid-loop Escape can't hide the "Excluded K of N — …" outcome.
      if (e.key === 'Escape' && !busy) onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose, busy]);

  const doExclude = async () => {
    setBusy(true);
    setResult(null);
    let excluded = 0;
    for (const frame of frames) {
      try {
        await api.invoke('exclude_collab_frame', {
          projectId,
          frameUuid: frame.frameUuid,
          reason: trimmed,
        });
        excluded += 1;
      } catch (err) {
        // Never swallow: log first, then stop and report the partial count
        // inline so the coordinator knows exactly how far it got.
        console.error('[exclude] failed:', err);
        const msg = err instanceof Error ? err.message : String(err);
        setResult(`Excluded ${excluded} of ${frames.length} — ${msg}`);
        setBusy(false);
        onDone(excluded);
        return;
      }
    }
    setBusy(false);
    onDone(excluded);
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={() => !busy && onClose()}
    >
      <div
        className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center gap-2">
          <Ban size={16} className="text-error" />
          <h2 className="font-medium text-content">Exclude {frames.length} {frames.length === 1 ? 'frame' : 'frames'} from the project</h2>
          <button
            onClick={onClose}
            disabled={busy}
            className="ml-auto text-content-muted transition-colors hover:text-content disabled:opacity-50"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>
        <p className="mb-2 text-xs text-content-muted">
          Excluded frames stop counting toward the project and are no longer exchanged. You can restore
          them from the frame&apos;s panel.
        </p>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          rows={4}
          autoFocus
          placeholder="Why are these frames excluded?"
          className="w-full resize-none rounded border border-border bg-surface-elevated p-2 text-sm text-content placeholder:text-content-muted focus:border-accent focus:outline-none"
        />
        <div className="mt-1 flex items-center justify-between text-xs">
          <span className={tooLong ? 'text-error' : 'text-content-muted'}>
            {trimmedLength} / {REASON_MAX}
          </span>
        </div>
        {result && <p className="mt-2 text-sm text-error">{result}</p>}
        <div className="mt-3 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void doExclude()}
            disabled={!valid || busy}
            className="inline-flex items-center gap-1 rounded bg-error px-3 py-1.5 text-sm text-surface transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy && <Loader2 size={12} className="animate-spin" />} Exclude
          </button>
        </div>
      </div>
    </div>
  );
}
