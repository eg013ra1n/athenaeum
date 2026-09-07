import { createContext, useCallback, useContext, useMemo, useRef, ReactNode } from 'react';
import { useLocation, useNavigate, useNavigationType } from 'react-router-dom';

/**
 * Session-scoped back/forward history for the app shell.
 *
 * The router already keeps a history stack — this context only answers "can we
 * step back/forward *inside this app session*", which the router does not
 * expose. It does so by tracking the location keys it has seen rather than by
 * reading `window.history.state.idx` (a router implementation detail):
 *
 *   - PUSH    → drop everything after the current entry, append the new key
 *   - REPLACE → swap the current key in place (forward entries survive, as in
 *               the browser's own replaceState)
 *   - POP     → the new key must already be in the list; its position is the
 *               new index. A key we've never seen means the user stepped
 *               outside the entries this session created (possible in the web
 *               build, where the tab may hold history from before the app) —
 *               we reset to a one-entry stack rather than guess.
 *
 * Two consequences that are the whole point:
 *   - Back is disabled on the session's first entry, so it can never navigate
 *     out of the app into whatever the browser tab visited before it.
 *   - Nothing persists: a reload starts a fresh stack, which is exactly the
 *     "current session only" behaviour this feature asked for.
 *
 * The bookkeeping runs during render and is idempotent (a render that finds the
 * key already at the current index does nothing), so React 18 StrictMode's
 * double render can't append an entry twice.
 */
interface NavHistoryValue {
  /** True when a session entry exists before the current one. */
  canBack: boolean;
  /** True when the user has stepped back and not pushed a new entry since. */
  canForward: boolean;
  /** Step back one entry. No-op when `canBack` is false. */
  back: () => void;
  /** Step forward one entry. No-op when `canForward` is false. */
  forward: () => void;
  /**
   * Step back if there is history, else navigate to `fallback`. For in-page
   * "back to X" buttons: pressing one after arriving from X should return to X
   * rather than push a third entry onto the stack.
   */
  backOr: (fallback: string) => void;
}

const NavHistoryContext = createContext<NavHistoryValue | null>(null);

export function NavHistoryProvider({ children }: { children: ReactNode }) {
  const location = useLocation();
  const navigationType = useNavigationType();
  const navigate = useNavigate();

  const keys = useRef<string[]>([location.key]);
  const index = useRef(0);

  // Reconcile the stack with the location we are rendering. Runs on every
  // navigation (useLocation re-renders us) and is a no-op when already in sync.
  const key = location.key;
  if (keys.current[index.current] !== key) {
    if (navigationType === 'POP') {
      const found = keys.current.indexOf(key);
      if (found >= 0) {
        index.current = found;
      } else {
        keys.current = [key];
        index.current = 0;
      }
    } else if (navigationType === 'REPLACE') {
      keys.current[index.current] = key;
    } else {
      keys.current = [...keys.current.slice(0, index.current + 1), key];
      index.current = keys.current.length - 1;
    }
  }

  const canBack = index.current > 0;
  const canForward = index.current < keys.current.length - 1;

  const back = useCallback(() => {
    if (index.current > 0) navigate(-1);
  }, [navigate]);

  const forward = useCallback(() => {
    if (index.current < keys.current.length - 1) navigate(1);
  }, [navigate]);

  const backOr = useCallback(
    (fallback: string) => {
      if (index.current > 0) navigate(-1);
      else navigate(fallback);
    },
    [navigate],
  );

  const value = useMemo<NavHistoryValue>(
    () => ({ canBack, canForward, back, forward, backOr }),
    [canBack, canForward, back, forward, backOr],
  );

  return <NavHistoryContext.Provider value={value}>{children}</NavHistoryContext.Provider>;
}

export function useNavHistory() {
  const context = useContext(NavHistoryContext);
  if (!context) {
    throw new Error('useNavHistory must be used within NavHistoryProvider');
  }
  return context;
}
