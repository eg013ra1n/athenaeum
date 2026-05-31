import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from 'react';
import { AlertCircle, CheckCircle, Loader2, ScanSearch, XCircle } from 'lucide-react';
import { usePlateSolveProgressContext } from '../../contexts/PlateSolveProgressContext';
import { useSmoothedPercent } from '../../hooks/useSmoothedPercent';

export interface PlateSolveBatchPanelProps {
  /** All frame IDs shown in the current coordinates-missing list. Optional when the panel is driven imperatively. */
  frameIds?: number[];
  /** Frame IDs the user has checked/selected. Optional when the panel is driven imperatively. */
  selectedFrameIds?: number[];
  /** Called after a batch completes so the parent can refresh the file list. */
  onSolveComplete?: () => void;
  /** Hide the built-in "Solve Selected" / "Solve All" buttons. Use with the imperative handle to trigger a solve from an outer toolbar. */
  hideTriggerButtons?: boolean;
}

export interface PlateSolveBatchPanelHandle {
  /** Start solving the given frame IDs. No-op if ids is empty. */
  start: (frameIds: number[]) => void;
}

/**
 * Renders a live progress bar, running totals, and completion/error banners
 * for a plate-solve batch enqueued through the global plate-solve queue.
 * The queue context drives execution — this panel only visualises the most
 * recent batch it enqueued and forwards the completion callback.
 */
