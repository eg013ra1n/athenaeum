// Settings redesign (spec 2026-09-18 §2) — Blink tab, "Blink viewer".
// `blink.threads`'s range depends on `get_blink_threads_max` (CPU-count
// dependent, not a fixed default) and its write rebuilds the blink
// semaphore immediately, so it goes through `set_blink_threads` /
// `get_blink_threads_max` rather than the default `get_setting`/
// `set_setting` pair (plan Task C1 Step 3).
//
// Owner-visible regression fix round, Item 2: only the JPEG-quality slider
// matching the SELECTED resolution shows (as the pre-redesign page did) —
// the resolution `SettingSelect`'s `onValueChange` mirrors its committed
// value into local state so this component can pick which slider to render;
// `'full'` also restores the old "~10x slower" warning line under the
// select (`git show 5c5922f0^:src/pages/Settings.tsx`, the Blink Viewer
// block).
import { useEffect, useState } from 'react';
import { api } from '../../../api';
import { SettingsSection } from '../SettingsSection';
import { SettingSelect } from '../SettingSelect';
import { SettingNumber } from '../SettingNumber';
import { intCodec, enumCodec } from '../../../settings/codecs';

const RESOLUTION_OPTIONS = [
  { value: 'thumbnail', label: 'Thumbnail (4x downscale)' },
  { value: 'preview', label: 'Preview (2x2 binning)' },
  { value: 'full', label: 'Full Resolution' },
] as const;

type BlinkResolution = (typeof RESOLUTION_OPTIONS)[number]['value'];

const resolutionCodec = enumCodec(['thumbnail', 'preview', 'full'] as const);

export function BlinkViewerSection() {
  const [threadsMax, setThreadsMax] = useState(4);
  const [resolution, setResolution] = useState<BlinkResolution>('preview');

  useEffect(() => {
    let cancelled = false;
    api.invoke<number>('get_blink_threads_max')
      .then((max) => { if (!cancelled && typeof max === 'number') setThreadsMax(max); })
      .catch((err) => console.error('[Settings] get_blink_threads_max failed:', err));
    return () => { cancelled = true; };
  }, []);

  return (
    <SettingsSection id="blink.viewer">
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        <div>
          <SettingSelect
            section="blink.viewer"
            field="resolution"
            settingKey="blink.resolution"
            codec={resolutionCodec}
            options={RESOLUTION_OPTIONS}
            // `useSettingField`'s `value` is typed as the enum but can
            // genuinely be `undefined` at runtime with no committed value
            // AND no registry default yet — guard rather than let
            // `resolution` leave its narrow union type.
            onValueChange={(v) => { if (v) setResolution(v); }}
          />
          {resolution === 'full' && (
            <p className="text-xs text-warning mt-1">
              Full resolution is ~10x slower to load and uses significantly more memory. Use for detailed inspection only.
            </p>
          )}
        </div>

        {resolution === 'thumbnail' && (
          <SettingNumber
            section="blink.viewer"
            field="qualityThumbnail"
            settingKey="rustafits.quality.thumbnail"
            codec={intCodec(10, 100)}
            variant="slider"
            min={10}
            max={100}
          />
        )}
        {resolution === 'preview' && (
          <SettingNumber
            section="blink.viewer"
            field="qualityPreview"
            settingKey="rustafits.quality.preview"
            codec={intCodec(10, 100)}
            variant="slider"
            min={10}
            max={100}
          />
        )}
        {resolution === 'full' && (
          <SettingNumber
            section="blink.viewer"
            field="qualityFull"
            settingKey="rustafits.quality.full"
            codec={intCodec(10, 100)}
            variant="slider"
            min={10}
            max={100}
          />
        )}

        <SettingNumber
          section="blink.viewer"
          field="threads"
          settingKey="blink.threads"
          codec={intCodec(0, threadsMax)}
          write={(v) => api.invoke('set_blink_threads', { threads: v })}
        />
        <SettingNumber
          section="blink.viewer"
          field="cacheSize"
          settingKey="blink.memory_cache_size"
          codec={intCodec(10, 5000)}
          step={10}
        />
        <SettingNumber
          section="blink.viewer"
          field="cacheMaxMb"
          settingKey="blink.memory_cache_max_mb"
          codec={intCodec(64, 16384)}
          step={64}
        />
        <SettingNumber
          section="blink.viewer"
          field="retentionMinutes"
          settingKey="blink.memory_retention_minutes"
          codec={intCodec(1, 1440)}
        />
      </div>
    </SettingsSection>
  );
}
