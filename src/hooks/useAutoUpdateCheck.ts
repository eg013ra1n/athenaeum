import { useEffect, useRef } from 'react';
import { api } from '../api';
import { isTauri } from '../utils/platform';
import { useNotifications } from '../contexts/NotificationContext';
import type { UpdateInfo } from '../types/helpers';

/**
 * On desktop startup, run the existing update check once per launch (unless the
 * user opted out via `updates.auto_check`). If a newer version exists, surface
 * it as a toast + notification-bell entry. Failures (e.g. offline at launch)
 * are logged only — never surfaced, never block startup.
 */
export function useAutoUpdateCheck() {
  const { notify } = useNotifications();
  const ran = useRef(false);

  useEffect(() => {
    if (!isTauri) return;
    if (ran.current) return; // guard React StrictMode double-invoke
    ran.current = true;

    (async () => {
      try {
        const enabled = await api.invoke<string>('get_setting', {
          key: 'updates.auto_check',
          defaultValue: 'true',
        });
        if (enabled.toLowerCase() !== 'true') return;

        const info = await api.invoke<UpdateInfo>('check_for_updates');
        if (info.is_update_available) {
          notify({
            title: `Update available: v${info.latest_version}`,
            detail: `You have v${info.current_version}. Click to view and download on the About page.`,
            tone: 'success',
            kind: 'update',
            link: '/about',
          });
        }
      } catch (err) {
        console.error('auto update check:', err);
      }
    })();
  }, [notify]);
}
