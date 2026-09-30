import type { JSX } from 'react';
import { Loader2 } from 'lucide-react';
import { Button, DialogShell } from '../../ui';
import { formatSize } from '../format';

/** The confirm every publish goes through (moved out of `ProjectDetail`). */
export default function PublishConfirmDialog({
  title,
  count,
  estimatedBytes,
  needsApproval,
  coordinatorName,
  busy,
  error,
  onConfirm,
  onCancel,
}: {
  title: string;
  count: number;
  estimatedBytes: number;
  needsApproval: boolean;
  coordinatorName: string;
  busy: boolean;
  error: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  return (
    <DialogShell
      title={`Publish to ${title}`}
      onClose={onCancel}
      busy={busy}
      footer={<>
        <Button onClick={onCancel} disabled={busy}>Cancel</Button>
        <Button variant="primary" onClick={onConfirm} disabled={busy} data-autofocus>
          {busy && <Loader2 size={12} className="animate-spin" />}Publish
        </Button>
      </>}
    >
      <p>{count} passing {count === 1 ? 'frame' : 'frames'} will be calibrated and announced to the project.</p>
      <p className="mt-1.5 text-[11.5px] text-content-faint">Estimated size ≈ {formatSize(estimatedBytes)} — the exact size is measured when each frame is generated.</p>
      {needsApproval && <p className="mt-1.5 text-[12px] text-warning">This project requires approval — your contribution goes to {coordinatorName} for review.</p>}
      {error && <p className="mt-1.5 text-[12.5px] text-error">{error}</p>}
    </DialogShell>
  );
}
