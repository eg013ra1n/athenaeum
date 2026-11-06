import { describe, it, expect } from 'vitest';
import type { FrameRow, Outcome, OutcomeKind } from './frameRows';
import {
  computeOutliers,
  filterRows,
  formatNightLabel,
  groupRows,
  matchesStatus,
  statusCounts,
  type FilterState,
  type GroupContext,
  type OutlierFlags,
} from './frameView';

const OUTCOME: Record<OutcomeKind, Omit<Outcome, 'reason'>> = {
  reference: { kind: 'reference', label: 'Reference', tone: 'accent' },
  included: { kind: 'included', label: 'Included', tone: 'success' },
  regFailed: { kind: 'regFailed', label: 'Reg failed', tone: 'error' },
  excluded: { kind: 'excluded', label: 'Excluded', tone: 'warning' },
  notStackable: { kind: 'notStackable', label: 'Not stackable here', tone: 'warning' },
};

let seq = 0;
/** A row with every field defaulted; `kind` sets the outcome (`null` for none). */
function row(
  kind: OutcomeKind | null,
  over: Partial<FrameRow> & { reason?: string | null } = {},
): FrameRow {
  seq += 1;
  const { reason, ...rest } = over;
  return {
    key: `k${seq}`,
    frameId: seq,
    filename: `frame_${seq}.fits`,
    publisher: null,
    groupKey: null,
    night: null,
    exptimeSec: null,
    weight: null,
    fwhmPx: null,
    eccentricity: null,
    stars: null,
    regRmsPx: null,
    lnScale: null,
    regInliers: null,
    regModel: null,
    outcome: kind ? { ...OUTCOME[kind], reason: reason ?? null } : null,
    outOfNextRun: false,
    editable: true,
    unavailable: null,
    pinnable: true,
    pinned: false,
    pinNote: null,
    ...rest,
  };
}

const NO_FILTER: FilterState = { query: '', status: 'all', outliersOnly: false };
const CTX: GroupContext = { groupOrder: ['L', 'B'], groupTitle: (k) => `Group ${k}` };
const noFlags = new Map<string, OutlierFlags>();

describe('formatNightLabel', () => {
  it('night_label_matches_analysis', () => {
    // One calendar date: `Month D, YYYY`.
    expect(formatNightLabel('2025-10-18T18:00:00+00:00', '2025-10-18T23:30:00+00:00')).toBe('October 18, 2025');
    // Through midnight inside a month: an en dash with no spaces.
    expect(formatNightLabel('2025-09-13T20:00:00+00:00', '2025-09-14T04:00:00+00:00')).toBe('September 13–14, 2025');
    // Across a month boundary: a spaced en dash and the end's year.
    expect(formatNightLabel('2025-10-31T20:00:00+00:00', '2025-11-01T04:00:00+00:00')).toBe(
      'October 31 – November 1, 2025',
    );
    expect(formatNightLabel('2025-12-31T22:00:00+00:00', '2026-01-01T04:00:00+00:00')).toBe(
      'December 31 – January 1, 2026',
    );
    // A night that starts after UTC midnight still reads as that UTC date.
    expect(formatNightLabel('2023-12-03T00:37:00+00:00', '2023-12-03T01:26:59+00:00')).toBe('December 3, 2023');
    // Offsets are read in UTC, like the Analysis tab: 23:30 at -05:00 is 04:30 the next day.
    expect(formatNightLabel('2025-03-09T23:30:00-05:00', '2025-03-10T23:30:00-05:00')).toBe('March 10–11, 2025');
    // The `Z` spelling and fractional seconds parse too.
    expect(formatNightLabel('2025-10-18T18:00:00.250Z', '2025-10-18T23:30:00Z')).toBe('October 18, 2025');
  });

  it('gives the raw start when an end cannot be read', () => {
    expect(formatNightLabel('not a date', '2025-10-18T23:30:00+00:00')).toBe('not a date');
    expect(formatNightLabel('2025-10-18T18:00:00+00:00', 'garbage')).toBe('2025-10-18T18:00:00+00:00');
    // A calendar date or a time that does not exist is not a date.
    expect(formatNightLabel('2025-13-40T00:00:00+00:00', '2025-13-40T01:00:00+00:00')).toBe('2025-13-40T00:00:00+00:00');
    expect(formatNightLabel('2025-02-30T20:00:00+00:00', '2025-03-01T04:00:00+00:00')).toBe('2025-02-30T20:00:00+00:00');
    expect(formatNightLabel('2025-02-27T25:00:00+00:00', '2025-02-28T04:00:00+00:00')).toBe('2025-02-27T25:00:00+00:00');
    // No offset is not an RFC 3339 instant either.
    expect(formatNightLabel('2025-10-18T18:00:00', '2025-10-18T23:30:00')).toBe('2025-10-18T18:00:00');
  });

  it('spells a plain start date the way the Analysis tab does', () => {
    // `format_night_display` falls back to `format_date_display`, zero-padded day.
    expect(formatNightLabel('2025-10-08', '2025-10-09')).toBe('October 08, 2025');
  });
});

