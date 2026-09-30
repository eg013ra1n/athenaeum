import { useMemo, type JSX } from 'react';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import { distinctNames, peerLabel, sumRate } from '../exchange/state';
import { formatDurationPadded, formatRate } from '../format';
import { Button, Card, Chip, EmptyState, FilterDot, MemberDot } from '../../ui';
import { filterOrder } from './table/model';
import { deriveAttention, type AttentionTarget } from './attention';
import { useMemberColor } from './MemberColorsContext';
import type { Segment } from './MyFramesTab';
import type { MemberSummary, OwnFrameRow, ProjectFrameView, ThresholdRuleView } from '../../../types/models';

/**
 * Overview tab — the mockup's four cards: integration toward goal (member
 * coloured bars against the coordinator's goals), My contribution (three
 * tiles), Needs attention (one row per held-back cause, each with an action
 * that opens the right segment with a pre-set filter — `attention.ts`),
 * Exchange now and Quality thresholds. While `own` is still loading
 * (`null`), the contribution and attention cards read `Loading…`
 * (`Not available.` once `ownError` says the load failed).
 */

export interface OverviewTabProps {
  projectId: string;
  goals: Record<string, number> | null;
  members: MemberSummary[] | null;
  own: OwnFrameRow[] | null;
  /** `own` failed to load: the cards say so instead of `Loading…` (the
   *  shell shows the error line above the tab). */
  ownError?: boolean;
  /** The project's library frames (null until loaded). */
  library: ProjectFrameView[] | null;
  pending: number;
  canModerate: boolean;
  thresholds: ThresholdRuleView[];
  thresholdsVersion: number | null;
  onOpenSegment: (s: Segment) => void;
  onOpenTab: (t: string) => void;
  onAttention: (target: AttentionTarget) => void;
}

// Keys are the gate's METRIC_REGISTRY (crates/athenaeum-core/src/collab/gate.rs).
function thresholdLine(r: ThresholdRuleView): string {
  const v = typeof r.value === 'number' ? r.value : null;
  if (r.op === 'reject_if') return r.metricKey === 'not_trailed' ? 'Reject trailed frames' : `${r.metricKey} — reject if ${String(r.value)}`;
  const op = r.op === 'lte' ? '≤' : r.op === 'gte' ? '≥' : r.op;
  if (r.metricKey === 'fwhm_arcsec' && v !== null) return `FWHM ${op} ${v.toFixed(2)}″`;
  if (r.metricKey === 'eccentricity' && v !== null) return `Eccentricity ${op} ${v.toFixed(2)}`;
  if (r.metricKey === 'stars_detected' && v !== null) return `Stars ${op} ${v}`;
  if (r.metricKey === 'median_snr' && v !== null) return `SNR ${op} ${v}`;
  return `${r.metricKey} ${op} ${String(r.value)}`;
}

