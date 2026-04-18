import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from 'react';
import {
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
  Loader2,
  Target,
  XCircle,
} from 'lucide-react';
import { api } from '../../api';
import type {
  AutofindProgressEvent,
  AutofindCompleteEvent,
  AutofindStatus,
} from '../../types/plate-solve';

export interface FillObjectsPanelProps {
  /** All frame IDs shown in the current missing-object list. Optional when the panel is driven imperatively. */
  frameIds?: number[];
  /** Frame IDs the user has checked/selected. Optional when the panel is driven imperatively. */
  selectedFrameIds?: number[];
  /** Called after a batch completes so the parent can refresh the file list. */
  onComplete?: () => void;
  /** Hide the built-in trigger buttons. Use with the imperative handle to start the batch from an outer toolbar. */
  hideTriggerButtons?: boolean;
}

export interface FillObjectsPanelHandle {
  /** Start autofind for the given frame IDs. No-op if another batch is active or ids is empty. */
  start: (frameIds: number[]) => void;
}

interface RunningTotals {
  labeled: number;
  no_match: number;
  already_labeled: number;
  missing_coords: number;
  errors: number;
}

const EMPTY_TOTALS: RunningTotals = {
  labeled: 0,
  no_match: 0,
  already_labeled: 0,
  missing_coords: 0,
  errors: 0,
};

/** One stored outcome per frame — populated from the terminal progress event. */
interface FrameOutcome {
  frame_id: number;
  status: Exclude<AutofindStatus, 'processing'>;
  designation: string | null;
  distance_deg: number | null;
  reason: 'contains' | 'nearest' | null;
  frame_ra: number | null;
  frame_dec: number | null;
  closest_designation: string | null;
  closest_distance_deg: number | null;
}

const STATUS_LABELS: Record<Exclude<AutofindStatus, 'processing'>, string> = {
  labeled: 'Labeled',
  no_match: 'No match',
  already_labeled: 'Already labeled',
  missing_coords: 'Missing coordinates',
  error: 'Errors',
};

/**
 * "Autofind Object" batch panel. Looks up `frame.object` from stored RA/Dec
 * using the bundled DSO catalog (tight 0.2° proximity tolerance) and writes
 * `"Autofind: <designation>"` back to the database for frames that are
 * missing an object name.
 *
 * Designed to sit in FileManager's Missing Metadata tab when the
 * "Missing Object Name" category is selected, mirroring the shape of
 * PlateSolveBatchPanel in the Missing Coordinates slot.
 */
