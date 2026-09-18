// Settings redesign (spec 2026-09-18 §5): the typed-config half of autosave
// — one document, saved whole, debounced. This is `StackingSection.tsx`'s
// own load/dirty/debounce/unmount-flush discipline
// (`src/components/settings/StackingSection.tsx:113-204`) generalized over
// `opts.load`/`opts.save`/`opts.resetAll`/`opts.defaults`, for Analysis,
// Plate Solving, Calibration Matching and Logging to adopt in later tasks.
// See `useSettingField` for the one-KV-key half.
//
// Discipline preserved verbatim: `dirtyRef` is set ONLY by `patch`/
// `resetField`, never by the mount-load effect nor by a successful
// `resetAll` reload, so opening the page (or resetting) can never
// materialize a write that wasn't already dirty. A patch during an
// in-flight debounce window replaces the pending document — the last one
// wins. A write failure keeps `doc` as the user left it, exposes `error`,
// and raises one `notify()` warning; the same failure on the unmount flush
// is silent (no toast for a page the user has already left).

import { useCallback, useEffect, useRef, useState } from 'react';
import { useNotifications, type NotifyLike } from '../contexts/NotificationContext';

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Reads a dotted path (`'a.b.c'`) out of a plain object/array tree. Returns
 *  `undefined` for any missing segment — no lodash. */
function getPath(obj: unknown, path: string): unknown {
  return path.split('.').reduce<unknown>((acc, segment) => {
    if (acc === null || typeof acc !== 'object') return undefined;
    return (acc as Record<string, unknown>)[segment];
  }, obj);
}

/** Returns a new tree with `value` set at a dotted path, shallow-cloning
 *  only the objects/arrays on the path (everything else is shared with the
 *  original) — no lodash. */
function setPath<D>(obj: D, path: string, value: unknown): D {
  const segments = path.split('.');
  const cloneLevel = (level: unknown): any =>
    Array.isArray(level) ? [...level] : { ...(level as Record<string, unknown>) };

  const root: any = cloneLevel(obj);
  let cursor = root;
  for (let i = 0; i < segments.length - 1; i++) {
    const segment = segments[i];
    const next = cursor[segment];
    cursor[segment] = next !== null && typeof next === 'object' ? cloneLevel(next) : {};
    cursor = cursor[segment];
  }
  cursor[segments[segments.length - 1]] = value;
  return root as D;
}

const DEFAULT_DEBOUNCE_MS = 500;

export interface UseAutosaveDocumentOptions<D> {
  load(): Promise<D>;
  save(doc: D): Promise<void>;
  /** Calls the typed config's `reset_*` command. `resetAll()` on the
   *  returned object is a no-op (logged) when this is omitted. */
  resetAll?(): Promise<void>;
  /** From `useSettingsDefaults()` — `null` while defaults haven't loaded
   *  (or failed to). `isDefault`/`resetField` degrade to false/no-op. */
  defaults: D | null;
  /** 500 by default — the `StackingSection` value. */
  debounceMs?: number;
  /** Human-readable document name for the failure `notify()` title, e.g.
   *  "Analysis settings". Defaults to "Settings". */
  label?: string;
}

export interface UseAutosaveDocumentResult<D> {
  doc: D | null;
  patch(p: Partial<D> | ((d: D) => D)): void;
  error: string | null;
  saving: boolean;
  savedAt: number | null;
  isDefault(path: string): boolean;
  resetField(path: string): void;
  resetAll(): Promise<void>;
}

