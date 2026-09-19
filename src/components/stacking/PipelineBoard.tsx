import { StageRow, type StageRowToggle } from './StageRow';
import { rowState, stageSummary, type BoardStage } from './stageSummary';
import type { Stage, StackingConfig, StackingPlan } from '../../types/stacking';
import type { RunOutcome, RunProgress } from '../../hooks/useStackingRuns';

const ROWS: { stage: BoardStage; index: number; label: string }[] = [
  { stage: 'masters', index: 0, label: 'Masters' },
  { stage: 'calibrate', index: 1, label: 'Calibrate' },
  { stage: 'debayer', index: 2, label: 'Debayer' },
  { stage: 'measure', index: 3, label: 'Measure & select' },
  { stage: 'reference', index: 4, label: 'Reference' },
  { stage: 'register', index: 5, label: 'Register' },
  { stage: 'normalize', index: 6, label: 'Local normalization' },
  { stage: 'integrate', index: 7, label: 'Integrate' },
  { stage: 'drizzle', index: 8, label: 'Drizzle' },
  { stage: 'output', index: 9, label: 'Output' },
];

export interface PipelineBoardProps {
  plan: StackingPlan | null;
  config: StackingConfig;
  progress: RunProgress | undefined;
  outcome: RunOutcome | undefined;
  /** The `outcome` run's own finished-stage list (Plan 5b final fix wave,
   *  click-through item A5) — `null`/`undefined` when the caller doesn't
   *  have it loaded yet, or has a DIFFERENT run's detail loaded, in which
   *  case `rowState` falls back to its old coarse per-row read. See
   *  `rowState`'s own doc comment for the derivation. */
  finishedStages?: readonly Stage[] | null;
  selectedStage: BoardStage;
  onSelectStage: (stage: BoardStage) => void;
  /** The Drizzle row's on/off toggle (M3 Task 6 — live; previously
   *  disabled with a "coming in M3" note). Patches `config.drizzle.enabled`
   *  through the draft-config path, so the preset chip flips to Custom.
   *  The Register row used to carry a write-registered-frames toggle of
   *  the same shape; since Task 6a the run always writes the registered
   *  frame (the field is inert), so that row is read-only now — see
   *  `RegisterPanel.tsx`'s note. The Normalize row's LN toggle edits
   *  `config` directly through the inspector panel, so it never carried
   *  one of these either. */
  onToggleDrizzle: (checked: boolean) => void;
}

/** The nine-row pipeline board (spec §11.1). Every row's state and summary
 *  text come from the pure functions in `stageSummary.ts` — this component
 *  only lays them out and wires the one live toggle (on/off on Drizzle). */
export function PipelineBoard({
  plan,
  config,
  progress,
  outcome,
  finishedStages,
  selectedStage,
  onSelectStage,
  onToggleDrizzle,
}: PipelineBoardProps) {
  return (
    <div className="bg-surface-elevated rounded-lg p-2 space-y-0.5">
      {ROWS.map(({ stage, index, label }) => {
        let toggle: StageRowToggle | undefined;
        if (stage === 'drizzle') {
          toggle = {
            checked: config.drizzle.enabled,
            onChange: onToggleDrizzle,
            // B13 (M3 final fix wave, M10): the row is already titled
            // "8 · Drizzle" — "drizzle" next to its own toggle just restated
            // the row. "enabled" says what the toggle does without
            // repeating the row's own name (the register row's "write
            // registered frames" is the model: it names the ACTION, not
            // the stage).
            label: 'enabled',
          };
        }

        return (
          <StageRow
            key={stage}
            index={index}
            stage={stage}
            label={label}
            state={rowState(stage, plan, progress, outcome, config, finishedStages)}
            summary={stageSummary(stage, config, plan)}
            progress={progress?.stage === stage ? progress : undefined}
            selected={selectedStage === stage}
            onSelect={() => onSelectStage(stage)}
            toggle={toggle}
          />
        );
      })}
    </div>
  );
}
