import { useEffect, useState } from 'react';
import { api } from '../../api';
import { cancelArchiveOperation } from '../../api/archive';
import type { ArchiveProgressEvent } from '../../types/archive';

interface Props {
  operationId: number;
  onClose?: () => void;
}

export function ArchiveProgress({ operationId, onClose }: Props) {
  const [progress, setProgress] = useState<ArchiveProgressEvent | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    api.listen<ArchiveProgressEvent>('archive-progress', (payload) => {
      if (payload.operation_id === operationId) setProgress(payload);
    }).then(fn => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [operationId]);

  const percent = progress && progress.total > 0
    ? Math.round((progress.current / progress.total) * 100)
    : 0;

  return (
    <div className="border border-border rounded-lg p-3 bg-surface-elevated shadow-lg">
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm font-medium text-content">
          Archive operation #{operationId}
        </span>
        <button
          onClick={async () => {
            try {
              await cancelArchiveOperation(operationId);
            } catch (e) {
              console.error('cancel archive failed', e);
            }
            onClose?.();
          }}
          className="text-xs px-2 py-1 border border-border rounded hover:bg-warning/10 text-content-muted hover:text-warning transition-colors"
        >
          Cancel
        </button>
      </div>
      {progress ? (
        <div className="text-xs text-content-muted space-y-1">
          <p className="truncate">{progress.message}</p>
          <div className="w-full h-1.5 bg-surface rounded-full overflow-hidden">
            <div
              className="h-full bg-accent transition-all duration-300"
              style={{ width: `${percent}%` }}
            />
          </div>
          <p className="text-right">{percent}%</p>
        </div>
      ) : (
        <p className="text-xs text-content-muted">Starting…</p>
      )}
    </div>
  );
}
