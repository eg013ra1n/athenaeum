// The inspector slot next to the pipeline board — one panel per selected
// board row. Every panel is presentational: it reads `config`, calls
// `onChange(next)` with a whole new `StackingConfig` on every edit, and
// never persists on its own — `StackingTab` owns the debounce and the
// `set_stacking_config` write (spec §11.2 "submit state, never a re-read").

import { MastersPanel } from './panels/MastersPanel';
import { CalibratePanel } from './panels/CalibratePanel';
import { PreparePanel } from './panels/PreparePanel';
import { DebayerPanel } from './panels/DebayerPanel';
import { MeasurePanel } from './panels/MeasurePanel';
import { ReferencePanel } from './panels/ReferencePanel';
import { RegisterPanel } from './panels/RegisterPanel';
import { NormalizePanel } from './panels/NormalizePanel';
import { IntegratePanel } from './panels/IntegratePanel';
import { DrizzlePanel } from './panels/DrizzlePanel';
import { OutputPanel } from './panels/OutputPanel';
import { boardMode, stageLabel, type BoardStage } from './stageSummary';
import type { StackingConfig, StackingPlan } from '../../types/stacking';

export interface StageInspectorProps {
  stage: BoardStage;
  config: StackingConfig;
  onChange: (next: StackingConfig) => void;
  plan: StackingPlan | null;
  disabled?: boolean;
  /** The built-in Default preset's config — every panel's help line states
   *  its field's default by reading this, never a hard-coded literal (plan
   *  5b Task 3 "Decisions" item 1). */
  presetDefault: StackingConfig;
  /** Measure panel's "Re-measure" button (`startRun(setId, config,
   *  'measure')`), wired by `StackingTab` since it needs `useStackingContext`.
   *  Task 5: `StackingSection` (Settings → Stacking) binds this inspector to
   *  the GLOBAL defaults, which have no run to re-measure — both props are
   *  absent there, and `MeasurePanel` hides the button entirely rather than
   *  rendering it disabled with nothing to call. */
  onRemeasure?: () => void;
  remeasureDisabled?: boolean;
  /** Task 5: `'perSet'` (default) is the frame-set Stacking tab's own usage,
   *  unchanged from Task 3/4. `'global'` is `StackingSection`'s usage —
   *  `config.paths` stays `null`/`null` in the saved global defaults (a
   *  per-set folder OVERRIDE is meaningless there), so the Output panel
   *  hides its two folder-override cards; everything else in every panel
   *  (including Output's cleanup policy / format) is unchanged. */
  mode?: 'perSet' | 'global';
}

/** The number in front of a stage's title; the name comes from `stageLabel`, so the board, this title and the
 *  Re-run menu never disagree. */
const STAGE_NUMBER: Record<BoardStage, number> = {
  masters: 0,
  calibrate: 1,
  debayer: 2,
  measure: 3,
  reference: 4,
  register: 5,
  normalize: 6,
  integrate: 7,
  drizzle: 8,
  output: 9,
};

export function StageInspector({
  stage,
  config,
  onChange,
  plan,
  disabled,
  presetDefault,
  onRemeasure,
  remeasureDisabled,
  mode = 'perSet',
}: StageInspectorProps) {
  // Wave 6c: a project and an attested set show Prepare in place of Calibrate (same stage key, same slot).
  const board = boardMode(plan);
  const patchCalibration = (patch: Partial<StackingConfig['calibration']>) => {
    onChange({ ...config, calibration: { ...config.calibration, ...patch } });
  };

  return (
    <div className="bg-surface-elevated rounded-lg p-4 h-full">
      <h4 className="text-sm font-medium text-content mb-3">{`${STAGE_NUMBER[stage]} · ${stageLabel(stage, board)}`}</h4>
      {stage === 'masters' && <MastersPanel plan={plan} />}
      {stage === 'calibrate' &&
        (board === 'prepare' ? (
          <PreparePanel config={config} plan={plan} disabled={disabled} onPatchCalibration={patchCalibration} />
        ) : (
          <CalibratePanel config={config} onChange={onChange} plan={plan} disabled={disabled} />
        ))}
      {stage === 'debayer' && <DebayerPanel config={config} plan={plan} />}
      {stage === 'measure' && (
        <MeasurePanel
          config={config}
          onChange={onChange}
          disabled={disabled}
          defaults={presetDefault}
          onRemeasure={onRemeasure}
          remeasureDisabled={remeasureDisabled}
        />
      )}
      {stage === 'reference' && (
        <ReferencePanel config={config} onChange={onChange} plan={plan} disabled={disabled} />
      )}
      {stage === 'register' && (
        <RegisterPanel config={config} onChange={onChange} disabled={disabled} defaults={presetDefault} />
      )}
      {stage === 'normalize' && (
        <NormalizePanel config={config} onChange={onChange} plan={plan} disabled={disabled} defaults={presetDefault} />
      )}
      {stage === 'integrate' && (
        <IntegratePanel config={config} onChange={onChange} disabled={disabled} defaults={presetDefault} />
      )}
      {stage === 'drizzle' && (
        <DrizzlePanel
          config={config}
          onChange={onChange}
          disabled={disabled}
          defaults={presetDefault}
          plan={plan}
          mode={mode}
        />
      )}
      {stage === 'output' && (
        <OutputPanel config={config} onChange={onChange} plan={plan} disabled={disabled} mode={mode} />
      )}
    </div>
  );
}
