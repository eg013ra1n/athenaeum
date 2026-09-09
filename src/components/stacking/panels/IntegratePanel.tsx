// Stage 7 (Integrate) inspector panel: combination, rejection method + its
// per-variant parameters, the Auto rule note, min weight, range low/high,
// write-rejection-maps.

import { NullableNumericField, NumericField } from '../NumericField';
import { ParamPair } from '../ParamPair';
import { combinationLabel } from '../stageSummary';
import type { Combination, RejectionChoice, StackingConfig } from '../../../types/stacking';

const COMBINATIONS: Combination[] = ['average', 'median'];

type RejectionMethod = RejectionChoice['method'];

const METHODS: RejectionMethod[] = [
  'auto',
  'none',
  'percentileClip',
  'sigmaClip',
  'winsorizedSigma',
  'linearFitClip',
];

const METHOD_LABEL: Record<RejectionMethod, string> = {
  auto: 'Auto',
  none: 'None',
  percentileClip: 'Percentile clip',
  sigmaClip: 'Sigma clip',
  winsorizedSigma: 'Winsorized sigma',
  linearFitClip: 'Linear-fit clip',
};

/** A reasonable starting point when the user switches TO a parametric
 *  method — mirrors the numbers the Auto rule itself resolves to for that
 *  method's own size bucket (spec's per-group table), not invented values. */
function seedForMethod(method: RejectionMethod): RejectionChoice {
  switch (method) {
    case 'auto':
      return { method: 'auto' };
    case 'none':
      return { method: 'none' };
    case 'percentileClip':
      return { method: 'percentileClip', low: 0.2, high: 0.1 };
    case 'sigmaClip':
      return { method: 'sigmaClip', sigmaLow: 4.0, sigmaHigh: 3.0 };
    case 'winsorizedSigma':
      return { method: 'winsorizedSigma', sigmaLow: 4.0, sigmaHigh: 3.0 };
    case 'linearFitClip':
      return { method: 'linearFitClip', sigmaLow: 5.0, sigmaHigh: 3.5 };
  }
}

export interface IntegratePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
}

export function IntegratePanel({ config, onChange, disabled, defaults }: IntegratePanelProps) {
  const i = config.integration;

  const patch = (p: Partial<StackingConfig['integration']>) => {
    onChange({ ...config, integration: { ...i, ...p } });
  };

  return (
    <div className="space-y-3">
      <div>
        <label className="block text-xs text-content-secondary mb-1">Combination</label>
        <select
          value={i.combination}
          disabled={disabled}
          onChange={(e) => patch({ combination: e.target.value as Combination })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {COMBINATIONS.map((v) => (
            <option key={v} value={v}>{combinationLabel(v)}</option>
          ))}
        </select>
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Rejection method</label>
        <select
          value={i.rejection.method}
          disabled={disabled}
          onChange={(e) => patch({ rejection: seedForMethod(e.target.value as RejectionMethod) })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {METHODS.map((v) => (
            <option key={v} value={v}>{METHOD_LABEL[v]}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          Auto resolves per group: n &lt; 8 percentile 0.2/0.1 · 8–19 Winsorized 4.0/3.0 · ≥ 20 linear fit 5.0/3.5.
        </p>
      </div>

      {i.rejection.method === 'percentileClip' && (
        <ParamPair
          leftLabel="Low percentile"
          leftValue={i.rejection.low}
          onLeftCommit={(n) => patch({ rejection: { method: 'percentileClip', low: n, high: i.rejection.method === 'percentileClip' ? i.rejection.high : 0.1 } })}
          rightLabel="High percentile"
          rightValue={i.rejection.high}
          onRightCommit={(n) => patch({ rejection: { method: 'percentileClip', low: i.rejection.method === 'percentileClip' ? i.rejection.low : 0.2, high: n } })}
          min={0}
          max={1}
          step={0.01}
          disabled={disabled}
        />
      )}
      {i.rejection.method === 'sigmaClip' && (
        <ParamPair
          leftLabel="Sigma low"
          leftValue={i.rejection.sigmaLow}
          onLeftCommit={(n) => patch({ rejection: { method: 'sigmaClip', sigmaLow: n, sigmaHigh: i.rejection.method === 'sigmaClip' ? i.rejection.sigmaHigh : 3.0 } })}
          rightLabel="Sigma high"
          rightValue={i.rejection.sigmaHigh}
          onRightCommit={(n) => patch({ rejection: { method: 'sigmaClip', sigmaLow: i.rejection.method === 'sigmaClip' ? i.rejection.sigmaLow : 4.0, sigmaHigh: n } })}
          min={0}
          step={0.1}
          disabled={disabled}
        />
      )}
      {i.rejection.method === 'winsorizedSigma' && (
        <ParamPair
          leftLabel="Sigma low"
          leftValue={i.rejection.sigmaLow}
          onLeftCommit={(n) => patch({ rejection: { method: 'winsorizedSigma', sigmaLow: n, sigmaHigh: i.rejection.method === 'winsorizedSigma' ? i.rejection.sigmaHigh : 3.0 } })}
          rightLabel="Sigma high"
          rightValue={i.rejection.sigmaHigh}
          onRightCommit={(n) => patch({ rejection: { method: 'winsorizedSigma', sigmaLow: i.rejection.method === 'winsorizedSigma' ? i.rejection.sigmaLow : 4.0, sigmaHigh: n } })}
          min={0}
          step={0.1}
          disabled={disabled}
        />
      )}
      {i.rejection.method === 'linearFitClip' && (
        <ParamPair
          leftLabel="Sigma low"
          leftValue={i.rejection.sigmaLow}
          onLeftCommit={(n) => patch({ rejection: { method: 'linearFitClip', sigmaLow: n, sigmaHigh: i.rejection.method === 'linearFitClip' ? i.rejection.sigmaHigh : 3.5 } })}
          rightLabel="Sigma high"
          rightValue={i.rejection.sigmaHigh}
          onRightCommit={(n) => patch({ rejection: { method: 'linearFitClip', sigmaLow: i.rejection.method === 'linearFitClip' ? i.rejection.sigmaLow : 5.0, sigmaHigh: n } })}
          min={0}
          step={0.1}
          disabled={disabled}
        />
      )}

      <NumericField
        label="Min weight"
        value={i.minWeight}
        onCommit={(n) => patch({ minWeight: n })}
        min={0}
        max={1}
        step={0.001}
        disabled={disabled}
        help={`default ${defaults.integration.minWeight}`}
      />

      <NumericField
        label="Range low"
        value={i.rangeLow ?? 0}
        onCommit={(n) => patch({ rangeLow: n })}
        step={0.01}
        disabled={disabled}
        help={`default ${defaults.integration.rangeLow ?? 0} — reject raw ≤ this value`}
      />

      <NullableNumericField
        label="Range high"
        value={i.rangeHigh}
        seedValue={0.98}
        onCommit={(n) => patch({ rangeHigh: n })}
        step={0.01}
        disabled={disabled}
        help="off by default — reject raw ≥ this value once turned on"
      />

      <label className="flex items-center gap-2 cursor-pointer">
        <input
          type="checkbox"
          checked={i.writeRejectionMaps}
          disabled={disabled}
          onChange={(e) => patch({ writeRejectionMaps: e.target.checked })}
          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
        />
        <span className="text-sm text-content-secondary">Write rejection maps</span>
      </label>
    </div>
  );
}
