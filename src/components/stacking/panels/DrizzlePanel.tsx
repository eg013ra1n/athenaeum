// Stage 8 (Drizzle) inspector panel: every field rendered, all disabled —
// the stage arrives in M3. Read-only so no `onChange` prop is needed.

import { kernelLabel } from '../stageSummary';
import type { StackingConfig } from '../../../types/stacking';

export interface DrizzlePanelProps {
  config: StackingConfig;
}

export function DrizzlePanel({ config }: DrizzlePanelProps) {
  const d = config.drizzle;

  return (
    <div className="space-y-3 opacity-60">
      <p className="text-xs italic text-content-muted">Drizzle arrives in M3.</p>

      <label className="flex items-center gap-2 cursor-not-allowed">
        <input type="checkbox" checked={d.enabled} disabled className="w-4 h-4 rounded border-border" />
        <span className="text-sm text-content-muted">Enabled</span>
      </label>

      <div className="grid grid-cols-2 gap-2">
        <div>
          <label className="block text-xs text-content-muted mb-1">Scale</label>
          <input
            type="number"
            value={d.scale}
            disabled
            readOnly
            className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
          />
        </div>
        <div>
          <label className="block text-xs text-content-muted mb-1">Drop shrink</label>
          <input
            type="number"
            value={d.dropShrink}
            disabled
            readOnly
            className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
          />
        </div>
      </div>

      <div>
        <label className="block text-xs text-content-muted mb-1">Kernel</label>
        <select
          value={d.kernel}
          disabled
          className="w-full px-2 py-1 text-sm bg-surface text-content-muted rounded border border-border cursor-not-allowed"
        >
          <option value={d.kernel}>{kernelLabel(d.kernel)}</option>
        </select>
      </div>

      <label className="flex items-center gap-2 cursor-not-allowed">
        <input type="checkbox" checked={d.useRejection} disabled className="w-4 h-4 rounded border-border" />
        <span className="text-sm text-content-muted">Use rejection</span>
      </label>
      <label className="flex items-center gap-2 cursor-not-allowed">
        <input type="checkbox" checked={d.useWeights} disabled className="w-4 h-4 rounded border-border" />
        <span className="text-sm text-content-muted">Use weights</span>
      </label>
      <label className="flex items-center gap-2 cursor-not-allowed">
        <input type="checkbox" checked={d.useLocalNormalization} disabled className="w-4 h-4 rounded border-border" />
        <span className="text-sm text-content-muted">Use local normalization</span>
      </label>
      <label className="flex items-center gap-2 cursor-not-allowed">
        <input type="checkbox" checked={d.writeWeightMap} disabled className="w-4 h-4 rounded border-border" />
        <span className="text-sm text-content-muted">Write weight map</span>
      </label>
    </div>
  );
}
