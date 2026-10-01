import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { CollabLiveStatus, LiveState } from '../types/models';

/**
 * The live exchange's state, `null` until known: the initial
 * `get_collab_live_status` read, then every `collab-live-status` event. An
 * event that lands before the read resolves is newer and wins (the same rule
 * `CollabLiveStatus` keeps).
 */
export function useCollabLiveState(): LiveState | null {
  const [state, setState] = useState<LiveState | null>(null);
  const gotEvent = useRef(false);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<CollabLiveStatus | null>('get_collab_live_status')
      .then((s) => {
        if (!cancelled && !gotEvent.current && s) setState(s.state);
      })
      .catch((err) => console.error('[collab] get_collab_live_status failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabLiveStatus>('collab-live-status', (s) => {
        if (cancelled) return;
        gotEvent.current = true;
        setState(s.state);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] live-status listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return state;
}
