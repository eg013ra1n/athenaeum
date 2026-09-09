// The inspector slot next to the pipeline board — one panel per selected
// board row. Every panel is presentational: it reads `config`, calls
// `onChange(next)` with a whole new `StackingConfig` on every edit, and
// never persists on its own — `StackingTab` owns the debounce and the
// `set_stacking_config` write (spec §11.2 "submit state, never a re-read").

import { CalibratePanel } from './panels/CalibratePanel';
import { DebayerPanel } from './panels/DebayerPanel';
import { MeasurePanel } from './panels/MeasurePanel';
import { ReferencePanel } from './panels/ReferencePanel';
import { RegisterPanel } from './panels/RegisterPanel';
import { NormalizePanel } from './panels/NormalizePanel';
import { IntegratePanel } from './panels/IntegratePanel';
import { DrizzlePanel } from './panels/DrizzlePanel';
import { OutputPanel } from './panels/OutputPanel';
import type { BoardStage } from './stageSummary';
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
   *  'measure')`), wired by `StackingTab` since it needs `useStackingContext`. */
  onRemeasure: () => void;
  remeasureDisabled: boolean;
}

const STAGE_TITLE: Record<BoardStage, string> = {
  calibrate: '1 · Calibrate',
  debayer: '2 · Debayer',
  measure: '3 · Measure & select',
  reference: '4 · Reference',
  register: '5 · Register',
  normalize: '6 · Local normalization',
  integrate: '7 · Integrate',
  drizzle: '8 · Drizzle',
  output: '9 · Output',
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
}: StageInspectorProps) {
  return (
    <div className="bg-surface-elevated rounded-lg p-4 h-full">
      <h4 className="text-sm font-medium text-content mb-3">{STAGE_TITLE[stage]}</h4>
      {stage === 'calibrate' && (
        <CalibratePanel config={config} onChange={onChange} plan={plan} disabled={disabled} />
      )}
      {stage === 'debayer' && <DebayerPanel config={config} />}
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
        <NormalizePanel config={config} onChange={onChange} disabled={disabled} defaults={presetDefault} />
      )}
      {stage === 'integrate' && (
        <IntegratePanel config={config} onChange={onChange} disabled={disabled} defaults={presetDefault} />
      )}
      {stage === 'drizzle' && <DrizzlePanel config={config} />}
      {stage === 'output' && (
        <OutputPanel config={config} onChange={onChange} plan={plan} disabled={disabled} />
      )}
    </div>
  );
}
