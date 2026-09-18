// Settings redesign (spec 2026-09-18 §2) — Plate Solving tab.
// `PlateSolveSettingsPanel` (Task D1) now renders its own three registered
// `SettingsSection`s (plateSolving.catalog/solver/inputGate) directly — see
// `AnalysisTab.tsx` for the same pattern.
import { PlateSolveSettingsPanel } from '../../plate-solve';

export function PlateSolvingTab() {
  return (
    <div className="space-y-6">
      <PlateSolveSettingsPanel />
    </div>
  );
}
