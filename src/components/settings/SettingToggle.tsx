// Settings redesign (spec 2026-09-18 §4/§5): a KV boolean field — the
// `Checkbox` bound to one setting key through `useSettingField`. Label and
// help text come from the registry (`fieldMeta`, read inside the hook), not
// a prop, so a field's copy lives in exactly one place.

import { Checkbox } from './Checkbox';
import { ResetButton, SavedTick } from './ResetButton';
import { useSettingField, type UseSettingFieldOptions } from '../../hooks/useSettingField';
import { boolCodec } from '../../settings/codecs';

export interface SettingToggleProps {
  section: string;
  field: string;
  settingKey: string;
  disabled?: boolean;
  /** For a key with its own read/write command instead of the default
   *  `get_setting`/`set_setting` — forwarded to `useSettingField`. */
  write?: UseSettingFieldOptions<boolean>['write'];
  read?: UseSettingFieldOptions<boolean>['read'];
}

export function SettingToggle({ section, field, settingKey, disabled, write, read }: SettingToggleProps) {
  const { value, setValue, isDefault, reset, meta, savedAt, error, defaultValue } = useSettingField(
    section,
    field,
    settingKey,
    boolCodec,
    { write, read },
  );

  return (
    <div>
      <div className="flex items-start justify-between gap-2">
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
