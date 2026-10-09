// Stage 6 (Normalize) inspector panel: global output/rejection
// normalization + scale estimator, and the local-normalization (LN) block
// (M2 Task 8 — live). The rejection-normalization select's `local` option is
// enabled only while LN itself (`normalization.local.enabled`) is on — a
// local-rejection choice with no LN reference to rejection-normalize
// against isn't a coherent config. `localScale` (the local SCALE model, as
// opposed to local BACKGROUND normalization) went live in M4c (ruling
// R-M4c-8) and sits with the other LN parameters, inside the same
// `enabled` block: a scale surface with no LN pass to carry it would mean
// nothing.

import { useEffect, useRef, useState } from 'react';
import { NumericField } from '../NumericField';
import { LN_SCALES, apertureLabel, effectiveScaleMode, fluxEstimatorLabel, resolvedScaleLines } from './lnScale';
import { Checkbox } from '../../settings/Checkbox';
import { ParamPair } from '../ParamPair';
import { normalizePsfModelLabel, outputNormLabel, rejectionNormLabel } from '../stageSummary';
import type { LnAperture, LnFluxEstimator, OutputNormalization, PsfModel, RejectionNormalization, ScaleEstimator, StackingConfig, StackingPlan } from '../../../types/stacking';

const OUTPUT_NORMS: OutputNormalization[] = [
  'none',
  'additive',
  'additiveWithScaling',
  'multiplicative',
  'multiplicativeWithScaling',
];
const REJECTION_NORMS: RejectionNormalization[] = ['none', 'scaleZeroOffset', 'equalizeFluxes', 'local'];
const SCALE_ESTIMATORS: ScaleEstimator[] = ['bwmv', 'mad', 'avgDev'];
const PSF_MODELS: PsfModel[] = ['auto', 'moffat4'];
const FLUX_ESTIMATORS: LnFluxEstimator[] = ['psfSignal', 'commonAperture'];
const APERTURES: LnAperture[] = ['fwhm2', 'fwhm3', 'growthCurve'];
const SELECT_CLASS =
  'w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50';

/** Msec the "rejection reset" notice stays up after the user turns LN off
 *  while rejection normalization was set to `'local'` — long enough to read,
 *  short enough not to linger once the inspector moves on. */
const RESET_NOTICE_MS = 5000;

export interface NormalizePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
  plan?: StackingPlan | null;
}

