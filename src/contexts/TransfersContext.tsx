// Personal-sync Transfers context (task M3).
//
// Wraps a single `useSyncStatus` instance so the sidebar `TransferIndicator` and
// the root-mounted `TransfersPanel` share ONE poller, ONE event subscription,
// and ONE notification path (mounting the hook twice would double both). Also
// owns the slide-over open/close state the indicator toggles and the panel reads.
//
// Placed inside `NotificationProvider` (the hook calls `useNotifications`).

import { createContext, useContext, useMemo, useState, type ReactNode } from 'react';
import { useSyncStatus, type UseSyncStatus } from '../hooks/useSyncStatus';

interface TransfersContextValue extends UseSyncStatus {
  /** Whether the Transfers slide-over is open. */
  open: boolean;
  openPanel: () => void;
  closePanel: () => void;
  /**
   * Session-only dismissal of the "received files land in the app-data folder"
   * strip (audit UX-1). Lives in the app-root context — NOT localStorage — so it
   * survives route changes but resets on the next app launch.
   */
  appDataWarningDismissed: boolean;
  dismissAppDataWarning: () => void;
}

const TransfersContext = createContext<TransfersContextValue | null>(null);

export function TransfersProvider({ children }: { children: ReactNode }) {
  const sync = useSyncStatus();
  const [open, setOpen] = useState(false);
  const [appDataWarningDismissed, setAppDataWarningDismissed] = useState(false);

  const value = useMemo<TransfersContextValue>(
    () => ({
      ...sync,
      open,
      openPanel: () => setOpen(true),
      closePanel: () => setOpen(false),
      appDataWarningDismissed,
      dismissAppDataWarning: () => setAppDataWarningDismissed(true),
    }),
    [sync, open, appDataWarningDismissed],
  );

  return <TransfersContext.Provider value={value}>{children}</TransfersContext.Provider>;
}

export function useTransfers(): TransfersContextValue {
  const ctx = useContext(TransfersContext);
  if (!ctx) throw new Error('useTransfers must be used within TransfersProvider');
  return ctx;
}
