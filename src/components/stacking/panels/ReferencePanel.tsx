// Stage 4 (Reference) inspector panel: Auto/Manual mode radio, plus the
// two-pass pick (M4a Task 4, ruling R-M4a-5) under Auto. Manual mode has no
// in-tab picker yet (the frame is chosen on the Analysis tab) — this panel
// only reflects the plan's resolved reference and links there.

import { useNavigate } from 'react-router-dom';
import { CheckCircle2, XCircle } from 'lucide-react';
import type { ReferenceMode, StackingConfig, StackingPlan } from '../../../types/stacking';

export interface ReferencePanelProps {
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  plan: StackingPlan | null;
  disabled?: boolean;
}

export function ReferencePanel({ config, onChange, plan, disabled }: ReferencePanelProps) {
  const navigate = useNavigate();
  const mode = config.reference.mode;

  const setMode = (next: ReferenceMode) => {
    onChange({ ...config, reference: { ...config.reference, mode: next } });
  };

  const setTwoPass = (next: boolean) => {
    onChange({ ...config, reference: { ...config.reference, twoPass: next } });
  };

  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="radio"
            name="stacking-reference-mode"
            checked={mode === 'auto'}
            disabled={disabled}
            onChange={() => setMode('auto')}
            className="w-3.5 h-3.5 text-accent border-border focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Auto</span>
        </label>
        <label className="flex items-center gap-2 cursor-pointer">
          <input
            type="radio"
            name="stacking-reference-mode"
            checked={mode === 'manual'}
            disabled={disabled}
            onChange={() => setMode('manual')}
            className="w-3.5 h-3.5 text-accent border-border focus:ring-accent disabled:opacity-50"
          />
          <span className="text-sm text-content-secondary">Manual</span>
        </label>

        {/* Ruling R-M4a-5: the two-pass pick applies to Auto only — a manual
         *  pin never moves — so in Manual mode the checkbox stays visible
         *  (the stored value is never hidden) but inert. */}
        <label
          className={`flex items-start gap-2 pt-1 ${
            disabled || mode === 'manual' ? 'cursor-not-allowed' : 'cursor-pointer'
          }`}
          title={mode === 'manual' ? 'A manually pinned reference is never re-picked' : undefined}
        >
          <input
            type="checkbox"
            checked={config.reference.twoPass}
            disabled={disabled || mode === 'manual'}
            onChange={(e) => setTwoPass(e.target.checked)}
            className="w-4 h-4 mt-0.5 shrink-0 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          <span className={`text-sm ${mode === 'manual' ? 'text-content-muted' : 'text-content-secondary'}`}>
            Two-pass pick (re-choose the reference closest to the set&rsquo;s median transform)
          </span>
        </label>
      </div>

      {mode === 'auto' ? (
        <p className="text-xs text-content-muted">
          The best-weighted frame of the largest group, chosen at run time.
        </p>
      ) : (
        <div className="space-y-2">
          {plan?.reference.filename ? (
            <div className="flex items-center gap-1.5 text-xs">
              {plan.reference.onDisk ? (
                <CheckCircle2 size={13} className="text-success shrink-0" />
              ) : (
                <XCircle size={13} className="text-error shrink-0" />
              )}
              <span className="font-mono text-content-secondary truncate" title={plan.reference.filename}>
                {plan.reference.filename}
              </span>
            </div>
          ) : (
            <p className="text-xs text-content-muted">No reference frame chosen yet.</p>
          )}
          <button
            type="button"
            onClick={() => navigate('?tab=analysis', { replace: true })}
            className="text-xs underline text-content-secondary hover:no-underline"
          >
            Choose in Analysis
          </button>
        </div>
      )}
    </div>
  );
}
