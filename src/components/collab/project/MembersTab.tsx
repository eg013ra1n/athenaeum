import { Fragment, useEffect, useMemo, useRef, useState, type JSX } from 'react';
import { api } from '../../../api';
import { useSessionState } from '../../../contexts/SessionStateContext';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { formatBytes, formatDuration, formatRelative } from '../format';
import { filterOrder } from './table/model';
import { memberTone } from './memberColors';
import type { CameraQuality, MemberSummary } from '../../../types/models';

/**
 * Members tab — people, not frames (Task 13, wave 2 "Members" design). A
 * sortable table of the project's members: last seen, published frames,
 * integration per filter and holdings. A row expands to its devices and
 * per-camera quality. `onMembers` hands the loaded list back up so the
 * Overview tab (a later task) can reuse it without a second fetch.
 */

const CHIP = 'rounded px-1.5 py-0.5 text-[10px] font-medium bg-accent/20 text-accent';

type FilterKey = `filter:${string}`;
type SortKey = 'name' | 'role' | 'lastSeen' | 'published' | 'integration' | 'holds' | FilterKey;
interface SortState {
  key: SortKey;
  dir: 1 | -1;
}

const DEFAULT_SORT: SortState = { key: 'published', dir: -1 };
/** Columns that default to ascending on first click; every other column
 *  (numeric, or the special Last-seen ordering) defaults to descending —
 *  mirrors the design mockup's own `memberSort` click handler. */
const ASC_BY_DEFAULT: SortKey[] = ['name', 'role'];

function roleLabel(dataRole: string): string {
  if (dataRole === 'send_receive') return 'Processor';
  if (dataRole === 'send') return 'Contributor';
  return dataRole;
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
  if (key === 'role') return roleLabel(m.dataRole);
  if (key === 'lastSeen') return lastSeenRank(m);
  if (key === 'published') return m.publishedFrames;
  if (key === 'integration') return totalSeconds(m);
  if (key === 'holds') return m.holdsFrames;
  return m.secondsByFilter[key.slice('filter:'.length)] ?? 0;
}

function cameraLabel(camera: string): string {
  return camera === '' ? 'Unknown camera' : camera;
}

function qualityKey(q: CameraQuality, i: number): string {
  return `${q.camera}\u0000${q.filter}\u0000${i}`;
}