export const FillObjectsPanel = forwardRef<FillObjectsPanelHandle, FillObjectsPanelProps>(function FillObjectsPanel({
  frameIds = [],
  selectedFrameIds = [],
  onComplete,
  hideTriggerButtons = false,
}, ref) {
  const [isActive, setIsActive] = useState(false);
  const [isCancelling, setIsCancelling] = useState(false);
  const [progress, setProgress] = useState<{ current: number; total: number } | null>(null);
  const [currentFrameId, setCurrentFrameId] = useState<number | null>(null);
  const [totals, setTotals] = useState<RunningTotals>(EMPTY_TOTALS);
  const [completed, setCompleted] = useState<AutofindCompleteEvent | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Per-frame terminal outcomes, keyed by frame_id. */
  const [outcomes, setOutcomes] = useState<Map<number, FrameOutcome>>(new Map());
  /** Which status groups are expanded in the results details. */
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(
    () => new Set(['no_match', 'missing_coords', 'error']),
  );

  const isActiveRef = useRef(false);

  // Subscribe once to the two event names.
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;

    (async () => {
      unlistenProgress = await api.listen<AutofindProgressEvent>(
        'autofind-objects-progress',
        (payload) => {
          setCurrentFrameId(payload.frame_id);
          setProgress({ current: payload.current, total: payload.total });

          // Processing is the pre-event; skip counter bumps for it.
          if (payload.status === 'processing') return;

          setTotals((prev) => {
            const next = { ...prev };
            switch (payload.status) {
              case 'labeled':
                next.labeled += 1;
                break;
              case 'no_match':
                next.no_match += 1;
                break;
              case 'already_labeled':
                next.already_labeled += 1;
                break;
              case 'missing_coords':
                next.missing_coords += 1;
                break;
              case 'error':
                next.errors += 1;
                break;
            }
            return next;
          });

          // Store the terminal outcome for the results-details list. We've
          // already returned early on status === 'processing', so cast to the
          // terminal subset here — TS doesn't preserve narrowing across the
          // closure boundary.
          const terminalStatus = payload.status as FrameOutcome['status'];
          setOutcomes((prev) => {
            const next = new Map(prev);
            next.set(payload.frame_id, {
              frame_id: payload.frame_id,
              status: terminalStatus,
              designation: payload.designation,
              distance_deg: payload.distance_deg,
              reason: payload.reason,
              frame_ra: payload.frame_ra,
              frame_dec: payload.frame_dec,
              closest_designation: payload.closest_designation,
              closest_distance_deg: payload.closest_distance_deg,
            });
            return next;
          });
        },
      );

      unlistenComplete = await api.listen<AutofindCompleteEvent>(
        'autofind-objects-complete',
        (payload) => {
          setCompleted(payload);
          setIsActive(false);
          setIsCancelling(false);
          isActiveRef.current = false;
          setCurrentFrameId(null);
          onComplete?.();
        },
      );
    })();

    return () => {
      unlistenProgress?.();
      unlistenComplete?.();
    };
    // onComplete is captured by the listener; we want listeners to be
    // installed once per mount, not re-installed on every prop change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const start = useCallback(
    async (ids: number[]) => {
      if (isActiveRef.current || ids.length === 0) return;

      setError(null);
      setCompleted(null);
      setTotals(EMPTY_TOTALS);
      setOutcomes(new Map());
      setProgress({ current: 0, total: ids.length });
      setIsCancelling(false);
      setIsActive(true);
      isActiveRef.current = true;

      try {
        await api.invoke<void>('autofind_objects_from_coordinates', {
          frameIds: ids,
        });
      } catch (err) {
        const msg = String(err);
        setError(msg);
        setIsActive(false);
        isActiveRef.current = false;
        setCurrentFrameId(null);
        console.error('Autofind objects failed:', err);
      }
    },
    [],
  );

  const handleAutofindSelected = () => start(selectedFrameIds);
  const handleAutofindAll = () => start(frameIds);

  useImperativeHandle(
    ref,
    () => ({
      start: (ids: number[]) => {
        void start(ids);
      },
    }),
    [start],
  );

  const cancel = useCallback(async () => {
    if (!isActiveRef.current) return;
    setIsCancelling(true);
    try {
      await api.invoke<void>('cancel_autofind_objects', {});
    } catch (err) {
      console.error('Failed to cancel autofind:', err);
    }
  }, []);

  const progressFraction =
    progress && progress.total > 0 ? progress.current / progress.total : 0;

  const currentLabel = currentFrameId != null ? `Frame #${currentFrameId}` : null;

  const toggleGroup = useCallback((key: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }, []);

  /** Bucket outcomes by status for the results-details list. */
  const groupedOutcomes = useMemo(() => {
    const groups: Record<FrameOutcome['status'], FrameOutcome[]> = {
      labeled: [],
      no_match: [],
      already_labeled: [],
      missing_coords: [],
      error: [],
    };
    for (const o of outcomes.values()) {
      groups[o.status].push(o);
    }
    // Stable order inside each group — ascending frame_id.
    for (const k of Object.keys(groups) as Array<FrameOutcome['status']>) {
      groups[k].sort((a, b) => a.frame_id - b.frame_id);
    }
    return groups;
  }, [outcomes]);

  return (
    <div className="space-y-3">
      {/* Action buttons — hidden when driven by an outer toolbar */}
      {!hideTriggerButtons && (
        <div className="flex items-center gap-2 flex-wrap">
          <button
            onClick={handleAutofindSelected}
            disabled={isActive || selectedFrameIds.length === 0}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
              bg-emerald-600 hover:bg-emerald-500 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Target size={15} />
            Autofind Object Selected ({selectedFrameIds.length})
          </button>

          <button
            onClick={handleAutofindAll}
            disabled={isActive || frameIds.length === 0}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
              bg-emerald-600 hover:bg-emerald-500 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Target size={15} />
            Autofind Object All ({frameIds.length})
          </button>

          {isActive && (
            <button
              onClick={cancel}
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

      {/* Cancel button in hidden-buttons mode — only shown while active */}
      {hideTriggerButtons && isActive && (
        <div className="flex items-center justify-end">
          <button
            onClick={cancel}
            disabled={isCancelling}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors
              border border-border hover:bg-surface-hover disabled:opacity-50"
          >
            <XCircle size={15} />
            {isCancelling ? 'Cancelling...' : 'Cancel'}
          </button>
        </div>
      )}

      {/* Active progress */}
      {isActive && progress && (
        <div className="rounded-lg border border-border bg-surface p-3 space-y-2">
          <div className="flex items-center justify-between text-sm">
            <div className="flex items-center gap-2 text-content-secondary min-w-0">
              <Loader2 size={14} className="animate-spin flex-shrink-0 text-emerald-400" />
              <span className="truncate">
                {isCancelling
                  ? 'Cancelling...'
                  : currentLabel
                  ? `Looking up ${currentLabel}`
                  : 'Preparing...'}
              </span>
            </div>
            <span className="text-content-muted flex-shrink-0 ml-3">
              {progress.current} / {progress.total}
            </span>
          </div>

          {/* Progress bar */}
          <div className="h-1.5 w-full rounded-full bg-surface-hover overflow-hidden">
            <div
              className="h-full rounded-full bg-emerald-500 transition-all duration-200"
              style={{ width: `${progressFraction * 100}%` }}
            />
          </div>

          {/* Running totals */}
          {(totals.labeled > 0 ||
            totals.no_match > 0 ||
            totals.already_labeled > 0 ||
            totals.missing_coords > 0 ||
            totals.errors > 0) && (
            <div className="flex items-center gap-4 text-xs text-content-muted flex-wrap">
              {totals.labeled > 0 && (
                <span className="flex items-center gap-1 text-success">
                  <CheckCircle size={12} />
                  {totals.labeled} labeled
                </span>
              )}
              {totals.no_match > 0 && (
                <span>{totals.no_match} no match</span>
              )}
              {totals.already_labeled > 0 && (
                <span>{totals.already_labeled} already labeled</span>
              )}
              {totals.missing_coords > 0 && (
                <span>{totals.missing_coords} missing coords</span>
              )}
              {totals.errors > 0 && (
                <span className="flex items-center gap-1 text-error">
                  <XCircle size={12} />
                  {totals.errors} errors
                </span>
              )}
            </div>
          )}
        </div>
      )}

      {/* Error banner */}
      {error && !isActive && (
        <div className="p-3 bg-error-muted border border-error/50 rounded-lg flex items-start gap-2">
          <AlertCircle size={16} className="text-error flex-shrink-0 mt-0.5" />
          <div className="flex-1 min-w-0">
            <p className="text-sm font-medium text-error">Autofind error</p>
            <p className="text-xs text-error/80 mt-0.5 break-words">{error}</p>
          </div>
        </div>
      )}

      {/* Completion banner */}
      {completed && !isActive && (
        <div
          className={`p-3 rounded-lg border flex items-start gap-2 ${
            completed.errors === 0
              ? 'bg-success-muted border-success/50'
              : 'bg-warning-muted border-warning/50'
          }`}
        >
          {completed.errors === 0 ? (
            <CheckCircle size={16} className="text-success flex-shrink-0 mt-0.5" />
          ) : (
            <AlertCircle size={16} className="text-warning flex-shrink-0 mt-0.5" />
          )}
          <div className="flex-1 text-sm">
            <p
              className={`font-medium ${
                completed.errors === 0 ? 'text-success' : 'text-warning'
              }`}
            >
              {completed.cancelled ? 'Autofind cancelled' : 'Autofind complete'}
            </p>
            <p className="text-content-muted text-xs mt-0.5">
              {completed.labeled} labeled, {completed.no_match} no match,{' '}
              {completed.already_labeled} already labeled,{' '}
              {completed.missing_coords} missing coords
              {completed.errors > 0 && `, ${completed.errors} errors`} — out of{' '}
              {completed.total} frames in{' '}
              {(completed.total_time_ms / 1000).toFixed(1)}s
            </p>
          </div>
          <button
            onClick={() => setCompleted(null)}
            className="text-content-muted hover:text-content flex-shrink-0 ml-1"
            aria-label="Dismiss"
          >
            <XCircle size={15} />
          </button>
        </div>
      )}

      {/* Results details — collapsible per-frame list, grouped by status.
          Only visible after a run completes and only for non-empty groups. */}
      {completed && !isActive && outcomes.size > 0 && (
        <div className="rounded-lg border border-border bg-surface text-xs">
          <div className="px-3 py-2 text-content-muted border-b border-border">
            Results details
          </div>
          {(['no_match', 'missing_coords', 'error', 'labeled', 'already_labeled'] as const).map(
            (status) => {
              const list = groupedOutcomes[status];
              if (list.length === 0) return null;
              const expanded = expandedGroups.has(status);
              return (
                <div key={status} className="border-b border-border last:border-b-0">
                  <button
                    onClick={() => toggleGroup(status)}
                    className="w-full px-3 py-1.5 flex items-center gap-2 hover:bg-surface-hover text-left"
                  >
                    {expanded ? (
                      <ChevronDown size={12} className="flex-shrink-0" />
                    ) : (
                      <ChevronRight size={12} className="flex-shrink-0" />
                    )}
                    <span className="flex-1 font-medium">{STATUS_LABELS[status]}</span>
                    <span className="text-content-muted">{list.length}</span>
                  </button>
                  {expanded && (
                    <div className="px-3 py-1 pb-2 space-y-0.5 font-mono text-[10px] max-h-64 overflow-y-auto">
                      {list.slice(0, 500).map((o) => (
                        <div key={o.frame_id} className="text-content-muted">
                          <span className="text-content">#{o.frame_id}</span>
                          {o.frame_ra != null && o.frame_dec != null && (
                            <span className="ml-2">
                              ra={o.frame_ra.toFixed(3)}° dec={o.frame_dec.toFixed(3)}°
                            </span>
                          )}
                          {o.status === 'labeled' && o.designation && (
                            <span className="ml-2 text-success">
                              → {o.designation}
                              {o.distance_deg != null && ` (${o.distance_deg.toFixed(3)}°`}
                              {o.reason && `, ${o.reason}`}
                              {o.distance_deg != null && ')'}
                            </span>
                          )}
                          {o.status === 'no_match' && o.closest_designation && (
                            <span className="ml-2">
                              — closest {o.closest_designation} at{' '}
                              {o.closest_distance_deg?.toFixed(3)}° (outside tolerance)
                            </span>
                          )}
                          {o.status === 'no_match' && !o.closest_designation && (
                            <span className="ml-2">— no DSO found</span>
                          )}
                          {o.status === 'missing_coords' && (
                            <span className="ml-2">— frame has no RA/Dec</span>
                          )}
                          {o.status === 'already_labeled' && (
                            <span className="ml-2">— skipped (object already set)</span>
                          )}
                          {o.status === 'error' && (
                            <span className="ml-2 text-error">— SQL error (see console)</span>
                          )}
                        </div>
                      ))}
                      {list.length > 500 && (
                        <div className="text-content-muted italic pt-1">
                          … {list.length - 500} more (check stderr for the full list)
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            },
          )}
        </div>
      )}
    </div>
  );
});
