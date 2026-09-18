// Settings redesign (spec 2026-09-18 §2) — Analysis tab. `AnalysisSettingsPanel`
// (Task D1) renders its own five registered `SettingsSection`s
// (analysis.detection/measurement/psf/batch/rejection) from ONE component —
// all five entries below share the SAME element so `renderTabSections`
// mounts it exactly once (see `tabSectionEntry.tsx`).
import { AnalysisSettingsPanel } from '../../analysis/AnalysisSettingsPanel';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

const analysisPanel = <AnalysisSettingsPanel />;

export const ANALYSIS_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'analysis.detection', element: analysisPanel },
  { sectionId: 'analysis.measurement', element: analysisPanel },
  { sectionId: 'analysis.psf', element: analysisPanel },
  { sectionId: 'analysis.batch', element: analysisPanel },
  { sectionId: 'analysis.rejection', element: analysisPanel },
];

export function AnalysisTab() {
  return <div className="space-y-6">{renderTabSections(ANALYSIS_SECTIONS)}</div>;
}
