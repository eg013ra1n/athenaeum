// Settings redesign (spec 2026-09-18 §2) — General tab, "Frame set
// grouping". Replaces the old "Clustering Parameters" block AND the bottom
// "About Frame Set Grouping" card — the explanatory prose now lives in the
// registry's `general.grouping` `description`/field `help`, rendered by
// `SettingsSection` itself, so it isn't restated here.
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { SettingSelect } from '../SettingSelect';
import { floatCodec, enumCodec } from '../../../settings/codecs';

const THRESHOLD_UNITS = [
  { value: 'deg', label: 'degrees' },
  { value: 'arcmin', label: 'arcminutes' },
  { value: 'arcsec', label: 'arcseconds' },
] as const;

const thresholdUnitCodec = enumCodec(['deg', 'arcmin', 'arcsec'] as const);

export function FrameSetGroupingSection() {
  return (
    <SettingsSection id="general.grouping">
      <div className="flex flex-col sm:flex-row gap-3">
        <div className="flex-1">
          <SettingNumber
            section="general.grouping"
            field="threshold"
            settingKey="grouping.threshold.value"
            codec={floatCodec(0.001, 180)}
            step={0.1}
          />
        </div>
        <div>
          <SettingSelect
            section="general.grouping"
            field="thresholdUnit"
            settingKey="grouping.threshold.unit"
            codec={thresholdUnitCodec}
            options={THRESHOLD_UNITS}
          />
        </div>
      </div>
    </SettingsSection>
  );
}
