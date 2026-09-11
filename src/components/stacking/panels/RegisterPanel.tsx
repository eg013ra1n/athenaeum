// Stage 5 (Register) inspector panel: the geometry mode (M4b),
// model/distortion (including the M4c thin-plate spline and its λ, plus
// the local distortion loop) /interpolation, clamping, star cap, RANSAC
// tolerance/iterations, max RMS + fail-on-max-rms, write-registered-frames,
// and the detection pair under an Advanced disclosure.

import { useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { NumericField } from '../NumericField';
import { ParamPair } from '../ParamPair';
import { distortionLabel, interpolationLabel, modelLabel } from '../stageSummary';
import type {
  DistortionChoice,
  Interpolation,
  ModelChoice,
  RegistrationGeometry,
  StackingConfig,
} from '../../../types/stacking';

/** M4b ruling R-M4b-4: the two geometry modes, with the sentence that
 *  explains what each one does to the delivered masters. */
const GEOMETRIES: { value: RegistrationGeometry; label: string }[] = [
  {
    value: 'coRegistered',
    label: "Co-registered — every group is resampled into the set reference's geometry (one geometry for all masters)",
  },
  {
    value: 'native',
    label: 'Native — each group keeps its own reference and geometry (no cross-group registration)',
  },
];

const MODELS: ModelChoice[] = ['auto', 'similarity', 'affine', 'homography'];
const DISTORTIONS: DistortionChoice[] = [
  'off',
  'polynomial2',
  'polynomial3',
  'polynomial4',
  // M4c (ruling R-M4c-5). `auto` stays last: it is the resolved-by-inlier
  // -count option, not another model.
  'tps',
  'auto',
];
const INTERPOLATIONS: Interpolation[] = [
  'nearest',
  'bilinear',
  'bicubicSpline',
  'bicubicBSpline',
  'lanczos3',
  'lanczos4',
  'mitchellNetravali',
];

export interface RegisterPanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
}

