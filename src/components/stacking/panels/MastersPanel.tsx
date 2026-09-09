// Stage 0.5 (Masters) inspector panel: info only — like `DebayerPanel`, this
// stage has no config of its own (owner requirement 2026-09-09: the run
// builds/rebuilds its own missing calibration masters before calibrating).
// Lists exactly what `plan.mastersToBuild` says the run would do.

import type { StackingPlan } from '../../../types/stacking';

export interface MastersPanelProps {
  plan: StackingPlan | null;
}

export function MastersPanel({ plan }: MastersPanelProps) {
  const items = plan?.mastersToBuild ?? [];

  return (
    <div className="space-y-2">
      <p className="text-sm text-content-secondary">
        Every linked calibration set without a built master, and every built master
        whose file is missing from disk, is built or rebuilt here — before calibration.
      </p>
      {items.length === 0 ? (
        <p className="text-xs text-content-muted">Nothing to build — every linked master is already on disk.</p>
      ) : (
        <ul className="space-y-1">
          {items.map((m) => (
            <li
              key={`${m.kind}-${m.setId}`}
              className="flex items-center justify-between gap-2 text-xs text-content-secondary"
            >
              <span className="truncate">{m.label}</span>
              <span className="shrink-0 text-content-muted">
                {m.kind === 'build' ? 'build' : 'rebuild'} · {m.frameCount} frames
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
