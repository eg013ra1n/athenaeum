import { useMemo, useState, type JSX } from 'react';
import { PanelLayout, EmptyState, FilterDot, MemberDot, StatusDot } from '../../ui';
import { formatDurationPadded, formatRelative, formatSize } from '../format';
import { filterOrder } from './table/model';
import { useMemberColor } from './MemberColorsContext';
import MemberPanel, { roleLabel } from './MemberPanel';
import type { MemberSummary } from '../../../types/models';

/**
 * Members tab (wave 5.5 Task 13) — the mockup's plain table. Data comes from
 * the page shell (`members` / `error`); a row click opens the member card in
 * the docked side panel. Sort state is local to the tab.
 */

type FilterKey = `filter:${string}`;
type SortKey = 'name' | 'role' | 'devices' | 'lastSeen' | 'published' | 'integration' | 'fwhm' | 'holds' | FilterKey;
interface SortState {
  key: SortKey;
  dir: 1 | -1;
}

const DEFAULT_SORT: SortState = { key: 'published', dir: -1 };
/** Columns that default to ascending on first click; every other column
 *  (numeric, or the special Last-seen ordering) defaults to descending —
 *  mirrors the design mockup's own `memberSort` click handler. */
const ASC_BY_DEFAULT: SortKey[] = ['name', 'role'];

/** The role exactly as the Role cell reads (also its sort value). */
function roleText(m: MemberSummary): string {
  return m.coordinator ? 'Coordinator' : roleLabel(m.dataRole);
}

function totalSeconds(m: MemberSummary): number {
  let s = 0;
  for (const v of Object.values(m.secondsByFilter)) s += v;
  return s;
}

/** Online always outranks offline; among offline members, a more recent
 *  `lastSeenAt` outranks an older one; `null` (never seen) ranks lowest.
 *  `ONLINE_BONUS` is far larger than any real epoch-ms value so it can never
 *  be outranked by a timestamp. */
const ONLINE_BONUS = 1e15;
function lastSeenRank(m: MemberSummary): number {
  const t = m.lastSeenAt ? Date.parse(m.lastSeenAt) : NaN;
  const valid = !Number.isNaN(t);
  if (m.online) return ONLINE_BONUS + (valid ? t : 0);
  return valid ? t : -Infinity;
}

function valueFor(key: SortKey, m: MemberSummary): number | string {
  if (key === 'name') return m.displayName;
  if (key === 'role') return roleText(m);
  if (key === 'devices') return m.devices.filter((d) => d.online).length;
  if (key === 'fwhm') return weightedFwhm(m) ?? 0;
  if (key === 'lastSeen') return lastSeenRank(m);
  if (key === 'published') return m.publishedFrames;
  if (key === 'integration') return totalSeconds(m);
  if (key === 'holds') return m.holdsFrames;
  return m.secondsByFilter[key.slice('filter:'.length)] ?? 0;
}

/** Filters with integration or a goal, in `filterOrder`, `None` ("No filter") last. */
export function filterColumns(members: MemberSummary[], goals: Record<string, number> | null): string[] {
  const set = new Set<string>(Object.keys(goals ?? {}));
  for (const m of members) for (const [f, s] of Object.entries(m.secondsByFilter)) if (s > 0) set.add(f);
  const list = [...set].sort(filterOrder);
  return list.includes('None') ? [...list.filter((f) => f !== 'None'), 'None'] : list;
}

/** Median of `medianFwhm` over a member's cameras, weighted by frame count. */
function weightedFwhm(m: MemberSummary): number | null {
  const pts = m.qualityByCamera
    .filter((q): q is typeof q & { medianFwhm: number } => q.medianFwhm !== null && q.frames > 0)
    .sort((a, b) => a.medianFwhm - b.medianFwhm);
  const total = pts.reduce((n, q) => n + q.frames, 0);
  if (total === 0) return null;
  let acc = 0;
  for (const q of pts) {
    acc += q.frames;
    if (acc * 2 >= total) return q.medianFwhm;
  }
  return null;
}

const TH = 'whitespace-nowrap border-b border-border px-2 py-1.5 text-[11.5px] font-medium';
const TD = 'whitespace-nowrap border-b border-line-plain px-2 py-1.5';

function Ghost(): JSX.Element {
  return <span className="text-content-ghost">—</span>;
}

