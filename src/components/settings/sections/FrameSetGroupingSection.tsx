// Settings redesign (spec 2026-09-18 §2) — General tab, "Frame set
// grouping". Replaces the old "Clustering Parameters" block AND the bottom
// "About Frame Set Grouping" card — the explanatory prose now lives in the
// registry's `general.grouping` `description`/field `help`, rendered by
// `SettingsSection` itself, so it isn't restated here. The "Current value:
// X° (decimal degrees)" line is restored (owner-visible regression fix
// round, Item 3): the value/unit fields commit through `useSettingField`
// internally, so this component mirrors both via `onValueChange` to compute
// it — the same conversion `getThresholdInDegrees` used before this
// redesign (`git show 5c5922f0^:src/pages/Settings.tsx`).
import { useState } from 'react';
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { SettingSelect } from '../SettingSelect';
import { floatCodec, enumCodec } from '../../../settings/codecs';

const THRESHOLD_UNITS = [
  { value: 'deg', label: 'degrees' },
  { value: 'arcmin', label: 'arcminutes' },
  { value: 'arcsec', label: 'arcseconds' },
] as const;

type ThresholdUnit = (typeof THRESHOLD_UNITS)[number]['value'];

const thresholdUnitCodec = enumCodec(['deg', 'arcmin', 'arcsec'] as const);

/** `grouping.threshold.value`/`.unit` defaults — `crates/athenaeum-core/src/
 *  settings/mod.rs` — the safe initial state before either field's own
 *  mount read resolves. */
function thresholdToDegrees(value: number, unit: ThresholdUnit): string {
  switch (unit) {
    case 'arcsec':
      return (value / 3600).toFixed(4);
    case 'arcmin':
      return (value / 60).toFixed(4);
    case 'deg':
    default:
      return value.toFixed(4);
  }
}

export function FrameSetGroupingSection() {
  const [thresholdValue, setThresholdValue] = useState(3.0);
  const [thresholdUnit, setThresholdUnit] = useState<ThresholdUnit>('deg');

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
            // `useSettingField`'s `value` is typed `number` but can genuinely
            // be `undefined` at runtime when neither a committed value nor a
            // registry default exists yet (e.g. an incomplete
            // `get_settings_defaults` response) — guard rather than feed
            // `NaN`/`undefined` into `toFixed` below.
            onValueChange={(v) => { if (Number.isFinite(v)) setThresholdValue(v); }}
          />
        </div>
        <div>
          <SettingSelect
            section="general.grouping"
            field="thresholdUnit"
            settingKey="grouping.threshold.unit"
            codec={thresholdUnitCodec}
            options={THRESHOLD_UNITS}
            onValueChange={(v) => { if (v) setThresholdUnit(v); }}
          />
        </div>
      </div>
      <p className="text-xs text-content-muted mt-2">
        Current value: {thresholdToDegrees(thresholdValue, thresholdUnit)}° (decimal degrees)
      </p>
    </SettingsSection>
  );
}
