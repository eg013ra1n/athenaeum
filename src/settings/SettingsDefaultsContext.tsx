// Settings redesign (spec 2026-09-18 §6/§9): the one place the Settings page
// loads `get_settings_defaults` from. Mounted once by `Settings.tsx`
// (Task C1) around the whole page — every field/section compares its current
// value against `defaults` to decide whether to show the reset affordance.
//
// On failure (§9): the page still renders and edits normally; reset
// affordances are hidden (`defaults` stays `null`) and callers show a
// one-line note under the search field. This file only logs and stores the
// error — it never blocks rendering.

import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import { api } from '../api';
import type { AnalysisConfig } from '../types/analysis-config';
import type { PlateSolveConfig } from '../types/plate-solve';
import type { CalibrationMatchingConfig } from '../types/calibration-config';
import type { LoggingConfig } from '../types/models';
import type { StackingConfig } from '../types/stacking';

/**
 * Mirrors `athenaeum_core::api::settings::SettingsDefaults` (Task A1, this
 * plan). Declared locally because that Rust struct and its generated TS type
 * may not exist in `src/types/models.ts` yet when this file lands — swap this
 * for the generated `SettingsDefaults` import from `models.ts` once Task A1's
 * `ts_export` run has landed it there. Field names are already the Rust
 * struct's `#[serde(rename_all = "camelCase")]` names.
 */
export interface SettingsDefaults {
  kv: Record<string, string>;
  analysis: AnalysisConfig;
  plateSolve: PlateSolveConfig;
  calibrationMatching: CalibrationMatchingConfig;
  logging: LoggingConfig;
  stacking: StackingConfig;
}

interface SettingsDefaultsContextValue {
  defaults: SettingsDefaults | null;
  error: string | null;
}

const SettingsDefaultsContext = createContext<SettingsDefaultsContextValue | undefined>(undefined);

export function SettingsDefaultsProvider({ children }: { children: ReactNode }) {
  const [defaults, setDefaults] = useState<SettingsDefaults | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .invoke<SettingsDefaults>('get_settings_defaults')
      .then((result) => {
        if (cancelled) return;
        setDefaults(result);
        setError(null);
      })
      .catch((err) => {
        console.error('[Settings] get_settings_defaults failed:', err);
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <SettingsDefaultsContext.Provider value={{ defaults, error }}>
      {children}
    </SettingsDefaultsContext.Provider>
  );
}

/** Throws outside `SettingsDefaultsProvider` — every consumer lives under the
 *  Settings page, which always mounts the provider. */
export function useSettingsDefaults(): SettingsDefaultsContextValue {
  const ctx = useContext(SettingsDefaultsContext);
  if (!ctx) {
    throw new Error('useSettingsDefaults must be used within a SettingsDefaultsProvider');
  }
  return ctx;
}
