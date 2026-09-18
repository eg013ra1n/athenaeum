// Settings redesign (spec 2026-09-18 §4/§8): the logging level picker,
// extracted from `LoggingSettings.tsx` so the base-level select and each of
// the five module-row selects render through one component instead of five
// copies of the same `<select>` markup. `trace` is intentionally never an
// option — CLAUDE.md "Logging": it is env-only (`ATHENAEUM_LOG`) so a user
// can't accidentally melt their disk from the UI.

import type { ChangeEvent } from 'react';

export const LOG_LEVELS = ['error', 'warn', 'info', 'debug'] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

const LEVEL_LABEL: Record<LogLevel, string> = {
  error: 'Error',
  warn: 'Warn',
  info: 'Info',
  debug: 'Debug',
};

/** Sentinel `<select>` value meaning "no override — inherit the base level".
 *  A module row's own `toModuleValue`/`fromModuleValue` (Logging tab, Task
 *  D2) maps this to "the key is absent from `modules`"; this component only
 *  renders the option and reports the raw string back through `onChange`. */
export const LEVEL_INHERIT = 'inherit';

export interface LevelSelectProps {
  /** One of `LOG_LEVELS`, or `LEVEL_INHERIT` when `inherit` is given and the
   *  row currently has no override. */
  value: string;
  onChange: (value: string) => void;
  /** Adds a first "Inherit (<base>)" option — a module row only; the base
   *  level's own select omits this. */
  inherit?: { base: string };
  disabled?: boolean;
  /** Optional visible label above the control. Omitted by a caller that
   *  renders its own (e.g. a module row's name in a grid). */
  label?: string;
  className?: string;
}

export function LevelSelect({ value, onChange, inherit, disabled, label, className }: LevelSelectProps) {
  const handleChange = (e: ChangeEvent<HTMLSelectElement>) => onChange(e.target.value);

  return (
    <div className={className}>
      {label && <label className="block text-xs text-content-muted mb-1">{label}</label>}
      <select
        value={value}
        onChange={handleChange}
        disabled={disabled}
        className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {inherit && <option value={LEVEL_INHERIT}>Inherit ({inherit.base})</option>}
        {LOG_LEVELS.map((l) => (
          <option key={l} value={l}>
            {LEVEL_LABEL[l]}
          </option>
        ))}
      </select>
    </div>
  );
}
