import { Check, Layers } from 'lucide-react';
import type { CalibrationSetWithScore, ParameterVerdict } from '../../types/models';
import { blockersOf, describeDifference } from './pickerModel';

const pad = (n: number) => String(n).padStart(2, '0');
const day = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const clock = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;

/**
 * When the set was shot, close enough to choose between two of them.
 *
 * The date alone is not enough: two flat sets from the same night — dusk and
 * dawn, or before and after a filter change — are the common case, and the
 * clock is what tells them apart. Shown in local time, like every other
 * timestamp in the app.
 */
function spanLabel(start: string | null, end: string | null): string {
  if (!start) return 'Undated';
  const from = new Date(start);
  if (Number.isNaN(from.getTime())) return start.slice(0, 10);
  const to = end ? new Date(end) : null;
  if (!to || Number.isNaN(to.getTime())) return `${day(from)} ${clock(from)}`;
  if (day(from) === day(to)) {
    // One session: the date once, then the window it occupied.
    return clock(from) === clock(to)
      ? `${day(from)} ${clock(from)}`
      : `${day(from)} ${clock(from)}–${clock(to)}`;
  }
  // Across midnight (or longer): keep both dates, and the times that place them.
  const toDay = day(from).slice(0, 4) === day(to).slice(0, 4) ? day(to).slice(5) : day(to);
  return `${day(from)} ${clock(from)} → ${toDay} ${clock(to)}`;
}

/** The facts that identify a set at a glance — never the ones that differ. */
function identityFacts(set: CalibrationSetWithScore['set']): string[] {
  const facts = [set.instrume || 'Unknown camera'];
  if (set.exptime != null) facts.push(`${set.exptime} s`);
  if (set.ccd_temp != null) facts.push(`${set.ccd_temp.toFixed(1)} °C`);
  if (set.filter) facts.push(set.filter);
  if (set.binning) facts.push(set.binning);
  return facts;
}

/**
 * One candidate.
 *
 * The card answers, top to bottom: WHICH set is this (its nights and weight),
 * WHAT is it (the facts that identify it), and HOW does it differ from what
 * you need. Only the last line is coloured — the differences are the reason a
 * person is reading this list at all, and everything that agrees is context,
 * not news.
 */
export function CandidateCard({
  candidate,
  selected,
  isCurrent,
  onSelect,
}: {
  candidate: CalibrationSetWithScore;
  selected: boolean;
  isCurrent: boolean;
  onSelect: () => void;
}) {
  const { set, match_score, compatible, parameters } = candidate;
  const differences: ParameterVerdict[] = blockersOf(parameters);
  const percent = Math.round(match_score * 100);

  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`w-full text-left rounded-lg border px-3 py-2 transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-accent ${
        selected
          ? 'border-accent bg-accent/10'
          : 'border-border bg-surface-elevated/40 hover:bg-surface-hover'
      }`}
    >
      {/* Which set: its nights, and how much material it carries. */}
      <div className="flex items-baseline gap-2">
        {selected && <Check size={13} className="flex-shrink-0 text-accent" />}
        <span className="font-mono text-sm text-content">{spanLabel(set.date_start, set.date_end)}</span>
        <span className="text-xs text-content-muted">
          {set.frame_count} frame{set.frame_count === 1 ? '' : 's'}
        </span>
        {set.is_master && (
          <span className="inline-flex items-center gap-1 rounded bg-accent/15 px-1.5 py-0.5 text-xs font-medium text-accent">
            <Layers size={10} />
            Master
          </span>
        )}
        <span className="ml-auto flex items-baseline gap-2">
          {isCurrent && (
            <span className="rounded bg-info-muted px-1.5 py-0.5 text-xs font-medium text-info">
              Linked now
            </span>
          )}
          <span
            className={`text-xs tabular-nums ${compatible ? 'text-content-secondary' : 'text-content-muted'}`}
            title={
              compatible
                ? 'How close this set is on date, temperature and exposure'
                : 'Closeness only — this set does not satisfy your matching rules'
            }
          >
            {percent}%
          </span>
        </span>
      </div>

      {/* What it is. Everything here agrees with what you need, or is neutral. */}
      <div className="mt-0.5 text-xs text-content-muted">{identityFacts(set).join(' · ')}</div>

      {/* How it differs — the only coloured line on the card. */}
      {differences.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1">
          {differences.map(p => {
            const d = describeDifference(p);
            const unknown = p.status === 'unknown';
            return (
              <span key={p.name} className="inline-flex items-baseline gap-1.5 text-xs">
                <span className={unknown ? 'text-content-muted' : 'text-warning'}>
                  {unknown ? '?' : '✕'}
                </span>
                <span className="text-content-muted">{d.label}</span>
                <span className={unknown ? 'text-content-muted' : 'text-warning'}>{d.change}</span>
                {d.limit && <span className="text-content-muted">{d.limit}</span>}
              </span>
            );
          })}
        </div>
      )}
    </button>
  );
}
