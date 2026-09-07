import { createContext, useCallback, useContext, useRef, useState, ReactNode } from 'react';

/**
 * Per-session memory for page-local UI state.
 *
 * Pages are unmounted when you navigate away, so plain `useState` forgets which
 * tab you were on the moment you leave. This context holds a Map that outlives
 * those unmounts, so returning to a page — via the sidebar, the Back button or
 * Backspace — puts you back where you were.
 *
 * Session-scoped on purpose: the Map lives in the app root and dies with the
 * window, so a restart (and a browser reload in the web build) starts clean.
 * Nothing is written to localStorage. State that should survive a restart, or
 * that belongs in a shareable link, does NOT belong here — use localStorage or
 * a search param, as `DualPaneFileBrowser` and `Settings` respectively do.
 */
const SessionStateContext = createContext<Map<string, unknown> | null>(null);

export function SessionStateProvider({ children }: { children: ReactNode }) {
  const store = useRef(new Map<string, unknown>());
  return (
    <SessionStateContext.Provider value={store.current}>{children}</SessionStateContext.Provider>
  );
}

/**
 * `useState`, but the value is remembered across unmounts for the life of the
 * app session under `key`.
 *
 * Keys are global — prefix them with the page ("objects.tab") so two pages
 * can't collide. The initial value is only consulted the first time a key is
 * seen in the session.
 */
export function useSessionState<T>(
  key: string,
  initial: T | (() => T),
): [T, (value: T | ((prev: T) => T)) => void] {
  const store = useContext(SessionStateContext);
  if (!store) {
    throw new Error('useSessionState must be used within SessionStateProvider');
  }

  const [value, setValue] = useState<T>(() => {
    if (store.has(key)) return store.get(key) as T;
    return typeof initial === 'function' ? (initial as () => T)() : initial;
  });

  const set = useCallback(
    (next: T | ((prev: T) => T)) => {
      setValue((prev) => {
        const resolved = typeof next === 'function' ? (next as (p: T) => T)(prev) : next;
        store.set(key, resolved);
        return resolved;
      });
    },
    [key, store],
  );

  return [value, set];
}
