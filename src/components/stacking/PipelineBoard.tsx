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
  /** The Register row's write-registered-frames toggle. Drizzle's own toggle
   *  stays read-only until M3 (spec §14 item 12); the Normalize row's LN
   *  toggle went live in M2 Task 8 — it edits `config` directly through the
   *  inspector panel, so the board row carries no toggle of its own any
   *  more. */
  onToggleWriteRegisteredFrames: (checked: boolean) => void;
}

/** The nine-row pipeline board (spec §11.1). Every row's state and summary
 *  text come from the pure functions in `stageSummary.ts` — this component
 *  only lays them out and wires the one live toggle. */
export function PipelineBoard({
  plan,
  config,
  progress,
  outcome,
  finishedStages,
  selectedStage,
  onSelectStage,
  onToggleWriteRegisteredFrames,
}: PipelineBoardProps) {
  return (
    <div className="bg-surface-elevated rounded-lg p-2 space-y-0.5">
      {ROWS.map(({ stage, index, label }) => {
        let toggle: StageRowToggle | undefined;
        if (stage === 'drizzle') {
          toggle = {
            checked: config.drizzle.enabled,
            onChange: () => {},
            disabled: true,
            note: 'coming in M3',
          };
        } else if (stage === 'register') {
          toggle = {
            checked: config.registration.writeRegisteredFrames,
            onChange: onToggleWriteRegisteredFrames,
            label: 'write registered frames',
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
