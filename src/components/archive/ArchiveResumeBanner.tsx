import { useCallback, useEffect, useState } from 'react';
import { listUnfinishedArchiveOperations, resumeArchiveOperation, rollbackArchiveOperation } from '../../api/archive';
import type { ArchiveOperationSummary } from '../../types/archive';
import { ArchiveProgress } from './ArchiveProgress';

export function ArchiveResumeBanner() {
  const [ops, setOps] = useState<ArchiveOperationSummary[]>([]);
  const [dismissed, setDismissed] = useState(false);
  const [busy, setBusy] = useState(false);
  // Rollback runs on the operation queue and reports through the shared
  // archive-progress / archive-finished events — keep the widget mounted
  // until it says it is done, then retire the banner.
  const [rollingBack, setRollingBack] = useState(false);

  const handleRollbackFinished = useCallback(() => {
    setRollingBack(false);
    setDismissed(true);
  }, []);

  useEffect(() => {
    listUnfinishedArchiveOperations()
      .then(setOps)
      .catch(err => console.error('list unfinished archive ops failed', err));
  }, []);

  if (dismissed || ops.length === 0) return null;

  const op = ops[0];
  const name = op.frame_set_name
    ? op.frame_set_name
    : op.frames_set_id != null
      ? `Frame Set #${op.frames_set_id}`
      : `Calibration originals archive #${op.id}`;

  return (
    <>
      <div className="bg-warning/10 border-b border-warning/40 px-4 py-2 flex items-center gap-3 text-sm flex-wrap">
        <span className="font-medium text-content">
          Archive operation interrupted: {name}
          {ops.length > 1 && ` (and ${ops.length - 1} more)`}
        </span>
        <button
          disabled={busy || rollingBack}
          onClick={async () => {
            setBusy(true);
            try {
              await resumeArchiveOperation(op.id);
              setDismissed(true);
            } catch (e) {
              console.error('resume archive failed', e);
              alert(`Resume failed: ${e}`);
            } finally {
              setBusy(false);
            }
          }}
          className="px-2 py-0.5 rounded bg-accent text-white text-xs disabled:opacity-60"
        >
          Resume
        </button>
        <button
          disabled={busy || rollingBack}
          onClick={async () => {
            setBusy(true);
            setRollingBack(true);
            try {
              await rollbackArchiveOperation(op.id);
            } catch (e) {
              console.error('rollback archive failed', e);
              setRollingBack(false);
              alert(`Rollback failed: ${e}`);
            } finally {
              setBusy(false);
            }
          }}
          className="px-2 py-0.5 rounded border border-border text-xs text-content disabled:opacity-60"
        >
          Roll back
        </button>
        <button
          onClick={() => setDismissed(true)}
          className="px-2 py-0.5 rounded border border-border text-xs text-content-muted ml-auto"
        >
          Decide later
        </button>
      </div>
      {rollingBack && (
        <div className="fixed bottom-4 right-4 z-50 w-80">
          <ArchiveProgress operationId={op.id} onClose={handleRollbackFinished} />
        </div>
      )}
    </>
  );
}
