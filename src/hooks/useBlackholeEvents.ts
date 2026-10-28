import { useState, useEffect } from 'react';
import { api } from '../api';
import type { BlackholeChangedEvent } from '../types/helpers';

/**
 * Reactive blackhole state hook.
 * Fetches initial blackhole status for the given file IDs,
 * then listens for blackhole-changed events to keep state in sync.
 *
 * Caller must memoize `allFileIds` to avoid infinite re-fetches.
 */
export function useBlackholeEvents(allFileIds: number[]) {
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());
  const [isLoaded, setIsLoaded] = useState(false);

  // Fetch initial blackhole status
  useEffect(() => {
    if (allFileIds.length === 0) {
      setIsLoaded(true);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const ids = await api.invoke<number[]>('get_blackholed_file_ids', { fileIds: allFileIds });
        if (!cancelled) {
          setBlackholedFileIds(new Set(ids));
          setIsLoaded(true);
        }
      } catch (err) {
        console.error('Failed to fetch blackholed IDs:', err);
        if (!cancelled) setIsLoaded(true);
      }
    })();
    return () => { cancelled = true; };
  }, [allFileIds]);

  // Listen for blackhole-changed events (StrictMode-safe listener pattern). Whole-zip actions carry no
  // file id: they are not file-level changes, so this hook ignores them.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<BlackholeChangedEvent>('blackhole-changed', (payload) => {
      if (cancelled || !('file_id' in payload)) return;
      const fileId = payload.file_id;
      if (payload.action === 'blackholed') {
        setBlackholedFileIds(prev => {
          const next = new Set(prev);
          next.add(fileId);
          return next;
        });
      } else {
        setBlackholedFileIds(prev => {
          const next = new Set(prev);
          next.delete(fileId);
          return next;
        });
      }
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useBlackholeEvents] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  return { blackholedFileIds, isLoaded };
}
