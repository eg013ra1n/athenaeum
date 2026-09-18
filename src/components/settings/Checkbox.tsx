// Settings redesign (spec 2026-09-18 §4): the one checkbox control in the
// codebase. Every other `type="checkbox"` under `src/components/settings/`,
// `src/pages/Settings.tsx`, the stacking panels, analysis, plate-solve and
// calibration components is expected to render through this component (or
// through `SettingToggle`, which binds it to a KV setting) — see
// CLAUDE.md's Global Constraints for the sweep.
//
// Native `<input type="checkbox">` tinted with `accent-accent`: `text-*` /
// `border-*` / `focus:ring-*` only reach the control through
// `@tailwindcss/forms`, which this project does not install, so `accent-*`
// is the house pattern (see `SwitchRow.tsx`, now a thin wrapper over this).

import type { ReactNode } from 'react';

export interface CheckboxProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label?: ReactNode;
  description?: string;
  disabled?: boolean;
  /** `sm` = `w-3.5 h-3.5` (the stacking panels' rows), `md` = `w-4 h-4`
   *  (everywhere else). Default `md`. */
  size?: 'sm' | 'md';
  /** ARIA role — `switch` for an on/off toggle (`SwitchRow`, `SettingToggle`),
   *  `checkbox` (default) for a plain boolean option. */
  role?: 'checkbox' | 'switch';
}

export function Checkbox({ checked, onChange, label, description, disabled, size = 'md', role = 'checkbox' }: CheckboxProps) {
  const box = size === 'sm' ? 'w-3.5 h-3.5' : 'w-4 h-4';
  return (
    <label className={`flex items-start gap-2 ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
      <input
        type="checkbox"
        role={role}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className={`mt-0.5 shrink-0 ${box} accent-accent`}
      />
      {(label || description) && (
        <span className="flex-1 min-w-0">
          {label && <span className="block text-sm text-content-secondary">{label}</span>}
          {description && <span className="block text-xs text-content-muted leading-relaxed">{description}</span>}
        </span>
      )}
    </label>
  );
}