export default function OverviewTab({
  projectId,
  goals,
  members,
  own,
  ownError = false,
  library,
  pending,
  canModerate,
  thresholds,
  thresholdsVersion,
  onOpenSegment,
  onOpenTab,
  onAttention,
}: OverviewTabProps): JSX.Element {
  const { state } = useCollabExchange();
  const colorOf = useMemberColor();

  const rows = own ?? [];
  const readyCount = rows.filter((r) => r.segment === 'ready').length;
  const publishedCount = rows.filter((r) => r.segment === 'published').length;
  const heldCount = rows.filter((r) => r.segment === 'held').length;

  const items = useMemo(
    () => deriveAttention({ own: own ?? [], library: library ?? [], members: members ?? [], canModerate, pending, now: Date.now() }),
    [own, library, members, canModerate, pending],
  );

  // Integration: the union of every canonical filter with a goal or at
  // least one member's contribution, in the shared filter order.
  const filterSet = new Set<string>();
  if (goals) for (const f of Object.keys(goals)) filterSet.add(f);
  if (members) for (const m of members) for (const f of Object.keys(m.secondsByFilter)) filterSet.add(f);
  const filters = Array.from(filterSet).sort(filterOrder);
  // Members with any seconds in any filter, in the member list's order.
  const contributors = (members ?? []).filter((m) => Object.values(m.secondsByFilter).some((s) => s > 0));

  const recv = state.projects[projectId]?.recv ?? [];
  const send = state.projects[projectId]?.send ?? [];
  const label = (device: string) => peerLabel(state, projectId, device);

  const tiles = [
    ['ready', readyCount, 'ready to publish', 'text-accent'],
    ['published', publishedCount, 'published', 'text-success'],
    ['held', heldCount, 'held back', 'text-warning'],
  ] as const;

  return (
    <div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] items-start gap-3.5 max-[900px]:grid-cols-1">
      <Card title="Integration toward goal" subtitle="published, accepted frames · by member">
        {members === null ? (
          <EmptyState>Loading…</EmptyState>
        ) : filters.length === 0 ? (
          <EmptyState>No integration yet.</EmptyState>
        ) : (
          filters.map((f) => {
            const sec = (m: MemberSummary) => m.secondsByFilter[f] ?? 0;
            const total = members.reduce((a, m) => a + sec(m), 0);
            const goal = goals?.[f] ?? null;
            const scale = Math.max(goal ?? 0, total) || 1;
            const segs = members.filter((m) => sec(m) > 0);
            const left = goal !== null ? goal - total : null;
            return (
              <div key={f} className="my-[9px] grid grid-cols-[54px_1fr_150px] items-center gap-2.5">
                <span className="inline-flex items-center font-semibold text-content">
                  <FilterDot filter={f} />
                  {f}
                </span>
                <span className="relative h-3.5 rounded-[3px] bg-surface-hover">
                  <span className="flex h-full overflow-hidden rounded-[3px]">
                    {segs.map((m) => (
                      <i
                        key={m.accountId}
                        title={`${m.displayName} · ${formatDurationPadded(sec(m))}`}
                        className="block h-full"
                        style={{ width: `${(sec(m) / scale) * 100}%`, backgroundColor: colorOf(m.accountId) }}
                      />
                    ))}
                  </span>
                  {goal !== null && (
                    <span
                      aria-hidden
                      className="absolute -bottom-[3px] -top-[3px] w-0.5 bg-content"
                      style={{ left: `calc(${(goal / scale) * 100}% - 1px)` }}
                    />
                  )}
                </span>
                <span className="text-right text-[12px] text-content-muted">
                  <b className="font-semibold text-content">{formatDurationPadded(total)}</b>
                  {goal !== null && (
                    <>
                      {' '}of {formatDurationPadded(goal).replace(' 00m', '')} ·{' '}
                      {left! > 0 ? (
                        <span className="text-warning">{formatDurationPadded(left!)} to go</span>
                      ) : (
                        <span className="text-success">goal met</span>
                      )}
                    </>
                  )}
                </span>
              </div>
            );
          })
        )}
        {contributors.length > 0 && filters.length > 0 && (
          <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11.5px] text-content-faint">
            {contributors.map((m) => (
              <span key={m.accountId} className="inline-flex items-center gap-[5px]">
                <MemberDot color={colorOf(m.accountId)} />
                {m.displayName}
              </span>
            ))}
          </div>
        )}
      </Card>

      <div className="grid gap-3.5">
        <Card title="My contribution">
          {own === null ? (
            <EmptyState>{ownError ? 'Not available.' : 'Loading…'}</EmptyState>
          ) : (
            <div className="grid grid-cols-3 gap-2">
              {tiles.map(([seg, n, text, tone]) => (
                <button key={seg} type="button" onClick={() => onOpenSegment(seg)} className="rounded-md border border-line px-2.5 py-2 text-left hover:border-accent">
                  <span className={`block text-[20px] font-semibold ${tone}`}>{n.toLocaleString('en-US')}</span>{' '}
                  <span className="text-[11.5px] text-content-faint">{text}</span>
                </button>
              ))}
            </div>
          )}
        </Card>

        <Card title="Needs attention">
          {own === null ? (
            <EmptyState>{ownError ? 'Not available.' : 'Loading…'}</EmptyState>
          ) : items.length === 0 ? (
            <EmptyState>Nothing needs your attention.</EmptyState>
          ) : (
            items.map((it) => (
              <div key={it.key} className="flex items-start gap-2.5 border-t border-line py-[7px] text-[12.5px] first:border-t-0">
                <Chip tone={it.tone}>{it.count.toLocaleString('en-US')}</Chip>
                <span className="flex-1 text-content-secondary">
                  {it.title}
                  {it.detail && <small className="block text-[11.5px] text-content-faint">{it.detail}</small>}
                </span>
                <Button size="sm" onClick={() => onAttention(it.target)}>{it.action}</Button>
              </div>
            ))
          )}
        </Card>

        <Card title="Exchange now" action={<Button variant="link" onClick={() => onOpenTab('exchange')}>Open Exchange →</Button>}>
          {recv.length === 0 && send.length === 0 ? (
            <EmptyState>Nothing is moving.</EmptyState>
          ) : (
            <div className="grid gap-1 text-[12.5px] text-content-secondary">
              {recv.length > 0 && (
                <div>↓ <b className="font-semibold text-content">{formatRate(sumRate(recv))}</b> from {distinctNames(recv, label).join(', ')}</div>
              )}
              {send.length > 0 && (
                <div>↑ <b className="font-semibold text-content">{formatRate(sumRate(send))}</b> to {distinctNames(send, label).join(', ')}</div>
              )}
            </div>
          )}
        </Card>

        <Card title="Quality thresholds" subtitle={thresholdsVersion !== null ? `v${thresholdsVersion} · set by the coordinator` : 'set by the coordinator'}>
          {thresholds.length === 0 ? (
            <EmptyState>No quality rules set.</EmptyState>
          ) : (
            thresholds.map((r, i) => (
              <div key={`${r.metricKey}-${i}`} className="break-words text-[12.5px] text-content-secondary">{thresholdLine(r)}</div>
            ))
          )}
        </Card>
      </div>
    </div>
  );
}