describe('computeOutliers', () => {
  const fwhmRow = (groupKey: string, fwhm: number, publisher: string | null, kind: OutcomeKind = 'included') =>
    row(kind, { groupKey, fwhmPx: fwhm, publisher });

  it('project_outliers_per_publisher', () => {
    const alice = Array.from({ length: 6 }, () => fwhmRow('L', 2.0, 'Alice'));
    const bob = Array.from({ length: 2 }, () => fwhmRow('L', 4.0, 'Bob'));
    const rows = [...alice, ...bob];

    // Per publisher the limits are 2.6 (Alice) and 5.2 (Bob): nobody is flagged.
    const perPublisher = computeOutliers(rows, 'project');
    expect(rows.every((r) => perPublisher.get(r.key)?.fwhm === false)).toBe(true);

    // One bucket (a frame set): median 2, MAD 0, floor 0.2, limit 2.6 flags Bob.
    const single = computeOutliers(rows, 'frameSet');
    expect(alice.every((r) => single.get(r.key)?.fwhm === false)).toBe(true);
    expect(bob.every((r) => single.get(r.key)?.fwhm === true)).toBe(true);
  });

  it('builds the limit from included and reference rows only, flagging every row of the bucket', () => {
    const good = [2.0, 2.1, 2.2, 2.3].map((v) => fwhmRow('L', v, null));
    const reference = row('reference', { groupKey: 'L', fwhmPx: 2.2 });
    // A failed row's wild FWHM neither moves the limit nor escapes the flag.
    const failedWild = row('regFailed', { groupKey: 'L', fwhmPx: 9.0 });
    const excludedFine = row('excluded', { groupKey: 'L', fwhmPx: 2.0 });
    const noValue = row('included', { groupKey: 'L', fwhmPx: null });
    const rows = [...good, reference, failedWild, excludedFine, noValue];
    const flags = computeOutliers(rows, 'frameSet');
    expect(flags.get(failedWild.key)).toEqual({ fwhm: true, ecc: false });
    expect(flags.get(excludedFine.key)).toEqual({ fwhm: false, ecc: false });
    expect(flags.get(noValue.key)).toEqual({ fwhm: false, ecc: false });
    expect(flags.get(reference.key)).toEqual({ fwhm: false, ecc: false });
    expect(flags.size).toBe(rows.length);
  });

  it('gives FWHM and eccentricity their own limits, per group', () => {
    const l = [0.4, 0.4, 0.4, 0.4, 0.9].map((e) => row('included', { groupKey: 'L', fwhmPx: 2, eccentricity: e }));
    // Group B shoots a looser eccentricity; 0.9 is normal there.
    const b = [0.9, 0.9, 0.9].map((e) => row('included', { groupKey: 'B', fwhmPx: 2, eccentricity: e }));
    const flags = computeOutliers([...l, ...b], 'frameSet');
    expect(flags.get(l[4]!.key)).toEqual({ fwhm: false, ecc: true });
    expect(flags.get(l[0]!.key)).toEqual({ fwhm: false, ecc: false });
    expect(b.every((r) => flags.get(r.key)?.ecc === false)).toBe(true);
  });

  it('keeps rows with no group in a bucket of their own', () => {
    // Pooled with the three grouped rows (median 2, limit 2.6) the 5 would be flagged.
    const noGroup = row('included', { fwhmPx: 5 });
    const grouped = Array.from({ length: 3 }, () => row('included', { groupKey: 'L', fwhmPx: 2 }));
    const flags = computeOutliers([noGroup, ...grouped], 'frameSet');
    expect([...flags.values()].every((f) => !f.fwhm)).toBe(true);
  });
});

