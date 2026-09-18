// Settings redesign (spec 2026-09-18 §2) — General tab: Updates · Frame set
// grouping · Session detection · Monitoring · Auto-merge · Content index ·
// Archive · Data file locations · Logging.
import { ScrollText } from 'lucide-react';
import { isTauri } from '../../../utils/platform';
import { UpdatesSection } from '../sections/UpdatesSection';
import { FrameSetGroupingSection } from '../sections/FrameSetGroupingSection';
import { SessionDetectionSection } from '../sections/SessionDetectionSection';
import { MonitoringSection } from '../sections/MonitoringSection';
import { AutoMergeSection } from '../sections/AutoMergeSection';
import { ContentIndexSection } from '../sections/ContentIndexSection';
import { ArchiveSection } from '../sections/ArchiveSection';
import { DataLocationsSection } from '../sections/DataLocationsSection';
import { RegisteredSections } from '../RegisteredSections';
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

      {/* LoggingSettings keeps its own Save button and duplication until
          Task D2 rebuilds it on LevelSelect/useAutosaveDocument — this tab
          only registers its section id and supplies the card chrome. */}
      <RegisteredSections ids={['general.logging']}>
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <ScrollText size={20} />
            Logging
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Controls what gets written to the JSONL log file{isTauri ? ' shown above' : ''}. Debug is
            verbose — useful while diagnosing an issue, not recommended to leave on permanently.
          </p>
          <LoggingSettings />
        </div>
      </RegisteredSections>
    </div>
  );
}
