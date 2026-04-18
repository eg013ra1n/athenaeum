import { Crosshair, X } from 'lucide-react';
import { usePlateSolveProgressContext } from '../contexts/PlateSolveProgressContext';
import { useSmoothedPercent } from '../hooks/useSmoothedPercent';

interface PlateSolveQueueIndicatorProps {
  collapsed: boolean;
}

export function PlateSolveQueueIndicator({ collapsed }: PlateSolveQueueIndicatorProps) {
  const { currentBatch, queueLength, hasActiveBatches, cancelAll } = usePlateSolveProgressContext();

  const progress = currentBatch?.progress;
  const realPercent = progress && progress.total > 0 ? (progress.current / progress.total) * 100 : 0;
  const total = progress?.total ?? 0;
  const percent = useSmoothedPercent(realPercent, total);

  if (!hasActiveBatches) return null;

  const label = currentBatch?.label || 'Plate solve';
  const pendingCount = queueLength > 1 ? queueLength - 1 : 0;

  if (collapsed) {
    return (
      <div
        className="px-2 pb-2"
        title={`${label}: ${percent.toFixed(0)}%${pendingCount > 0 ? ` (+${pendingCount} queued)` : ''}`}
      >
        <div className="relative flex items-center justify-center py-3">
          <Crosshair size={20} className="text-accent" />
          {pendingCount > 0 && (
            <span className="absolute -top-0.5 -right-0.5 bg-accent text-surface text-[9px] font-bold rounded-full w-3.5 h-3.5 flex items-center justify-center">
              {pendingCount}
            </span>
          )}
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1">
          <div
            className="bg-accent h-1 rounded-full transition-[width] duration-100 ease-linear"
            style={{ width: `${percent}%` }}
          />
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <div className="bg-surface rounded-lg p-2.5 border border-border">
        <div className="flex items-center justify-between mb-1.5">
          <div className="flex items-center gap-1.5 min-w-0">
            <Crosshair size={14} className="text-accent shrink-0" />
            <span className="text-xs text-content-secondary truncate">{label}</span>
          </div>
          <button
            onClick={cancelAll}
            className="text-content-muted hover:text-content transition-colors shrink-0"
            title="Cancel plate solve"
          >
            <X size={12} />
          </button>
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1.5 mb-1">
          <div
            className="bg-accent h-1.5 rounded-full transition-[width] duration-100 ease-linear"
            style={{ width: `${percent}%` }}
          />
        </div>
        <div className="flex items-center justify-between text-[10px] text-content-muted">
          <span>{progress ? `${progress.current}/${progress.total}` : 'Starting...'}</span>
          {pendingCount > 0 && <span>+{pendingCount} queued</span>}
        </div>
      </div>
    </div>
  );
}
