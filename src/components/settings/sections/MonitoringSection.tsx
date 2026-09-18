// Settings redesign (spec 2026-09-18 §2) — General tab, "Monitoring".
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';

export function MonitoringSection() {
  return (
    <SettingsSection id="general.monitoring">
      <div className="space-y-4">
        <SettingToggle section="general.monitoring" field="enabledGlobal" settingKey="monitoring.enabled_global" />
        <SettingNumber
          section="general.monitoring"
          field="intervalMinutes"
          settingKey="monitoring.interval_minutes"
          codec={intCodec(1, 1440)}
        />
      </div>
    </SettingsSection>
  );
}
