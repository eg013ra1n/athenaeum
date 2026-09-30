import { useState, type JSX } from 'react';
import { Loader2, RefreshCw } from 'lucide-react';
import { formatBytes } from '../format';

/** Above this many frames a selection needs the typed count (owner ruling:
 *  nobody re-announces 100 TB with one click). "All" always needs it. */
export const REPUBLISH_TYPED_CONFIRM_ABOVE = 100;

function frames(n: number): string {
  return `${n} ${n === 1 ? 'frame' : 'frames'}`;
}

/**
 * The confirm every republish goes through (Task 16). It states how many
 * frames will be regenerated and the size of their source frames. For "all",
 * or for a selection above `REPUBLISH_TYPED_CONFIRM_ABOVE`, Republish stays
 * disabled until the exact count is typed. `busy` and `error` show inside the
 * dialog, as the old republish confirm did.
 */
export default function RepublishGuardDialog({
  count,
  sourceBytes,
  all,
  busy,
  error,
  onConfirm,
  onCancel,
}: {
  count: number;
  sourceBytes: number;
  all: boolean;
  busy: boolean;
  error: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  const [typed, setTyped] = useState('');
  const needsTyping = count > 0 && (all || count > REPUBLISH_TYPED_CONFIRM_ABOVE);
  const typedOk = !needsTyping || typed.trim() === String(count);
  const canConfirm = count > 0 && typedOk && !busy;
  const inputId = 'republish-guard-count';

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={() => !busy && onCancel()}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="republish-guard-title"
        className="w-[30rem] max-w-[90vw] rounded-lg border border-border bg-surface p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-center gap-2">
          <RefreshCw size={16} className="text-accent" />
          <h2 id="republish-guard-title" className="font-medium text-content">
            {all ? 'Recalibrate and republish all' : `Republish ${frames(count)}`}
          </h2>
        </div>
        {count === 0 ? (
          <p className="mb-2 text-sm text-content-muted">Nothing to republish.</p>
        ) : (
          <p className="mb-2 text-sm text-content-secondary">
            {`${frames(count)} · ${formatBytes(sourceBytes)} of source frames will be recalibrated. Every frame whose bytes change is posted as a new version, and every processor holding it downloads it again.`}
          </p>
        )}
        <p className="mb-2 text-xs text-warning">
          Every processor holding one of your frames re-downloads it once this finishes.
        </p>
        {needsTyping && (
          <div className="mb-2 space-y-1">
            <label htmlFor={inputId} className="block text-xs text-content-secondary">
              Type {count} to confirm
            </label>
            <input
              id={inputId}
              type="text"
              inputMode="numeric"
              autoComplete="off"
              value={typed}
              disabled={busy}
              onChange={(e) => setTyped(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canConfirm) onConfirm();
              }}
              className="w-full rounded border border-border bg-surface-elevated px-2 py-1 text-sm text-content focus:border-accent focus:outline-none"
            />
          </div>
        )}
        {error && <p className="mb-2 text-sm text-error">{error}</p>}
        <div className="mt-3 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={!canConfirm}
            className="inline-flex items-center gap-1 rounded bg-accent px-3 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy && <Loader2 size={12} className="animate-spin" />} Republish
          </button>
        </div>
      </div>
    </div>
  );
}