export function RegisterPanel({ config, onChange, disabled, defaults }: RegisterPanelProps) {
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const r = config.registration;

  const patch = (p: Partial<StackingConfig['registration']>) => {
    onChange({ ...config, registration: { ...r, ...p } });
  };
  const patchDetection = (p: Partial<StackingConfig['registration']['detection']>) => {
    patch({ detection: { ...r.detection, ...p } });
  };

  return (
    <div className="space-y-3">
      {/* Geometry (M4b, ruling R-M4b-4) — a radio, not a select: the two
       *  labels carry the whole explanation and there will never be a
       *  third. */}
      <div>
        <span className="block text-xs text-content-secondary mb-1">Geometry</span>
        <div className="space-y-1.5">
          {GEOMETRIES.map((g) => (
            <label key={g.value} className="flex items-start gap-2 cursor-pointer">
              <input
                type="radio"
                name="stacking-registration-geometry"
                value={g.value}
                checked={r.geometry === g.value}
                disabled={disabled}
                onChange={() => patch({ geometry: g.value })}
                className="mt-0.5 w-4 h-4 border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
              />
              <span className="text-sm text-content-secondary">{g.label}</span>
            </label>
          ))}
        </div>
        <p className="mt-1 text-[11px] text-content-muted">
          default {defaults.registration.geometry === 'native' ? 'Native' : 'Co-registered'}
        </p>
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Model</label>
        <select
          value={r.model}
          disabled={disabled}
          onChange={(e) => patch({ model: e.target.value as ModelChoice })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {MODELS.map((v) => (
            <option key={v} value={v}>{modelLabel(v)}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">default {modelLabel(defaults.registration.model)}</p>
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Distortion</label>
        <select
          value={r.distortion}
          disabled={disabled}
          onChange={(e) => patch({ distortion: e.target.value as DistortionChoice })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {DISTORTIONS.map((v) => (
            <option key={v} value={v}>{distortionLabel(v)}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {distortionLabel(defaults.registration.distortion)}
        </p>
      </div>

      {/* M4c (ruling R-M4c-5): λ is the spline's own knob and means
       *  nothing for the polynomial arms, so it only appears with the
       *  spline selected. */}
      {r.distortion === 'tps' && (
        <NumericField
          label="TPS smoothing (λ)"
          value={r.tpsSmoothing}
          onCommit={(n) => patch({ tpsSmoothing: n })}
          min={0}
          max={10}
          step={0.5}
          disabled={disabled}
          help={`0 = interpolating (lands every inlier exactly); default ${defaults.registration.tpsSmoothing}`}
        />
      )}

      {/* M4c (ruling R-M4c-7): the loop refits the distortion, so with
       *  none selected there is nothing for it to do. */}
      <label
        className={`flex items-center gap-2 ${
          r.distortion === 'off' ? 'cursor-default' : 'cursor-pointer'
        }`}
      >
        <input
          type="checkbox"
          checked={r.localDistortion}
          disabled={disabled || r.distortion === 'off'}
          onChange={(e) => patch({ localDistortion: e.target.checked })}
          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
        />
        <span
          className={`text-sm ${
            r.distortion === 'off' ? 'text-content-muted' : 'text-content-secondary'
          }`}
        >
          Local distortion loop
          {r.distortion === 'off' ? ' (needs a distortion model)' : ''}
        </span>
      </label>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Interpolation</label>
        <select
          value={r.interpolation}
          disabled={disabled}
          onChange={(e) => patch({ interpolation: e.target.value as Interpolation })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {INTERPOLATIONS.map((v) => (
            <option key={v} value={v}>{interpolationLabel(v)}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {interpolationLabel(defaults.registration.interpolation)}
        </p>
      </div>

      <NumericField
        label="Clamping threshold"
        value={r.clampingThreshold}
        onCommit={(n) => patch({ clampingThreshold: n })}
        min={0}
        max={1}
        step={0.01}
        disabled={disabled}
        help={`default ${defaults.registration.clampingThreshold.toFixed(2)}`}
      />

      <NumericField
        label="Max stars"
        value={r.maxStars}
        onCommit={(n) => patch({ maxStars: Math.round(n) })}
        min={1}
        step={1}
        disabled={disabled}
        help={`default ${defaults.registration.maxStars}`}
      />

      <NumericField
        label="RANSAC tolerance (px)"
        value={r.ransacTolerancePx}
        onCommit={(n) => patch({ ransacTolerancePx: n })}
        min={0}
        step={0.1}
        disabled={disabled}
        help={`default ${defaults.registration.ransacTolerancePx}`}
      />

      <NumericField
        label="RANSAC max iterations"
        value={r.ransacMaxIterations}
        onCommit={(n) => patch({ ransacMaxIterations: Math.round(n) })}
        min={1}
        step={1}
        disabled={disabled}
        help={`default ${defaults.registration.ransacMaxIterations}`}
      />

      <NumericField
        label="Max RMS (px)"
        value={r.maxRmsPx}
        onCommit={(n) => patch({ maxRmsPx: n })}
        min={0}
        step={0.1}
        disabled={disabled}
        help={`default ${defaults.registration.maxRmsPx}`}
      />

      <label className="flex items-center gap-2 cursor-pointer">
        <input
          type="checkbox"
          checked={r.failOnMaxRms}
          disabled={disabled}
          onChange={(e) => patch({ failOnMaxRms: e.target.checked })}
          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
        />
        <span className="text-sm text-content-secondary">Fail the frame when max RMS is exceeded</span>
      </label>

      <label className="flex items-center gap-2 cursor-pointer">
        <input
          type="checkbox"
          checked={r.writeRegisteredFrames}
          disabled={disabled}
          onChange={(e) => patch({ writeRegisteredFrames: e.target.checked })}
          className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
        />
        <span className="text-sm text-content-secondary">Write registered frames</span>
      </label>

      <div className="pt-2 border-t border-border/60">
        <button
          type="button"
          onClick={() => setAdvancedOpen((o) => !o)}
          aria-expanded={advancedOpen}
          className="flex items-center gap-1.5 text-xs font-medium text-content-secondary hover:text-content transition-colors"
        >
          {advancedOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          Advanced
        </button>
        {advancedOpen && (
          <div className="mt-2 ml-1">
            <ParamPair
              leftLabel="Min SNR"
              leftValue={r.detection.minSnr}
              onLeftCommit={(n) => patchDetection({ minSnr: n })}
              rightLabel="Max eccentricity"
              rightValue={r.detection.maxEccentricity}
              onRightCommit={(n) => patchDetection({ maxEccentricity: n })}
              min={0}
              rightMax={1}
              step={0.01}
              disabled={disabled}
              help={`defaults ${defaults.registration.detection.minSnr} / ${defaults.registration.detection.maxEccentricity}`}
            />
          </div>
        )}
      </div>
    </div>
  );
}
