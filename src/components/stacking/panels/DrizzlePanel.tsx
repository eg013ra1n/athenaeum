// Stage 8 (Drizzle) inspector panel (M3 Task 6 — live). Mirrors
// `NormalizePanel.tsx`'s editing pattern: `patch` merges a partial
// `DrizzleConfig` into `config.drizzle` and calls `onChange` with the whole
// new `StackingConfig`. The stage's own on/off toggle lives on the
// `PipelineBoard` row (`onToggleDrizzle`, wired through `StackingTab`) —
// this panel edits the settings drizzle would run WITH, whether or not the
// stage is currently enabled, same as every other panel here.

import { NumericField } from '../NumericField';
import { drizzleEstimate, kernelLabel } from '../stageSummary';
import { formatBytes } from '../formatBytes';
import type { DrizzleKernel, StackingConfig, StackingPlan } from '../../../types/stacking';

const KERNELS: DrizzleKernel[] = ['square', 'circle', 'gaussian'];
const SCALES = [1, 2, 3];

export interface DrizzlePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  disabled?: boolean;
  defaults: StackingConfig;
  /** `null` in Settings → Stacking (global defaults have no frame set to
   *  plan against) — the estimate line degrades to a "no plan" line rather
   *  than reading `plan.groups` at all. */
  plan: StackingPlan | null;
}

export function DrizzlePanel({ config, onChange, disabled, defaults, plan }: DrizzlePanelProps) {
  const d = config.drizzle;
  const lnOn = config.normalization.local.enabled;

  const patch = (p: Partial<StackingConfig['drizzle']>) => {
    onChange({ ...config, drizzle: { ...d, ...p } });
  };

  const estimate = plan ? drizzleEstimate(config, plan) : null;

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-2">
        <div>
          <label className="block text-xs text-content-secondary mb-1">Scale</label>
          <select
            value={d.scale}
            disabled={disabled}
            onChange={(e) => patch({ scale: Number(e.target.value) })}
            className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
          >
            {SCALES.map((v) => (
              <option key={v} value={v}>{v}×</option>
            ))}
          </select>
          <p className="mt-1 text-[11px] text-content-muted">default {defaults.drizzle.scale}×</p>
        </div>

        <NumericField
          label="Drop shrink"
          value={d.dropShrink}
          onCommit={(n) => patch({ dropShrink: n })}
          min={0.5}
          max={1.0}
          step={0.05}
          disabled={disabled}
          help={`default ${defaults.drizzle.dropShrink.toFixed(2)} — the drop's side as a fraction of a source pixel`}
        />
      </div>

      <div>
        <label className="block text-xs text-content-secondary mb-1">Kernel</label>
        <select
          value={d.kernel}
          disabled={disabled}
          onChange={(e) => patch({ kernel: e.target.value as DrizzleKernel })}
          className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50"
        >
          {KERNELS.map((v) => (
            <option key={v} value={v}>{kernelLabel(v)}</option>
          ))}
        </select>
        <p className="mt-1 text-[11px] text-content-muted">
          default {kernelLabel(defaults.drizzle.kernel)} — square = exact clipping; circle/gaussian = 16×16
          tabulated.
        </p>
      </div>

      <div className="space-y-1.5 pt-1">
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={d.useRejection}
            disabled={disabled}
            onChange={(e) => patch({ useRejection: e.target.checked })}
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Use rejection</span>
        </label>

        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={d.useWeights}
            disabled={disabled}
            onChange={(e) => patch({ useWeights: e.target.checked })}
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Use weights</span>
        </label>

        <div>
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={d.useLocalNormalization}
              disabled={disabled || !lnOn}
              onChange={(e) => patch({ useLocalNormalization: e.target.checked })}
              className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
            />
            <span className="text-sm text-content-secondary">Use local normalization</span>
          </label>
          {/* B8 (M3 final fix wave, M5 ruling): the toggle is kept as stored
           *  (never forced off) when LN itself is off for this set — the
           *  driver already falls back to each frame's own global
           *  normalization pair per frame, so the toggle has NO EFFECT
           *  until LN is turned on in the Normalize panel; the hint says
           *  so explicitly rather than leaving the reader to infer it. */}
          {!lnOn && (
            <p className="mt-1 ml-6 text-[11px] text-content-muted">
              no effect — local normalization is off for this set
            </p>
          )}
        </div>

        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="checkbox"
            checked={d.writeWeightMap}
            disabled={disabled}
            onChange={(e) => patch({ writeWeightMap: e.target.checked })}
            className="w-4 h-4 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Write weight map</span>
        </label>

        <p className="text-[11px] text-content-muted">
          defaults: {defaults.drizzle.scale}×, drop {defaults.drizzle.dropShrink.toFixed(2)},{' '}
          {kernelLabel(defaults.drizzle.kernel)} kernel, {defaults.drizzle.useRejection ? 'rejection on' : 'rejection off'},{' '}
          {defaults.drizzle.useWeights ? 'weights on' : 'weights off'},{' '}
          local normalization {defaults.drizzle.useLocalNormalization ? 'on' : 'off'}, weight map{' '}
          {defaults.drizzle.writeWeightMap ? 'on' : 'off'}.
        </p>
      </div>

      <div className="pt-2 border-t border-border/60">
        {plan && estimate ? (
          <p className="text-[11px] text-content-muted tabular-nums">
            Bitmaps ≈ {formatBytes(estimate.bitmapBytes)} (temporary) · output ≈ {formatBytes(estimate.outputBytes)}
            {' '}· ≈ {(estimate.seconds / 60).toFixed(1)} min
            {estimate.incomplete ? ' (some groups without geometry)' : ''}
          </p>
        ) : (
          <p className="text-[11px] text-content-muted">No plan loaded — estimate unavailable.</p>
        )}
      </div>
    </div>
  );
}