export function useAutosaveDocument<D>(opts: UseAutosaveDocumentOptions<D>): UseAutosaveDocumentResult<D> {
  const { notify } = useNotifications();
  const debounceMs = opts.debounceMs ?? DEFAULT_DEBOUNCE_MS;

  const [doc, setDoc] = useState<D | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);

  // Same "load must never write" discipline as `StackingTab.tsx`/
  // `StackingSection.tsx`: `dirtyRef` is set ONLY by `patch` (and
  // `resetField`, which patches), never by the mount-load effect or by a
  // successful `resetAll`'s reload.
  const dirtyRef = useRef(false);
  const pendingRef = useRef<D | null>(null);

  // "Latest ref" pattern: `opts.load`/`save`/`resetAll`/`defaults`/`label`
  // are typically fresh closures/values on every render (the caller is not
  // required to memoize `opts`) — reading them through refs keeps the
  // mount effect, the debounce effect and the unmount flush stable, so a
  // parent re-render never re-fetches or reschedules a pending write.
  const loadRef = useRef(opts.load);
  loadRef.current = opts.load;
  const saveRef = useRef(opts.save);
  saveRef.current = opts.save;
  const resetAllRef = useRef(opts.resetAll);
  resetAllRef.current = opts.resetAll;
  const defaultsRef = useRef(opts.defaults);
  defaultsRef.current = opts.defaults;
  const notifyRef = useRef<NotifyLike>(notify);
  notifyRef.current = notify;
  const labelRef = useRef(opts.label);
  labelRef.current = opts.label;

  // Mount load — StrictMode-safe cancelled-flag pattern; never writes.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const next = await loadRef.current();
        if (cancelled) return;
        setDoc(next);
        setError(null);
      } catch (err) {
        console.error('[useAutosaveDocument] load failed:', err);
        if (!cancelled) setError(errMsg(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const sendPending = useCallback(async (notifyOnFailure: boolean) => {
    const payload = pendingRef.current;
    if (!payload) return;
    dirtyRef.current = false;
    pendingRef.current = null;
    setSaving(true);
    try {
      await saveRef.current(payload);
      setSaving(false);
      setError(null);
      setSavedAt(Date.now());
    } catch (err) {
      setSaving(false);
      const msg = errMsg(err);
      console.error('[useAutosaveDocument] save failed:', err);
      setError(msg);
      if (notifyOnFailure) {
        notifyRef.current({
          kind: 'generic',
          tone: 'warning',
          title: `${labelRef.current ?? 'Settings'} not saved`,
          detail: msg,
        });
      }
    }
  }, []);

  // Persist (debounced), gated on `dirtyRef` — a load, and a successful
  // `resetAll` (which clears `dirtyRef` itself), never trigger a write.
  useEffect(() => {
    if (doc === null) return;
    if (!dirtyRef.current) return;
    pendingRef.current = doc;
    const t = setTimeout(() => {
      void sendPending(true);
    }, debounceMs);
    return () => clearTimeout(t);
  }, [doc, debounceMs, sendPending]);

  // Flush a still-pending write on unmount (e.g. the user switches Settings
  // tabs inside the debounce window) instead of silently dropping it — no
  // toast for a page the user has already left.
  useEffect(() => {
    return () => {
      if (dirtyRef.current && pendingRef.current) {
        void sendPending(false);
      }
    };
  }, [sendPending]);

  const patch = useCallback((p: Partial<D> | ((d: D) => D)) => {
    setDoc((prev) => {
      if (prev === null) {
        console.error('[useAutosaveDocument] patch called before the document loaded — dropping');
        return prev;
      }
      const next = typeof p === 'function' ? (p as (d: D) => D)(prev) : { ...prev, ...p };
      dirtyRef.current = true;
      return next;
    });
  }, []);

  const isDefault = useCallback(
    (path: string): boolean => {
      const defaults = defaultsRef.current;
      if (doc === null || defaults === null) return false;
      return JSON.stringify(getPath(doc, path)) === JSON.stringify(getPath(defaults, path));
    },
    [doc],
  );

  const resetField = useCallback(
    (path: string) => {
      const defaults = defaultsRef.current;
      if (defaults === null) {
        console.error('[useAutosaveDocument] resetField called with no defaults loaded — dropping');
        return;
      }
      const defaultValue = getPath(defaults, path);
      patch((prev) => setPath(prev, path, defaultValue));
    },
    [patch],
  );

  const resetAll = useCallback(async (): Promise<void> => {
    const doReset = resetAllRef.current;
    if (!doReset) {
      console.error('[useAutosaveDocument] resetAll called with no opts.resetAll provided — dropping');
      return;
    }
    try {
      await doReset();
      // The server's own canonical state, not a user edit — clear any
      // pending write so the debounce effect above does not immediately
      // re-send what was just reset, then reload to pick it up.
      dirtyRef.current = false;
      pendingRef.current = null;
      const next = await loadRef.current();
      setDoc(next);
      setError(null);
    } catch (err) {
      console.error('[useAutosaveDocument] resetAll failed:', err);
      const msg = errMsg(err);
      setError(msg);
      notifyRef.current({
        kind: 'generic',
        tone: 'warning',
        title: `${labelRef.current ?? 'Settings'} not reset`,
        detail: msg,
      });
    }
  }, []);

  return { doc, patch, error, saving, savedAt, isDefault, resetField, resetAll };
}
