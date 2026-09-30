import type { JSX } from 'react';
import { useCollabExchange } from '../../../contexts/CollabExchangeContext';
import { peerLabel } from '../exchange/state';
import { getFilterColor } from '../../../utils/filterColors';
import { formatDuration, formatRate } from '../format';
import { filterOrder } from './table/model';
import { copies, fromOwn, REASON_LABEL } from './frames';
import { memberColor } from './memberColors';
import type { Segment } from './MyFramesTab';
import type { MemberSummary, OwnFrameRow, ThresholdRuleView } from '../../../types/models';

/**
 * Overview tab — the redesigned collab project page's landing tab (Task 15,
 * wave 2, mockup `overviewView`). Five parts: integration per canonical
 * filter drawn as member-coloured bars against the coordinator's goals, "my
 * three numbers", Needs attention, a live Exchange-now one-liner, and the
 * quality-thresholds section moved verbatim from the old Overview tab of
 * `ProjectDetail.tsx`. While `own` is still loading (`null`), My frames and
 * Needs attention read `Loading…` rather than zero counts (`Not available.`
 * once `ownError` says the load failed).
 */

export interface OverviewTabProps {
  projectId: string;
  goals: Record<string, number> | null;
  members: MemberSummary[] | null;
  own: OwnFrameRow[] | null;
  /** `own` failed to load: the cards say so instead of `Loading…` (the
   *  shell shows the error line above the tab). */
  ownError?: boolean;
  libraryToCome: number;
  pending: number;
  canModerate: boolean;
  thresholds: ThresholdRuleView[];
  thresholdsVersion: number | null;
  onOpenSegment: (s: Segment) => void;
  onOpenTab: (t: string) => void;
}

const CARD = 'rounded-lg border border-border bg-surface-elevated p-3';
const HEADER = 'flex flex-wrap items-baseline gap-x-1.5 text-sm font-semibold text-content';
const SUB = 'font-normal text-xs text-content-muted';
const EMPTY = 'py-1.5 text-xs text-content-muted';
// Plain single-text-node buttons (no nested styled spans): Testing
// Library's default text matcher (`getNodeText`) only concatenates a
// node's DIRECT text-node children, so a "Ready" + "2" split across two
// child <span>s would never expose "Ready 2" as one matchable string —
// mirrors the plain-text convention `MyFramesTab.tsx`'s `SEG_BTN` already
// uses for the same reason.
const READY_BTN = 'rounded border border-accent/30 px-3 py-1.5 text-sm text-content transition-colors hover:bg-surface-hover';
const PUBLISHED_BTN = 'rounded border border-success/30 px-3 py-1.5 text-sm text-content transition-colors hover:bg-surface-hover';
const HELD_BTN = 'rounded border border-warning/30 px-3 py-1.5 text-sm text-content transition-colors hover:bg-surface-hover';
const ATTN_BTN =
  'block w-full rounded border border-border px-2.5 py-1.5 text-left text-xs text-content-secondary transition-colors hover:bg-surface-hover';

/** The most frequent `failures[0].kind` across a set of held-back rows —
 * ties keep the first-seen kind, so the result is deterministic. */
