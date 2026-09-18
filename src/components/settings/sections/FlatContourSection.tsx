// Settings redesign (spec 2026-09-18 §2) — Blink tab, "Flat contour plot".
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { intCodec, floatCodec } from '../../../settings/codecs';

export function FlatContourSection() {
  return (
    <SettingsSection id="blink.flatContour">
      <div className="grid grid-cols-2 gap-4">
        <SettingNumber
          section="blink.flatContour"
          field="resolutionPct"
          settingKey="flat_contour.resolution_pct"
          codec={intCodec(5, 100)}
        />
        <SettingNumber
          section="blink.flatContour"
          field="sigmaPx"
          settingKey="flat_contour.sigma_px"
          codec={floatCodec(0, 10)}
          step={0.1}
        />
        <SettingNumber
          section="blink.flatContour"
          field="contours"
          settingKey="flat_contour.contours"
          codec={intCodec(4, 20)}
        />
        <SettingNumber
          section="blink.flatContour"
          field="gradientPct"
          settingKey="flat_contour.gradient_pct"
          codec={intCodec(0, 200)}
        />
      </div>
    </SettingsSection>
  );
}
