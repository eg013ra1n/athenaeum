import { useState, useEffect } from 'react';
import { X, Hammer, AlertTriangle } from 'lucide-react';
import { api } from '../../api';
import type { MasterBuildPreview, MasterRecipe, CombineMethod } from '../../types/models';
import { useMasterBuildContext } from '../../contexts/MasterBuildContext';

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

export function CreateMasterDialog({ setIds, onClose }: CreateMasterDialogProps) {
  const { startBuild, startBatch } = useMasterBuildContext();
  const single = setIds.length === 1;
  const [preview, setPreview] = useState<MasterBuildPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
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

  const start = async () => {
    setStarting(true);
    setStartError(null);
    try {
      if (single) await startBuild(setIds[0], recipe());
      else await startBatch(setIds, recipe());
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

        {/* Preview (single-set only) */}
        {single && preview && (
          <div className="bg-surface rounded p-2.5 border border-border text-xs space-y-1 mb-3">
            <div><span className="text-content-muted">Frames:</span> <span className="text-content">{preview.frameCount}</span></div>
            <div><span className="text-content-muted">Method:</span> <span className="text-content font-mono">{JSON.stringify(preview.resolvedCombine)}</span></div>
            {preview.flatPrecal && (
              <div><span className="text-content-muted">Flat pre-cal:</span> <span className="text-content">{preview.flatPrecal}</span></div>
            )}
            <div><span className="text-content-muted">Target:</span> <span className="text-content font-mono break-all">{preview.targetPath}</span></div>
            {preview.warnings.map((w, i) => (
              <div key={i} className="flex items-start gap-1 text-warning">
                <AlertTriangle size={12} className="mt-0.5 shrink-0" />{w}
              </div>
            ))}
          </div>
        )}
        {previewError && <div className="text-xs text-error mb-2">{previewError}</div>}
        {startError && <div className="text-xs text-error mb-2">{startError}</div>}

        <div className="flex justify-end gap-2">
          <button onClick={onClose} className="px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover rounded">Cancel</button>
          <button onClick={start} disabled={starting}
                  className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded disabled:opacity-50">
            {starting ? 'Starting…' : single ? 'Create master' : 'Create all'}
          </button>
        </div>
      </div>
    </div>
  );
}
