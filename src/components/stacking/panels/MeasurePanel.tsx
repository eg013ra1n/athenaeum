// Stage 3 (Measure & select) inspector panel: weight mode, the formula
// sliders (only meaningful in `formula` mode), PSF model, max stars, the
// four selection filters and the Re-measure action.

import { RefreshCw } from 'lucide-react';
import { NullableNumericField, NumericField, nullableDefaultHelp } from '../NumericField';
import { psfModelLabel, weightModeLabel } from '../stageSummary';
import type { PsfModel, StackingConfig, WeightMode } from '../../../types/stacking';

const WEIGHT_MODES: WeightMode[] = [
  'psfSignalWeight',
  'psfSnr',
  'noise',
  'formula',
  'exposure',
  'keyword',
  'none',
];

const PSF_MODELS: PsfModel[] = ['auto', 'moffat4'];

/** A formula-weight slider — a plain `<input type="range">`, always a valid
 *  number on every change, so it needs none of `NumericField`'s partial-edit
 *  discipline. `defaultValue` (fix round 1, Minor #5) is the field's actual
 *  `presetDefault` number, never a hard-coded literal in this file. */
function WeightSlider({
  label,
  value,
  defaultValue,
  onCommit,
  disabled,
}: {
  label: string;
  value: number;
  defaultValue: number;
  onCommit: (n: number) => void;
  disabled?: boolean;
}) {
  return (
    <div>
      <div className="flex items-center justify-between text-xs text-content-secondary">
        <span>
          {label} <span className="text-content-muted">(default {defaultValue})</span>
        </span>
        <span className="tabular-nums text-content-muted">{value.toFixed(0)}</span>
      </div>
      <input
        type="range"
        min={0}
        max={100}
        step={1}
        value={value}
        disabled={disabled}
        onChange={(e) => onCommit(Number(e.target.value))}
        className="w-full accent-accent disabled:opacity-50"
      />
    </div>
  );
}

export interface MeasurePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
  /** Absent in `StageInspector`'s `'global'` mode (Settings → Stacking has
   *  no run to re-measure) — the button is hidden entirely rather than
   *  rendered disabled with nothing to call. */
  onRemeasure?: () => void;
  remeasureDisabled?: boolean;
}

