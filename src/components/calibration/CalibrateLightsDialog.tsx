import { useState, useEffect, useMemo } from 'react';
import { X, Wand2, Hammer, AlertTriangle, CheckCircle2, Info, Loader2, ChevronDown, ChevronRight, SlidersHorizontal } from 'lucide-react';
import { api } from '../../api';
import type { LightCalReadiness, FlatNormMode, LightCalParams, BiasFallback } from '../../types/models';
import { useLightCalibrationContext } from '../../contexts/LightCalibrationContext';

/** localStorage key for the "Normalize master flat" preference (default ON). */
export const LIGHTCAL_FLATNORM_KEY = 'athenaeum.lightcal.flatNorm';

/** localStorage key for the flat-normalization statistic (default centralThird). */
export const LIGHTCAL_FLATNORM_MODE_KEY = 'athenaeum.lightcal.flatNormMode';

/** localStorage key for the Advanced calibration parameters (JSON). */
export const LIGHTCAL_PARAMS_KEY = 'athenaeum.lightcal.params';

/** Advanced-parameter defaults — these reproduce the engine's current behavior
 *  (see `LightCalParams::default` in `calibration_library/light_cal.rs`). */
export const DEFAULT_LIGHTCAL_PARAMS: LightCalParams = {
  trimFraction: 0.05,
  pedestalDn: 0,
  biasFallback: 'subtractBias',
  cfaFlatScaling: true,
};

/** Read the persisted Advanced parameters, coercing every field into range and
 *  falling back to {@link DEFAULT_LIGHTCAL_PARAMS} when unset/corrupt. Exported so
 *  the badge/details readiness fetches in FrameSetDetail resolve staleness against
 *  the exact params the dialog would submit. */
export function readLightCalParamsPref(): LightCalParams {
  try {
    const raw = localStorage.getItem(LIGHTCAL_PARAMS_KEY);
    if (!raw) return { ...DEFAULT_LIGHTCAL_PARAMS };
    const parsed = JSON.parse(raw) as Partial<LightCalParams>;
    const trimFraction =
      typeof parsed.trimFraction === 'number' && Number.isFinite(parsed.trimFraction)
        ? Math.min(0.25, Math.max(0, parsed.trimFraction))
        : DEFAULT_LIGHTCAL_PARAMS.trimFraction;
    const pedestalDn =
      typeof parsed.pedestalDn === 'number' && Number.isFinite(parsed.pedestalDn)
        ? Math.max(0, parsed.pedestalDn)
        : DEFAULT_LIGHTCAL_PARAMS.pedestalDn;
    const biasFallback: BiasFallback =
      parsed.biasFallback === 'skipFrame' ? 'skipFrame' : 'subtractBias';
    // Default ON, so a preference blob written before this option existed keeps
    // the recommended behavior instead of silently opting out of it.
    const cfaFlatScaling = parsed.cfaFlatScaling !== false;
    return { trimFraction, pedestalDn, biasFallback, cfaFlatScaling };
  } catch {
    return { ...DEFAULT_LIGHTCAL_PARAMS };
  }
}

/** Read the persisted flat-norm preference (default ON when unset/corrupt). */
export function readFlatNormPref(): boolean {
  try {
    return localStorage.getItem(LIGHTCAL_FLATNORM_KEY) !== 'false';
  } catch {
    return true;
  }
}

/** Read the persisted flat-normalization statistic (default centralThird when
 *  unset/corrupt — any value other than the exact 'pixinsightTrimmed' token
 *  falls back to the default). */
