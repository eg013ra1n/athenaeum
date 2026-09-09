import { useEffect, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { isSkyFieldMetric, type SkyFieldMetric } from '../utils/skyFieldStyle';

const settingKey = 'skychart.field_metric';

export function useSkyFieldMetric() {
  const [metric, setMetric] = useState<SkyFieldMetric>('grouping');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const { notify } = useNotifications();

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<string>('get_setting', { key: settingKey, defaultValue: 'grouping' })
      .then(value => {
        if (!cancelled && isSkyFieldMetric(value)) setMetric(value);
      })
      .catch(error => {
        console.error('Failed to load sky field metric:', error);
        if (!cancelled)
          notify({
            title: 'Could not load field coloring',
            detail: 'Using grouping colors for now.',
            kind: 'generic',
            tone: 'warning',
          });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [notify]);

  async function changeMetric(value: SkyFieldMetric) {
    if (loading || saving) return;
    setSaving(true);
    // Commit the visible choice only when it is persisted. Disable the control
    // during the write so a slower response cannot overwrite a newer choice.
    try {
      await api.invoke('set_setting', { key: settingKey, value });
      setMetric(value);
    } catch (error) {
      console.error('Failed to save sky field metric:', error);
      notify({
        title: 'Could not save field coloring',
        detail: 'Your previous choice is still active. Please try again.',
        kind: 'generic',
        tone: 'warning',
      });
    } finally {
      setSaving(false);
    }
  }

  return { metric, changeMetric, busy: loading || saving };
}
