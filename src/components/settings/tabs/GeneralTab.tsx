// Settings redesign (spec 2026-09-18 §2) — General tab: Updates · Frame set
// grouping · Session detection · Monitoring · Auto-merge · Content index ·
// Archive · Data file locations · Logging.
import { UpdatesSection } from '../sections/UpdatesSection';
import { FrameSetGroupingSection } from '../sections/FrameSetGroupingSection';
import { SessionDetectionSection } from '../sections/SessionDetectionSection';
import { MonitoringSection } from '../sections/MonitoringSection';
import { AutoMergeSection } from '../sections/AutoMergeSection';
import { ContentIndexSection } from '../sections/ContentIndexSection';
import { ArchiveSection } from '../sections/ArchiveSection';
import { DataLocationsSection } from '../sections/DataLocationsSection';
import { SettingsSection } from '../SettingsSection';
import LoggingSettings from '../LoggingSettings';

export function GeneralTab() {
  return (
    <div className="space-y-6">
      <UpdatesSection />
      <FrameSetGroupingSection />
      <SessionDetectionSection />
      <MonitoringSection />
      <AutoMergeSection />
      <ContentIndexSection />
      <ArchiveSection />
      <DataLocationsSection />

      {/* Task D2: `LoggingSettings` is now a real registered-section body —
          title/description come from the registry via `SettingsSection`,
          never restated here. */}
      <SettingsSection id="general.logging">
        <LoggingSettings />
      </SettingsSection>
    </div>
  );
}