export default function MembersTab({
  projectId,
  onMembers,
  refreshToken = 0,
}: {
  projectId: string;
  onMembers?: (m: MemberSummary[]) => void;
  /** Bumped by the page shell on a trailing `collab-peers-changed` reload
   *  (Task 5). Re-invokes the load below WITHOUT resetting `sort`/`openId` —
   *  those are independent session state — and without clearing `members`
   *  first, so a presence-triggered refresh never flashes "Loading…". */
  refreshToken?: number;
}): JSX.Element {
  const [members, setMembers] = useState<MemberSummary[] | null>(null);
  const [error, setError] = useState(false);
  const [sort, setSort] = useSessionState<SortState>(`collab.${projectId}.members.sort`, DEFAULT_SORT);
  const [openId, setOpenId] = useSessionState<string | null>(`collab.${projectId}.members.open`, null);

  const onMembersRef = useRef(onMembers);
  onMembersRef.current = onMembers;

  // `firstLoadRef` distinguishes the initial mount / `projectId` change (show
  // "Loading…" while nothing is on screen yet) from a `refreshToken` bump
  // (keep the current rows visible while the re-read is in flight).
  const firstLoadRef = useRef(true);
  useEffect(() => {
    firstLoadRef.current = true;
  }, [projectId]);

  useEffect(() => {
    let cancelled = false;
    const isFirstLoad = firstLoadRef.current;
    firstLoadRef.current = false;
    if (isFirstLoad) {
      setMembers(null);
      setError(false);
    }
    api
      .invoke<MemberSummary[]>('get_collab_member_summary', { projectId })
      .then((list) => {
        if (cancelled) return;
        setMembers(list);
        setError(false);
        onMembersRef.current?.(list);
      })
      .catch((err) => {
        // Never swallow — log first, then an inline message (S6 convention).
        console.error('[members] get_collab_member_summary failed:', err);
        if (!cancelled) setError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, refreshToken]);

  const presentFilters = useMemo(() => {
    const set = new Set<string>();
    for (const m of members ?? []) for (const fl of Object.keys(m.secondsByFilter)) set.add(fl);
    return [...set].sort(filterOrder);
  }, [members]);

  const sorted = useMemo(() => {
    const list = members ?? [];
    return [...list].sort((a, b) => {
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

  const columns: { key: SortKey; label: string }[] = [
    { key: 'name', label: 'Member' },
    { key: 'role', label: 'Role' },
    { key: 'lastSeen', label: 'Last seen' },
    { key: 'published', label: 'Published' },
    { key: 'integration', label: 'Integration' },
    ...presentFilters.map((fl) => ({ key: `filter:${fl}` as FilterKey, label: fl })),
    { key: 'holds', label: 'Holds' },
  ];
  const colSpan = columns.length;

  return (
    <div className="space-y-3">
      <p className="text-xs text-content-muted">
        People, not frames. Filter columns are hours of published integration; Holds is how many project frames this
        member keeps and the share of the project. Click a member for devices and quality per camera.
      </p>

      {error && <p className="text-sm text-error">Could not load members — see console.</p>}

      {/* Gated on `!error` too: without it, a failed fetch left `members`
       *  null forever and this paragraph rendered underneath the error
       *  message, permanently — S6 (never a stuck "Loading…"). Deliberately
       *  not "set members to [] in the catch" instead: that would render an
       *  empty table with its own "No members yet." row, which misreads as
       *  a real, empty project rather than a failed load. */}
      {members === null && !error && <p className="text-sm text-content-muted">Loading…</p>}

      {members !== null && (
        <div className="overflow-auto rounded border border-border">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="sticky top-0 z-10 bg-surface">
                {columns.map((col) => {
                  const active = sort.key === col.key;
                  return (
                    <th
                      key={col.key}
                      onClick={() => onSortClick(col.key)}
                      className={`cursor-pointer select-none whitespace-nowrap border-b border-border px-2 py-1.5 text-left font-medium text-content-muted hover:text-content ${
                        active ? 'text-accent' : ''
                      }`}
                    >
                      {col.label}
                      {active ? (sort.dir === 1 ? ' ↑' : ' ↓') : ''}
                    </th>
                  );
                })}
              </tr>
            </thead>
            <tbody>
              {sorted.length === 0 ? (
                <tr>
                  <td colSpan={colSpan} className="p-3 text-content-muted">
                    No members yet.
                  </td>
                </tr>
              ) : (
                sorted.map((m) => {
                  const open = openId === m.accountId;
                  const tone = memberTone(m.accountId, sorted);
                  return (
                    <Fragment key={m.accountId}>
                      <tr
                        onClick={() => setOpenId(open ? null : m.accountId)}
                        className="cursor-pointer border-b border-border transition-colors hover:bg-surface-hover"
                      >
                        <td className="px-2 py-1.5">
                          <span className={`mr-1.5 inline-block h-2 w-2 rounded-full ${tone}`} />
                          <span data-testid="member-name">{m.displayName}</span>
                          {m.coordinator && <span className={`ml-1.5 ${CHIP}`}>Coordinator</span>}
                        </td>
                        <td className="px-2 py-1.5">{roleLabel(m.dataRole)}</td>
                        <td className="px-2 py-1.5">
                          {m.online ? (
                            <span className="text-success">online now</span>
                          ) : m.lastSeenAt ? (
                            <>
                              {formatTimestamp(m.lastSeenAt)}{' '}
                              <span className="text-content-muted">{formatRelative(m.lastSeenAt, Date.now())}</span>
                            </>
                          ) : (
                            <span className="text-content-muted">never</span>
                          )}
                        </td>
                        <td className="px-2 py-1.5 text-right">{m.publishedFrames.toLocaleString('en-US')}</td>
                        <td className="px-2 py-1.5 text-right">{formatDuration(totalSeconds(m))}</td>
                        {presentFilters.map((fl) => {
                          const secs = m.secondsByFilter[fl] ?? 0;
                          return (
                            <td key={fl} className="px-2 py-1.5 text-right text-content-secondary">
                              {secs ? formatDuration(secs) : '—'}
                            </td>
                          );
                        })}
                        <td className="px-2 py-1.5 text-right">
                          {m.holdsFrames.toLocaleString('en-US')} fr · {formatBytes(m.holdsBytes)}{' '}
                          <span className="text-content-muted">{(m.holdsShare * 100).toFixed(0)}%</span>
                        </td>
                      </tr>
                      {open && (
                        <tr className="border-b border-border bg-surface-hover/60">
                          <td colSpan={colSpan} className="px-3 py-2">
                            <div className="mb-2 flex flex-wrap gap-3">
                              {m.devices.length === 0 ? (
                                <span className="text-xs text-content-muted">No devices.</span>
                              ) : (
                                m.devices.map((d) => (
                                  <span
                                    key={d.device}
                                    className="inline-flex items-center gap-1.5 text-xs text-content-secondary"
                                  >
                                    <span
                                      className={`h-1.5 w-1.5 rounded-full ${d.online ? 'bg-success' : 'bg-border'}`}
                                    />
                                    {d.name ?? d.device.slice(0, 8)}
                                  </span>
                                ))
                              )}
                            </div>
                            {m.qualityByCamera.length === 0 ? (
                              <p className="text-xs text-content-muted">Nothing published yet.</p>
                            ) : (
                              <table className="text-xs">
                                <thead>
                                  <tr className="text-content-muted">
                                    <th className="pr-3 text-left font-medium">Camera</th>
                                    <th className="pr-3 text-left font-medium">Filter</th>
                                    <th className="pr-3 text-right font-medium">Frames</th>
                                    <th className="pr-3 text-right font-medium">x̃ FWHM</th>
                                    <th className="text-right font-medium">x̃ Ecc</th>
                                  </tr>
                                </thead>
                                <tbody>
                                  {[...m.qualityByCamera]
                                    .sort(
                                      (a, b) =>
                                        cameraLabel(a.camera).localeCompare(cameraLabel(b.camera)) ||
                                        filterOrder(a.filter, b.filter),
                                    )
                                    .map((q, i) => (
                                      <tr key={qualityKey(q, i)}>
                                        <td className="pr-3 text-content-secondary">{cameraLabel(q.camera)}</td>
                                        <td className="pr-3 text-content-secondary">{q.filter}</td>
                                        <td className="pr-3 text-right text-content-secondary">{q.frames}</td>
                                        <td className="pr-3 text-right text-content-secondary">
                                          {q.medianFwhm !== null ? `${q.medianFwhm.toFixed(2)}″` : '—'}
                                        </td>
                                        <td className="text-right text-content-secondary">
                                          {q.medianEcc !== null ? q.medianEcc.toFixed(2) : '—'}
                                        </td>
                                      </tr>
                                    ))}
                                </tbody>
                              </table>
                            )}
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })
              )}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
