import { useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';

/**
 * Resolve catalog frame IDs to one representative per confirmed exposure in this subset.
 * Returns null while the current selection is unresolved (including request failure),
 * and an empty set for an empty selection. Callers must not substitute raw file sums.
 * `metadataKey` invalidates results after relevant membership or metadata changes.
 */
export function useEffectiveExposures(frameIds: number[], metadataKey = ''): Set<number> | null {
  const idsKey = JSON.stringify([...new Set(frameIds)].sort((a, b) => a - b));
  const key = idsKey + metadataKey;
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{ key: string; revision: number; ids: number[] } | null>(
    null,
  );
  const { notify } = useNotifications();
  useEffect(() => {
    const changed = () => setRevision(v => v + 1);
    window.addEventListener('exposure-versions-changed', changed);
    return () => window.removeEventListener('exposure-versions-changed', changed);
  }, []);
  useEffect(() => {
    let cancelled = false;
    if (idsKey === '[]') {
      setResult({ key, revision, ids: [] });
      return;
    }
    api
      .invoke<number[]>('get_effective_exposure_frame_ids', { frameIds: JSON.parse(idsKey) })
      .then(ids => {
        if (!cancelled) setResult({ key, revision, ids });
      })
      .catch(error => {
        console.error('Exposure totals unavailable:', error);
        if (!cancelled)
          notify({
            title: 'Exposure totals unavailable',
            detail: String(error),
            kind: 'generic',
            tone: 'warning',
            hasErrors: true,
          });
      });
    return () => {
      cancelled = true;
    };
  }, [key, idsKey, revision, notify]);
  return useMemo(
    () => (result?.key === key && result.revision === revision ? new Set(result.ids) : null),
    [key, revision, result],
  );
}
