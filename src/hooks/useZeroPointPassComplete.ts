import { useEffect, useRef } from 'react';
import { api } from '../api';
import type { ZeroPointPassCompleteEvent } from '../types/helpers';

/**
 * Calls `onComplete` when the pass after a plate-solve batch has finished measuring the solved frames'
 * transparency (`zero-point-pass-complete`). A solve batch clears those zero points and the pass writes them
 * back later, so a view that read its "N to update" count and its zero point cells when the solve ended
 * reads them again here. The listener is registered once (StrictMode-safe); the latest `onComplete` is called.
 */
export function useZeroPointPassComplete(onComplete: (event: ZeroPointPassCompleteEvent) => void): void {
  const latest = useRef(onComplete);
  latest.current = onComplete;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<ZeroPointPassCompleteEvent>('zero-point-pass-complete', (payload) => {
        if (cancelled) return;
        latest.current(payload);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[useZeroPointPassComplete] listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
