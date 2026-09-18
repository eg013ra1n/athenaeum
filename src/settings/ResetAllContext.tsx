// Settings redesign (spec 2026-09-18 §6, plan Task E1 Step 3): a KV section
// (no `onResetAll` of its own — that prop is reserved for a typed-config
// section's `reset_*` command) still needs a "Reset all" that resets every
// field it contains back to its own default. The section itself holds no
// field state — each field's committed value and default live inside its
// own `useSettingField` call — so `SettingsSection` provides this registry
// and every `useSettingField` rendered underneath registers its own reset
// on mount, unregistering on unmount. `SettingsSection` shows "Reset all"
// when EITHER it was given an explicit `onResetAll` (typed doc) OR at least
// one field has registered here (spec: "a section without `onResetAll`
// shows Reset all iff at least one field registered").
//
// A field with no real default (`useSettingField`'s `parsedDefault` is an
// `Error` — e.g. the two legacy `analysis.rejection_defaults`/
// `fwhm_default_unit` keys) never registers — see `useRegisterFieldReset`'s
// `enabled` parameter — so it can never be silently "reset" to nothing.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';

export interface ResetAllRegistry {
  /** Registers `reset` under `key` for as long as the caller stays mounted;
   *  the returned function unregisters it. Re-registering the same `key`
   *  (a field's settingKey is stable) simply replaces the callback. */
  register(key: string, reset: () => Promise<void>): () => void;
  /** Resets every currently-registered field, in parallel. */
  resetAll(): Promise<void>;
}

const ResetAllContext = createContext<ResetAllRegistry | null>(null);

/** `SettingsSection`'s own hook: a fresh registry per section instance, plus
 *  the live count of registered fields (drives "show Reset all or not"). */
export function useResetAllRegistry(): { registry: ResetAllRegistry; registeredCount: number } {
  const mapRef = useRef(new Map<string, () => Promise<void>>());
  const [registeredCount, setRegisteredCount] = useState(0);

  const registry = useMemo<ResetAllRegistry>(
    () => ({
      register(key, reset) {
        mapRef.current.set(key, reset);
        setRegisteredCount(mapRef.current.size);
        return () => {
          mapRef.current.delete(key);
          setRegisteredCount(mapRef.current.size);
        };
      },
      async resetAll() {
        await Promise.all([...mapRef.current.values()].map((reset) => reset()));
      },
    }),
    [],
  );

  return { registry, registeredCount };
}

export function ResetAllProvider({ registry, children }: { registry: ResetAllRegistry; children: ReactNode }) {
  return <ResetAllContext.Provider value={registry}>{children}</ResetAllContext.Provider>;
}

/** A field's own side: registers `reset` with the nearest `SettingsSection`
 *  (a no-op outside one, e.g. a hand-written panel's `useSettingField` calls
 *  made above/beside its `SettingsSection`s rather than inside one — see
 *  `AnalysisSettingsPanel`). `enabled=false` (no real default) never
 *  registers. "Latest ref" for `reset` so a fresh closure every render
 *  doesn't re-register on every keystroke. */
export function useRegisterFieldReset(key: string, reset: () => Promise<void>, enabled: boolean): void {
  const registry = useContext(ResetAllContext);
  const resetRef = useRef(reset);
  resetRef.current = reset;

  const stableReset = useCallback(() => resetRef.current(), []);

  useEffect(() => {
    if (!registry || !enabled) return;
    return registry.register(key, stableReset);
  }, [registry, enabled, key, stableReset]);
}