export const PlateSolveBatchPanel = forwardRef<PlateSolveBatchPanelHandle, PlateSolveBatchPanelProps>(function PlateSolveBatchPanel({
  frameIds = [],
  selectedFrameIds = [],
  onSolveComplete,
  hideTriggerButtons = false,
}, ref) {
  const { enqueuePlateSolve, cancelBatch, dismissCompleted, activeBatches } =
    usePlateSolveProgressContext();

  const [myBatchId, setMyBatchId] = useState<number | null>(null);
  const myBatch = myBatchId != null ? activeBatches.get(myBatchId) ?? null : null;

  // Fire the onSolveComplete callback once, when our tracked batch transitions
  // to complete. The ref guards against re-firing if React re-renders.
  const firedCompleteRef = useRef<number | null>(null);
  useEffect(() => {
    if (myBatch?.isComplete && firedCompleteRef.current !== myBatchId && myBatchId != null) {
      firedCompleteRef.current = myBatchId;
      onSolveComplete?.();
    }
  }, [myBatch?.isComplete, myBatchId, onSolveComplete]);

  const isActive = myBatch != null && !myBatch.isComplete;
  const isCancelling = myBatch?.isCancelling ?? false;
  const progress = myBatch?.progress ?? null;
  const currentFrameId = myBatch?.currentFrameId ?? null;
  const completedSummary = myBatch?.isComplete ? myBatch.summary : null;
  const completedError = myBatch?.isComplete ? myBatch.errorMessage : null;
  const frameStatuses = myBatch?.frameStatuses ?? new Map();

  const startBatch = (ids: number[]) => {
    if (ids.length === 0) return;
    const batchId = enqueuePlateSolve(ids);
    if (batchId > 0) setMyBatchId(batchId);
  };

  const handleSolveSelected = () => startBatch(selectedFrameIds);
  const handleSolveAll = () => startBatch(frameIds);
  const handleCancel = () => {
    if (myBatchId != null) cancelBatch(myBatchId);
  };
  const handleDismiss = () => {
    if (myBatchId != null) {
      dismissCompleted(myBatchId);
      setMyBatchId(null);
    }
  };

  useImperativeHandle(
    ref,
    () => ({
      start: (ids: number[]) => startBatch(ids),
    }),
    // The underlying enqueue function is stable from the context, so no
    // dependency churn — this handle is safe to memoise against [].
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [enqueuePlateSolve],
  );

  // Show numeric frame id while solving; parents can enrich later.
  const currentLabel = currentFrameId != null ? `Frame #${currentFrameId}` : null;

  const solvedCount = Array.from(frameStatuses.values()).filter(
    (s) => s.kind === 'solved',
  ).length;
  const failedCount = Array.from(frameStatuses.values()).filter(
    (s) => s.kind === 'failed',
  ).length;

  // Per-frame failure reasons, surfaced as a list on completion. `code` lets
  // us style a gate-rejected solve ("REJECTED_LOW_CONFIDENCE") differently from
  // a genuine no-solution failure.
  const failedFrames: Array<{
    frameId: number;
    error: string;
    code?: string;
    filename?: string;
  }> = [];
  for (const [frameId, s] of frameStatuses.entries()) {
    if (s.kind === 'failed') {
      failedFrames.push({ frameId, error: s.error, code: s.code, filename: s.filename });
    }
  }

  const progressFraction =
    progress && progress.total > 0 ? progress.current / progress.total : 0;
  const smoothedPercent = useSmoothedPercent(
    progressFraction * 100,
    progress?.total ?? 0,
  );

  return (
    <div className="space-y-3">
      {/* Action buttons row — hidden when driven by an outer toolbar */}
      {!hideTriggerButtons && (
        <div className="flex items-center gap-2 flex-wrap">
          <button
            onClick={handleSolveSelected}
            disabled={selectedFrameIds.length === 0}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
              bg-violet-600 hover:bg-violet-500 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ScanSearch size={15} />
            Plate Solve Selected ({selectedFrameIds.length})
          </button>

          <button
            onClick={handleSolveAll}
            disabled={frameIds.length === 0}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
              bg-violet-600 hover:bg-violet-500 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <ScanSearch size={15} />
            Plate Solve All ({frameIds.length})
          </button>

          {isActive && (
            <button
              onClick={handleCancel}
              disabled={isCancelling}
              className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
                border border-border hover:bg-surface-hover disabled:opacity-50"
            >
              <XCircle size={15} />
              {isCancelling ? 'Cancelling...' : 'Cancel'}
            </button>
          )}
        </div>
      )}

      {/* Active progress bar — Cancel sits inline at the right end of the
          top row instead of in a separate block above, so the whole widget
          stays on one tight line. */}
      {isActive && progress && (
        <div className="rounded-lg border border-border bg-surface p-3 space-y-2">
          <div className="flex items-center justify-between gap-3 text-sm">
            <div className="flex items-center gap-2 text-content-secondary min-w-0 flex-1">
              <Loader2 size={14} className="animate-spin flex-shrink-0 text-violet-400" />
              <span className="truncate">
                {isCancelling
                  ? 'Cancelling...'
                  : currentLabel
                  ? `Solving ${currentLabel}`
                  : 'Preparing...'}
              </span>
            </div>
            <span className="text-content-muted flex-shrink-0 tabular-nums">
              {progress.current} / {progress.total}
            </span>
            {hideTriggerButtons && (
              <button
                onClick={handleCancel}
                disabled={isCancelling}
                title={isCancelling ? 'Cancelling…' : 'Cancel batch'}
                className="flex-shrink-0 inline-flex items-center justify-center w-6 h-6 rounded text-content-muted hover:text-error hover:bg-surface-hover transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                aria-label="Cancel plate-solve batch"
              >
                <XCircle size={14} />
              </button>
            )}
          </div>

          <div className="h-1.5 w-full rounded-full bg-surface-hover overflow-hidden">
            <div
              className="h-full rounded-full bg-violet-500 transition-[width] duration-100 ease-linear"
              style={{ width: `${smoothedPercent}%` }}
            />
          </div>

          {(solvedCount > 0 || failedCount > 0) && (
            <div className="flex items-center gap-4 text-xs text-content-muted">
              {solvedCount > 0 && (
                <span className="flex items-center gap-1 text-success">
                  <CheckCircle size={12} />
                  {solvedCount} solved
                </span>
              )}
              {failedCount > 0 && (
                <span className="flex items-center gap-1 text-error">
                  <XCircle size={12} />
                  {failedCount} failed
                </span>
              )}
            </div>
          )}
        </div>
      )}

      {/* Completed summary banner */}
      {completedSummary && !isActive && (
        <div
          className={`p-3 rounded-lg border flex items-start gap-2 ${
            completedError
              ? 'bg-error-muted border-error/50'
              : completedSummary.failed === 0
              ? 'bg-success-muted border-success/50'
              : 'bg-warning-muted border-warning/50'
          }`}
        >
          {completedError ? (
            <AlertCircle size={16} className="text-error flex-shrink-0 mt-0.5" />
          ) : completedSummary.failed === 0 ? (
            <CheckCircle size={16} className="text-success flex-shrink-0 mt-0.5" />
          ) : (
            <AlertCircle size={16} className="text-warning flex-shrink-0 mt-0.5" />
          )}
          <div className="flex-1 text-sm">
            <p
              className={`font-medium ${
                completedError
                  ? 'text-error'
                  : completedSummary.failed === 0
                  ? 'text-success'
                  : 'text-warning'
              }`}
            >
              {completedError ? 'Plate solve could not start' : 'Batch solve complete'}
            </p>
            {completedError ? (
              <p className="text-content-muted text-xs mt-0.5 break-words">
                {completedError}
              </p>
            ) : (
              <p className="text-content-muted text-xs mt-0.5">
                {completedSummary.solved} solved, {completedSummary.failed} failed out of{' '}
                {completedSummary.total} frames &mdash;{' '}
                {(completedSummary.total_time_ms / 1000).toFixed(1)}s total
              </p>
            )}
          </div>
          <button
            onClick={handleDismiss}
            className="text-content-muted hover:text-content flex-shrink-0 ml-1"
            aria-label="Dismiss"
          >
            <XCircle size={15} />
          </button>
        </div>
      )}

      {/* Per-frame failure reasons — shown on completion so the user can see
          WHY each frame failed (or was rejected) without reading the console. */}
      {completedSummary && !isActive && failedFrames.length > 0 && (
        <details className="rounded-lg border border-border bg-surface text-sm" open>
          <summary className="cursor-pointer select-none px-3 py-2 font-medium text-content-secondary">
            Failed frames ({failedFrames.length})
          </summary>
          <ul className="max-h-56 divide-y divide-border/60 overflow-y-auto px-3 pb-2">
            {failedFrames.map(({ frameId, error, code, filename }) => {
              const rejected = code === 'REJECTED_LOW_CONFIDENCE';
              return (
                <li key={frameId} className="flex items-start gap-2 py-2">
                  {rejected ? (
                    <AlertCircle size={13} className="mt-0.5 flex-shrink-0 text-warning" />
                  ) : (
                    <XCircle size={13} className="mt-0.5 flex-shrink-0 text-error" />
                  )}
                  <div className="min-w-0">
                    <p
                      className="truncate font-medium text-content-secondary"
                      title={filename ?? `Frame #${frameId}`}
                    >
                      {filename ?? `Frame #${frameId}`}
                    </p>
                    <p className="break-words text-xs text-content-muted">{error}</p>
                  </div>
                </li>
              );
            })}
          </ul>
        </details>
      )}
    </div>
  );
});
