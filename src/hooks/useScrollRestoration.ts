import { useEffect, useRef, RefObject } from 'react';
import { useLocation, useNavigationType } from 'react-router-dom';

/** Retries when restoring a position the freshly-mounted page can't hold yet. */
const RESTORE_ATTEMPTS = 8;
const RESTORE_INTERVAL_MS = 50;

/**
 * Remembers the scroll offset of the app's main content container per history
 * entry, restores it when the user steps back, and scrolls to the top on a new
 * navigation — the behaviour a browser gives a multi-page site for free.
 *
 * Keyed on `location.key`, so it is per history entry rather than per path: two
 * visits to the same page in one session keep their own offsets. Positions live
 * in a ref for the life of the app session and are never persisted.
 *
 * The declarative router (`BrowserRouter` + `Routes`) has no `ScrollRestoration`
 * component — that ships only with the data routers — hence this hook.
 *
 * Scoped to the one container Layout scrolls. Pages that scroll inside their own
 * panes (the dual-pane browser, most tables) are unaffected.
 */
export function useScrollRestoration(ref: RefObject<HTMLElement | null>) {
  const location = useLocation();
  const navigationType = useNavigationType();
  const positions = useRef(new Map<string, number>());
  const currentKey = useRef(location.key);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Saved on every scroll event, not on unmount: by the time a navigation's
    // effects run, the container already holds the next page and the offset we
    // wanted is gone.
    const onScroll = () => positions.current.set(currentKey.current, el.scrollTop);
    el.addEventListener('scroll', onScroll, { passive: true });
    return () => el.removeEventListener('scroll', onScroll);
  }, [ref]);

  useEffect(() => {
    currentKey.current = location.key;
    const el = ref.current;
    if (!el) return;

    // REPLACE is left alone on purpose: pages replace the URL to consume their
    // own deep-link params (FrameSetDetail, FileManager), and yanking the user
    // to the top when they do would be a change nobody asked for.
    if (navigationType === 'REPLACE') return;

    const saved = navigationType === 'POP' ? (positions.current.get(location.key) ?? 0) : 0;
    if (saved === 0) {
      el.scrollTop = 0;
      return;
    }

    // The page may still be fetching, leaving the container too short to hold
    // the offset. Re-apply until it sticks, then stop.
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    const apply = () => {
      el.scrollTop = saved;
      attempt += 1;
      if (Math.abs(el.scrollTop - saved) > 1 && attempt < RESTORE_ATTEMPTS) {
        timer = setTimeout(apply, RESTORE_INTERVAL_MS);
      }
    };
    apply();
    return () => {
      if (timer) clearTimeout(timer);
    };
  }, [location.key, navigationType, ref]);
}
