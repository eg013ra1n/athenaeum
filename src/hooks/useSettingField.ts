// Settings redesign (spec 2026-09-18 §5): the KV half of autosave — one
// setting key, read once, written through either the default `set_setting`
// command or a caller-supplied `write` override (for a key with its own
// command, e.g. `set_blink_threads`, `set_integration_band_budget`). See
// `useAutosaveDocument` for the typed-config half (one document, saved
// whole).
//
// Autosave rule (spec §5): a discrete control (checkbox/select/slider)
// commits through `setValue`, debounced 300 ms so a slider drag is one
// write. A text/number control edits `draft` freely and commits on
// blur/Enter via `commit()`; `escape()` restores the last committed value
// and clears the error. While the draft is invalid nothing is written. A
// failed write keeps the draft, shows `error` inline, and raises exactly
// one `notify()` warning — never a page-level banner (spec §9).

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { useSettingsDefaults } from '../settings/SettingsDefaultsContext';
import { fieldMeta, type SettingsFieldMeta } from '../settings/registry';
import type { Codec } from '../settings/codecs';

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

const SET_VALUE_DEBOUNCE_MS = 300;

export interface UseSettingFieldOptions<T> {
  /** Replaces the default `set_setting` write — for a key with its own
   *  command (e.g. `set_blink_threads`, `set_integration_band_budget`). */
  write?: (value: T) => Promise<void>;
  /** Replaces the default `get_setting` read. */
  read?: () => Promise<string>;
}

export interface UseSettingFieldResult<T> {
  /** The committed value — the context default (parsed) before the mount
   *  read resolves, so a caller never sees `undefined` in practice. */
  value: T;
  /** The text-field edit buffer. Independent of `value` while a draft is
   *  uncommitted or invalid. */
  draft: string;
  setDraft(raw: string): void;
  /** Commits `draft` (text/number fields): parses, validates, writes on
   *  success; on failure sets `error` and writes nothing. */
  commit(): Promise<void>;
  /** Commits `value` directly, debounced 300 ms (discrete controls). */
  setValue(value: T): Promise<void>;
  /** Restores `draft` to the last committed value and clears `error`. */
  escape(): void;
  error: string | null;
  saving: boolean;
  savedAt: number | null;
  defaultValue: T;
  isDefault: boolean;
  /** Writes `defaultValue` through the same commit path as any other
   *  change — a reset is a normal change, not a special command. */
  reset(): Promise<void>;
  meta: SettingsFieldMeta;
  label: string;
  help?: string;
}