describe('matchesStatus and statusCounts', () => {
  it('maps every filter to the outcomes the spec names', () => {
    const ref = row('reference');
    const inc = row('included');
    const failed = row('regFailed');
    const excl = row('excluded');
    const ns = row('notStackable');
    const none = row(null);
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'all'))).toEqual(Array(6).fill(true));
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'included'))).toEqual([
      true, true, false, false, false, false,
    ]);
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'regFailed'))).toEqual([
      false, false, true, false, false, false,
    ]);
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'excludedByRun'))).toEqual([
      false, false, false, true, false, false,
    ]);
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'notStackable'))).toEqual([
      false, false, false, false, true, false,
    ]);
    expect([ref, inc, failed, excl, ns, none].map((r) => matchesStatus(r, 'outOfNextRun'))).toEqual(
      Array(6).fill(false),
    );
  });

  it('out_of_next_run_counts_every_listed_row', () => {
    const rows = [
      row('reference', { outOfNextRun: true }),
      row('regFailed', { outOfNextRun: true }),
      row('included'),
    ];
    expect(statusCounts(rows)).toEqual({
      all: 3,
      included: 2,
      regFailed: 1,
      excludedByRun: 0,
      outOfNextRun: 2,
      notStackable: 0,
    });
  });

  it('counts an empty list as zeros', () => {
    expect(statusCounts([])).toEqual({
      all: 0, included: 0, regFailed: 0, excludedByRun: 0, outOfNextRun: 0, notStackable: 0,
    });
  });
});

describe('filterRows', () => {
  const a = row('included', { filename: 'M31_Ha_001.fits', publisher: 'Alice' });
  const b = row('regFailed', { filename: 'M31_OIII_002.fits', publisher: 'Bob' });
  const c = row('included', { filename: 'ngc7000_003.fits', publisher: null });
  const rows = [a, b, c];

  it('matches the file name case-insensitively and the publisher', () => {
    expect(filterRows(rows, { ...NO_FILTER, query: 'm31' }, noFlags)).toEqual([a, b]);
    expect(filterRows(rows, { ...NO_FILTER, query: 'OIII' }, noFlags)).toEqual([b]);
    expect(filterRows(rows, { ...NO_FILTER, query: 'alice' }, noFlags)).toEqual([a]);
    expect(filterRows(rows, { ...NO_FILTER, query: '  bob ' }, noFlags)).toEqual([b]);
    expect(filterRows(rows, { ...NO_FILTER, query: 'nothing' }, noFlags)).toEqual([]);
    expect(filterRows(rows, NO_FILTER, noFlags)).toEqual(rows);
  });

  it('combines the status chip, the query and the outliers switch', () => {
    const flags = new Map<string, OutlierFlags>([
      [a.key, { fwhm: false, ecc: false }],
      [b.key, { fwhm: true, ecc: false }],
      [c.key, { fwhm: false, ecc: true }],
    ]);
    expect(filterRows(rows, { ...NO_FILTER, status: 'included' }, flags)).toEqual([a, c]);
    expect(filterRows(rows, { ...NO_FILTER, outliersOnly: true }, flags)).toEqual([b, c]);
    expect(filterRows(rows, { query: 'm31', status: 'included', outliersOnly: true }, flags)).toEqual([]);
    expect(filterRows(rows, { query: 'm31', status: 'all', outliersOnly: true }, flags)).toEqual([b]);
    // A row the map has no entry for is not an outlier.
    expect(filterRows(rows, { ...NO_FILTER, outliersOnly: true }, noFlags)).toEqual([]);
  });
});

