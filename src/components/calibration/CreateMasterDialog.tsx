import { useState, useEffect, useMemo } from 'react';
import { X, Hammer, AlertTriangle, Loader2, XCircle } from 'lucide-react';
import { api } from '../../api';
import type { MasterBuildPreview, MasterRecipe, CombineMethod } from '../../types/models';
import { useMasterBuildContext } from '../../contexts/MasterBuildContext';
import { useNotifications } from '../../contexts/NotificationContext';

interface CreateMasterDialogProps {
  setIds: number[];          // 1 = single set, >1 = batch
  onClose: () => void;
}

type CombineChoice = 'auto' | 'mean' | 'median' | 'winsorized' | 'percentile';

function toCombineMethod(c: CombineChoice, sigLo: number, sigHi: number, pLo: number, pHi: number): CombineMethod | null {
  switch (c) {
    case 'auto': return null;
    case 'mean': return { method: 'mean' };
    case 'median': return { method: 'median' };
    case 'winsorized': return { method: 'winsorized_sigma_clip', sigma_low: sigLo, sigma_high: sigHi };
    case 'percentile': return { method: 'percentile_clip', low: pLo, high: pHi };
  }
}

// One row of the batch preview list — either a resolved preview, or the
// ApiError string `preview_master_build` rejected with (same validation as
// `start`, so an ineligible set here means it'll be skipped server-side too).
type BatchPreviewResult =
  | { setId: number; kind: 'ok'; preview: MasterBuildPreview }
  | { setId: number; kind: 'error'; error: string };

// Mirrors `type_build_rank` in crates/athenaeum-core/src/api/masters.rs
// exactly: Bias & DarkFlat build first (rank 0, order doesn't matter between
// them), then Dark (1), then Flat (2) — flats resolve pre-cal at run time so
// an earlier-in-batch dark/bias/darkflat master is already on disk by the
// time a flat build runs. Rows whose preview rejected sort last (4): we
// don't know their type, and they won't be submitted anyway.
const TYPE_BUILD_RANK: Record<string, number> = { Bias: 0, DarkFlat: 0, Dark: 1, Flat: 2 };

function batchRowRank(r: BatchPreviewResult): number {
  if (r.kind === 'error') return 4;
  return TYPE_BUILD_RANK[r.preview.imagetyp] ?? 3;
}

function typeBadgeClass(imagetyp: string): string {
  switch (imagetyp) {
    case 'Flat': return 'text-accent bg-accent/10';
    case 'Bias': return 'text-orange bg-orange/10';
    case 'Dark':
    case 'DarkFlat':
      return 'text-purple bg-purple/10';
    default:
      return 'text-content-muted bg-surface';
  }
}

