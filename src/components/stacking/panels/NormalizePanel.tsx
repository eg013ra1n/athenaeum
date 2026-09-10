// Stage 6 (Normalize) inspector panel: global output/rejection
// normalization + scale estimator, and the local-normalization (LN) block
// (M2 Task 8 — live). The rejection-normalization select's `local` option is
// enabled only while LN itself (`normalization.local.enabled`) is on — a
// local-rejection choice with no LN reference to rejection-normalize
// against isn't a coherent config. `localScale` (the local SCALE model, as
// opposed to local BACKGROUND normalization) stays disabled — it arrives in
// M4.

import { useEffect, useRef, useState } from 'react';
import { ParamPair } from '../ParamPair';
import { outputNormLabel, psfModelLabel, rejectionNormLabel } from '../stageSummary';
import type { OutputNormalization, PsfModel, RejectionNormalization, ScaleEstimator, StackingConfig } from '../../../types/stacking';

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

/** Msec the "rejection reset" notice stays up after the user turns LN off
 *  while rejection normalization was set to `'local'` — long enough to read,
 *  short enough not to linger once the inspector moves on. */
const RESET_NOTICE_MS = 5000;

export interface NormalizePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
}

export function NormalizePanel({ config, onChange, disabled, defaults }: NormalizePanelProps) {
  const n = config.normalization;

  const [resetNotice, setResetNotice] = useState(false);
  const noticeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (noticeTimerRef.current) clearTimeout(noticeTimerRef.current);
  }, []);

  const patch = (p: Partial<StackingConfig['normalization']>) => {
    onChange({ ...config, normalization: { ...n, ...p } });
  };
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

        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={n.local.enabled}
            disabled={disabled}
            onChange={(e) => handleLocalEnabledChange(e.target.checked)}
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Enable local normalization</span>
        </label>

        {resetNotice && (
          <p className="text-xs text-warning">
            Rejection normalization reset to {rejectionNormLabel('scaleZeroOffset')} — "local" needs local
            normalization enabled.
          </p>
        )}

        {n.local.enabled && (
          <div className="space-y-3 pt-1">
            <ParamPair
              leftLabel="Scale (px)"
              leftValue={n.local.scale}
              onLeftCommit={(v) => patchLocal({ scale: Math.round(v) })}
              leftMin={256}
              leftMax={4096}
              leftStep={256}
              rightLabel="Reference frames"
              rightValue={n.local.referenceFrames}
              onRightCommit={(v) => patchLocal({ referenceFrames: Math.round(v) })}
              rightMin={3}
              rightMax={50}
              rightStep={1}
              disabled={disabled}
              help={`defaults ${defaults.normalization.local.scale} / ${defaults.normalization.local.referenceFrames}`}
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
                  <option key={v} value={v}>{psfModelLabel(v)}</option>
                ))}
              </select>
              <p className="mt-1 text-[11px] text-content-muted">
                default {psfModelLabel(defaults.normalization.local.psfModel)}
              </p>
            </div>
          </div>
        )}

        <label
          className="flex items-center gap-2 cursor-not-allowed pt-1"
          title="Local scale model arrives in M4"
        >
          <input type="checkbox" checked={n.local.localScale} disabled className="w-4 h-4 rounded border-border" />
          <span className="text-sm text-content-muted">
            Local scale <span className="italic">(arrives in M4)</span>
          </span>
        </label>
      </div>
    </div>
  );
}
