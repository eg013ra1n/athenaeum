import { useState, useEffect, useMemo } from 'react';
import {
  Layers,
  PlayCircle,
  XCircle,
  CheckCircle2,
  AlertTriangle,
  Loader2,
  Star,
  Info,
  RefreshCw,
  FlipHorizontal,
} from 'lucide-react';
import { api } from '../api';
import { useRegistrationProgressContext } from '../contexts/RegistrationProgressContext';
import type { RegistrationRecord, FrameRegistrationStatus } from '../types/models';

interface StackingPrepTabProps {
  framesSetId: number;
  frameSetName?: string;
  /** The user-chosen reference frame id, provided by FrameSetDetail. */
  referenceFrameId: number | null;
}

// ── Status badge ─────────────────────────────────────────────────────────────

interface StatusBadgeProps {
  status: FrameRegistrationStatus | 'aligned' | 'aligned_flipped' | 'reference' | 'failed';
}

function StatusBadge({ status }: StatusBadgeProps) {
  switch (status) {
    case 'reference':
      return (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-accent/10 text-accent border-accent/30">
          <Star size={11} />
          Reference
        </span>
      );
    case 'aligned':
      return (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-success-muted text-success border-success">
          <CheckCircle2 size={11} />
          Aligned
        </span>
      );
    case 'aligned_flipped':
      return (
        <span
          className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-info-muted text-info border-info/40"
          title="Meridian-flipped: the transform includes a reflection. Registration is valid; stacking will mirror this frame at resample time."
        >
          <FlipHorizontal size={11} />
          Flipped
        </span>
      );
    case 'aligning':
      return (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-accent/10 text-accent border-accent/30">
          <Loader2 size={11} className="animate-spin" />
          Aligning
        </span>
      );
    case 'pending':
      return (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-surface-hover text-content-muted border-border">
          Pending
        </span>
      );
    case 'failed':
      return (
        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-xs bg-error-muted text-error border-error/40">
          <AlertTriangle size={11} />
          Failed
        </span>
      );
    default:
      return null;
  }
}

// ── Main component ────────────────────────────────────────────────────────────

