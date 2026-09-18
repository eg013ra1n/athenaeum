// Settings redesign (spec 2026-09-18 §2/§6) — Blink tab, "Star annotation
// display". The whole thing is one JSON value under the KV key
// `blink.annotation_config` (`''` stored means "use the built-in object"),
// so it goes through `useAutosaveDocument` rather than ten separate
// `useSettingField`s.
//
// `BUILT_IN_ANNOTATION_CONFIG` is the one exception to "defaults never
// restated in TypeScript" (spec §6) — but it isn't a NEW restatement: it is
// `DEFAULT_ANNOTATION_SETTINGS` (`src/types/helpers.ts`), the same object
// `BlinkViewer.tsx` already falls back to when the stored key is empty or
// fails to parse, aliased here so both call sites can never drift apart.
import { api } from '../../../api';
import { useAutosaveDocument } from '../../../hooks/useAutosaveDocument';
import { DEFAULT_ANNOTATION_SETTINGS as BUILT_IN_ANNOTATION_CONFIG } from '../../../types/helpers';
import type { AnnotationSettings } from '../../../types/analysis-config';
import { SettingsSection } from '../SettingsSection';
import { Checkbox } from '../Checkbox';
import { ResetButton, SavedTick } from '../ResetButton';

const ANNOTATION_CONFIG_KEY = 'blink.annotation_config';

async function loadAnnotationConfig(): Promise<AnnotationSettings> {
  const raw = await api.invoke<string>('get_setting', { key: ANNOTATION_CONFIG_KEY, defaultValue: '' });
  if (!raw) return BUILT_IN_ANNOTATION_CONFIG;
  try {
    return { ...BUILT_IN_ANNOTATION_CONFIG, ...(JSON.parse(raw) as Partial<AnnotationSettings>) };
  } catch (err) {
    console.error('[StarAnnotationSection] failed to parse stored blink.annotation_config:', err);
    return BUILT_IN_ANNOTATION_CONFIG;
  }
}

async function saveAnnotationConfig(next: AnnotationSettings): Promise<void> {
  await api.invoke('set_setting', { key: ANNOTATION_CONFIG_KEY, value: JSON.stringify(next) });
}

interface NumberRowProps {
  path: keyof AnnotationSettings & string;
  label: string;
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (n: number) => void;
  isDefault: boolean;
  defaultValue: number;
  onReset: () => void;
}

function NumberRow({ label, min, max, step, value, onChange, isDefault, defaultValue, onReset }: NumberRowProps) {
  return (
    <div>
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="block text-xs text-content-secondary">{label}</label>
        <ResetButton visible={!isDefault} defaultLabel={String(defaultValue)} onReset={onReset} />
      </div>
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        onChange={(e) => {
          const n = parseFloat(e.target.value);
          if (Number.isFinite(n)) onChange(n);
        }}
        className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
      />
    </div>
  );
}

export function StarAnnotationSection() {
  const { doc, patch, isDefault, resetField, savedAt } = useAutosaveDocument<AnnotationSettings>({
    load: loadAnnotationConfig,
    save: saveAnnotationConfig,
    defaults: BUILT_IN_ANNOTATION_CONFIG,
    debounceMs: 300,
    label: 'Star annotation display',
  });

  // Renders the built-in shape immediately (no loading flicker) — `patch`
  // itself refuses to run before `doc` resolves (logged, see
  // `useAutosaveDocument`), which only matters for a click in the sub-second
  // window before the mount read settles.
  const d = doc ?? BUILT_IN_ANNOTATION_CONFIG;

  const numberField = (path: keyof AnnotationSettings & string, label: string, min: number, max: number, step: number) => (
    <NumberRow
      key={path}
      path={path}
      label={label}
      min={min}
      max={max}
      step={step}
      value={d[path] as number}
      onChange={(n) => patch({ [path]: n } as Partial<AnnotationSettings>)}
      isDefault={isDefault(path)}
      defaultValue={BUILT_IN_ANNOTATION_CONFIG[path] as number}
      onReset={() => resetField(path)}
    />
  );

  return (
    <SettingsSection id="blink.annotations">
      <div className="space-y-4">
        <div className="flex items-center justify-end -mb-2">
          <SavedTick savedAt={savedAt} />
        </div>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-xs text-content-secondary mb-1">Color Scheme</label>
            <select
              value={d.color_scheme}
              onChange={(e) => patch({ color_scheme: e.target.value })}
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            >
              <option value="eccentricity">Eccentricity</option>
              <option value="fwhm">FWHM</option>
              <option value="uniform">Uniform (green)</option>
            </select>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Line Width</label>
            <select
              value={d.line_width}
              onChange={(e) => patch({ line_width: parseInt(e.target.value, 10) })}
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            >
              <option value="1">1 (thin)</option>
              <option value="2">2 (medium)</option>
              <option value="3">3 (thick)</option>
            </select>
          </div>
        </div>

        <Checkbox
          checked={d.show_direction_tick}
          onChange={(v) => patch({ show_direction_tick: v })}
          label="Show direction tick on elongated stars"
        />

        <div className="grid grid-cols-2 gap-4">
          {numberField('ecc_good', 'Eccentricity Good (<)', 0, 1, 0.05)}
          {numberField('ecc_warn', 'Eccentricity Warn (>)', 0, 1, 0.05)}
          {numberField('fwhm_good', 'FWHM Good (ratio <)', 0.5, 5, 0.1)}
          {numberField('fwhm_warn', 'FWHM Warn (ratio >)', 0.5, 10, 0.1)}
          {numberField('ellipse_scale', 'Ellipse Scale (×FWHM)', 0.5, 4, 0.1)}
          {numberField('min_radius', 'Min Radius (px)', 1, 30, 1)}
          {numberField('max_radius', 'Max Radius (px)', 10, 200, 5)}
        </div>
      </div>
    </SettingsSection>
  );
}
