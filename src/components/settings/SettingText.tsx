// Settings redesign (spec 2026-09-18 §4/§5): a KV string field — the same
// draft/blur/Enter/Escape discipline as `SettingNumber`, over a string
// `Codec` instead of a numeric one.

import type { KeyboardEvent } from 'react';
import { ResetButton, SavedTick } from './ResetButton';
import { useSettingField, type UseSettingFieldOptions } from '../../hooks/useSettingField';
import type { Codec } from '../../settings/codecs';

export interface SettingTextProps {
  section: string;
  field: string;
  settingKey: string;
  codec: Codec<string>;
  placeholder?: string;
  disabled?: boolean;
  /** For a key with its own read/write command — forwarded to `useSettingField`. */
  write?: UseSettingFieldOptions<string>['write'];
  read?: UseSettingFieldOptions<string>['read'];
}

export function SettingText({ section, field, settingKey, codec, placeholder, disabled, write, read }: SettingTextProps) {
  const { draft, setDraft, commit, escape, error, savedAt, isDefault, reset, meta, defaultValue } = useSettingField(
    section,
    field,
    settingKey,
    codec,
    { write, read },
  );

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
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="text-sm text-content-secondary">{meta.label}</label>
        <div className="flex items-center gap-1 shrink-0">
          <SavedTick savedAt={savedAt} />
          <ResetButton
            visible={!isDefault}
            defaultLabel={defaultValue === '' ? '(empty)' : defaultValue}
            onReset={() => void reset()}
            disabled={disabled}
          />
        </div>
      </div>
      <input
        type="text"
        value={draft}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={handleKeyDown}
        className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-content focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      />
      {meta.help && <p className="text-xs text-content-muted mt-1">{meta.help}</p>}
      {error && <p className="text-xs text-error mt-1">{error}</p>}
    </div>
  );
}
