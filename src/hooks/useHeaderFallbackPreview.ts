// Settings → General → FITS keywords: the "N frames read differently" preview (plan D4).
//
// One `preview_header_fallbacks` call when the document has loaded, one 800 ms after each successful save
// (`savedAt`), one on `refresh()` (Apply finished, Reset all). At most one call is in flight; a trigger during it
// queues exactly one follow-up, and the answer of a call that a later trigger has outdated is dropped. The preview
// only reads, so a stale one costs time, not data: there is no server-side cancel.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';
import type { HeaderFallbackCounts } from '../types/models';

export const PREVIEW_DEBOUNCE_MS = 800;

export interface UseHeaderFallbackPreviewResult {
  counts: HeaderFallbackCounts | null;
  error: string | null;
  /** Ask for a fresh preview now (coalesced with one already running). */
  refresh(): void;
}

export function useHeaderFallbackPreview(opts: {
  enabled: boolean;
  savedAt: number | null;
}): UseHeaderFallbackPreviewResult {
  const { enabled, savedAt } = opts;
  const { notify } = useNotifications();
  const notifyRef = useRef<NotifyLike>(notify);
  notifyRef.current = notify;

  const [counts, setCounts] = useState<HeaderFallbackCounts | null>(null);
  const [error, setError] = useState<string | null>(null);

  const aliveRef = useRef(true);
  const inFlightRef = useRef(false);
  const queuedRef = useRef(false);
  // Bumped by every trigger; a call answers only if no trigger came after it started.
  const wantedRef = useRef(0);

  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  const run = useCallback(async () => {
    if (inFlightRef.current) {
      queuedRef.current = true;
      return;
    }
    inFlightRef.current = true;
    try {
      do {
        queuedRef.current = false;
        const generation = wantedRef.current;
        try {
          const next = await api.invoke<HeaderFallbackCounts>('preview_header_fallbacks');
          if (aliveRef.current && generation === wantedRef.current) {
            setCounts(next);
            setError(null);
          }
        } catch (err) {
          console.error('[useHeaderFallbackPreview] preview failed:', err);
          if (aliveRef.current && generation === wantedRef.current) {
            const detail = err instanceof Error ? err.message : String(err);
            setError(detail);
            notifyRef.current({
              kind: 'generic',
              tone: 'warning',
              title: 'FITS keyword preview failed',
              detail,
              dedupeKey: 'header-fallbacks-preview',
            });
          }
        }
      } while (queuedRef.current && aliveRef.current);
    } finally {
      inFlightRef.current = false;
    }
  }, []);

  const refresh = useCallback(() => {
    wantedRef.current += 1;
    void run();
  }, [run]);

  // First call, when the document has loaded. The document never unloads, so this runs once.
  useEffect(() => {
    if (!enabled) return;
    refresh();
  }, [enabled, refresh]);

  // After a successful save: wait for the burst to end, then ask once.
  useEffect(() => {
    if (!enabled || savedAt === null) return;
    const t = setTimeout(refresh, PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [enabled, savedAt, refresh]);

  return { counts, error, refresh };
}
