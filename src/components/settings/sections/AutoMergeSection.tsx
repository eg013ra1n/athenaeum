// Settings redesign (spec 2026-09-18 §2) — General tab, "Auto-merge".
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';

export function AutoMergeSection() {
  return (
    <SettingsSection id="general.autoMerge">
      <div className="space-y-3">
        <SettingToggle section="general.autoMerge" field="onButtonClick" settingKey="auto_merge.on_button_click" />
        <SettingToggle section="general.autoMerge" field="onMonitorDetect" settingKey="auto_merge.on_monitor_detect" />
      </div>
    </SettingsSection>
  );
}
