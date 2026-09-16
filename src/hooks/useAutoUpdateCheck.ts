import { useEffect, useRef } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { useUpdates } from '../contexts/UpdatesContext';
import type { WhatsNew } from '../types/models';

/**
 * Launch sequence (spec §5.3), both hosts:
 *  1. `get_whats_new` — once per version; opens the What's-new dialog.
 *  2. unless `updates.auto_check` is off: `check_for_updates`; a newer
 *     version raises a toast + bell entry linking to /about?update.
 * Failures are console-only — offline at launch is normal, never a toast.
 */
export function useAutoUpdateCheck() {
  const { notify } = useNotifications();
  const { runCheck, openWhatsNew } = useUpdates();
  const ran = useRef(false);

  useEffect(() => {
    if (ran.current) return; // React StrictMode double-invoke guard
    ran.current = true;

    (async () => {
      try {
        const w = await api.invoke<WhatsNew | null>('get_whats_new');
        if (w) openWhatsNew(w);
      } catch (err) {
        console.error('get_whats_new:', err);
      }
      try {
        const enabled = await api.invoke<string>('get_setting', { key: 'updates.auto_check', defaultValue: 'true' });
        if (enabled.toLowerCase() !== 'true') return;
        const info = await runCheck();
        if (info?.isUpdateAvailable) {
          notify({
            title: `Update available: v${info.latestVersion}`,
            detail: info.platformSupported
              ? `You have v${info.currentVersion}. Click to read the notes and install.`
              : `You have v${info.currentVersion}. Click to read the notes.`,
            tone: 'success',
            kind: 'update',
            link: '/about?update',
            dedupeKey: `update-${info.latestVersion}`,
          });
        }
      } catch (err) {
        console.error('auto update check:', err);
      }
    })();
  }, [notify, runCheck, openWhatsNew]);
}