describe('groupRows', () => {
  it('lost_group_header', () => {
    const quad = 'quad seed failed (0 quad matches)';
    const few = 'too few stars (subject 0, reference 196)';
    const failed = [
      ...Array.from({ length: 22 }, () => row('regFailed', { groupKey: 'B', reason: quad })),
      ...Array.from({ length: 3 }, () => row('regFailed', { groupKey: 'B', reason: few })),
    ];
    const excluded = Array.from({ length: 3 }, () => row('excluded', { groupKey: 'B', reason: 'FWHM above limit' }));
    const all = [...failed, ...excluded];
    const groups = groupRows(all, all, 'group', CTX);
    expect(groups).toHaveLength(1);
    expect(groups[0]!.info).toEqual({
      total: 28,
      included: 0,
      integrationSec: 0,
      medianFwhmPx: null,
      failed: 25,
      excluded: 3,
      topReason: quad,
    });
  });

  it('header_totals_ignore_filter', () => {
    const all = [
      row('included', { groupKey: 'L', filename: 'keep.fits', exptimeSec: 180, fwhmPx: 2 }),
      row('included', { groupKey: 'L', filename: 'a.fits', exptimeSec: 180, fwhmPx: 3 }),
      row('regFailed', { groupKey: 'L', filename: 'b.fits', reason: 'x' }),
      row('included', { groupKey: 'B', filename: 'c.fits', exptimeSec: 60, fwhmPx: 4 }),
    ];
    const visible = filterRows(all, { ...NO_FILTER, query: 'keep' }, noFlags);
    expect(visible).toHaveLength(1);
    const groups = groupRows(visible, all, 'group', CTX);
    expect(groups.map((g) => g.key)).toEqual(['L']);
    expect(groups[0]!.rows).toEqual(visible);
    expect(groups[0]!.info).toMatchObject({ total: 3, included: 2, failed: 1, integrationSec: 360 });
  });

  it('sums the integration over included rows and takes the upper median FWHM', () => {
    const all = [
      row('included', { groupKey: 'L', exptimeSec: 120, fwhmPx: 3 }),
      row('reference', { groupKey: 'L', exptimeSec: 120, fwhmPx: 2 }),
      row('included', { groupKey: 'L', exptimeSec: null, fwhmPx: null }),
      row('included', { groupKey: 'L', exptimeSec: 120, fwhmPx: 5 }),
      // Not included: neither the exposure nor the FWHM counts.
      row('excluded', { groupKey: 'L', exptimeSec: 999, fwhmPx: 50 }),
    ];
    const info = groupRows(all, all, 'group', CTX)[0]!.info;
    expect(info.integrationSec).toBe(360);
    // FWHM 2 3 5: upper median sorted[floor(3/2)] = 3.
    expect(info.medianFwhmPx).toBe(3);
    expect(info.included).toBe(4);
    expect(info.excluded).toBe(1);
  });

  it('upper median of an even count is the upper of the two middle values', () => {
    const all = [2, 3, 4, 5].map((v) => row('included', { groupKey: 'L', fwhmPx: v }));
    expect(groupRows(all, all, 'group', CTX)[0]!.info.medianFwhmPx).toBe(4);
  });

  it('breaks a top-reason tie by the first occurrence in the full list', () => {
    const all = [
      row('regFailed', { groupKey: 'L', reason: 'first' }),
      row('regFailed', { groupKey: 'L', reason: 'second' }),
      row('regFailed', { groupKey: 'L', reason: 'second' }),
      row('regFailed', { groupKey: 'L', reason: 'first' }),
      row('regFailed', { groupKey: 'L', reason: null }),
    ];
    expect(groupRows(all, all, 'group', CTX)[0]!.info.topReason).toBe('first');
    expect(groupRows(all.slice(1), all, 'group', CTX)[0]!.info.topReason).toBe('first');
    const clean = [row('included', { groupKey: 'L' })];
    expect(groupRows(clean, clean, 'group', CTX)[0]!.info.topReason).toBeNull();
  });

  it('orders Group buckets by the plan, then unknown keys, then Not grouped yet; drops empty ones', () => {
    const all = [
      row('included', { groupKey: null }),
      row('included', { groupKey: 'X' }),
      row('included', { groupKey: 'B' }),
      row('included', { groupKey: 'L' }),
      row('included', { groupKey: 'Hidden' }),
    ];
    const visible = all.filter((r) => r.groupKey !== 'Hidden');
    const groups = groupRows(visible, all, 'group', CTX);
    expect(groups.map((g) => g.title)).toEqual(['Group L', 'Group B', 'Group X', 'Not grouped yet']);
    expect(groups.map((g) => g.rows.length)).toEqual([1, 1, 1, 1]);
  });

  it('night_buckets_keep_first_appearance_order_and_put_no_night_last', () => {
    const all = [
      row('included', { night: null }),
      row('included', { night: 'October 31 – November 1, 2025' }),
      row('included', { night: 'September 13–14, 2025' }),
      row('included', { night: 'October 31 – November 1, 2025' }),
      row('included', { night: 'October 18, 2025' }),
    ];
    const groups = groupRows(all, all, 'night', CTX);
    // Neither alphabetical nor date order: the order the rows came in (the nights' own).
    expect(groups.map((g) => g.title)).toEqual([
      'October 31 – November 1, 2025',
      'September 13–14, 2025',
      'October 18, 2025',
      'No night',
    ]);
    expect(groups[0]!.rows).toHaveLength(2);
    expect(groups[0]!.info.total).toBe(2);
  });

  it('orders Publisher buckets alphabetically with unknown publishers last', () => {
    const all = [
      row('included', { publisher: 'Zoe' }),
      row('included', { publisher: null }),
      row('included', { publisher: 'alice' }),
      row('included', { publisher: 'Bob' }),
    ];
    const groups = groupRows(all, all, 'publisher', CTX);
    expect(groups.map((g) => g.title)).toEqual(['alice', 'Bob', 'Zoe', 'Unknown publisher']);
  });

  it('status_mode_order', () => {
    const all = [
      row(null),
      row('notStackable', { reason: 'not on disk' }),
      row('excluded'),
      row('regFailed'),
      row('included'),
      row('reference'),
      row('included'),
    ];
    const groups = groupRows(all, all, 'status', CTX);
    expect(groups.map((g) => g.title)).toEqual([
      'Reference',
      'Included',
      'Reg failed',
      'Excluded',
      'Not stackable here',
      'Not run yet',
    ]);
    expect(groups.map((g) => g.rows.length)).toEqual([1, 2, 1, 1, 1, 1]);
    // No empty buckets: only the outcomes present show up.
    const some = [row('excluded'), row('reference')];
    expect(groupRows(some, some, 'status', CTX).map((g) => g.title)).toEqual(['Reference', 'Excluded']);
  });

  it('none returns one bucket and nothing for an empty view', () => {
    const all = [row('included', { groupKey: 'L' }), row('regFailed', { groupKey: 'B' })];
    const groups = groupRows(all, all, 'none', CTX);
    expect(groups).toHaveLength(1);
    expect(groups[0]!.key).toBe('all');
    expect(groups[0]!.rows).toEqual(all);
    expect(groups[0]!.info).toMatchObject({ total: 2, included: 1, failed: 1 });
    expect(groupRows([], all, 'none', CTX)).toEqual([]);
  });

  it('keeps the visible order of the rows inside a bucket', () => {
    const r1 = row('included', { groupKey: 'L', filename: 'b' });
    const r2 = row('included', { groupKey: 'L', filename: 'a' });
    expect(groupRows([r2, r1], [r1, r2], 'group', CTX)[0]!.rows).toEqual([r2, r1]);
  });
});
