import { useState, useEffect, useMemo } from 'react';
import { X, Wand2, Hammer, AlertTriangle, CheckCircle2, Info, Loader2 } from 'lucide-react';
import { api } from '../../api';
import type { LightCalReadiness } from '../../types/models';
import { useLightCalibrationContext } from '../../contexts/LightCalibrationContext';

/** localStorage key for the "Normalize master flat" preference (default ON). */
export const LIGHTCAL_FLATNORM_KEY = 'athenaeum.lightcal.flatNorm';

/** Read the persisted flat-norm preference (default ON when unset/corrupt). */
export function readFlatNormPref(): boolean {
  try {
    return localStorage.getItem(LIGHTCAL_FLATNORM_KEY) !== 'false';
  } catch {
    return true;
  }
}

interface CalibrateLightsDialogProps {
  setId: number;
  setName?: string;
  onClose: () => void;
}

export function CalibrateLightsDialog({ setId, setName, onClose }: CalibrateLightsDialogProps) {
  const { startCalibration, isCalibrating } = useLightCalibrationContext();

  const [flatNorm, setFlatNorm] = useState<boolean>(readFlatNormPref);
  const [onlyStale, setOnlyStale] = useState(true);
  const [readiness, setReadiness] = useState<LightCalReadiness | null>(null);
  const [readinessError, setReadinessError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const alreadyRunning = isCalibrating(setId);

  // Persist the flat-norm choice whenever it changes.
  useEffect(() => {
    try { localStorage.setItem(LIGHTCAL_FLATNORM_KEY, String(flatNorm)); } catch { /* ignore */ }
  }, [flatNorm]);

  // Re-query readiness on open and whenever flat-norm toggles — the flag feeds
  // staleness (a frame calibrated with a different flat-norm setting reads Stale).
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setReadinessError(null);
    api.invoke<LightCalReadiness>('get_light_calibration_readiness', { setId, flatNorm })
      .then(r => { if (!cancelled) { setReadiness(r); setLoading(false); } })
      .catch(e => { if (!cancelled) { setReadinessError(String(e)); setLoading(false); } });
    return () => { cancelled = true; };
  }, [setId, flatNorm]);

  // Aggregate which calibration steps are genuinely missing (no master AND no raw
  // set to build). Bias-missing is NON-blocking under the raw-master-dark
  // convention, so it's surfaced only as a gentle note, never as a blocker.
  const missing = useMemo(() => {
    if (!readiness) return { dark: 0, flat: 0, bias: 0 };
    let dark = 0, flat = 0, bias = 0;
    for (const f of readiness.frames) {
      if (f.dark === 'missing') dark++;
      if (f.flat === 'missing') flat++;
      if (f.bias === 'missing') bias++;
    }
    return { dark, flat, bias };
  }, [readiness]);

  const total = readiness?.frames.length ?? 0;
  const mastersToBuild = readiness?.rawSetIdsToBuild.length ?? 0;

  // How many frames the run will actually process under the chosen scope.
  const willProcess = useMemo(() => {
    if (!readiness) return 0;
    return onlyStale
      ? readiness.frames.filter(f => f.status !== 'calibrated').length
      : readiness.frames.length;
  }, [readiness, onlyStale]);

  const start = async () => {
    setStarting(true);
    setStartError(null);
    try {
      await startCalibration(setId, { onlyStale }, flatNorm);
      onClose();
    } catch (e) {
      setStartError(String(e));
      setStarting(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-surface-elevated rounded-lg border border-border w-[480px] max-h-[80vh] overflow-y-auto p-4"
           onClick={e => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-medium text-content flex items-center gap-2">
            <Wand2 size={16} className="text-accent" />
            Calibrate lights{setName ? ` — ${setName}` : ''}
          </h3>
          <button onClick={onClose} className="text-content-muted hover:text-content"><X size={16} /></button>
        </div>

        <p className="text-xs text-content-muted mb-3">
          Applies each light frame's linked master dark and master flat, writing calibrated
          copies into the calibration library. Frames calibrate with whatever masters exist —
          partial coverage is fine.
        </p>

        {/* Readiness summary */}
        {loading ? (
          <div className="flex items-center gap-2 text-xs text-content-muted mb-3">
            <Loader2 size={14} className="animate-spin" /> Checking calibration readiness…
          </div>
        ) : readinessError ? (
          <div className="text-xs text-error mb-3">{readinessError}</div>
        ) : readiness ? (
          <div className="bg-surface rounded p-2.5 border border-border text-xs space-y-1.5 mb-3">
            <div className="flex items-start gap-1.5 text-success">
              <CheckCircle2 size={13} className="mt-0.5 shrink-0" />
              <span>
                <span className="font-medium">{readiness.readyCount}</span> of {total} frame{total === 1 ? '' : 's'} ready to calibrate now.
              </span>
            </div>

            {mastersToBuild > 0 && (
              <div className="flex items-start gap-1.5 text-accent">
                <Hammer size={13} className="mt-0.5 shrink-0" />
                <span>
                  <span className="font-medium">{mastersToBuild}</span> master{mastersToBuild === 1 ? '' : 's'} will be built automatically first
                  {readiness.rawSetCount > 0 && ` (covers ${readiness.rawSetCount} frame${readiness.rawSetCount === 1 ? '' : 's'})`}.
                </span>
              </div>
            )}

            {(missing.dark > 0 || missing.flat > 0) && (
              <div className="flex items-start gap-1.5 text-warning">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                <span>
                  {[
                    missing.dark > 0 ? `${missing.dark} missing a dark master` : null,
                    missing.flat > 0 ? `${missing.flat} missing a flat master` : null,
                  ].filter(Boolean).join(' · ')}
                  {' '}— these calibrate with what exists (partial) or are skipped if nothing is linked.
                </span>
              </div>
            )}

            {missing.bias > 0 && missing.dark === 0 && (
              <div className="flex items-start gap-1.5 text-content-muted">
                <Info size={13} className="mt-0.5 shrink-0" />
                <span>{missing.bias} frame{missing.bias === 1 ? '' : 's'} without a bias link — optional, raw master darks already include bias.</span>
              </div>
            )}
          </div>
        ) : null}

        {/* Scope */}
        <div className="mb-3">
          <div className="text-xs text-content-muted mb-1.5">Scope</div>
          <label className="flex items-center gap-2 text-sm text-content-secondary mb-1">
            <input type="radio" name="lightcal-scope" checked={onlyStale} onChange={() => setOnlyStale(true)} />
            Only uncalibrated &amp; stale frames
          </label>
          <label className="flex items-center gap-2 text-sm text-content-secondary">
            <input type="radio" name="lightcal-scope" checked={!onlyStale} onChange={() => setOnlyStale(false)} />
            All light frames (re-calibrate everything)
          </label>
        </div>

        {/* Options */}
        <label className="flex items-center gap-2 text-sm text-content-secondary mb-4">
          <input type="checkbox" checked={flatNorm} onChange={e => setFlatNorm(e.target.checked)} />
          Normalize master flat (recommended)
        </label>

        {alreadyRunning && (
          <div className="flex items-center gap-1.5 text-xs text-accent mb-2">
            <Loader2 size={13} className="animate-spin shrink-0" /> A calibration is already running for this set.
          </div>
        )}
        {startError && <div className="text-xs text-error mb-2">{startError}</div>}

        <div className="flex justify-end gap-2">
          <button onClick={onClose} className="px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover rounded">Cancel</button>
          <button
            onClick={start}
            disabled={starting || alreadyRunning || loading || !readiness || willProcess === 0}
            className="px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {starting
              ? 'Starting…'
              : willProcess === 0
                ? 'Nothing to calibrate'
                : `Calibrate ${willProcess} light${willProcess === 1 ? '' : 's'}`}
          </button>
        </div>
      </div>
    </div>
  );
}
