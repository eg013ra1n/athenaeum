import { autoOutlierLimit } from '../analysis/outlierLimit';
import type { PlanGroup } from '../../types/stacking';
import type { FrameRow, OutcomeKind } from './frameRows';

/** How the Frames table groups its rows (spec §6.6). */
export type GroupMode = 'group' | 'night' | 'publisher' | 'status' | 'none';

/** The status chips (spec §6.4). A row can match more than one. */
export type StatusFilter = 'all' | 'included' | 'regFailed' | 'excludedByRun' | 'outOfNextRun' | 'notStackable';

export interface OutlierFlags {
  fwhm: boolean;
  ecc: boolean;
}

export interface FilterState {
  query: string;
  status: StatusFilter;
  outliersOnly: boolean;
}

export interface GroupHeaderInfo {
  total: number;
  included: number;
  integrationSec: number;
  medianFwhmPx: number | null;
  failed: number;
  excluded: number;
  topReason: string | null;
}

export interface FrameGroupView {
  key: string;
  title: string;
  rows: FrameRow[];
  info: GroupHeaderInfo;
}

export interface GroupContext {
  /** The plan's group keys, in the plan's order (Group mode). */
  groupOrder: string[];
  /** The header title of a group key (filter, exposure, colour mode). */
  groupTitle: (key: string) => string;
}

const MONTHS = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
];

/** A strict RFC 3339 date-time with an offset, the only spelling Rust's `parse_from_rfc3339` reads (a stored night
 *  start/end is always one). `Date.parse` alone would also take a bare date or a zone-less time. */
const RFC3339 = /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/;

/** The UTC calendar date of an RFC 3339 instant, `null` when it is not one. The fields are checked here rather than
 *  left to `Date`, which rolls `2025-02-30` over to March 2 where the backend refuses it. */
function utcDate(iso: string): { y: number; m: number; d: number } | null {
  const parts = RFC3339.exec(iso);
  if (!parts) return null;
  const [y, mo, d, h, mi, s] = [1, 2, 3, 4, 5, 6].map((i) => Number(parts[i]));
  const local = new Date(Date.UTC(y!, mo! - 1, d!, h!, mi!, s!));
  const real =
    local.getUTCFullYear() === y && local.getUTCMonth() === mo! - 1 && local.getUTCDate() === d &&
    h! <= 23 && mi! <= 59 && s! <= 59;
  if (!real) return null;
  const zone = parts[8]!;
  const sign = zone.startsWith('-') ? -1 : 1;
  const offsetMin = /^[Zz]$/.test(zone) ? 0 : sign * (Number(zone.slice(1, 3)) * 60 + Number(zone.slice(4, 6)));
  const instant = new Date(local.getTime() - offsetMin * 60_000);
  return { y: instant.getUTCFullYear(), m: instant.getUTCMonth(), d: instant.getUTCDate() };
}

/** What the backend's `format_date_display` makes of a start it could not read as a night: a plain `YYYY-MM-DD`
 *  becomes `October 08, 2025` (zero-padded day, as there), anything else stays as it came. */
function plainDateLabel(raw: string): string {
  const parts = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw);
  if (!parts) return raw;
  const month = MONTHS[Number(parts[2]) - 1];
  const probe = new Date(Date.UTC(Number(parts[1]), Number(parts[2]) - 1, Number(parts[3])));
  const real = probe.getUTCFullYear() === Number(parts[1]) && probe.getUTCMonth() === Number(parts[2]) - 1
    && probe.getUTCDate() === Number(parts[3]);
  return real && month ? `${month} ${parts[3]}, ${parts[1]}` : raw;
}

/** A night's label, the way the Analysis tab writes it (a port of `format_night_display` in
 *  `crates/athenaeum-core/src/db/calibration_links.rs`): `October 18, 2025` for one date, `September 13–14, 2025`
 *  through midnight inside a month, `October 31 – November 1, 2025` across a month boundary (the end's year). Both
 *  ends are read as UTC calendar dates, the frame a night's start and end are stored in, so a night that starts
 *  after UTC midnight never reads a day late. */
