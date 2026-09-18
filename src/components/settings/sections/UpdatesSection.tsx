// Settings redesign (spec 2026-09-18 §2) — General tab, "Updates" section.
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';

export function UpdatesSection() {
  return (
    <SettingsSection id="general.updates">
      <div className="space-y-4">
        <SettingToggle section="general.updates" field="autoCheck" settingKey="updates.auto_check" />
        <SettingToggle section="general.updates" field="checkBeta" settingKey="updates.check_beta" />
      </div>
    </SettingsSection>
  );
}