export function readFlatNormModePref(): FlatNormMode {
  try {
    return localStorage.getItem(LIGHTCAL_FLATNORM_MODE_KEY) === 'pixinsightTrimmed'
      ? 'pixinsightTrimmed'
      : 'centralThird';
  } catch {
    return 'centralThird';
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
  const [flatNormMode, setFlatNormMode] = useState<FlatNormMode>(readFlatNormModePref);
  const [onlyStale, setOnlyStale] = useState(true);

  // Advanced parameters (spec §2). `params` is the committed numeric source of
  // truth (persisted + submitted); the two `*Input` strings back the number
  // fields so partial edits like "0." don't get clobbered by re-parse.
  const [params, setParams] = useState<LightCalParams>(readLightCalParamsPref);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [trimInput, setTrimInput] = useState(() => String(params.trimFraction));
  const [pedestalInput, setPedestalInput] = useState(() => String(params.pedestalDn));

  const onTrimChange = (v: string) => {
    setTrimInput(v);
    const n = parseFloat(v);
    if (!Number.isNaN(n)) setParams(p => ({ ...p, trimFraction: Math.min(0.25, Math.max(0, n)) }));
  };
  const onPedestalChange = (v: string) => {
    setPedestalInput(v);
    const n = parseFloat(v);
    if (!Number.isNaN(n)) setParams(p => ({ ...p, pedestalDn: Math.max(0, n) }));
  };

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

  // Persist the normalization-statistic choice whenever it changes.
  useEffect(() => {
    try { localStorage.setItem(LIGHTCAL_FLATNORM_MODE_KEY, flatNormMode); } catch { /* ignore */ }
  }, [flatNormMode]);

  // Persist the Advanced parameters whenever they change.
  useEffect(() => {
    try { localStorage.setItem(LIGHTCAL_PARAMS_KEY, JSON.stringify(params)); } catch { /* ignore */ }
  }, [params]);

  // Re-query readiness on open and whenever the flat-norm flag, statistic, or
  // Advanced parameters change — all feed staleness (a frame calibrated with a
  // different flat-norm setting, statistic, or advanced param reads Stale).
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setReadinessError(null);
    api.invoke<LightCalReadiness>('get_light_calibration_readiness', { setId, flatNorm, flatNormMode, params })
      .then(r => { if (!cancelled) { setReadiness(r); setLoading(false); } })
      .catch(e => { if (!cancelled) { setReadinessError(String(e)); setLoading(false); } });
    return () => { cancelled = true; };
  }, [setId, flatNorm, flatNormMode, params]);

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
      await startCalibration(setId, { onlyStale }, flatNorm, flatNormMode, params);
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
        <div className="mb-4">
          <label className="flex items-center gap-2 text-sm text-content-secondary">
            <input type="checkbox" checked={flatNorm} onChange={e => setFlatNorm(e.target.checked)} />
            Normalize master flat (recommended)
          </label>

          {/* Normalization statistic — only meaningful while normalization is ON. */}
          {flatNorm && (
            <div className="mt-2 ml-6 space-y-1">
              <div className="text-xs text-content-muted mb-1">Normalization statistic</div>
              <label className="flex items-center gap-2 text-xs text-content-secondary">
                <input
                  type="radio"
                  name="lightcal-flatnorm-mode"
                  checked={flatNormMode === 'centralThird'}
                  onChange={() => setFlatNormMode('centralThird')}
                />
                Central third mean (Athenaeum)
              </label>
              <label className="flex items-center gap-2 text-xs text-content-secondary">
                <input
                  type="radio"
                  name="lightcal-flatnorm-mode"
                  checked={flatNormMode === 'pixinsightTrimmed'}
                  onChange={() => setFlatNormMode('pixinsightTrimmed')}
                />
                Full-frame trimmed mean (5% per tail)
              </label>

              {/* Per-channel CFA scaling — a refinement OF the normalization,
                  so it lives under the statistic it refines. Unavailable in the
                  PixInsight-trimmed mode, which is a whole-frame statistic. */}
              <label
                className={`flex items-center gap-2 text-xs pt-1 ${
                  flatNormMode === 'pixinsightTrimmed'
                    ? 'text-content-muted cursor-not-allowed'
                    : 'text-content-secondary'
                }`}
              >
                <input
                  type="checkbox"
                  checked={params.cfaFlatScaling && flatNormMode !== 'pixinsightTrimmed'}
                  disabled={flatNormMode === 'pixinsightTrimmed'}
                  onChange={e => setParams(p => ({ ...p, cfaFlatScaling: e.target.checked }))}
                />
                Per-channel CFA flat scaling
              </label>
              <p className="text-[11px] text-content-muted ml-6">
                {flatNormMode === 'pixinsightTrimmed'
                  ? 'Not available with the full-frame trimmed mean — that statistic is measured across all channels at once.'
                  : 'Applies to color (CFA) lights; mono lights are unaffected.'}
              </p>
            </div>
          )}
        </div>

        {/* Advanced parameters (spec §2) — collapsed by default. */}
        <div className="mb-4 border-t border-border/60 pt-3">
          <button
            type="button"
            onClick={() => setAdvancedOpen(o => !o)}
            className="flex items-center gap-1.5 text-xs font-medium text-content-secondary hover:text-content transition-colors"
          >
            {advancedOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <SlidersHorizontal size={12} className="text-content-muted" />
            Advanced
          </button>

          {advancedOpen && (
            <div className="mt-3 ml-1 space-y-3">
              {/* Trim fraction — only meaningful in the PixInsight-trimmed statistic. */}
              {flatNorm && flatNormMode === 'pixinsightTrimmed' && (
                <div>
                  <label className="block text-xs text-content-secondary mb-1">
                    Flat trim fraction (per tail)
                  </label>
                  <input
                    type="number"
                    min={0}
                    max={0.25}
                    step={0.01}
                    value={trimInput}
                    onChange={e => onTrimChange(e.target.value)}
                    className="w-28 px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent"
                  />
                  <p className="text-[11px] text-content-muted mt-1">
                    Fraction discarded from each tail of the trimmed mean (default 0.05).
                  </p>
                </div>
              )}

              {/* Output pedestal. */}
              <div>
                <label className="block text-xs text-content-secondary mb-1">
                  Output pedestal (DN)
                </label>
                <input
                  type="number"
                  min={0}
                  step={1}
                  value={pedestalInput}
                  onChange={e => onPedestalChange(e.target.value)}
                  className="w-28 px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent"
                />
                <p className="text-[11px] text-content-muted mt-1">
                  Added after the scale divide, for consumers that clip negatives. 0 = negatives preserved.
                </p>
              </div>

              {/* Bias fallback policy for dark-less lights. */}
              <div>
                <div className="text-xs text-content-secondary mb-1">Lights with no dark master</div>
                <label className="flex items-center gap-2 text-xs text-content-secondary mb-1">
                  <input
                    type="radio"
                    name="lightcal-bias-fallback"
                    checked={params.biasFallback === 'subtractBias'}
                    onChange={() => setParams(p => ({ ...p, biasFallback: 'subtractBias' }))}
                  />
                  Subtract bias (calibrate best-effort)
                </label>
                <label className="flex items-center gap-2 text-xs text-content-secondary">
                  <input
                    type="radio"
                    name="lightcal-bias-fallback"
                    checked={params.biasFallback === 'skipFrame'}
                    onChange={() => setParams(p => ({ ...p, biasFallback: 'skipFrame' }))}
                  />
                  Skip the frame (never bias-only)
                </label>
                <p className="text-[11px] text-content-muted mt-1">
                  When a light has no matched dark master, either subtract its linked bias or fail that frame.
                </p>
              </div>
            </div>
          )}
        </div>

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
