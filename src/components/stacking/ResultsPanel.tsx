import { useCallback, useEffect, useRef, useState } from 'react';
import { ChevronDown, Copy, Check, FolderOpen, Loader2, SquareStack, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { revealItemInDir } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { useNotifications } from '../../contexts/NotificationContext';
import { useStackingContext } from '../../contexts/StackingContext';
import { ConfirmDialog } from '../ConfirmDialog';
import { formatTimestamp } from '../../utils/dateFormatting';
import type { GroupStats, StackingRunDetail, StackingRunGroupRow, StackingRunSummary, WorkUsage } from '../../types/stacking';
import { readSelectedRunId, writeSelectedRunId } from './stackingPrefs';
import { ProvenanceModal } from './ProvenanceModal';

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Type guard for a parsed `GroupStats` (fix round 1, Important #7) —
 *  checks exactly the fields the card below reads, so a shape that doesn't
 *  actually support those reads is rejected here rather than trusted via
 *  `as GroupStats` and blowing up (or silently rendering `undefined`) at
 *  render time. */
function isGroupStats(x: unknown): x is GroupStats {
  if (!x || typeof x !== 'object') return false;
  const o = x as Record<string, unknown>;
  return (
    typeof o.rejectedLowFraction === 'number' &&
    typeof o.rejectedHighFraction === 'number' &&
    Array.isArray(o.masterNoise) &&
    Array.isArray(o.snrGain)
  );
}

function parseGroupStats(raw: string | null): GroupStats | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    console.error('[ResultsPanel] failed to parse group stats_json:', err);
    return null;
  }
  if (!isGroupStats(parsed)) {
    console.error('[ResultsPanel] group stats_json failed shape validation:', parsed);
    return null;
  }
  return parsed;
}

/** Reveal on desktop (`revealItemInDir` — a no-op on the web build); on the
 *  web build show the full path with a copy-to-clipboard button instead,
 *  branching on the exact `isTauri` check `src/api/desktop.ts` itself
 *  documents. Fix round 1, Minor #5: `revealItemInDir` (opens the item's
 *  PARENT folder and highlights it), not `openPath` (opens the file/folder
 *  itself) — the same function every other "reveal in file manager" call
 *  site in the app already uses (`RoleInspector.tsx`,
 *  `DualPaneFileBrowser.tsx`, `MonitoredInspector.tsx`,
 *  `ArchiveInspector.tsx`). */
function RevealOrPath({ path }: { path: string | null }) {
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);

  if (!path) return <span className="text-xs text-content-muted">no master file yet</span>;

  if (isTauri) {
    return (
      <button
        type="button"
        onClick={() => void revealItemInDir(path).catch((e) => console.error('[ResultsPanel] revealItemInDir failed:', e))}
        className="flex items-center gap-1 text-xs text-content-secondary hover:text-accent transition-colors"
      >
        <FolderOpen size={12} /> Reveal
      </button>
    );
  }

  const handleCopy = async () => {
    setCopyFailed(false);
    let ok = false;
    try {
      // Same "require the real method, never claim success without
      // actually copying" discipline as `SyncSection.tsx`'s ticket copy.
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(path);
        ok = true;
      } else if (typeof document !== 'undefined' && document.execCommand) {
        const ta = document.createElement('textarea');
        ta.value = path;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        ok = document.execCommand('copy');
        document.body.removeChild(ta);
      }
    } catch (err) {
      console.error('[ResultsPanel] copy path failed:', err);
    }
    if (ok) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } else {
      console.error('[ResultsPanel] copy path failed: clipboard unavailable');
      setCopyFailed(true);
      setTimeout(() => setCopyFailed(false), 2000);
    }
  };

  return (
    <div className="flex items-center gap-1.5 text-xs min-w-0">
      <span className="font-mono text-content-muted truncate" title={path}>{path}</span>
      <button
        type="button"
        onClick={() => void handleCopy()}
        title={copyFailed ? 'Copy failed — select the path and copy manually' : 'Copy path'}
        className="shrink-0 p-0.5 rounded hover:text-accent transition-colors text-content-muted"
      >
        {copyFailed ? <span className="text-error">!</span> : copied ? <Check size={12} /> : <Copy size={12} />}
      </button>
    </div>
  );
}

