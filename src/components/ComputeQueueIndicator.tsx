import { useState, useEffect } from 'react';
import { Layers, X } from 'lucide-react';
import { api } from '../api';
import type { ComputeQueueEntry } from '../types/models';

interface ComputeQueueIndicatorProps {
  collapsed: boolean;
}

/**
 * Sidebar card listing running + queued compute-queue jobs (Task 4/12) of
 * any kind except analysis — analysis already has its own indicator
 * (`AnalysisQueueIndicator`), so surfacing it here too would read as two
 * running jobs for the same work.
 *
 * The backend is the source of truth: `compute-queue-changed` notifications
 * can arrive out of order across worker threads, and a cancel() may produce
 * one notification whose snapshot shows no delta at all. Each event's
 * `entries` is therefore always treated as a full replacement snapshot, never
 * a diff/patch — re-rendering from the latest snapshot is idempotent
 * regardless of arrival order.
 */
export function ComputeQueueIndicator({ collapsed }: ComputeQueueIndicatorProps) {
  const [entries, setEntries] = useState<ComputeQueueEntry[]>([]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    api.invoke<ComputeQueueEntry[]>('get_compute_queue')
      .then((e) => { if (!cancelled) setEntries(e); })
      .catch((err) => console.error('[ComputeQueueIndicator] get_compute_queue failed:', err));

    api.listen<{ entries: ComputeQueueEntry[] }>('compute-queue-changed', (payload) => {
      if (!cancelled) setEntries(payload.entries);
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[ComputeQueueIndicator] listen failed:', err));

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Analysis already has its own indicator; duplicating it here would read
  // as two running jobs for the same work.
  const visible = entries.filter(e => e.kind !== 'analysis');
  if (visible.length === 0) return null;

  const cancel = (jobId: number) => {
    api.invoke('cancel_compute_job', { jobId }).catch(() => { /* may have finished */ });
  };

  if (collapsed) {
    return (
      <div className="px-2 pb-2" title={visible.map(e => e.label).join(', ')}>
        <div className="relative flex items-center justify-center py-3">
          <Layers size={20} className="text-accent" />
          <span className="absolute -top-0.5 -right-0.5 bg-accent text-surface text-[9px] font-bold rounded-full w-3.5 h-3.5 flex items-center justify-center">
            {visible.length}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 pb-2">
      <div className="bg-surface rounded-lg p-2.5 border border-border space-y-1.5">
        {visible.map(e => (
          <div key={e.jobId} className="flex items-center justify-between gap-1.5">
            <div className="flex items-center gap-1.5 min-w-0">
              <Layers size={14} className={e.state === 'running' ? 'text-accent' : 'text-content-muted'} />
              <span className="text-xs text-content-secondary truncate" title={e.label}>{e.label}</span>
            </div>
            <div className="flex items-center gap-1.5 shrink-0">
              <span className="text-[10px] text-content-muted">{e.state === 'running' ? 'running' : 'queued'}</span>
              <button
                onClick={() => cancel(e.jobId)}
                title="Cancel"
                className="text-content-muted hover:text-content transition-colors"
              >
                <X size={12} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
