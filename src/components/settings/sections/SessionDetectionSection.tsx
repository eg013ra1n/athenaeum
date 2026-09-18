// Settings redesign (spec 2026-09-18 §2) — General tab, "Session detection".
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { floatCodec } from '../../../settings/codecs';

export function SessionDetectionSection() {
  return (
    <SettingsSection id="general.sessions">
      <SettingNumber
        section="general.sessions"
        field="gapHours"
        settingKey="session_gap_threshold_hours"
        codec={floatCodec(0.5, 48)}
        step={0.5}
      />
    </SettingsSection>
  );
}
