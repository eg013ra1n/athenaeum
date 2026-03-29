import { useState, useEffect } from 'react';
import { api } from '../api';
import type { BlackholeChangedEvent } from '../types/models';

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

  // Listen for blackhole-changed events
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    (async () => {
      unlisten = await api.listen<BlackholeChangedEvent>('blackhole-changed', (payload) => {
        if (payload.action === 'blackholed') {
          setBlackholedFileIds(prev => {
            const next = new Set(prev);
            next.add(payload.file_id);
            return next;
          });
        } else {
          setBlackholedFileIds(prev => {
            const next = new Set(prev);
            next.delete(payload.file_id);
            return next;
          });
        }
      });
    })();
    return () => { unlisten?.(); };
  }, []);

  return { blackholedFileIds, isLoaded };
}
