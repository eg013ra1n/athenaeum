// App-root live collab exchange state (wave 2, Task 8).
//
// One provider, mounted once in `Layout.tsx` (inside `TransfersProvider`, next
// to it in spirit — same reason: the sidebar indicator, the Transfers page,
// the Transfers panel and the project Exchange tab must all read the SAME
// poller/listener pair, never one each). Owns:
//   - the initial `get_collab_exchange` snapshot (global, `projectId: null`),
//   - the `collab-exchange-progress` event stream (device ids only — an
//     unnamed device triggers at most one in-flight global refetch, never one
//     per event),
//   - the `collab-live-status` stream: the runtime sends no quiet payload
//     when it stops, so flows are cleared on any transition away from
//     `'live'`.
// The reducer itself is pure and lives in
// `components/collab/exchange/state.ts` — this file is only wiring.

import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { api } from '../api';
import type { CollabExchangeProgress, CollabLiveStatus, ExchangeSnapshot } from '../types/models';
import {
  applyProgress,
  applySnapshot,
  clearFlows,
  EMPTY_EXCHANGE,
  unknownDevices,
  type ExchangeState,
} from '../components/collab/exchange/state';

interface CollabExchangeContextValue {
  state: ExchangeState;
  /** Refetch one project's flows (e.g. the project Exchange tab on open). */
  refreshProject: (projectId: string) => Promise<void>;
}

const CollabExchangeContext = createContext<CollabExchangeContextValue | null>(null);

export function CollabExchangeProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<ExchangeState>(EMPTY_EXCHANGE);
  // Guards the "unknown device" refetch: at most one in flight, regardless of
  // how many progress events name unresolved devices while it is pending.
  const refetchingRef = useRef(false);

  // Initial snapshot — the global, no-arg answer.
  useEffect(() => {
    let cancelled = false;
    api
      .invoke<ExchangeSnapshot>('get_collab_exchange', { projectId: null })
      .then((snap) => {
        if (cancelled) return;
        setState((prev) => applySnapshot(prev, snap, null));
      })
      .catch((err) => console.error('[collab-exchange] initial snapshot failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabExchangeProgress>('collab-exchange-progress', (ev) => {
        if (cancelled) return;
        setState((prev) => {
          const next = applyProgress(prev, ev);
          if (!refetchingRef.current && unknownDevices(next).length > 0) {
            refetchingRef.current = true;
            api
              .invoke<ExchangeSnapshot>('get_collab_exchange', { projectId: null })
              .then((snap) => {
                if (cancelled) return;
                setState((p) => applySnapshot(p, snap, null));
              })
              .catch((err) => console.error('[collab-exchange] name refetch failed:', err))
              .finally(() => {
                refetchingRef.current = false;
              });
          }
          return next;
        });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab-exchange] progress listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // The runtime emits no quiet payload on stop — clear on any non-'live' state.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabLiveStatus>('collab-live-status', (ev) => {
        if (cancelled) return;
        if (ev.state !== 'live') setState((prev) => clearFlows(prev));
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab-exchange] live-status listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const refreshProject = useCallback(async (projectId: string) => {
    try {
      const snap = await api.invoke<ExchangeSnapshot>('get_collab_exchange', { projectId });
      setState((prev) => applySnapshot(prev, snap, projectId));
    } catch (err) {
      console.error('[collab-exchange] project refresh failed:', err);
    }
  }, []);

  return (
    <CollabExchangeContext.Provider value={{ state, refreshProject }}>
      {children}
    </CollabExchangeContext.Provider>
  );
}

export function useCollabExchange(): CollabExchangeContextValue {
  const ctx = useContext(CollabExchangeContext);
  if (!ctx) throw new Error('useCollabExchange must be used within CollabExchangeProvider');
  return ctx;
}