function formatCombine(cm: CombineMethod): string {
  switch (cm.method) {
    case 'mean': return 'mean';
    case 'median': return 'median';
    case 'winsorized_sigma_clip': return `winsorized σ ${cm.sigma_low}/${cm.sigma_high}`;
    case 'percentile_clip': return `percentile ${cm.low}/${cm.high}`;
  }
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

// Mirrors `MIN_MASTER_FRAMES` in crates/athenaeum-core/src/api/masters.rs —
// kept as a literal here since the constant isn't exposed across the IPC
// boundary; a raw precal candidate below this floor can't itself be built
// into a master, so its checkbox renders disabled+muted instead of checked.
const MIN_MASTER_FRAMES = 3;

export function CreateMasterDialog({ setIds, onClose }: CreateMasterDialogProps) {
  const { startBuild, startBatch } = useMasterBuildContext();
  const { notify } = useNotifications();
  const single = setIds.length === 1;
  const [preview, setPreview] = useState<MasterBuildPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  // Single-set mode only: which of `preview.rawPrecalSets` the operator wants
  // built as a master BEFORE this one (see the effect below for defaulting).
  const [checkedRawIds, setCheckedRawIds] = useState<Set<number>>(new Set());
  const [batchResults, setBatchResults] = useState<BatchPreviewResult[] | null>(null);
  const [batchLoading, setBatchLoading] = useState(false);
  const [combine, setCombine] = useState<CombineChoice>('auto');
  const [sigLo, setSigLo] = useState(3.0);
  const [sigHi, setSigHi] = useState(3.0);
  const [pLo, setPLo] = useState(0.2);
  const [pHi, setPHi] = useState(0.02);
  const [syntheticBias, setSyntheticBias] = useState<string>('');
  const [archiveAfter, setArchiveAfter] = useState(false);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const recipe = (): MasterRecipe => ({
    combine: toCombineMethod(combine, sigLo, sigHi, pLo, pHi),
    syntheticBias: syntheticBias.trim() === '' ? null : Number(syntheticBias),
    archiveAfter,
  });

  // `preview_master_build` is cheap — no pixel I/O, just metadata resolution
  // (Task 12/14 backend note) — so re-fetching on every recipe change is
  // safe rather than a perf hazard. We still debounce ~250ms so a burst of
  // keystrokes in the synthetic-bias field doesn't fire one request per
  // character, and the `cancelled` flag guards against an in-flight
  // response for a now-stale recipe clobbering a newer one that resolved
  // first (out-of-order network replies).
  useEffect(() => {
    if (!single) return;
    setPreviewError(null);
    let cancelled = false;
    const t = setTimeout(() => {
      api.invoke<MasterBuildPreview>('preview_master_build', { setId: setIds[0], recipe: recipe() })
        .then(p => { if (!cancelled) setPreview(p); })
        .catch(e => { if (!cancelled) setPreviewError(String(e)); });
    }, 250);
    return () => { cancelled = true; clearTimeout(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setIds, combine, sigLo, sigHi, pLo, pHi, syntheticBias]);

  // Re-derive default-checked raw-precal candidates whenever a new preview
  // resolves. A recipe change (e.g. toggling synthetic bias) can change
  // which candidates even appear, so this deliberately resets any manual
  // check/uncheck the operator made against the PREVIOUS preview rather than
  // trying to preserve it across an unrelated set of candidates — simplest
  // correct behavior, and previews are cheap enough that re-checking is a
  // non-issue in practice.
  useEffect(() => {
    if (!preview) return;
    setCheckedRawIds(new Set(
      preview.rawPrecalSets.filter(c => c.frameCount >= MIN_MASTER_FRAMES).map(c => c.setId)
    ));
  }, [preview]);

  // Batch mode: preview every set with the current recipe. Same debounce +
  // stale-guard pattern as the single-set effect above, fanned out with
  // `Promise.allSettled` so one ineligible/rejected set doesn't take the
  // whole batch preview down — its row just renders the error instead.
  // `preview_master_build` is cheap (pure DB, no pixel I/O) so N parallel
  // calls for a 50-set batch is fine.
  useEffect(() => {
    if (single) return;
    setBatchLoading(true);
    let cancelled = false;
    const t = setTimeout(() => {
      const r = recipe();
      Promise.allSettled(
        setIds.map(setId => api.invoke<MasterBuildPreview>('preview_master_build', { setId, recipe: r }))
      ).then(settled => {
        if (cancelled) return;
        setBatchResults(settled.map((res, i): BatchPreviewResult =>
          res.status === 'fulfilled'
            ? { setId: setIds[i], kind: 'ok', preview: res.value }
            : { setId: setIds[i], kind: 'error', error: String(res.reason) }
        ));
        setBatchLoading(false);
      });
    }, 250);
    return () => { cancelled = true; clearTimeout(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setIds, combine, sigLo, sigHi, pLo, pHi, syntheticBias]);

  const sortedBatch = useMemo(() => {
    if (!batchResults) return [];
    return [...batchResults].sort((a, b) => {
      const ra = batchRowRank(a), rb = batchRowRank(b);
      return ra !== rb ? ra - rb : a.setId - b.setId;
    });
  }, [batchResults]);

  const batchOkCount = batchResults?.filter(r => r.kind === 'ok').length ?? 0;
  const batchErrorCount = batchResults?.filter(r => r.kind === 'error').length ?? 0;
  const batchWarningCount = batchResults?.filter(r => r.kind === 'ok' && r.preview.warnings.length > 0).length ?? 0;

  // Single-set mode: raw precal candidates the operator has ticked to build
  // first. Non-empty here means the start button submits a batch (this set
  // PLUS its checked dependencies) instead of a lone `startBuild` — the
  // backend's existing dependency ordering (`plan_batch` / `type_build_rank`)
  // takes care of sequencing bias/darkflat/dark before this flat.
  const checkedRawIdsList = useMemo(() => Array.from(checkedRawIds), [checkedRawIds]);
  const willBatchRawFirst = single && checkedRawIdsList.length > 0;

  // Warnings of the form "linked <Type> set #<id> is raw — build its master
  // first (skipped)" are suppressed once the operator has checked that
  // candidate's box — the warning is being actively addressed, not ignored.
  const visibleWarnings = useMemo(() => {
    if (!preview) return [];
    return preview.warnings.filter(w =>
      !preview.rawPrecalSets.some(c => checkedRawIds.has(c.setId) && w.includes(`#${c.setId} is raw`))
    );
  }, [preview, checkedRawIds]);

  const runBatch = async (ids: number[]) => {
    const report = await startBatch(ids, recipe());
    if (report.skipped.length > 0) {
      const detail = report.skipped
        .slice(0, 5)
        .map(s => `#${s.setId}: ${s.reason}`)
        .join('\n');
      notify({
        title: `${report.startedSetIds.length} builds started, ${report.skipped.length} skipped`,
        detail,
        kind: 'masterbuild',
        tone: 'warning',
      });
    }
  };

  const start = async () => {
    setStarting(true);
    setStartError(null);
    try {
      if (willBatchRawFirst) {
        await runBatch([...checkedRawIdsList, setIds[0]]);
      } else if (single) {
        await startBuild(setIds[0], recipe());
      } else {
        await runBatch(setIds);
      }
      onClose();
    } catch (e) {
      setStartError(String(e));
      setStarting(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-surface-elevated rounded-lg border border-border w-[520px] max-h-[80vh] overflow-y-auto p-4"
           onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-medium text-content flex items-center gap-2">
            <Hammer size={16} className="text-accent" />
            {single ? `Create master from set #${setIds[0]}` : `Create ${setIds.length} masters`}
          </h3>
          <button onClick={onClose} className="text-content-muted hover:text-content"><X size={16} /></button>
        </div>

        {/* Recipe */}
        <label className="block text-xs text-content-muted mb-1">Combination</label>
        <select value={combine} onChange={e => setCombine(e.target.value as CombineChoice)}
                className="w-full bg-surface border border-border rounded px-2 py-1.5 text-sm mb-2">
          <option value="auto">Auto (recommended — per type & frame count)</option>
          <option value="winsorized">Winsorized sigma clip</option>
          <option value="percentile">Percentile clip</option>
          <option value="median">Median</option>
          <option value="mean">Mean</option>
        </select>
        {combine === 'winsorized' && (
          <div className="flex gap-2 mb-2">
            <label className="text-xs text-content-muted">σ low
              <input type="number" step="0.1" value={sigLo} onChange={e => setSigLo(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
            <label className="text-xs text-content-muted">σ high
              <input type="number" step="0.1" value={sigHi} onChange={e => setSigHi(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
          </div>
        )}
        {combine === 'percentile' && (
          <div className="flex gap-2 mb-2">
            <label className="text-xs text-content-muted">low
              <input type="number" step="0.01" value={pLo} onChange={e => setPLo(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
            <label className="text-xs text-content-muted">high
              <input type="number" step="0.01" value={pHi} onChange={e => setPHi(Number(e.target.value))}
                     className="w-full bg-surface border border-border rounded px-2 py-1 text-sm" /></label>
          </div>
        )}
        <label className="block text-xs text-content-muted mb-1 mt-2">
          Synthetic bias for flats (ADU, optional — used only when no darkflat/dark/bias master is linked)
        </label>
        <input value={syntheticBias} onChange={e => setSyntheticBias(e.target.value)} placeholder="e.g. 500"
               className="w-full bg-surface border border-border rounded px-2 py-1.5 text-sm mb-2" />
        <label className="flex items-center gap-2 text-sm text-content-secondary mb-3">
          <input type="checkbox" checked={archiveAfter} onChange={e => setArchiveAfter(e.target.checked)} />
          Archive originals to zip after the master is built
        </label>

        {/* Preview (single-set only) — same visual language as the batch
            rows below: type badge + id + frame count header, short-form
            combine method, muted flat pre-cal line, mono truncated target
            basename (title = full path), amber warning rows. */}
        {single && preview && (
          <div className="bg-surface rounded p-2.5 border border-border text-xs space-y-1 mb-3">
            <div className="flex items-center gap-1.5">
              <span
                className={`shrink-0 font-mono text-[10px] font-bold rounded px-1 py-0.5 ${typeBadgeClass(preview.imagetyp)}`}
              >
                {preview.imagetyp}
              </span>
              <span className="font-mono text-content-muted shrink-0">#{preview.setId}</span>
              <span className="text-content-secondary shrink-0">× {preview.frameCount} frames</span>
            </div>
            <div><span className="text-content-muted">Method:</span> <span className="text-content">{formatCombine(preview.resolvedCombine)}</span></div>
            {preview.flatPrecal && (
              <div className="text-content-muted">Flat pre-cal: {preview.flatPrecal}</div>
            )}
            <div className="flex items-center gap-1">
              <span className="text-content-muted shrink-0">Target:</span>
              <span className="font-mono text-content truncate flex-1 min-w-0" title={preview.targetPath}>
                {basename(preview.targetPath)}
              </span>
            </div>
            {visibleWarnings.map((w, i) => (
              <div key={i} className="flex items-start gap-1 text-warning" title={w}>
                <AlertTriangle size={12} className="mt-0.5 shrink-0" /><span>{w}</span>
              </div>
            ))}
          </div>
        )}

        {/* "Build raw sub-cal masters first" (single-set, flat-precal-hits-raw only) */}
        {single && preview && preview.rawPrecalSets.length > 0 && (
          <div className="bg-surface rounded p-2.5 border border-border text-xs space-y-1.5 mb-3">
            <div className="text-content-secondary font-medium">Build raw sub-cal masters first</div>
            {preview.rawPrecalSets.map(c => {
              const tooFewFrames = c.frameCount < MIN_MASTER_FRAMES;
              return (
                <label key={c.setId} className={`flex items-start gap-2 ${tooFewFrames ? 'opacity-50' : ''}`}>
                  <input
                    type="checkbox"
                    className="mt-0.5"
                    checked={checkedRawIds.has(c.setId)}
                    disabled={tooFewFrames}
                    onChange={e => setCheckedRawIds(prev => {
                      const next = new Set(prev);
                      if (e.target.checked) next.add(c.setId); else next.delete(c.setId);
                      return next;
                    })}
                  />
                  <span className={tooFewFrames ? 'text-content-muted' : 'text-content'}>
                    Build{' '}
                    <span className={`font-mono text-[10px] font-bold rounded px-1 py-0.5 ${typeBadgeClass(c.calType)}`}>
                      {c.calType}
                    </span>{' '}
                    master from set #{c.setId} first (× {c.frameCount} frames)
                    {tooFewFrames && (
                      <span className="text-content-muted"> (only {c.frameCount} frames — minimum {MIN_MASTER_FRAMES})</span>
                    )}
                  </span>
                </label>
              );
            })}
            <div className="text-content-muted italic">
              The flat will automatically use the new master (links are repointed after each build).
            </div>
          </div>
        )}
        {previewError && <div className="text-xs text-error mb-2">{previewError}</div>}

        {/* Batch preview (batch mode only) */}
        {!single && (
          <div className="mb-3">
            <div className="text-xs text-content-muted mb-1 flex items-center gap-1.5">
              {batchLoading ? (
                <><Loader2 size={12} className="animate-spin shrink-0" /> Loading preview for {setIds.length} masters…</>
              ) : (
                `Build order — ${sortedBatch.length} masters:`
              )}
            </div>
            {/* Single scroll container for the list — the dialog's own
                overflow-y-auto handles the rest of the form, and this inner
                list is capped well under 80vh so the two never need to
                scroll at once; overscroll-contain stops scroll-chaining
                into the dialog once this list hits its own edge. */}
            <div className="max-h-64 overflow-y-auto overscroll-contain rounded border border-border divide-y divide-border">
              {batchLoading
                ? setIds.map(id => (
                    <div key={id} className="px-2 py-1.5">
                      <div className="h-3.5 w-full rounded bg-surface animate-pulse" />
                    </div>
                  ))
                : sortedBatch.map(row => (
                    <div key={row.setId} className={`px-2 py-1.5 ${row.kind === 'error' ? 'opacity-50' : ''}`}>
                      <div className="flex items-center gap-1.5 text-xs">
                        <span
                          className={`shrink-0 font-mono text-[10px] font-bold rounded px-1 py-0.5 ${
                            row.kind === 'ok' ? typeBadgeClass(row.preview.imagetyp) : 'text-error bg-error/10'
                          }`}
                        >
                          {row.kind === 'ok' ? row.preview.imagetyp : 'err'}
                        </span>
                        <span className="font-mono text-content-muted shrink-0">#{row.setId}</span>
                        {row.kind === 'ok' && (
                          <>
                            <span className="text-content-secondary shrink-0">× {row.preview.frameCount} frames</span>
                            <span className="text-content-secondary shrink-0">{formatCombine(row.preview.resolvedCombine)}</span>
                          </>
                        )}
                      </div>
                      {row.kind === 'ok' ? (
                        <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-content-muted">
                          {row.preview.flatPrecal && (
                            <span className="shrink-0 truncate max-w-[9rem]" title={row.preview.flatPrecal}>
                              {row.preview.flatPrecal}
                            </span>
                          )}
                          <span className="font-mono truncate flex-1 min-w-0" title={row.preview.targetPath}>
                            {basename(row.preview.targetPath)}
                          </span>
                        </div>
                      ) : (
                        <div className="mt-0.5 flex items-center gap-1 text-[11px] text-error" title={row.error}>
                          <XCircle size={11} className="shrink-0" />
                          <span className="truncate">{row.error}</span>
                        </div>
                      )}
                      {row.kind === 'ok' && row.preview.warnings.length > 0 && (
                        <div className="mt-0.5 flex items-center gap-1 text-[11px] text-warning" title={row.preview.warnings.join('\n')}>
                          <AlertTriangle size={11} className="shrink-0" />
                          <span className="truncate">{row.preview.warnings[0]}</span>
                        </div>
                      )}
                    </div>
                  ))
              }
            </div>
            {!batchLoading && (
              <div className="mt-1 text-[11px] text-content-muted">
                <div>{batchOkCount} will build · {batchWarningCount} warning{batchWarningCount === 1 ? '' : 's'} · {batchErrorCount} skipped</div>
                {batchErrorCount > 0 && <div className="italic mt-0.5">Rows with errors will be skipped by the backend.</div>}
              </div>
            )}
          </div>
        )}

        {startError && <div className="text-xs text-error mb-2">{startError}</div>}

        <div className="flex justify-end gap-2">
          <button onClick={onClose} className="px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover rounded">Cancel</button>
          <button onClick={start} disabled={starting}
                  className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded disabled:opacity-50">
            {starting
              ? 'Starting…'
              : willBatchRawFirst
                ? `Create ${checkedRawIdsList.length + 1} masters`
                : single ? 'Create master' : 'Create all'}
          </button>
        </div>
      </div>
    </div>
  );
}
