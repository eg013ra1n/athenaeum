// General tab, "Heavy jobs". `compute.max_concurrent` is read through the
// generic `get_setting` and written through `set_compute_max_concurrent`,
// which also resizes the live queue — the generic `set_setting` would store
// the value and leave the running queue as it was. KEEP the 1..8 range in
// sync with `validate_max_concurrent` in
// `crates/athenaeum-core/src/api/compute.rs`.
import { api } from '../../../api';
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';

export function ComputeQueueSection() {
  return (
    <SettingsSection id="general.compute">
      <SettingNumber
        section="general.compute"
        field="maxConcurrent"
        settingKey="compute.max_concurrent"
        codec={intCodec(1, 8)}
        write={(n) => api.invoke('set_compute_max_concurrent', { n })}
      />
    </SettingsSection>
  );
}
