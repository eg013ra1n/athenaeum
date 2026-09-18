// Settings redesign (spec 2026-09-18 §2) — Stacking tab: Pipeline defaults
// (stage list + inspector) · Default folders. `StackingSection` renders its
// own two registered sections (`stacking.pipeline`, `stacking.folders`)
// from ONE component — same shared-element pattern as `AnalysisTab.tsx`.
// Per-set overrides live on each frame set's own Stacking tab
// (`src/components/stacking/StackingTab.tsx`), unrelated to this file.
import StackingSection from '../StackingSection';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

const stackingSection = <StackingSection />;

export const STACKING_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'stacking.pipeline', element: stackingSection },
  { sectionId: 'stacking.folders', element: stackingSection },
];

export function StackingTab() {
  return <div className="space-y-6">{renderTabSections(STACKING_SECTIONS)}</div>;
}
