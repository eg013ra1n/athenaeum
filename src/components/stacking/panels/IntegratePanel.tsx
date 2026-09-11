// Stage 7 (Integrate) inspector panel: combination, rejection method + its
// per-variant parameters, the Auto rule note, min weight, range low/high,
// write-rejection-maps.

import { NullableNumericField, NumericField, nullableDefaultHelp } from '../NumericField';
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
  'minMax',
  'esd',
  'rcr',
];

const METHOD_LABEL: Record<RejectionMethod, string> = {
  auto: 'Auto',
  none: 'None',
  percentileClip: 'Percentile clip',
  sigmaClip: 'Sigma clip',
  winsorizedSigma: 'Winsorized sigma',
  linearFitClip: 'Linear-fit clip',
  minMax: 'Min/max',
  esd: 'Generalized ESD',
  rcr: 'Robust Chauvenet (RCR)',
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
    case 'minMax':
      return { method: 'minMax', low: 1, high: 1 };
    case 'esd':
      return { method: 'esd', outliersFraction: 0.3, alpha: 0.05, lowRelaxation: 1.5 };
    case 'rcr':
      return { method: 'rcr', limit: 0.5 };
  }
}

/** `minMax`'s counts are `usize` on the wire — a fractional value would
 *  fail to deserialize, and `NumericField` commits whatever parses. Round
 *  and floor at zero on the way out. */
function wholeCount(n: number): number {
  return Math.max(0, Math.round(n));
}

/** `largeScale.protectedLayers`/`growth` are `u8` on the wire, and the
 *  backend clamps both to these ranges anyway (`resolve_config`) — round
 *  and clamp here so the field never sends a value the run would silently
 *  move. */
function layerCount(n: number): number {
  return Math.min(6, Math.max(1, Math.round(n)));
}

function growthRadius(n: number): number {
  return Math.min(4, Math.max(0, Math.round(n)));
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
        <p className="mt-1 text-[11px] text-content-muted">
          default {combinationLabel(defaults.integration.combination)}
        </p>
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
          default {METHOD_LABEL[defaults.integration.rejection.method]} — resolves per group: n &lt; 8 percentile
          0.2/0.1 · 8–19 Winsorized 4.0/3.0 · ≥ 20 linear fit 5.0/3.5. Min/max, ESD and RCR are
          picked by hand only — Auto never resolves to them.
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
      {i.rejection.method === 'minMax' && (
        <ParamPair
          leftLabel="Drop lowest"
          leftValue={i.rejection.low}
          onLeftCommit={(n) => patch({ rejection: { method: 'minMax', low: wholeCount(n), high: i.rejection.method === 'minMax' ? i.rejection.high : 1 } })}
          rightLabel="Drop highest"
          rightValue={i.rejection.high}
          onRightCommit={(n) => patch({ rejection: { method: 'minMax', low: i.rejection.method === 'minMax' ? i.rejection.low : 1, high: wholeCount(n) } })}
          min={0}
          step={1}
          help="frames per pixel stack; at least one sample always survives"
          disabled={disabled}
        />
      )}
      {i.rejection.method === 'esd' && (
        <>
          <ParamPair
            leftLabel="Outliers fraction"
            leftValue={i.rejection.outliersFraction}
            onLeftCommit={(n) => patch({ rejection: { method: 'esd', outliersFraction: n, alpha: i.rejection.method === 'esd' ? i.rejection.alpha : 0.05, lowRelaxation: i.rejection.method === 'esd' ? i.rejection.lowRelaxation : 1.5 } })}
            rightLabel="Significance α"
            rightValue={i.rejection.alpha}
            onRightCommit={(n) => patch({ rejection: { method: 'esd', outliersFraction: i.rejection.method === 'esd' ? i.rejection.outliersFraction : 0.3, alpha: n, lowRelaxation: i.rejection.method === 'esd' ? i.rejection.lowRelaxation : 1.5 } })}
            leftMin={0.05}
            leftMax={0.5}
            leftStep={0.05}
            rightMin={0.001}
            rightMax={0.2}
            rightStep={0.001}
            help="the fraction of the stack that may be tested, and the test's significance"
            disabled={disabled}
          />
          <NumericField
            label="Low relaxation"
            value={i.rejection.lowRelaxation}
            onCommit={(n) => patch({ rejection: { method: 'esd', outliersFraction: i.rejection.method === 'esd' ? i.rejection.outliersFraction : 0.3, alpha: i.rejection.method === 'esd' ? i.rejection.alpha : 0.05, lowRelaxation: n } })}
            min={1}
            max={3}
            step={0.1}
            disabled={disabled}
            help="above 1 the faint side is rejected less eagerly than the bright side"
          />
        </>
      )}
      {i.rejection.method === 'rcr' && (
        <NumericField
          label="Limit"
          value={i.rejection.limit}
          onCommit={(n) => patch({ rejection: { method: 'rcr', limit: n } })}
          min={0.1}
          max={1}
          step={0.05}
          disabled={disabled}
          help="expected count at least as extreme; 0.5 is Chauvenet's criterion"
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

      {/* `rangeLow` is `number | null` in the contract, same as `rangeHigh` —
          the Default preset happens to leave it `Some(0.0)` rather than
          `None`, but the field must still be able to represent `null` (fix
          round 1, Minor #6), not silently coerce it to 0 on every commit. */}
      <NullableNumericField
        label="Range low"
        value={i.rangeLow}
        presetDefaultValue={defaults.integration.rangeLow}
        onCommit={(n) => patch({ rangeLow: n })}
        step={0.01}
        disabled={disabled}
        help={nullableDefaultHelp(defaults.integration.rangeLow, 'reject raw ≤ this value')}
      />

      <NullableNumericField
        label="Range high"
        value={i.rangeHigh}
        presetDefaultValue={defaults.integration.rangeHigh}
        onCommit={(n) => patch({ rangeHigh: n })}
        step={0.01}
        disabled={disabled}
        help={nullableDefaultHelp(defaults.integration.rangeHigh, 'reject raw ≥ this value once turned on')}
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

      {/* Large-scale rejection (M4c): a second integration pass that treats
          the filtered, grown per-pixel rejection map as forced rejections,
          so a satellite trail is removed as one object instead of as the
          speckle the per-pixel tests leave of it. */}
      <div className="pt-2 border-t border-border">
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={i.largeScale.enabled}
            disabled={disabled}
            onChange={(e) =>
              patch({ largeScale: { ...i.largeScale, enabled: e.target.checked } })
            }
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Large-scale rejection</span>
        </label>
        <p className="mt-1 text-[11px] text-content-muted">
          Keeps only the large rejected structures — trails, aircraft — grows them, and
          re-integrates with those samples forced out. Doubles the integration time.
        </p>
        {i.largeScale.enabled && (
          <div className="mt-2 space-y-2">
            <NumericField
              label="Protected layers"
              value={i.largeScale.protectedLayers}
              onCommit={(n) =>
                patch({
                  largeScale: { ...i.largeScale, protectedLayers: layerCount(n) },
                })
              }
              min={1}
              max={6}
              step={1}
              disabled={disabled}
              help="scale selector: a structure survives from about 2^layers / 2 px thick — 3 px at 2, 5 px at 3, 9 px at 4"
            />
            <NumericField
              label="Growth"
              value={i.largeScale.growth}
              onCommit={(n) =>
                patch({ largeScale: { ...i.largeScale, growth: growthRadius(n) } })
              }
              min={0}
              max={4}
              step={1}
              disabled={disabled}
              help="radius in px every surviving structure is grown by, to cover its own faint edges"
            />
          </div>
        )}
      </div>
    </div>
  );
}
