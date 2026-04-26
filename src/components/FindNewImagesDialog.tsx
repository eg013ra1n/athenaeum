import { useCallback, useEffect, useState } from 'react';
import { Loader2, Search, X } from 'lucide-react';
import { api } from '../api';
import type { CandidateFrame, FindNewFramesResult, MergeReport } from '../types/models';

interface Props {
  frameSetId: number;
  frameSetName: string;
  onClose: () => void;
  /** Called with the merge report after a successful merge. */
  onMerged: (report: MergeReport) => void;
}

type Phase = 'idle' | 'searching' | 'awaitingScan' | 'reviewing' | 'merging';

export function FindNewImagesDialog({ frameSetId, frameSetName, onClose, onMerged }: Props) {
  const [phase, setPhase] = useState<Phase>('idle');
  const [scanFirst, setScanFirst] = useState(false);
  const [candidates, setCandidates] = useState<CandidateFrame[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);

  const runSearch = useCallback(async () => {
    setError(null);
    setPhase(scanFirst ? 'awaitingScan' : 'searching');
    try {
      const result = await api.invoke<FindNewFramesResult>('find_new_frames_for_set', {
        framesSetId: frameSetId,
        scanFirst,
      });
      if (result.scan_was_awaited) {
        // The waiting happened inside the backend; transitioning to reviewing
        // just resets the banner.
      }
      setCandidates(result.candidates);
      // Start with all candidates selected — the user can uncheck.
      setSelected(new Set(result.candidates.map((c) => c.frame_id)));
      setPhase('reviewing');
    } catch (e) {
      setError(String(e));
      setPhase('idle');
    }
  }, [frameSetId, scanFirst]);

  // Auto-run the initial search on mount.
  useEffect(() => {
    runSearch();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const toggle = (frame_id: number) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(frame_id)) next.delete(frame_id);
      else next.add(frame_id);
      return next;
    });
  };

  const handleMerge = async () => {
    setError(null);
    setPhase('merging');
    try {
      const report = await api.invoke<MergeReport>('auto_merge_new_frames_for_set', {
        framesSetId: frameSetId,
        frameIds: Array.from(selected),
        source: 'button',
      });
      onMerged(report);
    } catch (e) {
      setError(String(e));
      setPhase('reviewing');
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="relative max-h-[80vh] w-[640px] max-w-[95vw] flex flex-col rounded-xl border border-border bg-surface-elevated shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <div>
            <h2 className="text-lg font-semibold">Find new images</h2>
            <p className="text-xs text-content-muted">Target: {frameSetName}</p>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="rounded p-1 text-content-muted hover:bg-surface-hover hover:text-content"
          >
            <X size={18} />
          </button>
        </div>

        <div className="border-b border-border px-5 py-3">
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={scanFirst}
              disabled={phase === 'searching' || phase === 'awaitingScan' || phase === 'merging'}
              onChange={(e) => setScanFirst(e.target.checked)}
              className="w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
            />
            <span>Scan disks first</span>
            <span className="text-xs text-content-muted">
              — will await any in-flight scan, then match
            </span>
          </label>
          <div className="mt-3 flex items-center gap-2">
            <button
              type="button"
              onClick={runSearch}
              disabled={phase === 'searching' || phase === 'awaitingScan' || phase === 'merging'}
              className="flex items-center gap-2 rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50"
            >
              {phase === 'searching' || phase === 'awaitingScan' ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Search size={14} />
              )}
              {phase === 'awaitingScan'
                ? 'Waiting for running scan…'
                : phase === 'searching'
                  ? 'Searching…'
                  : 'Re-run search'}
            </button>
          </div>
          {error && (
            <p className="mt-2 text-sm text-error">{error}</p>
          )}
        </div>

        <div className="flex-1 min-h-0 overflow-y-auto">
          {phase === 'reviewing' || phase === 'merging' ? (
            candidates.length === 0 ? (
              <div className="px-5 py-12 text-center text-sm text-content-muted">
                No unclustered lights match this target's coordinates.
              </div>
            ) : (
              <ul className="divide-y divide-border">
                {candidates.map((c) => {
                  const checked = selected.has(c.frame_id);
                  return (
                    <li key={c.frame_id} className="flex items-center gap-3 px-5 py-2 text-sm">
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={phase === 'merging'}
                        onChange={() => toggle(c.frame_id)}
                        className="w-4 h-4 rounded border-border text-accent focus:ring-2 focus:ring-accent focus:ring-offset-0 bg-surface-hover"
                      />
                      <div className="min-w-0 flex-1">
                        <p className="truncate font-mono" title={c.filename}>
                          {c.filename}
                        </p>
                        <p className="truncate text-xs text-content-muted">
                          {c.filter ?? '—'} · {c.instrume ?? '—'} ·{' '}
                          {c.date_obs ? c.date_obs.replace('T', ' ').slice(0, 16) : 'no date'}
                        </p>
                      </div>
                      <span className="shrink-0 font-mono text-xs text-content-muted">
                        {c.ra?.toFixed(3)}° / {c.dec?.toFixed(3)}°
                      </span>
                    </li>
                  );
                })}
              </ul>
            )
          ) : (
            <div className="flex items-center justify-center px-5 py-12 text-sm text-content-muted">
              <Loader2 size={16} className="mr-2 animate-spin" />
              {phase === 'awaitingScan' ? 'Waiting for running scan to finish…' : 'Searching…'}
            </div>
          )}
        </div>

        <div className="flex items-center justify-between border-t border-border px-5 py-3">
          <p className="text-xs text-content-muted">
            {phase === 'reviewing' || phase === 'merging'
              ? `${selected.size} of ${candidates.length} selected`
              : ''}
          </p>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={onClose}
              disabled={phase === 'merging'}
              className="rounded-lg border border-border bg-surface-hover px-3 py-1.5 text-sm hover:brightness-110 disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={handleMerge}
              disabled={
                phase !== 'reviewing' || selected.size === 0
              }
              className="flex items-center gap-2 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-surface hover:brightness-90 disabled:opacity-50"
            >
              {phase === 'merging' && <Loader2 size={14} className="animate-spin" />}
              Merge {selected.size > 0 ? `(${selected.size})` : ''}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