export function formatNightLabel(startIso: string, endIso: string): string {
  const a = utcDate(startIso);
  const b = utcDate(endIso);
  if (!a || !b) return plainDateLabel(startIso);
  const day = (x: { m: number; d: number }) => `${MONTHS[x.m]} ${x.d}`;
  if (a.y === b.y && a.m === b.m && a.d === b.d) return `${day(a)}, ${a.y}`;
  if (a.y === b.y && a.m === b.m) return `${day(a)}–${b.d}, ${a.y}`;
  return `${day(a)} – ${day(b)}, ${b.y}`;
}

/** A plan group's title: filter, exposure class and colour mode (`L · 180s · mono`), plus the binning when it is not
 *  1 (it is part of the key); the raw key when the plan does not list the group. One spelling for the reference
 *  strip, the group headers and Blink. */
export function planGroupTitle(planGroups: PlanGroup[], key: string): string {
  const group = planGroups.find((g) => g.key === key);
  if (!group) return key;
  const parts = [group.filter ?? 'No filter', group.exposureClass, group.colorMode];
  if (group.binning !== 1) parts.push(`bin${group.binning}`);
  return parts.join(' · ');
}

/** The title of the bucket of rows with no group yet: the Group mode's last header and Blink's strip. */
export const NOT_GROUPED_TITLE = 'Not grouped yet';

/** Whether an outcome counts as part of the stack: the run's reference is integrated like any included frame. */
function isIncluded(row: FrameRow): boolean {
  const kind = row.outcome?.kind;
  return kind === 'included' || kind === 'reference';
}

/** The bucket a row's outlier limit comes from (spec §6.4): its group for a frame set, its group and publisher for
 *  a project (each member shoots with their own rig). */
function outlierBucket(row: FrameRow, subjectKind: 'frameSet' | 'project'): string {
  return JSON.stringify(subjectKind === 'project' ? [row.groupKey, row.publisher] : [row.groupKey]);
}

/** The outlier flags of every row (spec §6.4): a row is flagged when its FWHM or its eccentricity is above its
 *  bucket's limit, `autoOutlierLimit` over the bucket's included and reference rows (FWHM and Ecc each have their
 *  own). A bucket with no value to build a limit from flags nothing. Every row gets an entry. */
export function computeOutliers(rows: FrameRow[], subjectKind: 'frameSet' | 'project'): Map<string, OutlierFlags> {
  const buckets = new Map<string, FrameRow[]>();
  for (const row of rows) {
    const bucket = outlierBucket(row, subjectKind);
    const list = buckets.get(bucket);
    if (list) list.push(row);
    else buckets.set(bucket, [row]);
  }
  const flags = new Map<string, OutlierFlags>();
  for (const members of buckets.values()) {
    const fwhmLimit = autoOutlierLimit(finiteValues(members.filter(isIncluded), (r) => r.fwhmPx));
    const eccLimit = autoOutlierLimit(finiteValues(members.filter(isIncluded), (r) => r.eccentricity));
    for (const row of members) {
      flags.set(row.key, {
        fwhm: fwhmLimit != null && row.fwhmPx != null && row.fwhmPx > fwhmLimit,
        ecc: eccLimit != null && row.eccentricity != null && row.eccentricity > eccLimit,
      });
    }
  }
  return flags;
}

function finiteValues(rows: FrameRow[], pick: (row: FrameRow) => number | null): number[] {
  const out: number[] = [];
  for (const row of rows) {
    const v = pick(row);
    if (v != null && Number.isFinite(v)) out.push(v);
  }
  return out;
}

/** Whether a row counts under a status chip (spec §6.4). `included` counts the run's reference too;
 *  `outOfNextRun` counts every row in the exclusion list, whatever its outcome. */
