// The stacker's PSF model choice, shared by the Measure and Normalize panels: Auto, Moffat-4 and "Moffat, own β"
// (a Moffat profile fitted at a β the user types). A `<select>` value is a string, so the object variant
// `{ moffat: β }` is the fixed key `moffat-own` here and is mapped to and from the config's value.
//
// The β field keeps NumericField's two-state discipline (a string draft that survives a partial edit, snapped back to
// the last saved value on blur), but differs on purpose in one thing: a β outside the bounds is NOT clamped and
// saved. The wire refuses a β outside [1.5, 100] when it decodes (a clamp would silently store another number than
// the one typed), so an out-of-range draft is shown with its bounds and nothing is committed.

import { useEffect, useId, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { PsfModel } from '../../types/stacking';

/** The bounds the measurement's β accepts (rustafits `BETA_BOUNDS`). */
export const MOFFAT_BETA_MIN = 1.5;
export const MOFFAT_BETA_MAX = 100;
/** A β chosen from the select starts at the fixed Moffat-4 profile's value. */
export const MOFFAT_BETA_START = 4;

const OWN_KEY = 'moffat-own';
const SELECT_CLASS =
  'w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50';

/** The caller's β, when the model is the own-β variant. */
export function ownBeta(v: PsfModel): number | null {
  return typeof v === 'object' ? v.moffat : null;
}

function selectKey(v: PsfModel): string {
  return typeof v === 'object' ? OWN_KEY : v;
}

function betaInBounds(n: number): boolean {
  return Number.isFinite(n) && n >= MOFFAT_BETA_MIN && n <= MOFFAT_BETA_MAX;
}

function BetaField({ value, onCommit, disabled }: { value: number; onCommit: (n: number) => void; disabled?: boolean }) {
  const [draft, setDraft] = useState(() => String(value));
  const [rejected, setRejected] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const inputId = useId();

  useEffect(() => {
    if (document.activeElement !== inputRef.current) {
      setDraft(String(value));
      setRejected(false);
    }
  }, [value]);

  const handleChange = (text: string) => {
    setDraft(text);
    const n = parseFloat(text);
    if (betaInBounds(n)) {
      setRejected(false);
      onCommit(n);
    } else {
      setRejected(true);
    }
  };

  const handleBlur = () => {
    setDraft(String(value));
    setRejected(false);
  };

  return (
    <div className="mt-2">
      <label htmlFor={inputId} className="block text-xs text-content-secondary mb-1">
        Moffat β
      </label>
      <input
        id={inputId}
        ref={inputRef}
        type="number"
        min={MOFFAT_BETA_MIN}
        max={MOFFAT_BETA_MAX}
        step={0.1}
        value={draft}
        disabled={disabled}
        aria-invalid={rejected}
        onChange={(e) => handleChange(e.target.value)}
        onBlur={handleBlur}
        className="w-full px-2 py-1 text-sm bg-surface text-content rounded border border-border focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      />
      <p className={`mt-1 text-[11px] ${rejected ? 'text-error' : 'text-content-muted'}`}>
        β from {MOFFAT_BETA_MIN} to {MOFFAT_BETA_MAX}
        {rejected ? ' — not saved' : ''}
      </p>
    </div>
  );
}

export interface PsfModelSelectProps {
  value: PsfModel;
  onChange: (next: PsfModel) => void;
  /** The panel's own label for a model (the Normalize stage words Auto differently). */
  label: (v: PsfModel) => string;
  disabled?: boolean;
  /** The help paragraph under the controls. */
  help: ReactNode;
}

export function PsfModelSelect({ value, onChange, label, disabled, help }: PsfModelSelectProps) {
  const beta = ownBeta(value);

  const handleSelect = (key: string) => {
    if (key === OWN_KEY) {
      onChange({ moffat: beta ?? MOFFAT_BETA_START });
    } else if (key === 'auto' || key === 'moffat4') {
      onChange(key);
    } else {
      console.error('[PsfModelSelect] unknown PSF model key', key);
    }
  };

  return (
    <div>
      <label className="block text-xs text-content-secondary mb-1">PSF model</label>
      <select
        value={selectKey(value)}
        disabled={disabled}
        onChange={(e) => handleSelect(e.target.value)}
        className={SELECT_CLASS}
      >
        <option value="auto">{label('auto')}</option>
        <option value="moffat4">{label('moffat4')}</option>
        <option value={OWN_KEY}>Moffat, own β</option>
      </select>
      {beta !== null && <BetaField value={beta} onCommit={(n) => onChange({ moffat: n })} disabled={disabled} />}
      <p className="mt-1 text-[11px] text-content-muted">{help}</p>
    </div>
  );
}
