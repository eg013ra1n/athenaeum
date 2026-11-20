import { useState } from 'react';
import { ChevronDown, ChevronRight, Loader2, Lock } from 'lucide-react';
import { Chip } from '../ui';
import { Checkbox } from '../settings/Checkbox';
import type { McpPlanView, PlanGroup, PlanItem, PlanReport, PlanSelection, ReportLine } from '../../types/mcp';

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
      {!plan.report &&
        state.kind !== 'applied' &&
        plan.groups.map((g, i) => (
          <Group
            key={g.id}
            group={g}
            chosen={selection.groups.find((s) => s.groupId === g.id)?.chosen ?? null}
            included={selection.groups.find((s) => s.groupId === g.id)?.included ?? []}
            columns={plan.columns}
            readOnly={readOnly}
            defaultOpen={plan.groups.length <= OPEN_ALL_UP_TO || i === 0}
            onToggleItem={onToggleItem}
            onToggleGroup={onToggleGroup}
            onChoose={onChoose}
          />
        ))}
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

function Group({
  group,
  chosen,
  included,
  columns,
  readOnly,
  defaultOpen,
  onToggleItem,
  onToggleGroup,
  onChoose,
}: {
  group: PlanGroup;
  chosen: string | null;
  included: string[];
  columns: McpPlanView['columns'];
  readOnly: boolean;
  defaultOpen: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onToggleGroup: (groupId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const tickable = group.items.filter((i) => i.locked === null && i.id !== chosen);
  const ticked = tickable.filter((i) => included.includes(i.id)).length;
  const bodyId = `plan-${group.id}`;
  return (
    <section className="rounded-md border border-border bg-surface">
      <div className="flex items-start gap-2 px-2.5 py-2">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={bodyId}
          aria-label={`${open ? 'Collapse' : 'Expand'} ${group.title}`}
          onClick={() => setOpen((o) => !o)}
          className="mt-px shrink-0 rounded text-content-muted hover:text-content focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </button>
        <Checkbox
          size="sm"
          checked={tickable.length > 0 && ticked === tickable.length}
          indeterminate={ticked > 0 && ticked < tickable.length}
          disabled={readOnly || tickable.length === 0}
          onChange={() => onToggleGroup(group.id)}
          ariaLabel={`Include all in ${group.title}`}
        />
        <div className="min-w-0 flex-1">
          <h3 className="text-[12.5px] font-semibold text-content">{group.title}</h3>
          {group.note && <p className="mt-0.5 text-content-muted">{group.note}</p>}
        </div>
        <span className="shrink-0 text-[11px] text-content-muted">
          {ticked} of {tickable.length} to move
        </span>
      </div>
      {open && (
        <div id={bodyId} className="overflow-x-auto border-t border-border">
          <table className="w-full border-collapse text-left text-[11.5px]">
            <thead>
              <tr className="text-[11px] text-content-muted">
                <th className="w-8 py-1 pl-2.5 pr-1 font-normal">
                  <span className="sr-only">Include</span>
                </th>
                {group.chooseOne !== null && <th className="w-24 px-1.5 py-1 font-normal">{group.chooseOne}</th>}
                <th className="px-1.5 py-1 font-normal">File</th>
                {columns.map((c) => (
                  <th key={c.key} className="whitespace-nowrap px-1.5 py-1 font-normal">
                    {c.label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {group.items.map((item) => (
                <Row
                  key={item.id}
                  group={group}
                  item={item}
                  columns={columns}
                  isChosen={item.id === chosen}
                  isIncluded={included.includes(item.id)}
                  readOnly={readOnly}
                  onToggleItem={onToggleItem}
                  onChoose={onChoose}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function Row({
  group,
  item,
  columns,
  isChosen,
  isIncluded,
  readOnly,
  onToggleItem,
  onChoose,
}: {
  group: PlanGroup;
  item: PlanItem;
  columns: McpPlanView['columns'];
  isChosen: boolean;
  isIncluded: boolean;
  readOnly: boolean;
  onToggleItem: (groupId: string, itemId: string) => void;
  onChoose: (groupId: string, itemId: string) => void;
}) {
  const locked = item.locked !== null;
  return (
    <tr className={`border-t border-border/60 align-top ${locked ? 'opacity-60' : ''}`}>
      <td className={`border-l-2 py-1.5 pl-2 pr-1 ${isChosen ? 'border-success' : 'border-transparent'}`}>
        <Checkbox
          size="sm"
          checked={isIncluded}
          disabled={readOnly || locked || isChosen}
          onChange={() => onToggleItem(group.id, item.id)}
          ariaLabel={`Include ${item.fileName}`}
        />
      </td>
      {group.chooseOne !== null && (
        <td className="px-1.5 py-1.5">
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              name={`keep-${group.id}`}
              className="accent-[theme(colors.accent.DEFAULT)]"
              checked={isChosen}
              disabled={readOnly || !item.candidate}
              onChange={() => onChoose(group.id, item.id)}
              aria-label={`${group.chooseOne}: ${item.fileName}`}
            />
            {isChosen && <Chip tone="ok">stays</Chip>}
          </label>
        </td>
      )}
      <td className="min-w-[220px] max-w-[320px] px-1.5 py-1.5">
        <div className="truncate font-medium text-content" title={item.fileName}>
          {item.fileName}
        </div>
        <div className="truncate text-content-muted" title={item.folder}>
          {item.folder}
        </div>
        {locked ? (
          <div className="mt-0.5 flex items-start gap-1 text-content-muted">
            <Lock size={11} className="mt-[3px] shrink-0" aria-hidden />
            <span>{item.locked}</span>
          </div>
        ) : (
          <div className="mt-0.5 text-content-muted">
            <span>{item.action}</span>
            {item.evidence && <span className="block italic">{item.evidence}</span>}
          </div>
        )}
      </td>
      {columns.map((c) => (
        <td key={c.key} className="whitespace-nowrap px-1.5 py-1.5 text-content-secondary">
          {item.values[c.key] ?? ''}
        </td>
      ))}
    </tr>
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
