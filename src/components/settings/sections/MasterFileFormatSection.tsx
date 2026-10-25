// Settings redesign (spec 2026-09-18 §2) — Calibration tab, "Master file
// format".
import { SettingsSection } from '../SettingsSection';
import { SettingSelect } from '../SettingSelect';
import { enumCodec } from '../../../settings/codecs';

const FORMAT_OPTIONS = [
  { value: 'fits', label: 'FITS — float32, the format every tool reads' },
  { value: 'xisf', label: 'XISF — what WBPP requires for master calibration files' },
] as const;

const formatCodec = enumCodec(['fits', 'xisf'] as const);

export function MasterFileFormatSection() {
  return (
    <SettingsSection id="calibration.masterFormat">
      <SettingSelect
        section="calibration.masterFormat"
        field="format"
        settingKey="calibration.master_format"
        codec={formatCodec}
        options={FORMAT_OPTIONS}
      />
    </SettingsSection>
  );
}
