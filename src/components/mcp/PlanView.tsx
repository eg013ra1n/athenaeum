import { Loader2 } from 'lucide-react';
import { Chip } from '../ui';
import { PlanGroupCard } from './PlanGroupCard';
import { factKeys } from './planLabels';
import type { McpPlanView, PlanReport, PlanSelection, ReportLine } from '../../types/mcp';

/** The body of the plan window: state text, groups of items with their ticks, or the report. Presentational. */
export interface PlanViewProps {
  plan: McpPlanView;
  selection: PlanSelection;
  /** Ticks and the keeper cannot be changed (the plan is not waiting, or is superseded). */
  readOnly: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onToggleGroup: (groupId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}

/** More groups than this start collapsed, so a long plan opens as a list of titles. */
const OPEN_ALL_UP_TO = 6;

export function PlanView({ plan, selection, readOnly, onToggleItem, onToggleGroup, onChoose }: PlanViewProps) {
  const state = plan.state;
  const facts = factKeys(plan);
  return (
    <div className="space-y-3">
      {plan.notes.length > 0 && (
        <ul className="space-y-0.5 text-content-muted">
          {plan.notes.map((n, i) => (
            <li key={i}>{n}</li>
          ))}
        </ul>
      )}
      {state.kind === 'preparing' && <Preparing done={state.done} total={state.total} />}
      {state.kind === 'applying' && (
        <p className="flex items-center gap-2 text-content-secondary" role="status">
          <Loader2 size={14} className="animate-spin" aria-hidden /> {applyingWords(plan.kind)}
        </p>
      )}
      {state.kind === 'applied' && plan.kind === 'plate_solve' && (
        <p className="text-content-secondary" role="status">
          {startedWords(plan.report)}
        </p>
      )}
      {state.kind === 'expired' && (
        <p className="text-warning">This plan expired before it was applied. Ask the assistant to prepare it again.</p>
      )}
      {state.kind === 'cancelled' && <p className="text-content-muted">This plan was cancelled. Nothing was changed.</p>}
      {state.kind === 'failed' && (
        <p className="text-error">
          {plan.kind === 'plate_solve'
            ? 'Nothing was started'
            : plan.report
              ? 'This plan failed'
              : 'Preparing the plan failed'}
          : {state.error}
        </p>
      )}
      {state.kind === 'failed' && plan.report && isEmptyReport(plan.report) && plan.kind !== 'plate_solve' && (
        <p className="text-content-muted">{plan.kind === 'metadata' ? 'Nothing was written.' : 'No file was recorded as moved.'}</p>
      )}
      {plan.report && <Report report={plan.report} kind={plan.kind} />}
      {!plan.report && state.kind !== 'applied' && plan.groups.length > 0 && (
        // The same list the Duplicates page puts its group cards in.
        <div className="divide-y divide-border/40 overflow-hidden rounded-lg border border-border bg-surface">
          {plan.groups.map((g, i) => {
            const sel = selection.groups.find((x) => x.groupId === g.id);
            return (
              <PlanGroupCard
                key={g.id}
                kind={plan.kind}
                group={g}
                facts={facts}
                chosen={sel?.chosen ?? null}
                included={sel?.included ?? []}
                readOnly={readOnly}
                defaultOpen={plan.groups.length <= OPEN_ALL_UP_TO || i === 0}
                onToggleItem={onToggleItem}
                onToggleGroup={onToggleGroup}
                onChoose={onChoose}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

/** What the window says while a plan is applied: each kind of plan does something else. */
function applyingWords(kind: McpPlanView['kind']): string {
  if (kind === 'metadata') return 'Writing the values to the catalog…';
  if (kind === 'plate_solve') return 'Starting the plate solve…';
  return 'The plan is being applied. Files are being moved.';
}

/** A plate-solve plan that was applied started a run; it solved nothing yet. */
function startedWords(report: PlanReport | null): string {
  const runId = report?.runId ?? null;
  if (!report || !runId) return 'No plate solve was started.';
  const n = report.applied.length;
  return `Started a plate solve of ${n} frame${n === 1 ? '' : 's'} (run ${runId}). Its progress shows in the plate-solve panel.`;
}

const isEmptyReport = (r: PlanReport) => r.applied.length + r.skipped.length + r.failed.length === 0;

function Preparing({ done, total }: { done: number; total: number }) {
  const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  return (
    <div role="status" className="space-y-1.5">
      <p className="text-content-secondary">
        Preparing the plan: {done} of {total} checked
      </p>
      <div className="h-1 overflow-hidden rounded bg-surface-hover" aria-hidden>
        <div className="h-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

function Report({ report, kind }: { report: PlanReport; kind: McpPlanView['kind'] }) {
  return (
    <div className="space-y-3">
      {/* A plate-solve plan only started a run: its lines are in the sentence above, not an "applied" count. */}
      {kind !== 'plate_solve' && <ReportList lines={report.applied} tone="ok" word="applied" />}
      <ReportList lines={report.skipped} tone="warn" word="skipped" />
      <ReportList lines={report.failed} tone="err" word="failed" />
    </div>
  );
}

function ReportList({ lines, tone, word }: { lines: ReportLine[]; tone: 'ok' | 'warn' | 'err'; word: string }) {
  if (lines.length === 0) return null;
  return (
    <section>
      <Chip tone={tone}>
        {lines.length} {word}
      </Chip>
      <ul className="mt-1.5 divide-y divide-border/60 rounded-md border border-border bg-surface">
        {lines.map((l) => (
          <li key={l.itemId} className="px-2.5 py-1.5">
            <div className="truncate font-medium text-content" title={l.fileName}>
              {l.fileName} <span className="font-normal text-content-muted">{l.folder}</span>
            </div>
            <div className="text-content-muted">{l.reason}</div>
          </li>
        ))}
      </ul>
    </section>
  );
}
