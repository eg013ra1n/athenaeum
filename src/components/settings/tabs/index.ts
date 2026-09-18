// Settings redesign (spec 2026-09-18 §2/§7, plan Task E1 Step 2): the one
// place that knows which real element renders a given registry section id.
// Built by concatenating every tab file's own ordered `*_SECTIONS` list —
// `sectionComponent(id)` is DERIVED from the exact data each tab already
// renders from (`tabSectionEntry.tsx`'s `renderTabSections`), never a
// second, hand-maintained id → component map. `SearchResults` is the one
// consumer: it renders a matching section's real element outside its tab so
// editing there commits through the same component, not a copy.
import { GENERAL_SECTIONS } from './GeneralTab';
import { BLINK_SECTIONS } from './BlinkTab';
import { ANALYSIS_SECTIONS } from './AnalysisTab';
import { PLATE_SOLVING_SECTIONS } from './PlateSolvingTab';
import { CALIBRATION_SECTIONS } from './CalibrationTab';
import { STACKING_SECTIONS } from './StackingTab';
import { TRANSFERS_SECTIONS } from './TransfersTab';
import type { TabSectionEntry } from './tabSectionEntry';

export type { TabSectionEntry } from './tabSectionEntry';

const ALL_SECTION_ENTRIES: TabSectionEntry[] = [
  ...GENERAL_SECTIONS,
  ...BLINK_SECTIONS,
  ...ANALYSIS_SECTIONS,
  ...PLATE_SOLVING_SECTIONS,
  ...CALIBRATION_SECTIONS,
  ...STACKING_SECTIONS,
  ...TRANSFERS_SECTIONS,
];

const ELEMENT_BY_SECTION_ID = new Map(ALL_SECTION_ENTRIES.map((e) => [e.sectionId, e.element]));

/** The real element that renders registry section `id` — throws on a miss,
 *  same dev-time-crash contract as `sectionById`/`fieldMeta` (a section
 *  missing here means some tab file forgot to list it in its own
 *  `*_SECTIONS` export, which the registry-coverage test also catches). */
export function sectionComponent(id: string) {
  const element = ELEMENT_BY_SECTION_ID.get(id);
  if (!element) {
    throw new Error(`[settings/tabs] no rendered element for section "${id}"`);
  }
  return element;
}

export { GeneralTab } from './GeneralTab';
export { BlinkTab } from './BlinkTab';
export { AnalysisTab } from './AnalysisTab';
export { PlateSolvingTab } from './PlateSolvingTab';
export { CalibrationTab } from './CalibrationTab';
export { StackingTab } from './StackingTab';
export { TransfersTab } from './TransfersTab';
