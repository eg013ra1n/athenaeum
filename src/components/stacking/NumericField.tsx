// The stage inspector's numeric input: the export tab's two-state discipline
// (`ExportTab.tsx` lines 164–216) — a string draft that survives a partial
// edit ("0."), committed to the caller only once it parses, and snapped back
// to the last-committed value on blur so an out-of-range or unparsable typed
// string never lingers on screen.
//
// Extension beyond the export tab's own fields (which are 1:1 local state):
// this field's `value` can also change from OUTSIDE while mounted — a preset
// applied from the toolbar, or a reload of the stored config — so the draft
// re-syncs to a changed `value` whenever the input is not the focused
// element. That keeps "never fight a partial edit" (don't resync while the
// user is actively typing) without leaving a preset's numbers stale on
// screen until the user happens to blur that particular field.

import { useEffect, useRef, useState } from 'react';

function clampNumber(n: number, min?: number, max?: number): number {
  let v = n;
  if (min !== undefined) v = Math.max(min, v);
  if (max !== undefined) v = Math.min(max, v);
  return v;
}

export interface NumericFieldProps {
  label: string;
  value: number;
  onCommit: (n: number) => void;
  min?: number;
  max?: number;
  step?: number;
  /** Help text under the field — states the field's default (never a
   *  hard-coded number; callers read it off `get_stacking_presets().default`). */
  help?: string;
  disabled?: boolean;
}

export function NumericField({ label, value, onCommit, min, max, step, help, disabled }: NumericFieldProps) {
  const [draft, setDraft] = useState(() => String(value));
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (document.activeElement !== inputRef.current) {
      setDraft(String(value));
    }
  }, [value]);

  const handleChange = (v: string) => {
    setDraft(v);
    const n = parseFloat(v);
    if (Number.isFinite(n)) {
      onCommit(clampNumber(n, min, max));
    }
  };

  const handleBlur = () => setDraft(String(value));

  return (
    <div>
      {label && (
        <label className="block text-xs text-content-secondary mb-1">{label}</label>
      )}
      <input
        ref={inputRef}
        type="number"
        min={min}
        max={max}
        step={step}
        value={draft}
        disabled={disabled}
        onChange={(e) => handleChange(e.target.value)}
        onBlur={handleBlur}
        className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      />
      {help && <p className="mt-1 text-[11px] text-content-muted">{help}</p>}
    </div>
  );
}

export interface NullableNumericFieldProps {
  label: string;
  value: number | null;
  /** Seeded when the "off" toggle turns the field on — a reasonable starting
   *  point, not a stored default (the field has none: it is `null`/off by
   *  default in every built-in preset). */
  seedValue: number;
  onCommit: (n: number | null) => void;
  min?: number;
  max?: number;
  step?: number;
  help?: string;
  disabled?: boolean;
}

/** A nullable numeric field with an explicit on/off toggle — `maxFwhmPx`,
 *  `maxEccentricity`, `minStars`, `integration.rangeHigh` all default to
 *  `null` ("off") and stay off until the user turns them on. */
export function NullableNumericField({
  label,
  value,
  seedValue,
  onCommit,
  min,
  max,
  step,
  help,
  disabled,
}: NullableNumericFieldProps) {
  const on = value !== null;
  return (
    <div>
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs text-content-secondary">{label}</span>
        <label className="flex items-center gap-1.5 text-[11px] text-content-muted cursor-pointer">
          <input
            type="checkbox"
            checked={on}
            disabled={disabled}
            onChange={(e) => onCommit(e.target.checked ? seedValue : null)}
            className="w-3.5 h-3.5 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          {on ? 'On' : 'Off'}
        </label>
      </div>
      {on ? (
        <NumericField
          label=""
          value={value}
          onCommit={onCommit}
          min={min}
          max={max}
          step={step}
          help={help}
          disabled={disabled}
        />
      ) : (
        help && <p className="mt-1 text-[11px] text-content-muted">{help}</p>
      )}
    </div>
  );
}