function mostCommonFirstKind(rows: OwnFrameRow[]): string | null {
  const counts = new Map<string, number>();
  for (const r of rows) {
    const kind = r.failures[0]?.kind;
    if (!kind) continue;
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  let best: string | null = null;
  let bestCount = -1;
  for (const [kind, count] of counts) {
    if (count > bestCount) {
      best = kind;
      bestCount = count;
    }
  }
  return best;
}

export default function OverviewTab({
  projectId,
  goals,
  members,
  own,
  ownError = false,
  libraryToCome,
  pending,
  canModerate,
  thresholds,
  thresholdsVersion,
  onOpenSegment,
  onOpenTab,
}: OverviewTabProps): JSX.Element {
  const { state } = useCollabExchange();

  const rows = own ?? [];
  const readyCount = rows.filter((r) => r.segment === 'ready').length;
  const publishedRows = rows.filter((r) => r.segment === 'published');
  const publishedCount = publishedRows.length;
  const heldRows = rows.filter((r) => r.segment === 'held');
  const heldCount = heldRows.length;

  const publishedVMs = publishedRows.map(fromOwn);
  const diskIssueCount = publishedVMs.filter((v) => v.disk === 'missing' || v.disk === 'changed').length;
  const singleCopyCount = publishedVMs.filter((v) => copies(v) === 1).length;

  const attentionItems: { key: string; text: string; onClick: () => void }[] = [];
  if (heldCount > 0) {
    const kind = mostCommonFirstKind(heldRows);
    const label = kind ? (REASON_LABEL[kind] ?? kind) : 'various reasons';
    attentionItems.push({
      key: 'held',
      text: `${heldCount} held back — mostly ${label}`,
      onClick: () => onOpenSegment('held'),
    });
  }
  if (diskIssueCount > 0) {
    attentionItems.push({
      key: 'disk',
      text: `${diskIssueCount} published frames not on disk or changed`,
      onClick: () => onOpenSegment('published'),
    });
  }
  if (singleCopyCount > 0) {
    attentionItems.push({
      key: 'single',
      text: `${singleCopyCount} of your frames exist in one copy only`,
      onClick: () => onOpenSegment('published'),
    });
  }
  if (libraryToCome > 0) {
    attentionItems.push({
      key: 'library',
      text: `${libraryToCome} library frames still to come`,
      onClick: () => onOpenTab('library'),
    });
  }
  if (canModerate && pending > 0) {
    attentionItems.push({
      key: 'moderation',
      text: `${pending} frames wait for your review`,
      onClick: () => onOpenTab('moderation'),
    });
  }

  // Integration: the union of every canonical filter with a goal or at
  // least one member's contribution, in the shared filter order.
  const filterSet = new Set<string>();
  if (goals) for (const f of Object.keys(goals)) filterSet.add(f);
  if (members) for (const m of members) for (const f of Object.keys(m.secondsByFilter)) filterSet.add(f);
  const filters = Array.from(filterSet).sort(filterOrder);

  const recv = state.projects[projectId]?.recv ?? [];
  const send = state.projects[projectId]?.send ?? [];
  const recvRate = recv.reduce((a, f) => a + f.rateBps, 0);
  const sendRate = send.reduce((a, f) => a + f.rateBps, 0);
  const nameFor = (device: string) => {
    const label = peerLabel(state, projectId, device);
    return label.member ?? label.device;
  };
  const recvNames = recv.map((f) => nameFor(f.device)).join(', ');
  const sendNames = send.map((f) => nameFor(f.device)).join(', ');

  return (
    <div className="space-y-4 text-sm">
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[3fr_2fr]">
        <div className={CARD}>
          <h2 className={HEADER}>
            Integration <span className={SUB}>toward goal, by member</span>
          </h2>
          {members === null ? (
            <p className={EMPTY}>Loading…</p>
          ) : filters.length === 0 ? (
            <p className={EMPTY}>No integration yet.</p>
          ) : (
            <>
              <div className="mt-1">
                {filters.map((f) => {
                  const total = members.reduce((a, m) => a + (m.secondsByFilter[f] ?? 0), 0);
                  const goal = goals?.[f] ?? null;
                  const scale = Math.max(goal ?? 0, total) || 1;
                  const segs = members.filter((m) => (m.secondsByFilter[f] ?? 0) > 0);
                  const left = goal !== null ? goal - total : null;
                  return (
                    <div key={f} className="my-1.5 grid grid-cols-[70px_1fr_120px] items-center gap-2.5">
                      <span className="flex items-center gap-1.5 text-content">
                        <span
                          className="inline-block h-2 w-2 shrink-0 rounded-full"
                          style={{ backgroundColor: getFilterColor(f) }}
                        />
                        <b>{f}</b>
                      </span>
                      <div className="relative h-3.5 rounded bg-surface-hover">
                        <div className="flex h-full overflow-hidden rounded">
                          {segs.map((m) => {
                            const secs = m.secondsByFilter[f] ?? 0;
                            return (
                              <span
                                key={m.accountId}
                                title={`${m.displayName} · ${formatDuration(secs)}`}
                                className="h-full"
                                style={{ width: `${(secs / scale) * 100}%`, backgroundColor: memberColor(m.accountId, members, null) }}
                              />
                            );
                          })}
                        </div>
                        {goal !== null && (
                          <span
                            className="absolute top-0 bottom-0 border-l-2 border-content"
                            style={{ left: `${Math.min(100, (goal / scale) * 100)}%` }}
                          />
                        )}
                      </div>
                      <span className="text-right text-xs">
                        {left !== null ? (
                          left > 0 ? (
                            <span className="text-warning">{formatDuration(left)} to go</span>
                          ) : (
                            <span className="text-success">goal met</span>
                          )
                        ) : (
                          <span className="text-content-muted">{formatDuration(total)}</span>
                        )}
                      </span>
                    </div>
                  );
                })}
              </div>
              <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs text-content-muted">
                {(members ?? []).map((m) => (
                  <span key={m.accountId} className="inline-flex items-center gap-1">
                    <span className="inline-block h-2 w-2 rounded-full" style={{ backgroundColor: memberColor(m.accountId, members ?? [], null) }} />
                    {m.displayName}
                  </span>
                ))}
              </div>
            </>
          )}
        </div>

        <div className="space-y-4">
          <div className={CARD}>
            <h2 className={HEADER}>My frames</h2>
            {/* While own frames load, never zero counts (they would read as
                "you have nothing"). */}
            {own === null ? (
              <p className={EMPTY}>{ownError ? 'Not available.' : 'Loading…'}</p>
            ) : (
              <div className="mt-1 flex flex-wrap gap-2">
                <button type="button" onClick={() => onOpenSegment('ready')} className={READY_BTN}>
                  Ready {readyCount}
                </button>
                <button type="button" onClick={() => onOpenSegment('published')} className={PUBLISHED_BTN}>
                  Published {publishedCount}
                </button>
                <button type="button" onClick={() => onOpenSegment('held')} className={HELD_BTN}>
                  Held back {heldCount}
                </button>
              </div>
            )}
          </div>

          <div className={CARD}>
            <h2 className={HEADER}>Needs attention</h2>
            {own === null ? (
              <p className={EMPTY}>{ownError ? 'Not available.' : 'Loading…'}</p>
            ) : attentionItems.length === 0 ? (
              <p className={EMPTY}>Nothing needs attention.</p>
            ) : (
              <div className="mt-1 space-y-1">
                {attentionItems.map((it) => (
                  <button key={it.key} type="button" onClick={it.onClick} className={ATTN_BTN}>
                    {it.text}
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className={CARD}>
            <h2 className={HEADER}>
              Exchange now
              <button
                type="button"
                onClick={() => onOpenTab('exchange')}
                className="ml-auto text-xs font-normal text-accent hover:underline"
              >
                Open Exchange →
              </button>
            </h2>
            <div className="mt-1 space-y-0.5 text-xs text-content-muted">
              {recv.length === 0 && send.length === 0 ? (
                <p>Quiet.</p>
              ) : (
                <>
                  {recv.length > 0 && (
                    <p>
                      ↓ {formatRate(recvRate)} from {recvNames}
                    </p>
                  )}
                  {send.length > 0 && (
                    <p>
                      ↑ {formatRate(sendRate)} to {sendNames}
                    </p>
                  )}
                </>
              )}
            </div>
          </div>

          <div className={CARD}>
            <h2 className={HEADER}>
              Quality thresholds
              {thresholdsVersion != null ? ` (v${thresholdsVersion})` : ''}
            </h2>
            {thresholds.length === 0 ? (
              <p className="text-content-muted">No thresholds set.</p>
            ) : (
              <ul className="text-content-secondary">
                {thresholds.map((r, i) => (
                  <li key={`${r.metricKey}-${i}`} className="break-words">
                    {r.op === 'reject_if'
                      ? r.metricKey === 'not_trailed'
                        ? 'Reject trailed frames'
                        : `${r.metricKey} — reject if ${String(r.value)}`
                      : `${r.metricKey} ${r.op === 'lte' ? '≤' : r.op === 'gte' ? '≥' : r.op} ${String(r.value)}`}
                  </li>
                ))}
              </ul>
            )}
            <p className="mt-1 text-xs text-content-muted">
              Thresholds are set by the coordinator on the portal. Changes are prospective —
              already-published frames stay published.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