export function matchesStatus(row: FrameRow, f: StatusFilter): boolean {
  switch (f) {
    case 'all':
      return true;
    case 'included':
      return isIncluded(row);
    case 'regFailed':
      return row.outcome?.kind === 'regFailed';
    case 'excludedByRun':
      return row.outcome?.kind === 'excluded';
    case 'notStackable':
      return row.outcome?.kind === 'notStackable';
    case 'outOfNextRun':
      return row.outOfNextRun;
  }
}

const STATUS_FILTERS: StatusFilter[] = ['all', 'included', 'regFailed', 'excludedByRun', 'outOfNextRun', 'notStackable'];

/** How many of `rows` each status chip counts. */
export function statusCounts(rows: FrameRow[]): Record<StatusFilter, number> {
  const counts = { all: 0, included: 0, regFailed: 0, excludedByRun: 0, outOfNextRun: 0, notStackable: 0 };
  for (const row of rows) {
    for (const f of STATUS_FILTERS) {
      if (matchesStatus(row, f)) counts[f] += 1;
    }
  }
  return counts;
}

/** The rows the toolbar leaves visible: the status chip, the outliers switch, and a case-insensitive substring of
 *  the file name (or, for a project row, the publisher). */
export function filterRows(rows: FrameRow[], f: FilterState, outliers: Map<string, OutlierFlags>): FrameRow[] {
  const needle = f.query.trim().toLowerCase();
  return rows.filter((row) => {
    if (!matchesStatus(row, f.status)) return false;
    if (f.outliersOnly) {
      const flags = outliers.get(row.key);
      if (!flags || !(flags.fwhm || flags.ecc)) return false;
    }
    if (needle === '') return true;
    return row.filename.toLowerCase().includes(needle) || (row.publisher?.toLowerCase().includes(needle) ?? false);
  });
}

/** Keys the buckets of a row with no value use. A plan group key is a filter/exposure spelling, so these cannot
 *  collide with one. */
const NO_GROUP_KEY = '\u0000none';

const STATUS_ORDER: { key: OutcomeKind | 'notRun'; title: string }[] = [
  { key: 'reference', title: 'Reference' },
  { key: 'included', title: 'Included' },
  { key: 'regFailed', title: 'Reg failed' },
  { key: 'excluded', title: 'Excluded' },
  { key: 'notStackable', title: 'Not stackable here' },
  { key: 'notRun', title: 'Not run yet' },
];

/** The bucket key of a row under a grouping mode. */
function bucketKey(row: FrameRow, mode: GroupMode): string {
  switch (mode) {
    case 'group':
      return row.groupKey ?? NO_GROUP_KEY;
    case 'night':
      return row.night ?? NO_GROUP_KEY;
    case 'publisher':
      return row.publisher ?? NO_GROUP_KEY;
    case 'status':
      return row.outcome?.kind ?? 'notRun';
    case 'none':
      return 'all';
  }
}

/** The header facts of a bucket, over ALL its rows whatever the filter shows (spec §6.6). */
function headerInfo(rows: FrameRow[]): GroupHeaderInfo {
  const included = rows.filter(isIncluded);
  const fwhm = finiteValues(included, (r) => r.fwhmPx).sort((a, b) => a - b);
  const reasons = new Map<string, number>();
  let failed = 0;
  let excluded = 0;
  for (const row of rows) {
    const kind = row.outcome?.kind;
    if (kind === 'excluded') excluded += 1;
    if (kind === 'regFailed') {
      failed += 1;
      const reason = row.outcome?.reason;
      if (reason != null) reasons.set(reason, (reasons.get(reason) ?? 0) + 1);
    }
  }
  // On a tie the reason that appears first in `all` wins: the Map keeps first-appearance order and `>` is strict.
  let topReason: string | null = null;
  let topCount = 0;
  for (const [reason, count] of reasons) {
    if (count > topCount) {
      topReason = reason;
      topCount = count;
    }
  }
  return {
    total: rows.length,
    included: included.length,
    integrationSec: included.reduce((sum, r) => sum + (r.exptimeSec ?? 0), 0),
    medianFwhmPx: fwhm.length > 0 ? fwhm[Math.floor(fwhm.length / 2)]! : null,
    failed,
    excluded,
    topReason,
  };
}

