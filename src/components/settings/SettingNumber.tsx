// Settings redesign (spec 2026-09-18 §4/§5): a KV numeric field. The
// `variant="input"` (default) path is the `NumericField.tsx` draft/blur/
// Enter/Escape discipline generalized to the shared hook — `useSettingField`
// already carries the draft, this component only renders it: typing edits
// `draft` freely, blur or Enter commits (parses + validates through the
// field's `Codec`, writes on success), Escape restores the last committed
// value. `variant="slider"` is a discrete control instead (spec §5): every
// `onChange` commits through `setValue`, debounced 300ms by the hook, so a
// drag ends as one write.

import { useEffect, useRef, type KeyboardEvent } from 'react';
import { ResetButton, SavedTick } from './ResetButton';
import { useSettingField, type UseSettingFieldOptions } from '../../hooks/useSettingField';
import type { Codec } from '../../settings/codecs';
import { useSearchHighlight, isFieldHighlighted } from './SearchHighlightContext';

export interface SettingNumberProps {
  section: string;
  field: string;
  settingKey: string;
  codec: Codec<number>;
  /** Shown after the label, e.g. "px", "s", "MB". */
  unit?: string;
  step?: number;
  placeholder?: string;
  /** HTML `min`/`max` — advisory for `variant="input"` (the codec is the
   *  real validator), required for a useful `variant="slider"` range. */
  min?: number;
  max?: number;
  variant?: 'input' | 'slider';
  disabled?: boolean;
  /** For a key with its own read/write command — forwarded to `useSettingField`. */
  write?: UseSettingFieldOptions<number>['write'];
  read?: UseSettingFieldOptions<number>['read'];
  /** Fired on mount (once the initial value is known) and again on every
   *  committed change — lets a sibling field in the same section mirror
   *  this one's value into local state. */
  onValueChange?: (value: number) => void;
}

export function SettingNumber({
  section,
  field,
  settingKey,
  codec,
  unit,
  step,
  placeholder,
  min,
  max,
  variant = 'input',
  disabled,
  write,
  read,
  onValueChange,
}: SettingNumberProps) {
  const { value, draft, setDraft, commit, setValue, escape, error, savedAt, isDefault, reset, meta, defaultValue } =
    useSettingField(section, field, settingKey, codec, { write, read });

  const onValueChangeRef = useRef(onValueChange);
  onValueChangeRef.current = onValueChange;
  useEffect(() => {
    onValueChangeRef.current?.(value);
  }, [value]);

  const highlighted = isFieldHighlighted(useSearchHighlight(), section, field);

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      void commit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      escape();
    }
  };

  return (
    <div>
      <div className={`flex items-center justify-between gap-2 mb-1 ${highlighted ? 'ring-1 ring-accent/60 rounded' : ''}`}>
        <label className="text-sm text-content-secondary">
          {meta.label}
          {unit && <span className="text-content-muted"> ({unit})</span>}
        </label>
        <div className="flex items-center gap-1 shrink-0">
          <SavedTick savedAt={savedAt} />
          <ResetButton
            visible={!isDefault}
            defaultLabel={unit ? `${defaultValue} ${unit}` : String(defaultValue)}
            onReset={() => void reset()}
            disabled={disabled}
          />
        </div>
      </div>

      {variant === 'slider' ? (
        <div className="flex items-center gap-3">
          <input
            type="range"
            value={value}
            min={min}
            max={max}
            step={step}
            disabled={disabled}
            onChange={(e) => void setValue(Number(e.target.value))}
            className="flex-1 accent-accent"
          />
          <span className="text-sm text-content tabular-nums w-14 text-right">
            {value}
            {unit ? ` ${unit}` : ''}
          </span>
        </div>
      ) : (
        <input
          type="number"
          value={draft}
          min={min}
          max={max}
          step={step}
          placeholder={placeholder}
          disabled={disabled}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void commit()}
          onKeyDown={handleKeyDown}
          className="w-full sm:w-64 bg-surface-hover border border-border rounded-lg px-3 py-2 text-content focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
        />
      )}

      {meta.help && <p className="text-xs text-content-muted mt-1">{meta.help}</p>}
      {error && <p className="text-xs text-error mt-1">{error}</p>}
    </div>
  );
}
