import { Fragment, type ReactNode } from 'react';
import { AlertTriangle, ChevronsDownUp, ChevronsUpDown, Info } from 'lucide-react';
import { Chip } from '../ui';
import { formatBytes } from '../duplicates/presentation';
import { counted, factText, factTitle, fmt } from './planLabels';
import type { McpPlanView, SummaryCount } from '../../types/mcp';

/**
 * What stays put above the list of the plan window (mockup "Design notes"): one stats row in the Duplicates toolbar
 * style, the service facts as chips, and the one explanatory sentence. The counts come from the plan's `summary`; the
 * headline ("N files to the Black Hole · size will free") follows the ticks.
 */
export interface PlanHeaderProps {
  plan: McpPlanView;
  /** The ticked items and their bytes: the live headline. */
  live: { items: number; bytes: number };
  hasSizes: boolean;
  /** The stats row is shown for a plan that is looked over or being applied, not for a finished or failed one. */
  showStats: boolean;
  /** The plan's expiry as a chip: only while it waits. */
  showExpiry: boolean;
  /** Black Hole groups that still need a copy that stays; this follows the user's picks, so it is not the summary's one. */
  needKeeper: number;
  /** `Collapse all` when something is open, `Expand all` otherwise; `null` hides the button. */
  toggleAll: { anyOpen: boolean; onClick: () => void } | null;
}

const sep = <span aria-hidden className="mx-2 text-border">·</span>;

const plural = (c: SummaryCount) => (c.count === 1 ? c.labelOne : c.label);

function Count({ n, label, tone }: { n: number | string; label: string; tone?: 'ok' | 'err' }) {
  const color = tone === 'ok' ? 'text-success' : tone === 'err' ? 'text-error' : 'text-content';
  return (
    <>
      <span className={color}>{typeof n === 'number' ? fmt(n) : n}</span>
      <span className="text-content-muted"> {label}</span>
    </>
  );
}

/** The units of what Apply does, per kind. */
function headline(plan: McpPlanView, live: { items: number; bytes: number }, hasSizes: boolean): ReactNode[] {
  const n = live.items;
  if (plan.kind === 'black_hole') {
    const out = [<Count key="n" n={n} label={`${n === 1 ? 'file' : 'files'} to the Black Hole`} tone="err" />];
    if (hasSizes) out.push(<Count key="b" n={formatBytes(live.bytes)} label="will free" tone="ok" />);
    return out;
  }
  if (plan.kind === 'metadata') return [<Count key="n" n={n} label={`${n === 1 ? 'value' : 'values'} to write`} tone="ok" />];
  return [<Count key="n" n={n} label={`${n === 1 ? 'frame' : 'frames'} to solve`} tone="ok" />];
}

export function PlanHeader({ plan, live, hasSizes, showStats, showExpiry, needKeeper, toggleAll }: PlanHeaderProps) {
  const { counts, leftOut, facts, explain } = plan.summary;
  // The summary's "needs a copy that stays" is the plan's state at preparation; the live count replaces it.
  const warnings: string[] = plan.summary.warnings
    .filter((w) => !/copy that stays/.test(w.label))
    .map((w) => counted(w.count, w.labelOne, w.label));
  if (needKeeper > 0) warnings.push(counted(needKeeper, 'group needs a copy that stays', 'groups need a copy that stays'));

  const countSegs = counts.map((c, i) => <Count key={`c${i}`} n={c.count} label={plural(c)} />);
  const leftSegs = leftOut.map((c, i) => (
    <Fragment key={`l${i}`}>
      {i === 0 && <span className="text-content-faint">Left out: </span>}
      <Count n={c.count} label={plural(c)} />
    </Fragment>
  ));
  const segs: ReactNode[] = [
    // A Black Hole plan names its groups first; the others lead with what the plan will do.
    ...(plan.kind === 'black_hole' ? [...countSegs, ...headline(plan, live, hasSizes)] : [...headline(plan, live, hasSizes), ...countSegs]),
    ...leftSegs,
  ];
  if (warnings.length > 0) {
    segs.push(
      <span key="warn" className="inline-flex items-center gap-1 text-warning">
        <AlertTriangle size={11} aria-hidden />
        {warnings.join(' · ')}
      </span>,
    );
  }

  const info = `${plan.source.build} ${plan.source.version} · catalog ${plan.source.catalog}`;
  const expiry = showExpiry && plan.expiresAt ? `Expires ${plan.expiresAt}` : null;

  return (
    <div className="shrink-0" data-testid="plan-header">
      {showStats && (
        <div className="flex items-center gap-2.5">
          <div data-testid="plan-stats" className="min-w-0 flex-1 text-xs text-content-secondary">
            {segs.map((s, i) => (
              <Fragment key={i}>
                {i > 0 && sep}
                {s}
              </Fragment>
            ))}
          </div>
          {toggleAll && (
            <button
              type="button"
              onClick={toggleAll.onClick}
              className="inline-flex h-7 items-center gap-1 whitespace-nowrap rounded-lg border border-border bg-surface-hover px-2.5 text-xs font-medium text-content-secondary hover:bg-surface-elevated hover:text-content focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
            >
              {toggleAll.anyOpen ? <ChevronsDownUp size={12} aria-hidden /> : <ChevronsUpDown size={12} aria-hidden />}
              {toggleAll.anyOpen ? 'Collapse all' : 'Expand all'}
            </button>
          )}
        </div>
      )}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-1.5 gap-y-1">
        {facts.map((f) => (
          <Chip key={f.label} tone={f.tone === 'ok' ? 'ok' : f.tone === 'warn' ? 'warn' : 'mute'} title={factTitle(f)}>
            {factText(f)}
          </Chip>
        ))}
        {expiry && <Chip tone="mute">{expiry}</Chip>}
        {/* The build and catalog the plan came from: the assistant's own answer already says them. */}
        <span title={info} aria-label={info} role="note" className="ml-0.5 inline-flex text-content-faint">
          <Info size={13} aria-hidden />
        </span>
      </div>
      {explain && <p className="mt-[5px] max-w-[78ch] text-[11px] text-content-faint">{explain}</p>}
      <div className="h-2" aria-hidden />
    </div>
  );
}
