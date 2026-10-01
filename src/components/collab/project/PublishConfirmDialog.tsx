import type { JSX } from 'react';
import { Button, DialogShell } from '../../ui';
import { formatSize } from '../format';

/** The confirm every publish goes through (moved out of `ProjectDetail`). It
 *  closes the moment the user confirms: the run panel in My frames shows the
 *  work (with Cancel), and a refusal returned before the run starts shows in
 *  My frames (final-review ruling, spec §16.1). */
export default function PublishConfirmDialog({
  title,
  count,
  bytes,
  needsApproval,
  coordinatorName,
  onConfirm,
  onCancel,
}: {
  title: string;
  count: number;
  bytes: number;
  needsApproval: boolean;
  coordinatorName: string;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  return (
    <DialogShell
      title={`Publish to ${title}`}
      onClose={onCancel}
      footer={<>
        <Button onClick={onCancel}>Cancel</Button>
        <Button variant="primary" onClick={onConfirm} data-autofocus>Publish</Button>
      </>}
    >
      <p>{count} calibrated {count === 1 ? 'frame' : 'frames'} will be announced to the project.</p>
      <p className="mt-1.5 text-[11.5px] text-content-faint">Size {formatSize(bytes)}</p>
      {needsApproval && <p className="mt-1.5 text-[12px] text-warning">This project requires approval — your contribution goes to {coordinatorName} for review.</p>}
    </DialogShell>
  );
}
