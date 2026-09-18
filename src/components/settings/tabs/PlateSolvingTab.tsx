// Settings redesign (spec 2026-09-18 §2) — Plate Solving tab.
// `PlateSolveSettingsPanel` (Task D1) renders its own three registered
// `SettingsSection`s (plateSolving.catalog/solver/inputGate) from ONE
// component — see `AnalysisTab.tsx` for the same shared-element pattern.
import { PlateSolveSettingsPanel } from '../../plate-solve';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

const plateSolvingPanel = <PlateSolveSettingsPanel />;

export const PLATE_SOLVING_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'plateSolving.catalog', element: plateSolvingPanel },
  { sectionId: 'plateSolving.solver', element: plateSolvingPanel },
  { sectionId: 'plateSolving.inputGate', element: plateSolvingPanel },
];

export function PlateSolvingTab() {
  return <div className="space-y-6">{renderTabSections(PLATE_SOLVING_SECTIONS)}</div>;
}