function MasterCard({ group }: { group: StackingRunGroupRow }) {
  const stats = parseGroupStats(group.statsJson);

  return (
    <div className="bg-surface rounded-lg border border-border p-3 space-y-2 min-w-0">
      <div className="flex items-start gap-2">
        <SquareStack size={28} className="text-content-muted shrink-0 mt-0.5" />
        <div className="min-w-0">
          <p className="text-sm font-medium text-content truncate" title={group.masterPath ?? group.groupKey}>
            {group.masterPath ? basename(group.masterPath) : group.groupKey}
          </p>
          <p className="text-[10px] text-content-muted">Thumbnail preview — arrives in M4</p>
        </div>
      </div>

      <p className="text-xs text-content-secondary tabular-nums">{group.includedCount} frames</p>

      {/* Fix round 1, Important #7: omit the stats line entirely when
       *  `statsJson` is absent or fails shape validation — no "—"
       *  placeholders standing in for numbers that were never computed
       *  (a group whose stats stage hasn't run yet, or an imported/corrupt
       *  row) rather than a real zero. */}
      {stats && (
        <p className="text-xs text-content-secondary tabular-nums">
          {((stats.rejectedLowFraction + stats.rejectedHighFraction) * 100).toFixed(3)}% rejected
          {' · '}
          {stats.masterNoise.length > 0 ? `noise ${stats.masterNoise[0].toExponential(3)}` : 'noise —'}
          {' · '}
          {stats.snrGain.length > 0 ? `SNR gain ${stats.snrGain[0].toFixed(2)}×` : 'SNR gain —'}
        </p>
      )}

      {group.status === 'failed' && group.error && (
        <p className="text-xs text-error">{group.error}</p>
      )}

      <RevealOrPath path={group.masterPath} />
    </div>
  );
}

export interface ResultsPanelProps {
  setId: number;
  /** True while a run is active for this set (`StackingTab`'s own
   *  `isRunning(setId) || plan?.activeRunId != null`, threaded through
   *  rather than recomputed here so the two never disagree on what
   *  "running" means). Gates the cleanup button. */
  running: boolean;
  /** Lifts the currently-selected run's detail up to `StackingTab` so the
   *  Frames table (a sibling, not a child, of this panel) can join its own
   *  rows against it. */
  onSelectedRunDetailChange: (detail: StackingRunDetail | null) => void;
}

/** The results panel (spec §11.2): run history dropdown, master cards per
 *  group, Provenance, and the working-folder usage/cleanup line. Owns its
 *  own runs/detail/usage fetching — the same self-contained-fetch pattern
 *  Task 3's `OutputPanel` used for `get_stacking_paths`. */
