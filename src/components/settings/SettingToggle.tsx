// Settings redesign (spec 2026-09-18 §4/§5): a KV boolean field — the
// `Checkbox` bound to one setting key through `useSettingField`. Label and
// help text come from the registry (`fieldMeta`, read inside the hook), not
// a prop, so a field's copy lives in exactly one place.

import { useEffect, useRef } from 'react';
import { Checkbox } from './Checkbox';
import { ResetButton, SavedTick } from './ResetButton';
import { useSettingField, type UseSettingFieldOptions } from '../../hooks/useSettingField';
import { boolCodec } from '../../settings/codecs';
import { useSearchHighlight, isFieldHighlighted } from './SearchHighlightContext';

export interface SettingToggleProps {
  section: string;
  field: string;
  settingKey: string;
  disabled?: boolean;
  /** For a key with its own read/write command instead of the default
   *  `get_setting`/`set_setting` — forwarded to `useSettingField`. */
  write?: UseSettingFieldOptions<boolean>['write'];
  read?: UseSettingFieldOptions<boolean>['read'];
  /** Fired on mount (once the initial value is known) and again on every
   *  committed change — lets a sibling field in the same section mirror
   *  this one's value into local state (e.g. disabling another field while
   *  this toggle is off). Never fired for an in-flight, uncommitted draft. */
  onValueChange?: (value: boolean) => void;
}

export function SettingToggle({ section, field, settingKey, disabled, write, read, onValueChange }: SettingToggleProps) {
  const { value, setValue, isDefault, reset, meta, savedAt, error, defaultValue } = useSettingField(
    section,
    field,
    settingKey,
    boolCodec,
    { write, read },
  );

  const onValueChangeRef = useRef(onValueChange);
  onValueChangeRef.current = onValueChange;
  useEffect(() => {
    onValueChangeRef.current?.(value);
  }, [value]);

  const highlighted = isFieldHighlighted(useSearchHighlight(), section, field);

  return (
    <div>
      <div className={`flex items-start justify-between gap-2 ${highlighted ? 'ring-1 ring-accent/60 rounded' : ''}`}>
        <Checkbox
          checked={value}
          onChange={(v) => void setValue(v)}
          disabled={disabled}
          label={meta.label}
          description={meta.help}
        />
        <div className="flex items-center gap-1 shrink-0 pt-0.5">
          <SavedTick savedAt={savedAt} />
          <ResetButton
            visible={!isDefault}
            defaultLabel={defaultValue ? 'On' : 'Off'}
            onReset={() => void reset()}
            disabled={disabled}
          />
        </div>
      </div>
      {error && <p className="text-xs text-error mt-1 ml-6">{error}</p>}
    </div>
  );
}
