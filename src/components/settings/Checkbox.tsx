// Settings redesign (spec 2026-09-18 §4): the one checkbox control in the
// codebase. Every other `type="checkbox"` under `src/components/settings/`,
// `src/pages/Settings.tsx`, the stacking panels, analysis, plate-solve and
// calibration components is expected to render through this component (or
// through `SettingToggle`, which binds it to a KV setting) — see
// CLAUDE.md's Global Constraints for the sweep.
//
// Visually hidden real `<input type="checkbox">` (keyboard, label and a11y
// behaviour stay native) plus the mockup `.cb` box drawn as a sibling span
// driven by the `peer` state: 13 px, radius 3, accent fill when checked.

import type { ReactNode } from 'react';

export interface CheckboxProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label?: ReactNode;
  description?: string;
  disabled?: boolean;
  /** Changes only the label font (`sm` = 12 px, `md` = `text-sm`); the box is
   *  always 13 px. Default `md`. */
  size?: 'sm' | 'md';
  /** ARIA role — `switch` for an on/off toggle (`SwitchRow`, `SettingToggle`),
   *  `checkbox` (default) for a plain boolean option. */
  role?: 'checkbox' | 'switch';
}

export function Checkbox({ checked, onChange, label, description, disabled, size = 'md', role = 'checkbox' }: CheckboxProps) {
  const labelFont = size === 'sm' ? 'text-[12px]' : 'text-sm';
  return (
    <label className={`flex items-start gap-2 ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}`}>
      <input
        type="checkbox"
        role={role}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="peer sr-only"
      />
      <span
        data-testid="cb-box"
        aria-hidden
        className="relative mt-0.5 inline-block h-[13px] w-[13px] shrink-0 rounded-[3px] border border-border bg-surface peer-checked:border-accent peer-checked:bg-accent peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-accent peer-disabled:opacity-45"
      >
        {checked && (
          <svg width="9" height="9" viewBox="0 0 9 9" className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-surface">
            <path d="M1.5 4.5 3.5 6.5 7.5 2" stroke="currentColor" strokeWidth="2" fill="none" />
          </svg>
        )}
      </span>
      {(label || description) && (
        <span className="flex-1 min-w-0">
          {label && <span className={`block ${labelFont} text-content-secondary`}>{label}</span>}
          {description && <span className="block text-xs text-content-muted leading-relaxed">{description}</span>}
        </span>
      )}
    </label>
  );
}
