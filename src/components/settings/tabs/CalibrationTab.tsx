// Settings redesign (spec 2026-09-18 §2) — Calibration tab: Matching (the
// six collapsible groups as today) · Master build memory · Master file
// format. `CalibrationMatchingConfig` renders its own `SettingsSection`
// (Task D3) — the registry supplies its title/description, so this tab no
// longer restates them.
import { CalibrationMatchingConfig } from '../../calibration';
import { MasterBuildMemorySection } from '../sections/MasterBuildMemorySection';
import { MasterFileFormatSection } from '../sections/MasterFileFormatSection';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

export const CALIBRATION_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'calibration.matching', element: <CalibrationMatchingConfig /> },
  { sectionId: 'calibration.memory', element: <MasterBuildMemorySection /> },
  { sectionId: 'calibration.masterFormat', element: <MasterFileFormatSection /> },
];

export function CalibrationTab() {
  return <div className="space-y-6">{renderTabSections(CALIBRATION_SECTIONS)}</div>;
}
