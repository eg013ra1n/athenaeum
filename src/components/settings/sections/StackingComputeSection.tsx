// Task 13c — Stacking tab, "Compute". `compute.memory_budget_mb` rides the
// generic `get_setting`/`set_setting`; 0 = automatic, 1 is valid (forces
// single-frame admission).
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';

export function StackingComputeSection() {
  return (
    <SettingsSection id="stacking.compute">
      <SettingNumber
        section="stacking.compute"
        field="memoryBudget"
        settingKey="compute.memory_budget_mb"
        codec={intCodec(0, 1048576)}
        unit="0 = automatic"
      />
    </SettingsSection>
  );
}
