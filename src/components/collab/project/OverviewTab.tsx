import { useMemo, useState, type JSX, type ReactNode } from 'react';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import { distinctNames, peerLabel, sumRate } from '../exchange/state';
import { formatDurationPadded, formatRate, formatSize } from '../format';
import { Button, Card, Chip, EmptyState, FilterDot, MemberDot } from '../../ui';
import { filterOrder } from './table/model';
import { contributionTiles } from './contribution';
import { deriveAttention, type AttentionTarget, type NavTarget } from './attention';
import FilterMappingDialog from '../FilterMappingDialog';
import { useMemberColor } from './MemberColorsContext';
import type { Segment } from './MyFramesTab';
import type { MemberSummary, OwnFrameRow, ProjectFrameView, ThresholdRuleView } from '../../../types/models';

/**
 * Overview tab — the mockup's four cards: integration toward goal (member
 * coloured bars against the coordinator's goals), My contribution (four
 * detailed tiles, first), Needs attention (one row per held-back cause, each with an action
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
  /** This member receives (has the Library tab). */
  canReceive: boolean;
  /** This device's live exchange is running (Needs attention words the
   *  "missing here" row by it). */
  liveRunning: boolean;
  /** Reload own frames after the filter mapping is saved. */
  onReloadOwn: () => void;
  onAttention: (target: NavTarget) => void;
  /** The Project settings card, rendered first in the right column. */
  settings?: ReactNode;
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

const TILE_META: Record<Segment, { label: string; tone: string }> = {
  ready: { label: 'ready to calibrate', tone: 'text-accent' },
  review: { label: 'to review', tone: 'text-purple' },
  published: { label: 'published', tone: 'text-success' },
  held: { label: 'held back', tone: 'text-warning' },
};

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
  canReceive,
  liveRunning,
  onReloadOwn,
  onAttention,
  settings,
}: OverviewTabProps): JSX.Element {
  const { state } = useCollabExchange();
  const colorOf = useMemberColor();
  const [mapOpen, setMapOpen] = useState(false);
  const act = (t: AttentionTarget) => (t.kind === 'map' ? setMapOpen(true) : onAttention(t));

  const rows = useMemo(() => own ?? [], [own]);
  const tiles = useMemo(() => contributionTiles(rows), [rows]);

  const items = useMemo(
    () => deriveAttention({ own: own ?? [], library: library ?? [], members: members ?? [], canModerate, canReceive, liveRunning, pending, now: Date.now() }),
    [own, library, members, canModerate, canReceive, liveRunning, pending],
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

  return (
    <>
    <div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)] items-start gap-3.5 max-[900px]:grid-cols-1">
      <div data-col="left" className="grid gap-3.5">
      <Card title="My contribution">
        {own === null ? (
          <EmptyState>{ownError ? 'Not available.' : 'Loading…'}</EmptyState>
        ) : (
          <div className="grid grid-cols-4 gap-2 max-[900px]:grid-cols-2">
            {tiles.map((t) => {
              const meta = TILE_META[t.segment];
              return (
                <button
                  key={t.segment}
                  type="button"
                  aria-label={`${t.count} ${meta.label}`}
                  onClick={() => onOpenSegment(t.segment)}
                  className="flex flex-col rounded-md border border-line px-2.5 py-2 text-left hover:border-accent"
                >
                  <span className={`block text-[22px] font-semibold ${meta.tone}`}>{t.count.toLocaleString('en-US')}</span>
                  <span className="text-[11.5px] text-content-faint">{meta.label}</span>
                  <span className="mt-1 block text-[11.5px] text-content-secondary">
                    {formatDurationPadded(t.seconds)} · {t.nights} {t.nights === 1 ? 'night' : 'nights'} · {formatSize(t.bytes)}
                  </span>
                  <span className="mt-1 grid gap-0.5 text-[11.5px] text-content-muted">
                    {t.filters.map((f) => (
                      <span key={f.filter} className="flex items-center justify-between">
                        <span className="inline-flex items-center"><FilterDot filter={f.filter} />{f.filter}</span>
                        <span>{formatDurationPadded(f.seconds)}</span>
                      </span>
                    ))}
                  </span>
                  <span className="mt-auto border-t border-line pt-1.5 text-[11.5px] text-content-faint">{t.footer}</span>
                </button>
              );
            })}
          </div>
        )}
      </Card>

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

      </div>

      <div data-col="right" className="grid gap-3.5">
        {settings}
        <Card title="Needs attention">
          {own === null ? (
            <EmptyState>{ownError ? 'Not available.' : 'Loading…'}</EmptyState>
          ) : items.length === 0 ? (
            <EmptyState>Nothing needs your attention.</EmptyState>
          ) : (
            items.map((it) => (
              <div key={it.key} className="flex items-start gap-2.5 border-t border-line py-[7px] text-[12.5px] first-of-type:border-t-0">
                <Chip tone={it.tone}>{it.count.toLocaleString('en-US')}</Chip>
                <span className="flex-1 text-content-secondary">
                  {it.title}
                  {it.detail && <small className="block text-[11.5px] text-content-faint">{it.detail}</small>}
                </span>
                <Button size="sm" onClick={() => act(it.target)}>{it.action}</Button>
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
    {mapOpen && (
      <FilterMappingDialog
        projectId={projectId}
        onClose={() => setMapOpen(false)}
        onSaved={() => {
          setMapOpen(false);
          onReloadOwn();
        }}
      />
    )}
    </>
  );
}
