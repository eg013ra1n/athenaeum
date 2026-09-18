// Settings redesign (spec 2026-09-18 §2) — Transfers tab: Account · Sync ·
// Folders · Upload speed limit · Simultaneous incoming transfers · Transfer
// storage. `AccountSection`/`SyncSection`/`TransfersSection` keep their own
// Save buttons until Task D3 — see `AnalysisTab.tsx` for the same
// registration pattern. Account and Sync moved here from General (spec §2).
import { ArrowLeftRight, RefreshCw, UserCircle } from 'lucide-react';
import { RegisteredSections } from '../RegisteredSections';
import AccountSection from '../AccountSection';
import SyncSection from '../SyncSection';
import TransfersSection from '../TransfersSection';

export function TransfersTab() {
  return (
    <div className="space-y-6">
      <RegisteredSections ids={['transfers.account']}>
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <UserCircle size={20} />
            Account
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Sign in to link this machine to your account for syncing frames between devices.
            Optional — every feature works without an account.
          </p>
          <AccountSection />
        </div>
      </RegisteredSections>

      <RegisteredSections ids={['transfers.sync']}>
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <RefreshCw size={20} />
            Sync
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Send frames between your machines. A Capture device queues its frames to a paired
            Primary; the Primary receives and ingests them. Transfer folders, bandwidth and
            storage live below.
          </p>
          <SyncSection />
        </div>
      </RegisteredSections>

      <RegisteredSections ids={['transfers.folders', 'transfers.upload', 'transfers.receiving', 'transfers.storage']}>
        <div className="bg-surface-elevated rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
            <ArrowLeftRight size={20} />
            Transfers
          </h3>
          <p className="text-xs text-content-muted mb-4">
            Where transfers keep their working data, how fast they may upload, how many may arrive at once.
          </p>
          <TransfersSection />
        </div>
      </RegisteredSections>
    </div>
  );
}