export default function MembersTab({
  members,
  error,
  goals,
}: {
  members: MemberSummary[] | null;
  error: boolean;
  goals: Record<string, number> | null;
}): JSX.Element {
  const colorOf = useMemberColor();
  const [sort, setSort] = useState<SortState>(DEFAULT_SORT);
  const [openId, setOpenId] = useState<string | null>(null);

  const filters = useMemo(() => filterColumns(members ?? [], goals), [members, goals]);

  const sorted = useMemo(() => {
    const list = members ?? [];
    return [...list].sort((a, b) => {
      if (sort.key === 'fwhm') {
        // A member without FWHM data sorts last in both directions.
        const fa = weightedFwhm(a);
        const fb = weightedFwhm(b);
        if (fa === null || fb === null) {
          if (fa === fb) return a.displayName.localeCompare(b.displayName);
          return fa === null ? 1 : -1;
        }
      }
      const x = valueFor(sort.key, a);
      const y = valueFor(sort.key, b);
      let c: number;
      if (typeof x === 'string' || typeof y === 'string') c = String(x).localeCompare(String(y));
      else c = x === y ? 0 : x < y ? -1 : 1;
      if (c !== 0) return c * sort.dir;
      return a.displayName.localeCompare(b.displayName);
    });
  }, [members, sort]);

  function onSortClick(key: SortKey): void {
    setSort((prev) =>
      prev.key === key
        ? { key, dir: (prev.dir * -1) as 1 | -1 }
        : { key, dir: ASC_BY_DEFAULT.includes(key) ? 1 : -1 },
    );
  }

  const open = members?.find((m) => m.accountId === openId) ?? null;

  function head(key: SortKey | null, label: JSX.Element | string, right: boolean): JSX.Element {
    const active = key !== null && sort.key === key;
    return (
      <th
        key={key ?? String(label)}
        onClick={key ? () => onSortClick(key) : undefined}
        className={`${TH} select-none ${right ? 'text-right' : 'text-left'} ${
          active ? 'text-accent' : 'text-content-faint'
        } ${key ? 'cursor-pointer hover:text-content' : ''}`}
      >
        {label}
        {active ? (sort.dir === 1 ? ' ↑' : ' ↓') : ''}
      </th>
    );
  }

  return (
    <PanelLayout panel={open ? <MemberPanel member={open} onClose={() => setOpenId(null)} /> : null}>
      <div className="space-y-3">
        {error && <p className="text-[12.5px] text-error">Could not load members — see console.</p>}
        {members === null && !error && <EmptyState>Loading…</EmptyState>}
        {members !== null && sorted.length === 0 && <EmptyState>No members yet.</EmptyState>}
        {members !== null && sorted.length > 0 && (
          <div data-testid="members-scroll" className="overflow-x-auto rounded-md border border-line">
            <table className="w-full border-collapse text-[12.5px]">
              <thead>
                <tr>
                  {head('name', 'Member', false)}
                  {head('role', 'Role', false)}
                  {head('devices', 'Devices', false)}
                  {head('published', 'Published', true)}
                  {filters.map((f) =>
                    head(
                      `filter:${f}`,
                      <span className="inline-flex items-center">
                        <FilterDot filter={f} />
                        {f === 'None' ? 'No filter' : f}
                      </span>,
                      true,
                    ),
                  )}
                  {head('integration', 'Σ', true)}
                  {head('fwhm', 'FWHM x̃', true)}
                  {head('holds', 'Holds', true)}
                  {head('lastSeen', 'Last seen', false)}
                </tr>
              </thead>
              <tbody>
                {sorted.map((m) => {
                  const fwhm = weightedFwhm(m);
                  return (
                    <tr
                      key={m.accountId}
                      onClick={() => setOpenId(openId === m.accountId ? null : m.accountId)}
                      className={`cursor-pointer hover:bg-[rgba(67,76,94,0.45)] ${
                        openId === m.accountId ? 'bg-accent/[0.16]' : ''
                      }`}
                    >
                      <td className={TD}>
                        {/* align-top: the truncating name is a scroll container, which moves
                            a baseline-aligned inline-flex's baseline to its bottom edge and
                            stretches the row past the mockup's 30.5px */}
                        <span className="inline-flex items-center gap-[5px] align-top">
                          <MemberDot color={colorOf(m.accountId)} />
                          <b
                            data-testid="member-name"
                            title={m.displayName}
                            className="max-w-[16rem] truncate font-semibold text-content"
                          >
                            {m.displayName}
                          </b>
                        </span>
                      </td>
                      <td className={TD}>
                        {roleText(m)}
                        {m.coordinator && m.dataRole === 'send_receive' && (
                          <span className="text-content-faint"> (Processor data)</span>
                        )}
                        {m.coordinator && m.dataRole === 'send' && (
                          <span className="text-content-faint"> (Contributor data)</span>
                        )}
                      </td>
                      <td className={TD}>
                        <span className="inline-flex items-center gap-0.5 align-middle">
                          {m.devices.map((d) => (
                            <span key={d.device} title={d.name ?? d.device.slice(0, 8)} className="inline-flex">
                              <StatusDot state={d.online ? 'online' : 'offline'} />
                            </span>
                          ))}
                        </span>
                      </td>
                      <td className={`${TD} text-right`}>{m.publishedFrames.toLocaleString('en-US')}</td>
                      {filters.map((f) => {
                        const s = m.secondsByFilter[f] ?? 0;
                        return (
                          <td key={f} className={`${TD} text-right text-content-secondary`}>
                            {s > 0 ? formatDurationPadded(s) : <Ghost />}
                          </td>
                        );
                      })}
                      <td className={`${TD} text-right`}>
                        {totalSeconds(m) > 0 ? <b className="font-semibold text-content">{formatDurationPadded(totalSeconds(m))}</b> : <Ghost />}
                      </td>
                      <td className={`${TD} text-right text-content-secondary`}>
                        {fwhm !== null ? `${fwhm.toFixed(2)}″` : <Ghost />}
                      </td>
                      <td className={`${TD} text-right`}>
                        {`${m.holdsFrames.toLocaleString('en-US')} fr · ${formatSize(m.holdsBytes)} `}
                        <span className="text-content-faint">{Math.round(m.holdsShare * 100)}%</span>
                      </td>
                      <td className={TD}>
                        {m.online ? (
                          <span className="text-success">now</span>
                        ) : (
                          <span className="text-content-faint">
                            {m.lastSeenAt ? formatRelative(m.lastSeenAt, Date.now()) : 'never'}
                          </span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </PanelLayout>
  );
}
