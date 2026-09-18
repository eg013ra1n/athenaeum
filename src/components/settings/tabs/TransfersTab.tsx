// Settings redesign (spec 2026-09-18 §2) — Transfers tab: Account · Sync ·
// Folders · Upload speed limit · Simultaneous incoming transfers · Transfer
// storage. `AccountSection`/`SyncSection` each render one `SettingsSection`;
// `TransfersSection` renders four from one component (shared-element
// pattern, see `AnalysisTab.tsx`) — the registry supplies every
// title/description. Account and Sync moved here from General (spec §2).
import AccountSection from '../AccountSection';
import SyncSection from '../SyncSection';
import TransfersSection from '../TransfersSection';
import { renderTabSections, type TabSectionEntry } from './tabSectionEntry';

const transfersSection = <TransfersSection />;

export const TRANSFERS_SECTIONS: TabSectionEntry[] = [
  { sectionId: 'transfers.account', element: <AccountSection /> },
  { sectionId: 'transfers.sync', element: <SyncSection /> },
  { sectionId: 'transfers.folders', element: transfersSection },
  { sectionId: 'transfers.upload', element: transfersSection },
  { sectionId: 'transfers.receiving', element: transfersSection },
  { sectionId: 'transfers.storage', element: transfersSection },
];

export function TransfersTab() {
  return <div className="space-y-6">{renderTabSections(TRANSFERS_SECTIONS)}</div>;
}