/** The order of the bucket keys present in `all`, and each key's title. */
function bucketLayout(
  all: FrameRow[],
  mode: GroupMode,
  ctx: GroupContext,
): { keys: string[]; title: (key: string) => string } {
  const firstSeen: string[] = [];
  const seen = new Set<string>();
  for (const row of all) {
    const key = bucketKey(row, mode);
    if (!seen.has(key)) {
      seen.add(key);
      firstSeen.push(key);
    }
  }
  const withoutNone = firstSeen.filter((k) => k !== NO_GROUP_KEY);
  const noneLast = (keys: string[]) => (seen.has(NO_GROUP_KEY) ? [...keys, NO_GROUP_KEY] : keys);
  switch (mode) {
    case 'group': {
      const rank = new Map(ctx.groupOrder.map((k, i) => [k, i]));
      // Plan groups in the plan's order, then any group the plan does not list (first appearance), then no group.
      const known = withoutNone.filter((k) => rank.has(k)).sort((a, b) => rank.get(a)! - rank.get(b)!);
      const unknown = withoutNone.filter((k) => !rank.has(k));
      return {
        keys: noneLast([...known, ...unknown]),
        title: (k) => (k === NO_GROUP_KEY ? NOT_GROUPED_TITLE : ctx.groupTitle(k)),
      };
    }
    case 'night':
      // The nights' own order is the order the rows arrive in; labels do not sort by date.
      return { keys: noneLast(withoutNone), title: (k) => (k === NO_GROUP_KEY ? 'No night' : k) };
    case 'publisher':
      return {
        keys: noneLast([...withoutNone].sort((a, b) => a.localeCompare(b))),
        title: (k) => (k === NO_GROUP_KEY ? 'Unknown publisher' : k),
      };
    case 'status': {
      const titleByKey = new Map(STATUS_ORDER.map((s) => [s.key as string, s.title]));
      return {
        keys: STATUS_ORDER.map((s) => s.key as string).filter((k) => seen.has(k)),
        title: (k) => titleByKey.get(k) ?? k,
      };
    }
    case 'none':
      return { keys: firstSeen, title: () => 'All frames' };
  }
}

/** The table's group blocks (spec §6.6). `visible` is what the filters leave, `all` every row of the table: a
 *  header's totals are over its bucket in `all`, so a filter never changes them. A bucket with no visible row is
 *  dropped. Rows keep their order from `visible` (the caller sorts them first). */
export function groupRows(visible: FrameRow[], all: FrameRow[], mode: GroupMode, ctx: GroupContext): FrameGroupView[] {
  const layout = bucketLayout(all, mode, ctx);
  const visibleByKey = new Map<string, FrameRow[]>();
  for (const row of visible) {
    const key = bucketKey(row, mode);
    const list = visibleByKey.get(key);
    if (list) list.push(row);
    else visibleByKey.set(key, [row]);
  }
  const allByKey = new Map<string, FrameRow[]>();
  for (const row of all) {
    const key = bucketKey(row, mode);
    const list = allByKey.get(key);
    if (list) list.push(row);
    else allByKey.set(key, [row]);
  }
  const groups: FrameGroupView[] = [];
  for (const key of layout.keys) {
    const rows = visibleByKey.get(key);
    if (!rows || rows.length === 0) continue;
    groups.push({ key, title: layout.title(key), rows, info: headerInfo(allByKey.get(key) ?? rows) });
  }
  return groups;
}