export function StackingPrepTab({ framesSetId, frameSetName, referenceFrameId }: StackingPrepTabProps) {
  const { enqueueRegistration, cancelRegistration, getRegistrationState } =
    useRegistrationProgressContext();
  const runState = getRegistrationState(framesSetId);

  // Persisted records loaded from the backend on mount and after each run.
  const [records, setRecords] = useState<RegistrationRecord[] | null>(null);
  const [loadingRecords, setLoadingRecords] = useState(false);

  const loadRecords = async () => {
    setLoadingRecords(true);
    try {
      const result = await api.invoke<RegistrationRecord[]>('get_frame_set_registration', {
        framesSetId,
      });
      setRecords(result);
    } catch (err) {
      console.error('[StackingPrepTab] get_frame_set_registration failed:', err);
      setRecords([]);
    } finally {
      setLoadingRecords(false);
    }
  };

  // Load persisted state on mount.
  useEffect(() => {
    loadRecords();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [framesSetId]);

  // Derive run booleans from the nullable global state.
  // isRunning = an entry exists in the global map and has not completed yet.
  const isRunning = runState != null && !runState.isComplete;
  const isCancelling = runState?.isCancelling ?? false;
  const progress = runState?.progress ?? null;
  const liveStatuses = runState?.frameStatuses ?? new Map();

  // Reload persisted records once a run completes.
  useEffect(() => {
    if (runState?.isComplete) {
      loadRecords();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runState?.isComplete]);

  // ── Derived display data ──────────────────────────────────────────────────

  const hasPersistedResults = records != null && records.length > 0;
  const referenceRecord = records?.find((r) => r.isReference);

  // Resolve a frame id to a display name. RegistrationRecord and FrameAnalysis
  // don't carry filenames, so we fall back to "frame #N". When the live run is
  // active the live status map may have a filename (populated by progress events).
  const resolveFilename = useMemo(() => {
    return (frameId: number): string => {
      // Check live statuses first (available during/after a run).
      const live = liveStatuses.get(frameId);
      if (live?.filename) return live.filename;
      return `frame #${frameId}`;
    };
  }, [liveStatuses]);

  // The filename to show for the *chosen* reference (the user-picked one).
  const chosenReferenceFilename: string | null =
    referenceFrameId != null ? resolveFilename(referenceFrameId) : null;

  // Staleness: persisted results exist, and the reference they were computed
  // against differs from the current user-chosen reference.
  const isStale =
    hasPersistedResults &&
    referenceRecord != null &&
    referenceFrameId != null &&
    referenceRecord.referenceFrameId !== referenceFrameId;

  // Median RMS from persisted records (aligned frames only — includes
  // meridian-flipped subs, which are still validly aligned).
  const alignedRecords =
    records?.filter((r) => r.status === 'aligned' || r.status === 'aligned_flipped') ?? [];
  const medianRms = (() => {
    const vals = alignedRecords
      .map((r) => r.rmsResidualArcsec ?? null)
      .filter((v): v is number => v != null)
      .sort((a, b) => a - b);
    if (vals.length === 0) return null;
    const mid = Math.floor(vals.length / 2);
    return vals.length % 2 === 0 ? (vals[mid - 1] + vals[mid]) / 2 : vals[mid];
  })();

  // Build a unified row list for the table. During a run use the live map
  // so rows update in real time; otherwise fall back to persisted records.
  type TableRow =
    | { source: 'live'; frameId: number; filename?: string; status: FrameRegistrationStatus; matchedStars?: number; rmsArcsec?: number; error?: string }
    | { source: 'persisted'; record: RegistrationRecord };

  const tableRows: TableRow[] = isRunning
    ? Array.from(liveStatuses.entries()).map(([frameId, s]) => ({
        source: 'live' as const,
        frameId,
        filename: s.filename,
        status: s.status,
        matchedStars: s.matchedStars,
        rmsArcsec: s.rmsArcsec,
        error: s.error,
      }))
    : (records ?? []).map((r) => ({ source: 'persisted' as const, record: r }));

  // ── Render ────────────────────────────────────────────────────────────────

  const progressPct =
    progress != null && progress.total > 0
      ? Math.round((progress.current / progress.total) * 100)
      : 0;

  return (
    <div className="flex flex-col gap-4 h-full overflow-y-auto pb-4">
      {/* Header / description */}
      <div className="bg-surface-elevated rounded-lg border border-border p-4">
        <div className="flex items-start gap-3">
          <Layers size={20} className="text-accent mt-0.5 shrink-0" />
          <div className="flex-1 min-w-0">
            <h2 className="text-sm font-semibold mb-1">Stacking Preparation</h2>
            <p className="text-xs text-content-muted leading-relaxed">
              Align every light frame in{' '}
              <span className="text-content font-medium">
                {frameSetName ?? `frame set #${framesSetId}`}
              </span>{' '}
              to a common reference so the frames can be combined in an external stacking tool.
              One frame is designated as the reference; all others are aligned to it using
              star-pattern matching.
            </p>
            {/* Chosen reference frame */}
            {chosenReferenceFilename && (
              <div className="mt-2 flex items-center gap-2 text-xs text-content-muted">
                <Star size={12} className="text-accent shrink-0" />
                <span>
                  Reference:{' '}
                  <span className="font-mono text-content">{chosenReferenceFilename}</span>
                  {' '}
                  <span className="text-content-muted italic">
                    — to change it, use the Analysis tab
                  </span>
                </span>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Staleness warning — reference changed since the last run */}
      {isStale && !isRunning && (
        <div className="flex items-start gap-3 rounded-lg border border-warning/40 bg-warning/10 px-4 py-3">
          <RefreshCw size={16} className="text-warning mt-0.5 shrink-0" />
          <div className="text-xs text-warning leading-relaxed">
            <span className="font-semibold">Reference changed since the last run.</span>
            {' '}Re-run stacking preparation to update the alignment data for the new reference.
            The results below are from the previous run and may no longer be valid.
          </div>
        </div>
      )}

      {/* Action bar */}
      <div className="flex items-center gap-3 flex-wrap">
        {isRunning ? (
          <>
            <div className="flex items-center gap-2 text-sm text-content-muted">
              <Loader2 size={15} className="animate-spin text-accent" />
              {progress != null ? (
                <span>
                  Aligning {progress.current}/{progress.total} frames
                  {' '}({progressPct}%)
                </span>
              ) : (
                <span>Starting…</span>
              )}
            </div>
            <button
              type="button"
              onClick={() => cancelRegistration(framesSetId)}
              disabled={isCancelling}
              className="flex items-center gap-2 px-3 py-1.5 text-sm rounded-lg border border-error/40 bg-error-muted text-error hover:bg-error/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <XCircle size={15} />
              {isCancelling ? 'Cancelling…' : 'Cancel'}
            </button>
          </>
        ) : (
          <button
            type="button"
            onClick={() =>
              enqueueRegistration(
                framesSetId,
                frameSetName,
                referenceFrameId ?? undefined,
              )
            }
            disabled={loadingRecords}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg bg-accent text-white hover:brightness-110 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            <PlayCircle size={15} />
            {hasPersistedResults ? 'Re-run preparation' : 'Prepare for stacking'}
          </button>
        )}

        {/* Readiness summary — shown when persisted results exist and not running */}
        {!isRunning && hasPersistedResults && records && (
          <div className="flex items-center gap-2 text-xs text-content-muted bg-surface-hover rounded-lg px-3 py-2 border border-border">
            <Info size={13} className="shrink-0 text-accent" />
            <span>
              <span className="text-content font-medium">{alignedRecords.length}</span>
              {'/'}
              <span className="text-content font-medium">{records.length}</span>
              {' '}aligned
              {referenceRecord && (
                <>
                  {' · '}last run reference: <span className="text-content font-medium">{resolveFilename(referenceRecord.frameId)}</span>
                </>
              )}
              {medianRms != null && (
                <>
                  {' · '}median RMS{' '}
                  <span className="text-content font-medium">{medianRms.toFixed(2)}″</span>
                </>
              )}
            </span>
          </div>
        )}
      </div>

      {/* Progress bar (visible while running) */}
      {isRunning && progress != null && (
        <div className="w-full bg-surface-hover rounded-full h-1.5 overflow-hidden">
          <div
            className="h-full bg-accent transition-all duration-300 rounded-full"
            style={{ width: `${progressPct}%` }}
          />
        </div>
      )}

      {/* Table */}
      {loadingRecords && !isRunning ? (
        <div className="flex items-center justify-center gap-2 py-10 text-content-muted text-sm">
          <Loader2 size={16} className="animate-spin" />
          Loading registration data…
        </div>
      ) : tableRows.length === 0 ? (
        <div className="flex items-center justify-center gap-2 py-10 text-content-muted text-sm">
          <Layers size={16} className="opacity-40" />
          No registration data yet — click "Prepare for stacking" to start.
        </div>
      ) : (
        <div className="rounded-lg border border-border overflow-hidden bg-surface-elevated">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border bg-surface-hover">
                <th className="text-left px-4 py-2.5 text-xs font-medium text-content-muted">Filename</th>
                <th className="text-left px-3 py-2.5 text-xs font-medium text-content-muted">Status</th>
                <th className="text-right px-3 py-2.5 text-xs font-medium text-content-muted">Matched stars</th>
                <th className="text-right px-4 py-2.5 text-xs font-medium text-content-muted">RMS (″)</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {tableRows.map((row, idx) => {
                if (row.source === 'live') {
                  return (
                    <tr key={row.frameId} className="hover:bg-surface-hover transition-colors">
                      <td className="px-4 py-2 font-mono text-xs text-content truncate max-w-[280px]">
                        <span title={row.filename}>
                          {row.filename ?? `frame #${row.frameId}`}
                        </span>
                      </td>
                      <td className="px-3 py-2">
                        <StatusBadge status={row.status} />
                      </td>
                      <td className="px-3 py-2 text-right text-content-muted tabular-nums">
                        {row.matchedStars != null ? row.matchedStars : '—'}
                      </td>
                      <td className="px-4 py-2 text-right text-content-muted tabular-nums">
                        {row.status === 'failed' ? (
                          <span
                            className="text-error cursor-help"
                            title={row.error ?? 'Alignment failed'}
                          >
                            Error
                          </span>
                        ) : row.rmsArcsec != null ? (
                          row.rmsArcsec.toFixed(2)
                        ) : (
                          '—'
                        )}
                      </td>
                    </tr>
                  );
                } else {
                  const r = row.record;
                  return (
                    <tr key={r.frameId ?? idx} className="hover:bg-surface-hover transition-colors">
                      <td className="px-4 py-2 font-mono text-xs text-content truncate max-w-[280px]">
                        <span title={`frame #${r.frameId}`}>
                          frame #{r.frameId}
                        </span>
                      </td>
                      <td className="px-3 py-2">
                        <StatusBadge status={r.status} />
                      </td>
                      <td className="px-3 py-2 text-right text-content-muted tabular-nums">
                        {r.matchedStars > 0 ? r.matchedStars : '—'}
                      </td>
                      <td className="px-4 py-2 text-right text-content-muted tabular-nums">
                        {r.status === 'failed' ? (
                          <span
                            className="text-error cursor-help"
                            title={r.error ?? 'Alignment failed'}
                          >
                            Error
                          </span>
                        ) : r.rmsResidualArcsec != null ? (
                          r.rmsResidualArcsec.toFixed(2)
                        ) : r.rmsResidualPx > 0 ? (
                          `${r.rmsResidualPx.toFixed(2)} px`
                        ) : (
                          '—'
                        )}
                      </td>
                    </tr>
                  );
                }
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
