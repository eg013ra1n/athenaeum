// Settings redesign (spec 2026-09-18 §2) — General tab: Updates · Frame set
// grouping · Session detection · Monitoring · Auto-merge · Content index ·
// Archive · Data file locations · Logging.
//
// Task E1: this file now exports its ordered `GENERAL_SECTIONS` list
// (`{ sectionId, element }`) alongside the `GeneralTab` component so
// `tabs/index.ts` can derive `sectionComponent(id)` for `SearchResults`
// from the exact same elements this tab renders — `LoggingSettings` now
// wraps its own `SettingsSection` internally (see that file), so it is
// listed here like every other single-section component.
import { UpdatesSection } from '../sections/UpdatesSection';
import { FrameSetGroupingSection } from '../sections/FrameSetGroupingSection';
import { SessionDetectionSection } from '../sections/SessionDetectionSection';
import { MonitoringSection } from '../sections/MonitoringSection';
import { AutoMergeSection } from '../sections/AutoMergeSection';
import { ContentIndexSection } from '../sections/ContentIndexSection';
import { ArchiveSection } from '../sections/ArchiveSection';
import { DataLocationsSection } from '../sections/DataLocationsSection';
import LoggingSettings from '../LoggingSettings';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

export const GENERAL_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'general.updates', element: <UpdatesSection /> },
  { sectionId: 'general.grouping', element: <FrameSetGroupingSection /> },
  { sectionId: 'general.sessions', element: <SessionDetectionSection /> },
  { sectionId: 'general.monitoring', element: <MonitoringSection /> },
  { sectionId: 'general.autoMerge', element: <AutoMergeSection /> },
  { sectionId: 'general.contentIndex', element: <ContentIndexSection /> },
  { sectionId: 'general.archive', element: <ArchiveSection /> },
  { sectionId: 'general.dataLocations', element: <DataLocationsSection /> },
  { sectionId: 'general.logging', element: <LoggingSettings /> },
];

export function GeneralTab() {
  return <div className="space-y-6">{renderTabSections(GENERAL_SECTIONS)}</div>;
}
