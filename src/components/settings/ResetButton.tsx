// Settings redesign (spec 2026-09-18 §4/§5): the reset affordance and the
// "saved" feedback every field component shows next to its control —
// grouped in one file since both live in the same header-row corner and
// share the "quiet feedback" idea (spec §5: a successful write is quiet, a
// reset is a normal write through the same commit path).

import { useEffect, useState } from 'react';
import { Check, RotateCcw } from 'lucide-react';

export interface ResetButtonProps {
  /** Rendered only when `true` — a field/section already at its default
   *  shows nothing here (spec §4: "rendered only when `value !== default`"). */
  visible: boolean;
  /** The default value's display text, shown in the button's title. */
  defaultLabel: string;
  onReset: () => void;
  disabled?: boolean;
}

/** The `↺` icon button — resets one field (or, from `SettingsSection`, every
 *  field in a section) back to its default. */
export function ResetButton({ visible, defaultLabel, onReset, disabled }: ResetButtonProps) {
  if (!visible) return null;
  return (
    <button
      type="button"
      onClick={onReset}
      disabled={disabled}
      title={`Reset to default (${defaultLabel})`}
      aria-label={`Reset to default (${defaultLabel})`}
      className="shrink-0 p-1 rounded text-content-muted hover:text-content hover:bg-surface-hover disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
    >
      <RotateCcw size={14} />
    </button>
  );
}

export interface SavedTickProps {
  /** `useSettingField`/`useAutosaveDocument`'s `savedAt` — a timestamp that
   *  changes on every successful write. `null` before the first write. */
  savedAt: number | null;
}

/** A quiet `Check` that fades in beside a field for 1.5s after `savedAt`
 *  changes (spec §5's "successful write is quiet" feedback) — shared by
 *  every field component next to its `ResetButton`. Always mounted (so nearby
 *  controls don't shift when it appears) and toggles opacity with a CSS
 *  transition rather than mounting/unmounting. */
export function SavedTick({ savedAt }: SavedTickProps) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (savedAt === null) return;
    setVisible(true);
    const t = setTimeout(() => setVisible(false), 1500);
    return () => clearTimeout(t);
  }, [savedAt]);

  return (
    <Check
      size={12}
      aria-hidden="true"
      className={`text-success transition-opacity duration-300 ${visible ? 'opacity-100' : 'opacity-0'}`}
    />
  );
}
