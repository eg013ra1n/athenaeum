import { useEffect, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';

export type SkyBackground = 'original' | 'dss-color';

export function useSkyBackground() {
  const [background, setBackground] = useState<SkyBackground>('original');
  const [busy, setBusy] = useState(true);
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<string>('get_setting', { key: 'skychart.background', defaultValue: 'original' })
      .then(value => {
        if (!cancelled && (value === 'original' || value === 'dss-color')) setBackground(value);
      })
      .catch(error => {
        console.error('Failed to load sky background:', error);
        if (!cancelled)
          notify({
            title: 'Could not load sky background preference',
            detail: 'Using the original star chart for now.',
            kind: 'generic',
            tone: 'warning',
          });
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [notify]);

  async function changeBackground(value: SkyBackground) {
    if (busy) return;
    setBusy(true);
    try {
      await api.invoke('set_setting', { key: 'skychart.background', value });
      setBackground(value);
    } catch (error) {
      console.error('Failed to save sky background:', error);
      notify({
        title: 'Could not save sky background',
        detail: 'The previous choice is still active. Please try again.',
        kind: 'generic',
        tone: 'warning',
      });
    } finally {
      setBusy(false);
    }
  }

  return { background, changeBackground, busy };
}
