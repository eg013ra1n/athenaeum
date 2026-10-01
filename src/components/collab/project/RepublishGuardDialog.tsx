import { useState, type JSX } from 'react';
import { Button, DialogShell, TextInput } from '../../ui';
import { formatSize } from '../format';

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
 * disabled until the exact count is typed. It closes the moment the user
 * confirms: the run panel in My frames shows the work (with Cancel), and a
 * refusal returned before the run starts shows in My frames (final-review
 * ruling, spec §16.1).
 */
export default function RepublishGuardDialog({
  count,
  sourceBytes,
  all,
  onConfirm,
  onCancel,
}: {
  count: number;
  sourceBytes: number;
  all: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  const [typed, setTyped] = useState('');
  const needsTyping = count > 0 && (all || count > REPUBLISH_TYPED_CONFIRM_ABOVE);
  const typedOk = !needsTyping || typed.trim() === String(count);
  const canConfirm = count > 0 && typedOk;
  const inputId = 'republish-guard-count';

  return (
    <DialogShell
      title={all ? 'Recalibrate and republish all' : `Republish ${frames(count)}`}
      onClose={onCancel}
      footer={<>
        <Button onClick={onCancel} data-autofocus={count === 0 ? true : undefined}>Cancel</Button>
        <Button variant="primary" onClick={onConfirm} disabled={!canConfirm} data-autofocus={needsTyping || count === 0 ? undefined : true}>
          Republish
        </Button>
      </>}
    >
      {count === 0 ? (
        <p>Nothing to republish.</p>
      ) : (
        <p>
          {`${frames(count)} · ${formatSize(sourceBytes)} of source frames will be recalibrated. Every frame whose bytes change is posted as a new version, and every processor holding it downloads it again.`}
        </p>
      )}
      <p className="mt-1.5 text-[12px] text-warning">
        Every processor holding one of your frames re-downloads it once this finishes.
      </p>
      {needsTyping && (
        <div className="mt-2 space-y-1">
          <label htmlFor={inputId} className="block text-[11.5px] text-content-faint">
            Type {count} to confirm
          </label>
          <TextInput
            id={inputId}
            inputMode="numeric"
            autoComplete="off"
            value={typed}
            data-autofocus
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && canConfirm) onConfirm();
            }}
            className="w-full"
          />
        </div>
      )}
    </DialogShell>
  );
}
