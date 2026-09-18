// Settings redesign (spec 2026-09-18 §4/§5): a KV enum field — a `<select>`
// bound to one setting key through `useSettingField`. A discrete control:
// commits through `setValue` on change (debounced 300ms by the hook), never
// a draft/blur cycle.

import { ResetButton, SavedTick } from './ResetButton';
import { useSettingField, type UseSettingFieldOptions } from '../../hooks/useSettingField';
import type { Codec } from '../../settings/codecs';

export interface SettingSelectOption<T extends string> {
  value: T;
  label: string;
}

export interface SettingSelectProps<T extends string> {
  section: string;
  field: string;
  settingKey: string;
  codec: Codec<T>;
  options: readonly SettingSelectOption<T>[];
  disabled?: boolean;
  /** For a key with its own read/write command — forwarded to `useSettingField`. */
  write?: UseSettingFieldOptions<T>['write'];
  read?: UseSettingFieldOptions<T>['read'];
}

export function SettingSelect<T extends string>({
  section,
  field,
  settingKey,
  codec,
  options,
  disabled,
  write,
  read,
}: SettingSelectProps<T>) {
  const { value, setValue, isDefault, reset, meta, savedAt, error, defaultValue } = useSettingField(
    section,
    field,
    settingKey,
    codec,
    { write, read },
  );

  const defaultLabel = options.find((o) => o.value === defaultValue)?.label ?? String(defaultValue);

  return (
    <div>
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="text-sm text-content-secondary">{meta.label}</label>
        <div className="flex items-center gap-1 shrink-0">
          <SavedTick savedAt={savedAt} />
          <ResetButton visible={!isDefault} defaultLabel={defaultLabel} onReset={() => void reset()} disabled={disabled} />
        </div>
      </div>
      <select
        value={value}
        disabled={disabled}
        onChange={(e) => void setValue(e.target.value as T)}
        className="w-full sm:w-64 bg-surface-hover border border-border rounded-lg px-3 py-2 text-content focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {meta.help && <p className="text-xs text-content-muted mt-1">{meta.help}</p>}
      {error && <p className="text-xs text-error mt-1">{error}</p>}
    </div>
  );
}
