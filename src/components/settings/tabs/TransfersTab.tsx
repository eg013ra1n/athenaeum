// Settings redesign (spec 2026-09-18 §2) — Transfers tab: Account · Sync ·
// Folders · Upload speed limit · Simultaneous incoming transfers · Transfer
// storage. `AccountSection`/`SyncSection`/`TransfersSection` each render
// their own `SettingsSection`(s) now (Task D3) — the registry supplies every
// title/description, so this tab is just the tab's section order. Account
// and Sync moved here from General (spec §2).
import AccountSection from '../AccountSection';
import SyncSection from '../SyncSection';
import TransfersSection from '../TransfersSection';

export function TransfersTab() {
  return (
    <div className="space-y-6">
      <AccountSection />
      <SyncSection />
      <TransfersSection />
    </div>
  );
}