export function ResultsPanel({ setId, running, onSelectedRunDetailChange }: ResultsPanelProps) {
  const { notify } = useNotifications();
  const { lastOutcome } = useStackingContext();

  const [runs, setRuns] = useState<StackingRunSummary[] | null>(null);
  const [runsError, setRunsError] = useState<string | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<number | null>(null);
  const [runDetail, setRunDetail] = useState<StackingRunDetail | null>(null);
  const [usage, setUsage] = useState<WorkUsage | null>(null);
  const [usageError, setUsageError] = useState<string | null>(null);
  const [provenanceOpen, setProvenanceOpen] = useState(false);
  const [confirmCleanupOpen, setConfirmCleanupOpen] = useState(false);
  const [cleaning, setCleaning] = useState(false);

  const lastOutcomeRunId = lastOutcome.get(setId)?.runId;

  const runsSeqRef = useRef(0);
  const fetchRuns = useCallback(async () => {
    const seq = ++runsSeqRef.current;
    try {
      const list = await api.invoke<StackingRunSummary[]>('get_stacking_runs', { setId, limit: 20 });
      if (seq !== runsSeqRef.current) return; // superseded by a later call
      setRuns(list);
      setRunsError(null);
      setSelectedRunId((prev) => {
        if (prev != null && list.some((r) => r.run.id === prev)) return prev;
        const remembered = readSelectedRunId(setId);
        if (remembered != null && list.some((r) => r.run.id === remembered)) return remembered;
        return list.length > 0 ? list[0].run.id : null; // newest first
      });
    } catch (err) {
      console.error('[ResultsPanel] get_stacking_runs failed:', err);
      if (seq === runsSeqRef.current) setRunsError(String(err));
    }
  }, [setId]);

  // Mount / set change, every completion of a run for this set, and any
  // catalog change a completed run (or something else) might have caused.
  useEffect(() => { void fetchRuns(); }, [fetchRuns, lastOutcomeRunId]);
  useEffect(() => {
    const handler = () => { void fetchRuns(); };
    window.addEventListener('library-updated', handler);
    return () => window.removeEventListener('library-updated', handler);
  }, [fetchRuns]);

  // The selected run's detail — re-fetched on selection AND whenever this
  // set gets a fresh outcome (the previously-selected run might be the one
  // that just finished, its `summary` now populated for the first time).
  const detailSeqRef = useRef(0);
  useEffect(() => {
    if (selectedRunId == null) {
      setRunDetail(null);
      return;
    }
    writeSelectedRunId(setId, selectedRunId);
    let cancelled = false;
    const seq = ++detailSeqRef.current;
    (async () => {
      try {
        const detail = await api.invoke<StackingRunDetail>('get_stacking_run', { runId: selectedRunId });
        if (cancelled || seq !== detailSeqRef.current) return;
        setRunDetail(detail);
      } catch (err) {
        console.error('[ResultsPanel] get_stacking_run failed:', err);
        if (!cancelled && seq === detailSeqRef.current) setRunDetail(null);
      }
    })();
    return () => { cancelled = true; };
  }, [selectedRunId, setId, lastOutcomeRunId]);

  useEffect(() => {
    onSelectedRunDetailChange(runDetail);
  }, [runDetail, onSelectedRunDetailChange]);

  const fetchUsage = useCallback(async () => {
    try {
      const u = await api.invoke<WorkUsage>('get_stacking_work_usage', { setId });
      setUsage(u);
      setUsageError(null);
    } catch (err) {
      console.error('[ResultsPanel] get_stacking_work_usage failed:', err);
      // Fix round 1, Minor #6: a fetch failure must not leave the line
      // reading "loading…" forever — that's indistinguishable from a slow
      // request still in flight.
      setUsageError(String(err));
    }
  }, [setId]);
  useEffect(() => { void fetchUsage(); }, [fetchUsage, lastOutcomeRunId]);

  const handleCleanup = useCallback(async () => {
    setCleaning(true);
    try {
      const freed = await api.invoke<number>('cleanup_stacking_work', { setId, what: 'intermediates' });
      notify({
        title: 'Stacking intermediates deleted',
        detail: `Freed ${formatBytes(freed)}`,
        kind: 'stacking',
        tone: 'success',
      });
      // Fix round 1, Important #1: `cleanup_stacking_work` deletes the
      // artifact rows `build_plan` derives `calibratedCached`/
      // `metricsCached`/`staleStages` from — without this, the board keeps
      // showing them as cached and "Re-run from" keeps offering stale
      // stages after a successful cleanup. `StackingTab` already listens
      // for this event (the same one a finished master build or another
      // completed run dispatches).
      window.dispatchEvent(new Event('library-updated'));
      await fetchUsage();
    } catch (err) {
      console.error('[ResultsPanel] cleanup_stacking_work failed:', err);
      notify({
        title: 'Could not delete intermediates',
        detail: String(err),
        kind: 'stacking',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setCleaning(false);
      setConfirmCleanupOpen(false);
    }
  }, [setId, notify, fetchUsage]);

  const selectedRun = runs?.find((r) => r.run.id === selectedRunId) ?? null;
  // Brief's literal gate: disabled only while a run is active for this set
  // (the backend's own `Conflict` refusal is the same condition, checked
  // again server-side — see the catch below).
  const cleanupDisabled = running || cleaning;

  return (
    <div className="bg-surface-elevated rounded-lg p-3 space-y-3">
      <div className="flex items-center justify-between gap-2 flex-wrap">
        <h4 className="text-sm font-medium text-content">Results</h4>

        {runs && runs.length > 0 && (
          <div className="relative">
            <select
              value={selectedRunId ?? ''}
              onChange={(e) => setSelectedRunId(Number(e.target.value))}
              className="appearance-none pr-7 pl-2 py-1 text-xs bg-surface border border-border rounded text-content-secondary focus:outline-none focus:ring-1 focus:ring-accent"
            >
              {runs.map((r) => (
                <option key={r.run.id} value={r.run.id}>
                  #{r.run.id} · {formatTimestamp(r.run.startedAt)} · {r.run.status}
                </option>
              ))}
            </select>
            <ChevronDown size={12} className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-content-muted" />
          </div>
        )}
      </div>

      {runsError && <p className="text-sm text-error">Failed to load run history: {runsError}</p>}

      {runs == null && !runsError && (
        <div className="flex items-center gap-2 text-sm text-content-muted py-4">
          <Loader2 size={14} className="animate-spin" /> Loading run history…
        </div>
      )}

      {runs != null && runs.length === 0 && (
        <p className="text-sm text-content-muted py-2">No runs yet.</p>
      )}

      {selectedRun && runDetail && (
        <>
          <div className="flex items-center justify-between gap-2">
            <p className="text-xs text-content-muted">
              {runDetail.groups.length} group{runDetail.groups.length === 1 ? '' : 's'}
              {runDetail.run.error ? ` · ${runDetail.run.error}` : ''}
            </p>
            <button
              type="button"
              onClick={() => setProvenanceOpen(true)}
              disabled={!runDetail.summary}
              title={runDetail.summary ? undefined : 'Provenance is available once the run finishes'}
              className={`text-xs font-medium transition-colors ${
                runDetail.summary
                  ? 'text-accent hover:text-accent-hover'
                  : 'text-content-muted cursor-not-allowed'
              }`}
            >
              Provenance
            </button>
          </div>

          {runDetail.groups.length > 0 ? (
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
              {runDetail.groups.map((g) => (
                <MasterCard key={g.id} group={g} />
              ))}
            </div>
          ) : (
            <p className="text-sm text-content-muted">This run has no groups yet.</p>
          )}
        </>
      )}

      {/* Working-folder usage */}
      <div className="pt-2 border-t border-border/60 flex items-center justify-between gap-3 flex-wrap">
        <p className="text-xs text-content-muted">
          {usage
            ? `Working folder: ${formatBytes(usage.totalBytes)} total — ${formatBytes(usage.calibratedBytes)} calibrated · ${formatBytes(usage.registeredBytes)} registered · ${formatBytes(usage.lnBytes)} local-norm · ${formatBytes(usage.runsBytes)} run archives`
            : usageError
              ? 'Working folder usage: unavailable'
              : 'Working folder usage: loading…'}
        </p>
        <button
          type="button"
          onClick={() => setConfirmCleanupOpen(true)}
          disabled={cleanupDisabled}
          title={running ? 'A run is active for this set' : undefined}
          className={`flex items-center gap-1.5 px-2.5 py-1 rounded text-xs font-medium border transition-colors ${
            cleanupDisabled
              ? 'border-border text-content-muted cursor-not-allowed'
              : 'border-border text-content-secondary hover:bg-surface-hover'
          }`}
        >
          <Trash2 size={12} />
          Delete intermediates
        </button>
      </div>

      {provenanceOpen && runDetail?.summary && (
        <ProvenanceModal summary={runDetail.summary} onClose={() => setProvenanceOpen(false)} />
      )}

      <ConfirmDialog
        isOpen={confirmCleanupOpen}
        title="Delete stacking intermediates?"
        message="Removes this set's registered, calibrated and local-normalization working files — the reproducible stage output. Run manifests and master lights are kept. This cannot be undone."
        confirmText={cleaning ? 'Deleting…' : 'Delete intermediates'}
        confirmDanger
        onConfirm={() => { if (!cleaning) void handleCleanup(); }}
        onCancel={() => { if (!cleaning) setConfirmCleanupOpen(false); }}
      />
    </div>
  );
}
