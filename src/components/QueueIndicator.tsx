import type { LucideIcon } from 'lucide-react';
import { X } from 'lucide-react';

export interface QueueIndicatorProps {
  collapsed: boolean;
  icon: LucideIcon;
  active: boolean;
  label: string; // already-resolved display name
  percent: number; // 0-100, already smoothed if the caller smooths
  current?: number;
  total?: number;
  queueLength: number;
  cancelTitle: string;
  onCancelAll: () => void;
  /** 'linear' = plate-solve's JS-smoothed bar; 'smooth' = default CSS transition */
  barTransition?: 'smooth' | 'linear';
}

export function QueueIndicator({
  collapsed,
  icon: Icon,
  active,
  label,
  percent,
  current,
  total,
  queueLength,
  cancelTitle,
  onCancelAll,
  barTransition = 'smooth',
}: QueueIndicatorProps) {
  if (!active) return null;

  const pendingCount = queueLength > 1 ? queueLength - 1 : 0;
  const hasProgress = current !== undefined && total !== undefined;
  const barTransitionClass =
    barTransition === 'linear' ? 'transition-[width] duration-100 ease-linear' : 'transition-all duration-300';

  if (collapsed) {
    return (
      <div
        className="px-2 pb-2"
        title={`${label}: ${percent.toFixed(0)}%${pendingCount > 0 ? ` (+${pendingCount} queued)` : ''}`}
      >
        <div className="relative flex items-center justify-center py-3">
          <Icon size={20} className="text-accent" />
          {pendingCount > 0 && (
            <span className="absolute -top-0.5 -right-0.5 bg-accent text-surface text-[9px] font-bold rounded-full w-3.5 h-3.5 flex items-center justify-center">
              {pendingCount}
            </span>
          )}
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1">
          <div className={`bg-accent h-1 rounded-full ${barTransitionClass}`} style={{ width: `${percent}%` }} />
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <div className="bg-surface rounded-lg p-2.5 border border-border">
        <div className="flex items-center justify-between mb-1.5">
          <div className="flex items-center gap-1.5 min-w-0">
            <Icon size={14} className="text-accent shrink-0" />
            <span className="text-xs text-content-secondary truncate">{label}</span>
          </div>
          <button
            onClick={onCancelAll}
            className="text-content-muted hover:text-content transition-colors shrink-0"
            title={cancelTitle}
          >
            <X size={12} />
          </button>
        </div>
        <div className="w-full bg-surface-hover rounded-full h-1.5 mb-1">
          <div className={`bg-accent h-1.5 rounded-full ${barTransitionClass}`} style={{ width: `${percent}%` }} />
        </div>
        <div className="flex items-center justify-between text-[10px] text-content-muted">
          <span>{hasProgress ? `${current}/${total}` : 'Starting...'}</span>
          {pendingCount > 0 && <span>+{pendingCount} queued</span>}
        </div>
      </div>
    </div>
  );
}
