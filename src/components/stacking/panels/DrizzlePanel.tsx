// Stage 8 (Drizzle) inspector panel (M3 Task 6 — live). Mirrors
// `NormalizePanel.tsx`'s editing pattern: `patch` merges a partial
// `DrizzleConfig` into `config.drizzle` and calls `onChange` with the whole
// new `StackingConfig`. Per-set (`mode === 'perSet'`, the default), the
// stage's own on/off toggle lives on the `PipelineBoard` row
// (`onToggleDrizzle`, wired through `StackingTab`) — this panel edits the
// settings drizzle would run WITH, whether or not the stage is currently
// enabled, same as every other panel here. In `mode === 'global'`
// (`StackingSection`, Settings → Stacking) there is no board row to hold
// that toggle, so this panel renders its own "Enable drizzle by default"
// checkbox at the top, bound to `config.drizzle.enabled` through the same
// `patch` every other field here uses — otherwise a new frame set could
// only ever start with drizzle on via the Maximum-quality preset.

import { NumericField } from '../NumericField';
import { Checkbox } from '../../settings/Checkbox';
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
  /** `'global'` is `StackingSection`'s usage (Settings → Stacking) — see the
   *  header comment. Defaults to `'perSet'`, unchanged behavior. */
  mode?: 'perSet' | 'global';
}

export function DrizzlePanel({ config, onChange, disabled, defaults, plan, mode = 'perSet' }: DrizzlePanelProps) {
  const d = config.drizzle;
  const lnOn = config.normalization.local.enabled;

  const patch = (p: Partial<StackingConfig['drizzle']>) => {
    onChange({ ...config, drizzle: { ...d, ...p } });
  };

  const estimate = plan ? drizzleEstimate(config, plan) : null;
  // Only a hint, never a disabled control: with no plan loaded (Settings →
  // Stacking) nothing is known about the set's groups, and a set can gain an
  // OSC group later.
  const noOscGroup = plan ? !plan.groups.some((g) => g.colorMode === 'osc') : false;

  return (
    <div className="space-y-3">
      {mode === 'global' && (
        <div className="pb-1 border-b border-border/60">
          <Checkbox
            checked={d.enabled}
            onChange={(checked) => patch({ enabled: checked })}
            disabled={disabled}
            label="Enable drizzle by default"
          />
          <p className="mt-1 ml-6 text-[11px] text-content-muted">
            New frame sets start with drizzle on. Each set's Stacking tab can still turn it off.
          </p>
        </div>
      )}

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

      {/* M4d Task 1 (ruling R-M4d-2): Bayer drizzle. Under the kernel, since
       *  it changes what the deposit READS (each colour's own mosaic
       *  samples) rather than how a drop is spread. Never disabled: a set's
       *  config is edited here and in Settings → Stacking alike, where there
       *  is no plan to ask about OSC groups — the note carries the "OSC
       *  only" part, and a mono group ignores the flag silently. */}
      <div className="pt-1">
        <Checkbox
          checked={d.bayer}
          onChange={(checked) => patch({ bayer: checked })}
          disabled={disabled}
          label="Bayer drizzle (deposit each colour's own samples, OSC only)"
        />
        <p className="mt-1 ml-6 text-[11px] text-content-muted">
          R and B cover a quarter of the pixels each — use drop shrink ≥ 0.9 or more frames.
          {noOscGroup ? ' This set has no OSC group, so it changes nothing.' : ''}
        </p>
        {/* Fix round 1 (I1): with the debayer off, an OSC group's calibrated
         *  frame IS the mosaic and the group is single-plane, so no mosaic is
         *  kept and the toggle has no effect — the run warns about exactly
         *  this, and the panel that owns the toggle should not make the user
         *  discover it there. */}
        {d.bayer && !config.calibration.debayerOsc && (
          <p className="mt-1 ml-6 text-[11px] text-warning">
            No effect — the OSC debayer is off for this set (Calibrate), so no CFA mosaic is kept.
          </p>
        )}
      </div>

      <div className="space-y-1.5 pt-1">
        <Checkbox
          checked={d.useRejection}
          onChange={(checked) => patch({ useRejection: checked })}
          disabled={disabled}
          label="Use rejection"
        />

        <Checkbox
          checked={d.useWeights}
          onChange={(checked) => patch({ useWeights: checked })}
          disabled={disabled}
          label="Use weights"
        />

        <div>
          <Checkbox
            checked={d.useLocalNormalization}
            onChange={(checked) => patch({ useLocalNormalization: checked })}
            disabled={disabled || !lnOn}
            label="Use local normalization"
          />
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

        <Checkbox
          checked={d.writeWeightMap}
          onChange={(checked) => patch({ writeWeightMap: checked })}
          disabled={disabled}
          label="Write weight map"
        />

        <p className="text-[11px] text-content-muted">
          defaults: {defaults.drizzle.scale}×, drop {defaults.drizzle.dropShrink.toFixed(2)},{' '}
          {kernelLabel(defaults.drizzle.kernel)} kernel, {defaults.drizzle.useRejection ? 'rejection on' : 'rejection off'},{' '}
          {defaults.drizzle.useWeights ? 'weights on' : 'weights off'},{' '}
          local normalization {defaults.drizzle.useLocalNormalization ? 'on' : 'off'}, weight map{' '}
          {defaults.drizzle.writeWeightMap ? 'on' : 'off'}, Bayer{' '}
          {defaults.drizzle.bayer ? 'on' : 'off'}.
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