export function MeasurePanel({
  config,
  onChange,
  disabled,
  defaults,
  onRemeasure,
  remeasureDisabled,
}: MeasurePanelProps) {
  const m = config.measurement;
  const sel = config.selection;

  const patchMeasurement = (patch: Partial<StackingConfig['measurement']>) => {
    onChange({ ...config, measurement: { ...m, ...patch } });
  };
  const patchFormula = (patch: Partial<StackingConfig['measurement']['formula']>) => {
    patchMeasurement({ formula: { ...m.formula, ...patch } });
  };
  const patchSelection = (patch: Partial<StackingConfig['selection']>) => {
    onChange({ ...config, selection: { ...sel, ...patch } });
  };

  return (
    <div className="space-y-4">
      <div>
        <label className="block text-xs text-content-secondary mb-1">Weight mode</label>
        <select
          value={m.weightMode}
          disabled={disabled}
          onChange={(e) => patchMeasurement({ weightMode: e.target.value as WeightMode })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {WEIGHT_MODES.map((v) => (
            <option key={v} value={v}>
              {weightModeLabel(v)}
            </option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {weightModeLabel(defaults.measurement.weightMode)}
        </p>
      </div>

      {m.weightMode === 'keyword' && (
        <div>
          <label className="block text-xs text-content-secondary mb-1">FITS keyword</label>
          <input
            type="text"
            value={m.keyword}
            disabled={disabled}
            onChange={(e) => patchMeasurement({ keyword: e.target.value })}
            className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
          />
          <p className="mt-1 text-[11px] text-content-muted">
            default {defaults.measurement.keyword}
          </p>
        </div>
      )}

      {m.weightMode === 'formula' && (
        <div className="space-y-2 pt-1">
          <h4 className="text-xs font-medium text-content-secondary">Formula weights</h4>
          <WeightSlider
            label="FWHM"
            value={m.formula.fwhm}
            defaultValue={defaults.measurement.formula.fwhm}
            onCommit={(n) => patchFormula({ fwhm: n })}
            disabled={disabled}
          />
          <WeightSlider
            label="Eccentricity"
            value={m.formula.eccentricity}
            defaultValue={defaults.measurement.formula.eccentricity}
            onCommit={(n) => patchFormula({ eccentricity: n })}
            disabled={disabled}
          />
          <WeightSlider
            label="SNR"
            value={m.formula.snr}
            defaultValue={defaults.measurement.formula.snr}
            onCommit={(n) => patchFormula({ snr: n })}
            disabled={disabled}
          />
          <WeightSlider
            label="Stars"
            value={m.formula.stars}
            defaultValue={defaults.measurement.formula.stars}
            onCommit={(n) => patchFormula({ stars: n })}
            disabled={disabled}
          />
          <WeightSlider
            label="Pedestal"
            value={m.formula.pedestal}
            defaultValue={defaults.measurement.formula.pedestal}
            onCommit={(n) => patchFormula({ pedestal: n })}
            disabled={disabled}
          />
        </div>
      )}

      <div>
        <label className="block text-xs text-content-secondary mb-1">PSF model</label>
        <select
          value={m.psfModel}
          disabled={disabled}
          onChange={(e) => patchMeasurement({ psfModel: e.target.value as PsfModel })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {PSF_MODELS.map((v) => (
            <option key={v} value={v}>
              {psfModelLabel(v)}
            </option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {psfModelLabel(defaults.measurement.psfModel)}
        </p>
      </div>

      <NumericField
        label="Max stars"
        value={m.maxStars}
        onCommit={(n) => patchMeasurement({ maxStars: Math.round(n) })}
        min={1}
        step={1}
        disabled={disabled}
        help={`default ${defaults.measurement.maxStars}`}
      />

      <NumericField
        label="Detection threshold (σ)"
        value={m.detectionSigma}
        onCommit={(n) => patchMeasurement({ detectionSigma: n })}
        min={1}
        max={100}
        step={0.5}
        disabled={disabled}
        help={`default ${defaults.measurement.detectionSigma} — a star's peak above the local sky, in noise σ. Lower measures more, fainter stars.`}
      />

      <div className="pt-3 border-t border-border/60 space-y-3">
        <h4 className="text-xs font-medium text-content-secondary">Selection filters</h4>
        <NumericField
          label="Min weight fraction"
          value={sel.minWeightFraction}
          onCommit={(n) => patchSelection({ minWeightFraction: n })}
          min={0}
          max={1}
          step={0.01}
          disabled={disabled}
          help={`default ${defaults.selection.minWeightFraction}`}
        />
        <NullableNumericField
          label="Max FWHM (px)"
          value={sel.maxFwhmPx}
          presetDefaultValue={defaults.selection.maxFwhmPx}
          onCommit={(n) => patchSelection({ maxFwhmPx: n })}
          min={0}
          step={0.1}
          disabled={disabled}
          help={nullableDefaultHelp(defaults.selection.maxFwhmPx, 'every frame passes regardless of FWHM')}
        />
        <NullableNumericField
          label="Max eccentricity"
          value={sel.maxEccentricity}
          presetDefaultValue={defaults.selection.maxEccentricity}
          onCommit={(n) => patchSelection({ maxEccentricity: n })}
          min={0}
          max={1}
          step={0.01}
          disabled={disabled}
          help={nullableDefaultHelp(defaults.selection.maxEccentricity, 'every frame passes regardless of eccentricity')}
        />
        <NullableNumericField
          label="Min stars"
          value={sel.minStars}
          presetDefaultValue={defaults.selection.minStars}
          onCommit={(n) => patchSelection({ minStars: n === null ? null : Math.round(n) })}
          min={0}
          step={1}
          disabled={disabled}
          help={nullableDefaultHelp(defaults.selection.minStars, 'every frame passes regardless of star count')}
        />
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={sel.excludeOnRegistrationFailure}
            disabled={disabled}
            onChange={(e) => patchSelection({ excludeOnRegistrationFailure: e.target.checked })}
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Exclude frames whose registration failed</span>
        </label>
      </div>

      {onRemeasure && (
        <div className="pt-3 border-t border-border/60">
          <button
            type="button"
            onClick={onRemeasure}
            disabled={remeasureDisabled}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors ${
              remeasureDisabled
                ? 'bg-surface text-content-muted cursor-not-allowed'
                : 'bg-accent text-surface hover:bg-accent-hover'
            }`}
          >
            <RefreshCw size={14} />
            Re-measure
          </button>
        </div>
      )}
    </div>
  );
}