export function useSettingField<T>(
  section: string,
  field: string,
  key: string,
  codec: Codec<T>,
  opts?: UseSettingFieldOptions<T>,
): UseSettingFieldResult<T> {
  const { notify } = useNotifications();
  const { defaults, error: defaultsError } = useSettingsDefaults();
  const meta = fieldMeta(section, field);

  const defaultRaw = defaults?.kv[key];
  const parsedDefault = defaultRaw !== undefined ? codec.parse(defaultRaw) : new Error(`no default for "${key}"`);

  const [committed, setCommitted] = useState<T | null>(null);
  const [draft, setDraftState] = useState<string>(defaultRaw ?? '');
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);

  // "Latest ref" pattern: a caller typically passes a fresh inline
  // `read`/`write` closure — and, just as often, a fresh `codec` object
  // (e.g. `intCodec(1, 32)` instantiated inline in JSX) — on every render.
  // Reading all three through refs keeps the callbacks/effects below stable
  // across such renders, so a parent re-render never re-fetches on mount or
  // reschedules a pending debounce.
  const readRef = useRef(opts?.read);
  readRef.current = opts?.read;
  const writeRef = useRef(opts?.write);
  writeRef.current = opts?.write;
  const codecRef = useRef(codec);
  codecRef.current = codec;

  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingValueRef = useRef<T | null>(null);

  // The default-path mount read needs `defaultRaw` from
  // `useSettingsDefaults()`, which resolves asynchronously and independently
  // of this hook — without gating, a field mounted before the context
  // settles would race it and call `get_setting` with `defaultValue: ''`
  // even though a real default exists, and (since `get_setting` never
  // persists its fallback) show the wrong value until the next full
  // remount. `settingsSettled` waits for the context to either resolve or
  // fail before firing that call.
  const settingsSettled = defaults !== null || defaultsError !== null;

  const applyInitial = useCallback(
    (raw: string) => {
      const parsed = codecRef.current.parse(raw);
      if (parsed instanceof Error) {
        console.error(`[useSettingField] ${key}: stored value "${raw}" failed to parse: ${parsed.message}`);
        return;
      }
      setCommitted(parsed);
      setDraftState(codecRef.current.format(parsed));
    },
    [key],
  );

  // Mount read, caller-supplied `read` override — StrictMode-safe
  // cancelled-flag pattern; never writes. Independent of `settingsSettled`:
  // a custom read doesn't depend on this hook's default plumbing at all.
  useEffect(() => {
    if (!readRef.current) return;
    let cancelled = false;
    readRef
      .current()
      .then((raw) => {
        if (!cancelled) applyInitial(raw);
      })
      .catch((err) => {
        console.error(`[useSettingField] ${key} read failed:`, err);
      });
    return () => {
      cancelled = true;
    };
    // See the "Latest ref" comment above for why `read` itself isn't a dep.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, applyInitial]);

  // Mount read, default `get_setting` path — same cancelled-flag pattern,
  // gated on `settingsSettled` (see above). `settingsSettled` only ever
  // goes false → true once the context provider's own single fetch settles,
  // so this fires its real work exactly once per key.
  useEffect(() => {
    if (readRef.current) return; // handled by the effect above
    if (!settingsSettled) return;
    let cancelled = false;
    api
      .invoke<string>('get_setting', { key, defaultValue: defaultRaw ?? '' })
      .then((raw) => {
        if (!cancelled) applyInitial(raw);
      })
      .catch((err) => {
        console.error(`[useSettingField] ${key} read failed:`, err);
      });
    return () => {
      cancelled = true;
    };
    // `defaultRaw` is read fresh via closure each run — it can only change
    // alongside `settingsSettled` (both come from the same context value),
    // already listed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, settingsSettled, applyInitial]);

  const writeValue = useCallback(
    async (v: T): Promise<void> => {
      setSaving(true);
      try {
        if (writeRef.current) {
          await writeRef.current(v);
        } else {
          await api.invoke('set_setting', { key, value: codecRef.current.format(v) });
        }
        setSaving(false);
        setError(null);
        setSavedAt(Date.now());
      } catch (err) {
        setSaving(false);
        const msg = errMsg(err);
        console.error(`[useSettingField] ${key} write failed:`, err);
        setError(msg);
        notify({
          kind: 'generic',
          tone: 'warning',
          title: 'Setting not saved',
          detail: `${meta.label}: ${msg}`,
          dedupeKey: key,
        });
      }
    },
    [key, notify, meta.label],
  );

  const setValue = useCallback(
    (v: T): Promise<void> => {
      // Optimistic: the control reflects `v` immediately, the actual write
      // is what's debounced.
      setCommitted(v);
      setDraftState(codecRef.current.format(v));
      setError(null);
      pendingValueRef.current = v;
      if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current);
      debounceTimerRef.current = setTimeout(() => {
        const pending = pendingValueRef.current;
        pendingValueRef.current = null;
        debounceTimerRef.current = null;
        if (pending !== null) void writeValue(pending);
      }, SET_VALUE_DEBOUNCE_MS);
      return Promise.resolve();
    },
    [writeValue],
  );

  const setDraft = useCallback((raw: string) => {
    setDraftState(raw);
  }, []);

  const commit = useCallback(async (): Promise<void> => {
    if (debounceTimerRef.current) {
      clearTimeout(debounceTimerRef.current);
      debounceTimerRef.current = null;
      pendingValueRef.current = null;
    }
    const parsed = codecRef.current.parse(draft);
    if (parsed instanceof Error) {
      setError(parsed.message);
      return;
    }
    const validationError = codecRef.current.validate?.(parsed);
    if (validationError) {
      setError(validationError);
      return;
    }
    setCommitted(parsed);
    setError(null);
    await writeValue(parsed);
  }, [draft, writeValue]);

  const escape = useCallback(() => {
    const restored = committed !== null ? committed : parsedDefault instanceof Error ? null : parsedDefault;
    if (restored !== null) setDraftState(codecRef.current.format(restored));
    setError(null);
  }, [committed, parsedDefault]);

  const reset = useCallback(async (): Promise<void> => {
    if (parsedDefault instanceof Error) {
      console.error(`[useSettingField] ${key} has no default to reset to`);
      return;
    }
    if (debounceTimerRef.current) {
      clearTimeout(debounceTimerRef.current);
      debounceTimerRef.current = null;
      pendingValueRef.current = null;
    }
    setCommitted(parsedDefault);
    setDraftState(codecRef.current.format(parsedDefault));
    setError(null);
    await writeValue(parsedDefault);
  }, [parsedDefault, writeValue, key]);

  const value: T = committed !== null ? committed : parsedDefault instanceof Error ? (undefined as unknown as T) : parsedDefault;
  // When there is genuinely no default for this key (a misconfigured
  // registry entry — every real field is either a KV default or part of a
  // typed config), fall back to the current value so `defaultValue` stays
  // typed; `isDefault` below still correctly reports `false` in that case.
  const defaultValue: T = parsedDefault instanceof Error ? value : parsedDefault;
  const isDefault = committed !== null && !(parsedDefault instanceof Error) && committed === parsedDefault;

  return {
    value,
    draft,
    setDraft,
    commit,
    setValue,
    escape,
    error,
    saving,
    savedAt,
    defaultValue,
    isDefault,
    reset,
    meta,
    label: meta.label,
    help: meta.help,
  };
}
