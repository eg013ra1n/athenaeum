import { useState, useEffect, useMemo, useRef } from 'react';
import { api } from '../api';
import type { BlackholeChangedEvent } from '../types/helpers';

/**
 * Reactive blackhole state hook.
 * Fetches initial blackhole status for the given file IDs, then listens for blackhole-changed events to
 * keep state in sync. `blackholedFileIds` is the union of two kinds: files moved to the Black Hole one by
 * one, and files of a zip that is itself in the Black Hole (its frames are Black Hole like any other).
 *
 * Caller must memoize `allFileIds` to avoid infinite re-fetches.
 */
export function useBlackholeEvents(allFileIds: number[]) {
  const [fileLevel, setFileLevel] = useState<Set<number>>(new Set());
  const [zipLevel, setZipLevel] = useState<Set<number>>(new Set());
  const [isLoaded, setIsLoaded] = useState(false);
  const idsRef = useRef(allFileIds);
  idsRef.current = allFileIds;

  // Fetch initial blackhole status
  useEffect(() => {
    if (allFileIds.length === 0) {
      setFileLevel(new Set());
      setZipLevel(new Set());
      setIsLoaded(true);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const [ids, zipIds] = await Promise.all([
          api.invoke<number[]>('get_blackholed_file_ids', { fileIds: allFileIds }),
          api.invoke<number[]>('get_zip_blackholed_file_ids', { fileIds: allFileIds }),
        ]);
        if (!cancelled) {
          setFileLevel(new Set(ids));
          setZipLevel(new Set(zipIds));
          setIsLoaded(true);
        }
      } catch (err) {
        console.error('Failed to fetch blackholed IDs:', err);
        if (!cancelled) setIsLoaded(true);
      }
    })();
    return () => { cancelled = true; };
  }, [allFileIds]);

  // Listen for blackhole-changed events (StrictMode-safe listener pattern). A whole-zip action carries no
  // file id: it changes which of the page's files sit in a Black Hole zip, so the zip-level set is fetched
  // again.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<BlackholeChangedEvent>('blackhole-changed', (payload) => {
      if (cancelled) return;
      if (!('file_id' in payload)) {
        const ids = idsRef.current;
        if (ids.length === 0) return;
        api.invoke<number[]>('get_zip_blackholed_file_ids', { fileIds: ids })
          .then((zipIds) => { if (!cancelled) setZipLevel(new Set(zipIds)); })
          .catch((err) => console.error('[useBlackholeEvents] zip refresh failed:', err));
        return;
      }
      const fileId = payload.file_id;
      setFileLevel((prev) => {
        const next = new Set(prev);
        if (payload.action === 'blackholed') next.add(fileId);
        else next.delete(fileId);
        return next;
      });
    })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[useBlackholeEvents] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  const blackholedFileIds = useMemo(() => {
    if (zipLevel.size === 0) return fileLevel;
    return new Set([...fileLevel, ...zipLevel]);
  }, [fileLevel, zipLevel]);

  return { blackholedFileIds, isLoaded };
}
