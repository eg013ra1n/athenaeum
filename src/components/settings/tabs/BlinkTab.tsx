// Settings redesign (spec 2026-09-18 §2) — Blink tab: Blink viewer · Flat
// contour plot · Star annotation display. See `GeneralTab.tsx` for why this
// file also exports its ordered `BLINK_SECTIONS` list (Task E1).
import { BlinkViewerSection } from '../sections/BlinkViewerSection';
import { FlatContourSection } from '../sections/FlatContourSection';
import { StarAnnotationSection } from '../sections/StarAnnotationSection';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

export const BLINK_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'blink.viewer', element: <BlinkViewerSection /> },
  { sectionId: 'blink.flatContour', element: <FlatContourSection /> },
  { sectionId: 'blink.annotations', element: <StarAnnotationSection /> },
];

export function BlinkTab() {
  return <div className="space-y-6">{renderTabSections(BLINK_SECTIONS)}</div>;
}
