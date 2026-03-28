import { BarChart3, X } from 'lucide-react';
import { useAnalysisProgressContext } from '../contexts/AnalysisProgressContext';

interface AnalysisQueueIndicatorProps {
  collapsed: boolean;
}

export function AnalysisQueueIndicator({ collapsed }: AnalysisQueueIndicatorProps) {
  const { currentAnalysis, queueLength, hasActiveAnalyses, cancelAll } = useAnalysisProgressContext();

  if (!hasActiveAnalyses) return null;

  const progress = currentAnalysis?.progress;
  const percent = progress?.percent ?? 0;
  const name = currentAnalysis?.frameSetName || 'Analysis';
  const pendingCount = queueLength > 1 ? queueLength - 1 : 0;

  if (collapsed) {
    return (
      <div className="px-2 pb-2" title={`${name}: ${percent.toFixed(0)}%${pendingCount > 0 ? ` (+${pendingCount} queued)` : ''}`}>
        <div className="relative flex items-center justify-center py-3">
          <BarChart3 size={20} className="text-accent" />
          {pendingCount > 0 && (
            <span className="absolute -top-0.5 -right-0.5 bg-accent text-surface text-[9px] font-bold rounded-full w-3.5 h-3.5 flex items-center justify-center">
              {pendingCount}
            </span>
          )}
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1">
          <div className="bg-accent h-1 rounded-full transition-all duration-300" style={{ width: `${percent}%` }} />
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <div className="bg-surface rounded-lg p-2.5 border border-border">
        <div className="flex items-center justify-between mb-1.5">
          <div className="flex items-center gap-1.5 min-w-0">
            <BarChart3 size={14} className="text-accent shrink-0" />
            <span className="text-xs text-content-secondary truncate">{name}</span>
          </div>
          <button
            onClick={cancelAll}
            className="text-content-muted hover:text-content transition-colors shrink-0"
            title="Cancel analysis"
          >
            <X size={12} />
          </button>
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1.5 mb-1">
          <div className="bg-accent h-1.5 rounded-full transition-all duration-300" style={{ width: `${percent}%` }} />
        </div>
        <div className="flex items-center justify-between text-[10px] text-content-muted">
          <span>{progress ? `${progress.current}/${progress.total}` : 'Starting...'}</span>
          {pendingCount > 0 && (
            <span>+{pendingCount} queued</span>
          )}
        </div>
      </div>
    </div>
  );
}
