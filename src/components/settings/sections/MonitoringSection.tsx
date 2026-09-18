// Settings redesign (spec 2026-09-18 §2) — General tab, "Monitoring". The
// interval field is disabled while the global switch is off (owner-visible
// regression fix round, Item 1): `onValueChange` mirrors the toggle's own
// committed value into local state so a sibling field in the same section
// can read it — `monitoring.enabled_global` defaults to `true`
// (`crates/athenaeum-core/src/settings/mod.rs`), so that is this state's
// safe initial value before the field's own mount read resolves.
import { useState } from 'react';
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';

export function MonitoringSection() {
  const [enabled, setEnabled] = useState(true);

  return (
    <SettingsSection id="general.monitoring">
      <div className="space-y-4">
        <SettingToggle
          section="general.monitoring"
          field="enabledGlobal"
          settingKey="monitoring.enabled_global"
          // `useSettingField`'s `value` is typed `boolean` but can genuinely
          // be `undefined` at runtime with no committed value AND no
          // registry default yet — guard so `enabled` never leaves its
          // `boolean` type.
          onValueChange={(v) => { if (typeof v === 'boolean') setEnabled(v); }}
        />
        <SettingNumber
          section="general.monitoring"
          field="intervalMinutes"
          settingKey="monitoring.interval_minutes"
          codec={intCodec(1, 1440)}
          disabled={!enabled}
        />
      </div>
    </SettingsSection>
  );
}
