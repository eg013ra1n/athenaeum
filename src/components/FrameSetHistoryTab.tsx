import { useCallback, useEffect, useState } from 'react';
import { ChevronRight, History, Loader2, Zap, Mouse } from 'lucide-react';
import { api } from '../api';
import type { MergeLogEntry } from '../types/models';
import { formatTimestamp } from '../utils/dateFormatting';

interface Props {
  frameSetId: number;
}

/**
 * History tab for a frame set — shows every auto-merge operation with
 * timestamp, source (button vs. monitor), and an expandable per-frame
 * breakdown for audit.
 */
export function FrameSetHistoryTab({ frameSetId }: Props) {
  const [entries, setEntries] = useState<MergeLogEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const load = useCallback(async () => {
    setError(null);
    try {
      const result = await api.invoke<MergeLogEntry[]>('get_frame_set_merge_log', {
        framesSetId: frameSetId,
        limit: 100,
      });
      setEntries(result);
    } catch (e) {
      setError(String(e));
    }
  }, [frameSetId]);

  useEffect(() => {
    load();
  }, [load]);

  const toggle = (id: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (error) {
    return (
      <div className="rounded-lg border border-error/50 bg-error-muted p-4 text-sm text-error">
        Failed to load merge history: {error}
      </div>
    );
  }

  if (entries === null) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-content-muted">
        <Loader2 size={16} className="mr-2 animate-spin" />
        Loading history…
      </div>
    );
  }

  if (entries.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center gap-2 py-16 text-content-muted">
        <History size={32} className="opacity-50" />
        <p className="text-sm">No auto-merge activity yet for this frame set.</p>
        <p className="text-xs">
          Use "Find new images" to attach newly-discovered frames. Each operation will be
          recorded here for audit.
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {entries.map((entry) => {
        const isOpen = expanded.has(entry.id);
        const Icon = entry.source === 'monitor' ? Zap : Mouse;
        return (
          <div
            key={entry.id}
            className="rounded-lg border border-border bg-surface-elevated"
          >
            <button
              type="button"
              onClick={() => toggle(entry.id)}
              className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-surface-hover"
            >
              <ChevronRight
                size={14}
                className={`shrink-0 text-content-muted transition-transform ${isOpen ? 'rotate-90' : ''}`}
              />
              <Icon size={14} className="shrink-0 text-content-muted" />
              <div className="min-w-0 flex-1">
                <p className="text-sm">
                  <span className="font-medium text-success">
                    +{entry.added_count}
                  </span>
                  {entry.skipped_count > 0 && (
                    <>
                      {' · '}
                      <span className="text-warning">{entry.skipped_count} skipped</span>
                    </>
                  )}
                  <span className="ml-2 text-xs text-content-muted">
                    via {entry.source}
                  </span>
                </p>
                <p className="mt-0.5 text-xs text-content-muted">
                  {formatTimestamp(entry.occurred_at)} · threshold{' '}
                  {entry.threshold_arcmin.toFixed(1)}′
                </p>
              </div>
            </button>

            {isOpen && (
              <div className="border-t border-border px-4 py-3 text-xs">
                {entry.frames_added.length > 0 && (
                  <div className="mb-3">
                    <p className="mb-1 font-medium text-content-secondary">
                      Added ({entry.frames_added.length})
                    </p>
                    <ul className="space-y-0.5 font-mono">
                      {entry.frames_added.map((f) => (
                        <li key={f.frame_id} className="text-content-muted">
                          {f.filename}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                {entry.frames_skipped.length > 0 && (
                  <div>
                    <p className="mb-1 font-medium text-content-secondary">
                      Skipped ({entry.frames_skipped.length})
                    </p>
                    <ul className="space-y-0.5 font-mono">
                      {entry.frames_skipped.map((f) => (
                        <li key={f.frame_id} className="text-content-muted">
                          {f.filename} — {readableReason(f.reason)}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function readableReason(reason: string): string {
  switch (reason) {
    case 'no_coords':
      return 'missing coordinates or date_obs';
    default:
      return reason;
  }
}