export function NormalizePanel({ config, onChange, disabled, defaults, plan }: NormalizePanelProps) {
  const n = config.normalization;
  const scaleMode = effectiveScaleMode(n.local);

  const [resetNotice, setResetNotice] = useState(false);
  const noticeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (noticeTimerRef.current) clearTimeout(noticeTimerRef.current);
  }, []);

  const patch = (p: Partial<StackingConfig['normalization']>) => {
    onChange({ ...config, normalization: { ...n, ...p } });
  };
  const resolvedLines = resolvedScaleLines(plan);
  const patchLocal = (p: Partial<StackingConfig['normalization']['local']>) => {
    patch({ local: { ...n.local, ...p } });
  };

  /** Turning LN off while rejection normalization is `'local'` would leave
   *  the config pointing at a rejection method that just became unselectable
   *  — reset it to the M1 default and say so inline (Task 8 brief) rather
   *  than silently stranding the stored value. */
  const handleLocalEnabledChange = (checked: boolean) => {
    if (!checked && n.rejection === 'local') {
      onChange({
        ...config,
        normalization: { ...n, local: { ...n.local, enabled: false }, rejection: 'scaleZeroOffset' },
      });
      if (noticeTimerRef.current) clearTimeout(noticeTimerRef.current);
      setResetNotice(true);
      noticeTimerRef.current = setTimeout(() => setResetNotice(false), RESET_NOTICE_MS);
      return;
    }
    patchLocal({ enabled: checked });
  };

  return (
    <div className="space-y-3">
      <div>
        <label className="block text-xs text-content-secondary mb-1">Output normalization</label>
        <select
          value={n.output}
          disabled={disabled}
          onChange={(e) => patch({ output: e.target.value as OutputNormalization })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {OUTPUT_NORMS.map((v) => (
            <option key={v} value={v}>{outputNormLabel(v)}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">default {outputNormLabel(defaults.normalization.output)}</p>
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Rejection normalization</label>
        <select
          value={n.rejection}
          disabled={disabled}
          onChange={(e) => patch({ rejection: e.target.value as RejectionNormalization })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {REJECTION_NORMS.map((v) => (
            <option key={v} value={v} disabled={v === 'local' && !n.local.enabled}>
              {rejectionNormLabel(v)}
            </option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {rejectionNormLabel(defaults.normalization.rejection)}
          {!n.local.enabled && ' — "local" needs local normalization enabled below'}
        </p>
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Scale estimator</label>
        <select
          value={n.scaleEstimator}
          disabled={disabled}
          onChange={(e) => patch({ scaleEstimator: e.target.value as ScaleEstimator })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {SCALE_ESTIMATORS.map((v) => (
            <option key={v} value={v}>{v.toUpperCase()}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {defaults.normalization.scaleEstimator.toUpperCase()} — kept equal to the Measure stage's own
          estimator.
        </p>
      </div>

      <div className="pt-3 border-t border-border/60 space-y-2">
        <h4 className="text-xs font-medium text-content-secondary">Local normalization</h4>

        <Checkbox
          checked={n.local.enabled}
          onChange={handleLocalEnabledChange}
          disabled={disabled}
          label="Enable local normalization"
        />

        {resetNotice && (
          <p className="text-xs text-warning">
            Rejection normalization reset to {rejectionNormLabel('scaleZeroOffset')} — "local" needs local
            normalization enabled.
          </p>
        )}

        {n.local.enabled && (
          <div className="space-y-3 pt-1">
            <div>
              <label className="block text-xs text-content-secondary mb-1">Scale</label>
              <select
                value={scaleMode === 'auto' ? 'auto' : String(n.local.scale)}
                disabled={disabled}
                onChange={(e) => {
                  if (e.target.value === 'auto') patchLocal({ scaleMode: 'auto' });
                  else patchLocal({ scaleMode: 'fixed', scale: Number(e.target.value) });
                }}
                className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
              >
                <option value="auto">Auto</option>
                {LN_SCALES.map((v) => (
                  <option key={v} value={String(v)}>{v} px</option>
                ))}
                {scaleMode === 'fixed' && !(LN_SCALES as readonly number[]).includes(n.local.scale) && (
                  <option value={String(n.local.scale)}>{n.local.scale} px</option>
                )}
              </select>
              <p className="mt-1 text-[11px] text-content-muted">
                default {effectiveScaleMode(defaults.normalization.local) === 'auto' ? 'Auto' : `${defaults.normalization.local.scale} px`} — The
                background model follows structures larger than this and treats smaller ones as signal.
              </p>
            </div>

            {scaleMode === 'auto' && (
              <>
                <NumericField
                  label="Grid size"
                  value={n.local.gridSize}
                  onCommit={(v) => patchLocal({ gridSize: Math.round(v) })}
                  min={2}
                  max={8}
                  step={1}
                  disabled={disabled}
                  help={`Auto scale is the frames' short side divided by this, snapped to a supported size. default ${defaults.normalization.local.gridSize}`}
                />
                {resolvedLines.length > 0 && (
                  <div>
                    <label className="block text-xs text-content-secondary mb-1">Resolved scale</label>
                    {resolvedLines.map((line) => (
                      <p key={line} className="text-xs text-content-muted">{line}</p>
                    ))}
                  </div>
                )}
              </>
            )}

            <NumericField
              label="Reference frames"
              value={n.local.referenceFrames}
              onCommit={(v) => patchLocal({ referenceFrames: Math.round(v) })}
              min={3}
              max={50}
              step={1}
              disabled={disabled}
              help={`default ${defaults.normalization.local.referenceFrames}`}
            />

            <div>
              <label className="block text-xs text-content-secondary mb-1">Star flux</label>
              <select
                value={n.local.fluxEstimator}
                disabled={disabled}
                onChange={(e) => patchLocal({ fluxEstimator: e.target.value as LnFluxEstimator })}
                className={SELECT_CLASS}
              >
                {FLUX_ESTIMATORS.map((v) => (
                  <option key={v} value={v}>{fluxEstimatorLabel(v)}</option>
                ))}
              </select>
              <p className="mt-1 text-[11px] text-content-muted">
                default {fluxEstimatorLabel(defaults.normalization.local.fluxEstimator)} — PSF signal measures each
                star inside its fitted profile; common aperture measures both frames in one circle sized to the
                broader star, independent of seeing.
              </p>
            </div>

            {n.local.fluxEstimator === 'commonAperture' && (
              <div>
                <label className="block text-xs text-content-secondary mb-1">Aperture</label>
                <select
                  value={n.local.aperture}
                  disabled={disabled}
                  onChange={(e) => patchLocal({ aperture: e.target.value as LnAperture })}
                  className={SELECT_CLASS}
                >
                  {APERTURES.map((v) => (
                    <option key={v} value={v}>{apertureLabel(v)}</option>
                  ))}
                </select>
                <p className="mt-1 text-[11px] text-content-muted">
                  default {apertureLabel(defaults.normalization.local.aperture)} — The circle radius in units of the
                  broader frame's FWHM; the growth curve adds the light beyond it from the brightest stars.
                </p>
              </div>
            )}

            <ParamPair
              leftLabel="Minimum peak SNR"
              leftValue={n.local.starSelection.minPeakSnr}
              onLeftCommit={(v) => patchLocal({ starSelection: { ...n.local.starSelection, minPeakSnr: v } })}
              leftMin={5}
              leftMax={200}
              leftStep={1}
              rightLabel="Brightest stars"
              rightValue={n.local.starSelection.maxStars}
              onRightCommit={(v) =>
                patchLocal({ starSelection: { ...n.local.starSelection, maxStars: Math.round(v) } })}
              rightMin={20}
              rightMax={5000}
              rightStep={10}
              disabled={disabled}
              help={`Only stars this far above the local noise, and at most this many of the brightest, set the scale. default ${defaults.normalization.local.starSelection.minPeakSnr} / ${defaults.normalization.local.starSelection.maxStars}`}
            />

            <NumericField
              label="Saturation limit (× plane maximum)"
              value={n.local.starSelection.saturation}
              onCommit={(v) => patchLocal({ starSelection: { ...n.local.starSelection, saturation: v } })}
              min={0.5}
              max={1}
              step={0.05}
              disabled={disabled}
              help={`Stars with a pixel above this fraction of the brightest pixel in the frame are left out. default ${defaults.normalization.local.starSelection.saturation}`}
            />

            <div>
              <label className="block text-xs text-content-secondary mb-1">PSF model</label>
              <select
                value={n.local.psfModel}
                disabled={disabled}
                onChange={(e) => patchLocal({ psfModel: e.target.value as PsfModel })}
                className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
              >
                {PSF_MODELS.map((v) => (
                  <option key={v} value={v}>{normalizePsfModelLabel(v)}</option>
                ))}
              </select>
              <p className="mt-1 text-[11px] text-content-muted">
                default {normalizePsfModelLabel(defaults.normalization.local.psfModel)}
              </p>
            </div>

            <div>
              <Checkbox
                checked={n.local.localScale}
                onChange={(checked) => patchLocal({ localScale: checked })}
                disabled={disabled}
                label="Local scale"
              />
              <p className="mt-1 text-[11px] text-content-muted">
                Model the flux scale as a smooth surface (matched-star flux ratios, thin-plate spline)
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
