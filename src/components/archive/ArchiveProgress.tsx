import { useEffect, useState } from 'react';
import { api } from '../../api';
import { cancelArchiveOperation } from '../../api/archive';
import type { ArchiveProgressEvent } from '../../types/archive';
import { useNotifications } from '../../contexts/NotificationContext';

interface Props {
  operationId: number;
  onClose?: () => void;
  onFinished?: (outcome: string) => void;
}

interface FinishedEvent {
  operation_id: number;
  outcome: 'completed' | 'cancelled' | 'failed' | string;
  /** Optional discriminator: when set to "restore", the worker was running a
   *  restore (Unarchive). The widget tweaks its title accordingly. */
  kind?: 'restore' | string;
}

// Restore-stage labels emitted by the Rust restore module.
const RESTORE_STAGES = new Set(['extract', 'verify', 'update_catalog', 'cleanup']);

export function ArchiveProgress({ operationId, onClose, onFinished }: Props) {
  const [progress, setProgress] = useState<ArchiveProgressEvent | null>(null);
  const [finished, setFinished] = useState<FinishedEvent | null>(null);
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | null = null;
    let unlistenFinished: (() => void) | null = null;
    api.listen<ArchiveProgressEvent>('archive-progress', (payload) => {
      if (cancelled) return;
      if (payload.operation_id === operationId) setProgress(payload);
    })
      .then(fn => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch(err => console.error('[ArchiveProgress] listen failed:', err));
    api.listen<FinishedEvent>('archive-finished', (payload) => {
      if (cancelled) return;
      if (payload.operation_id !== operationId) return;
      setFinished(payload);
      onFinished?.(payload.outcome);
      const verb = payload.kind === 'restore' ? 'Restore' : 'Archive';
      notify({
        title: `${verb} ${payload.outcome}`,
        detail: `Operation #${payload.operation_id}`,
        kind: 'archive',
        hasErrors: payload.outcome === 'failed',
        tone:
          payload.outcome === 'failed'
            ? 'warning'
            : payload.outcome === 'cancelled'
              ? 'info'
              : 'success',
        dedupeKey: `archive-finished-${payload.operation_id}`,
      });
      // Brief pause so the user sees the final state, then dismiss.
      window.setTimeout(() => { onClose?.(); }, 1500);
    })
      .then(fn => { if (cancelled) fn(); else unlistenFinished = fn; })
      .catch(err => console.error('[ArchiveProgress] listen failed:', err));
    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, [operationId, onClose, onFinished]);

  const percent = finished
    ? 100
    : progress && progress.total > 0
      ? Math.round((progress.current / progress.total) * 100)
      : 0;

  // Restore doesn't roll back — it just stops mid-flight if it fails. So
  // pick a different terminal label depending on whether the worker was an
  // archive (rollback restores sources) or a restore (no rollback).
  const isRestoreFinish = finished?.kind === 'restore';
  const statusLabel = finished
    ? finished.outcome === 'completed'
      ? 'Completed'
      : finished.outcome === 'cancelled'
        ? isRestoreFinish ? 'Cancelled' : 'Cancelled — rolled back'
        : isRestoreFinish ? 'Failed' : 'Failed — rolled back'
    : progress?.message ?? 'Starting…';

  const barColor = finished
    ? finished.outcome === 'completed'
      ? 'bg-success'
      : finished.outcome === 'cancelled'
        ? 'bg-warning'
        : 'bg-error'
    : 'bg-accent';

  return (
    <div className="border border-border rounded-lg p-3 bg-surface-elevated shadow-lg">
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm font-medium text-content">
          {(() => {
            const isRestore =
              finished?.kind === 'restore' ||
              (progress?.stage && RESTORE_STAGES.has(progress.stage));
            return `${isRestore ? 'Restore' : 'Archive'} operation #${operationId}`;
          })()}
        </span>
        {!finished && (
          <button
            onClick={async () => {
              try {
                await cancelArchiveOperation(operationId);
              } catch (e) {
                console.error('cancel archive failed', e);
              }
            }}
            className="text-xs px-2 py-1 border border-border rounded hover:bg-warning/10 text-content-muted hover:text-warning transition-colors"
          >
            Cancel
          </button>
        )}
      </div>
      <div className="text-xs text-content-muted space-y-1">
        <p className="truncate">{statusLabel}</p>
        <div className="w-full h-1.5 bg-surface rounded-full overflow-hidden">
          <div
            className={`h-full ${barColor} transition-all duration-300`}
            style={{ width: `${percent}%` }}
          />
        </div>
        <p className="text-right">{percent}%</p>
      </div>
    </div>
  );
}
