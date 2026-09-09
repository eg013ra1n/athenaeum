import { Circle, CircleDot, CheckCircle2, XCircle, MinusCircle, AlertCircle } from 'lucide-react';
import type { BoardStage, RowState } from './stageSummary';
import type { RunProgress } from '../../hooks/useStackingRuns';

/** A stage row's optional toggle (LN / drizzle / write-registered-frames). */
export interface StageRowToggle {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** Short text before the checkbox, e.g. "write registered frames". */
  label?: string;
  /** e.g. "coming in M2" for the still-disabled LN/drizzle toggles. */
  disabled?: boolean;
  note?: string;
}

export interface StageRowProps {
  index: number;
  stage: BoardStage;
  label: string;
  state: RowState;
  summary: string;
  progress?: RunProgress;
  selected: boolean;
  onSelect: () => void;
  toggle?: StageRowToggle;
}

const STATE_LABEL: Record<RowState, string> = {
  ready: 'Ready',
  blocked: 'Blocked',
  stale: 'Stale',
  queued: 'Queued',
  running: 'Running',
  done: 'Done',
  skipped: 'Skipped',
  failed: 'Failed',
  off: 'Off',
};

const STATE_CLASSES: Record<RowState, string> = {
  ready: 'text-content-muted',
  blocked: 'text-error',
  stale: 'text-warning',
  queued: 'text-content-muted',
  running: 'text-accent',
  done: 'text-success',
  skipped: 'text-content-muted',
  failed: 'text-error',
  off: 'text-content-muted',
};

function StateGlyph({ state }: { state: RowState }) {
  const cls = STATE_CLASSES[state];
  switch (state) {
    case 'running': return <CircleDot size={14} className={cls} />;
    case 'done': return <CheckCircle2 size={14} className={cls} />;
    case 'failed': return <XCircle size={14} className={cls} />;
    case 'blocked': return <AlertCircle size={14} className={cls} />;
    case 'stale': return <AlertCircle size={14} className={cls} />;
    case 'off':
    case 'skipped':
      return <MinusCircle size={14} className={cls} />;
    default:
      return <Circle size={14} className={cls} />;
  }
}

/** One row of the pipeline board (`PipelineBoard.tsx`). Presentational only —
 *  all state derives from `stageSummary.ts`'s pure `stageSummary`/`rowState`.
 *
 *  Markup note (fix round 1, Important #3): the optional toggle is a real
 *  `<label><input type="checkbox">` pair, which is invalid HTML nested
 *  inside a `<button>` (Firefox/Safari suppress the control). The row is a
 *  plain `<div>`; the row-select action is its own `<button>` covering only
 *  the glyph/index/label/summary — the toggle and the state chip are
 *  siblings outside it, not descendants. */
export function StageRow({ index, stage, label, state, summary, progress, selected, onSelect, toggle }: StageRowProps) {
  return (
    <div
      data-stage={stage}
      className={`w-full flex items-center gap-3 px-3 py-2 rounded-lg transition-colors ${
        selected ? 'bg-surface-hover' : 'hover:bg-surface-hover/50'
      }`}
    >
      <button
        type="button"
        onClick={onSelect}
        className="flex-1 min-w-0 flex items-center gap-3 text-left"
      >
        <StateGlyph state={state} />
        <span className="w-5 shrink-0 text-xs text-content-muted tabular-nums">{index}</span>
        <span className="w-40 shrink-0 text-sm font-medium text-content truncate">{label}</span>

        <span className="flex-1 min-w-0 text-xs text-content-muted truncate">
          {state === 'running' && progress ? (
            <span className="flex items-center gap-2">
              <span className="flex-1 h-1.5 rounded-full bg-surface overflow-hidden max-w-[120px]">
                <span
                  className="block h-full bg-accent"
                  style={{ width: `${Math.max(0, Math.min(100, progress.percent))}%` }}
                />
              </span>
              <span className="tabular-nums">
                {progress.current} / {progress.total} · {Math.round(progress.percent)}%
              </span>
              {progress.groupKey && <span className="truncate">{progress.groupKey}</span>}
            </span>
          ) : (
            summary
          )}
        </span>
      </button>

      {toggle && (
        <label
          className="flex items-center gap-1.5 text-xs text-content-muted shrink-0"
          title={toggle.note}
        >
          {toggle.label && <span>{toggle.label}</span>}
          <input
            type="checkbox"
            checked={toggle.checked}
            disabled={toggle.disabled}
            onChange={(e) => toggle.onChange(e.target.checked)}
            className="w-3.5 h-3.5 rounded border-border bg-surface-hover text-accent focus:ring-accent disabled:opacity-50"
          />
          {toggle.note && <span className="italic">{toggle.note}</span>}
        </label>
      )}

      <span className={`w-16 shrink-0 text-right text-xs font-medium ${STATE_CLASSES[state]}`}>
        {STATE_LABEL[state]}
      </span>
    </div>
  );
}
