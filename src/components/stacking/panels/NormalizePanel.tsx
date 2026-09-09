// Stage 6 (Normalize) inspector panel: global output/rejection
// normalization + scale estimator (live), and the local-normalization (LN)
// block — every field rendered but disabled, arriving in M2. The rejection
// normalization select carries a `local` option that is itself disabled
// (M2), so the field a live-M1 config could set is bounded to the
// M1-supported choices.

import { outputNormLabel, psfModelLabel, rejectionNormLabel } from '../stageSummary';
import type { OutputNormalization, RejectionNormalization, ScaleEstimator, StackingConfig } from '../../../types/stacking';

const OUTPUT_NORMS: OutputNormalization[] = [
  'none',
  'additive',
  'additiveWithScaling',
  'multiplicative',
  'multiplicativeWithScaling',
];
const REJECTION_NORMS: Exclude<RejectionNormalization, 'local'>[] = ['none', 'scaleZeroOffset', 'equalizeFluxes'];
const SCALE_ESTIMATORS: ScaleEstimator[] = ['bwmv', 'mad', 'avgDev'];

export interface NormalizePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
}

export function NormalizePanel({ config, onChange, disabled, defaults }: NormalizePanelProps) {
  const n = config.normalization;

  const patch = (p: Partial<StackingConfig['normalization']>) => {
    onChange({ ...config, normalization: { ...n, ...p } });
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
            <option key={v} value={v}>{rejectionNormLabel(v)}</option>
          ))}
          <option value="local" disabled>
            Local (arrives in M2)
          </option>
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {rejectionNormLabel(defaults.normalization.rejection)}
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

      <div className="pt-3 border-t border-border/60 space-y-2 opacity-60">
        <h4 className="text-xs font-medium text-content-secondary">
          Local normalization <span className="italic text-content-muted">(arrives in M2)</span>
        </h4>
        <label className="flex items-center gap-2 cursor-not-allowed">
          <input type="checkbox" checked={n.local.enabled} disabled className="w-4 h-4 rounded border-border" />
          <span className="text-sm text-content-muted">Enable local normalization</span>
        </label>
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="block text-xs text-content-muted mb-1">Tile size (px)</label>
            <input
              type="number"
              value={n.local.scale}
              disabled
              readOnly
              className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
            />
          </div>
          <div>
            <label className="block text-xs text-content-muted mb-1">Reference frames</label>
            <input
              type="number"
              value={n.local.referenceFrames}
              disabled
              readOnly
              className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
            />
          </div>
        </div>
        <div>
          <label className="block text-xs text-content-muted mb-1">PSF model</label>
          <select
            value={n.local.psfModel}
            disabled
            className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
          >
            <option value={n.local.psfModel}>{psfModelLabel(n.local.psfModel)}</option>
          </select>
        </div>
        <label className="flex items-center gap-2 cursor-not-allowed">
          <input type="checkbox" checked={n.local.localScale} disabled className="w-4 h-4 rounded border-border" />
          <span className="text-sm text-content-muted">Local scale</span>
        </label>
      </div>
    </div>
  );
}
