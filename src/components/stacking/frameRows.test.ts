// Frames block Task 3: the Frames table's row model. A row carries the selected run's outcome and, apart from it, the
// next run's exclusion (spec §6.4); a project row is keyed by its frame uuid, a frame-set row by its frame id.
import { describe, expect, it } from 'vitest';
import {
  buildFrameRows,
  buildProjectFrameRows,
  sortRows,
  type FrameRow,
  type LightFrameRef,
  type RowContext,
} from './frameRows';
import type { PlanProjectFrame } from '../../types/stacking';
import { CANNOT_OPEN, PLAN_FRAMES, detailOf, pf, runRow, summaryFrame } from './testFixtures';

const ctxOf = (over: Partial<RowContext> = {}): RowContext => ({
  planGroupByFrameId: new Map(),
  pinnedFrameId: null,
  ...over,
});

const build = (
  detail: ReturnType<typeof detailOf> | null = null,
  excluded: string[] = [],
  referenceFrameUuid: string | null = null,
  frames: PlanProjectFrame[] = PLAN_FRAMES,
  ctx: RowContext = ctxOf(),
) =>
  buildProjectFrameRows(
    frames,
    detail && { run: detail.run, groups: detail.groups, frames: detail.frames },
    detail?.summary ?? null,
    new Set(excluded),
    referenceFrameUuid,
    ctx,
  );

const byKey = (rows: FrameRow[], key: string) => {
  const row = rows.find((r) => r.key === key);
  if (!row) throw new Error(`no row ${key}`);
  return row;
};

const light = (frameId: number, filename: string, over: Partial<LightFrameRef> = {}): LightFrameRef => ({
  frameId, filename, night: null, exptimeSec: null, ...over,
});

describe('buildProjectFrameRows', () => {
  it('has one row per plan frame, keyed by its uuid, with the publisher and the file name', () => {
    const rows = build();
    expect(rows.map((r) => r.key)).toEqual(['u1', 'u2', 'u3', 'u4', 'u5']);
    expect(rows.map((r) => r.filename)).toEqual(['a.fits', 'b.fits', 'c.fits', 'd.fits', 'e.fits']);
    expect(rows.map((r) => r.publisher)).toEqual(['Alice', 'Bob', 'Alice', 'Cleo', 'Bob']);
    expect(rows.every((r) => r.editable)).toBe(true);
    expect(rows.every((r) => r.groupKey === null && r.weight === null)).toBe(true);
  });

  it('takes the exposure from the plan frame and has no night', () => {
    const rows = build(null, [], null, [pf({ frameUuid: 'u1', exptimeSec: 180 })]);
    expect(rows[0]).toMatchObject({ exptimeSec: 180, night: null });
  });

  it('carries the reason of an unusable frame, whatever onDisk says, and pins only usable ones', () => {
    const rows = build();
    expect(rows.map((r) => r.unavailable)).toEqual([
      null, 'not on this device', 'unsupported file: 8-bit JPEG', null, CANNOT_OPEN,
    ]);
    // u5 is landed (`onDisk` true) yet carries a reason: still not pinnable.
    expect(rows.map((r) => r.pinnable)).toEqual([true, false, false, true, false]);
    // The frame without a WCS is pinnable: a project of older publishers has no other manual reference.
    expect(byKey(rows, 'u4').pinnable).toBe(true);
  });

  it('marks the pinned frame, and none when nothing is pinned', () => {
    expect(build().some((r) => r.pinned)).toBe(false);
    expect(build(null, [], 'u4').map((r) => r.pinned)).toEqual([false, false, false, true, false]);
  });

  it('reads the exclusion state from the uuid set, apart from the outcome', () => {
    const rows = build(null, ['u1', 'u4']);
    expect(rows.map((r) => r.outOfNextRun)).toEqual([true, false, false, true, false]);
    // No run row and no plan reason: no outcome, the exclusion is the only signal.
    expect(byKey(rows, 'u1')).toMatchObject({ outcome: null, outOfNextRun: true });
    expect(byKey(rows, 'u4')).toMatchObject({ outcome: null, outOfNextRun: true });
    expect(build().find((r) => r.key === 'u1')).toMatchObject({ outcome: null, outOfNextRun: false });
  });

  it('says why a frame no run can stack is not stacked, whether it is excluded or not', () => {
    const rows = build(null, ['u2']);
    expect(byKey(rows, 'u2').outcome).toEqual({
      kind: 'notStackable', label: 'Not stackable here', tone: 'warning', reason: 'not on this device',
    });
    expect(byKey(rows, 'u2').outOfNextRun).toBe(true);
    expect(byKey(rows, 'u5').outcome).toMatchObject({ kind: 'notStackable', reason: CANNOT_OPEN });
  });

  it('project_unusable_row_is_not_stackable', () => {
    const rows = build(null, [], null, [pf({ frameUuid: 'u1', onDisk: false, reason: 'not on this device' })]);
    expect(rows[0]?.outcome).toMatchObject({ kind: 'notStackable', label: 'Not stackable here', reason: 'not on this device' });
  });

  it('joins a run row by its frame uuid, never by the handle', () => {
    // u2 carries handle 7, the very number the run row of u1 has as its frameId: the join is by uuid.
    const frames = [
      pf({ handle: 99, frameUuid: 'u1', fileName: 'a.fits' }),
      pf({ handle: 7, frameUuid: 'u9', fileName: 'z.fits' }),
    ];
    const rows = build(
      detailOf([runRow({ frameId: 7, frameUuid: 'u1' })], [summaryFrame({ frameId: 7, fwhmPx: 2.5 })]),
      [],
      null,
      frames,
    );
    expect(byKey(rows, 'u1')).toMatchObject({
      filename: 'a.fits', groupKey: 'L-mono', weight: 0.75, fwhmPx: 2.5, stars: 120, regModel: 'homography',
      outcome: { kind: 'included', label: 'Included' },
    });
    expect(byKey(rows, 'u9')).toMatchObject({ filename: 'z.fits', groupKey: null, weight: null, fwhmPx: null });
  });

  it('shows the run outcome of a joined row and still says the frame is unavailable now', () => {
    const rows = build(
      detailOf(
        [runRow({ frameId: 5, frameUuid: 'u5', included: false, exclusionReason: 'low weight' })],
        [summaryFrame({ frameId: 5, included: false, weight: null })],
      ),
    );
    expect(byKey(rows, 'u5')).toMatchObject({
      outcome: { kind: 'excluded', label: 'Excluded', reason: 'low weight' },
      unavailable: CANNOT_OPEN, pinnable: false, groupKey: 'L-mono',
    });
  });

  it('keeps a run row whose uuid has no plan row, by its uuid and the summary file name, with no toggle and no pin', () => {
    const rows = build(
      detailOf(
        [runRow({ frameId: 7, frameUuid: 'gone' })],
        [summaryFrame({ frameId: 7, filename: 'from-summary.fits' })],
      ),
      [],
      'gone',
    );
    expect(rows).toHaveLength(6);
    const gone = byKey(rows, 'gone');
    expect(gone).toMatchObject({
      filename: 'from-summary.fits', publisher: null, groupKey: 'L-mono', editable: false, pinnable: false,
      pinned: false, unavailable: null, fwhmPx: 2.5, night: null, exptimeSec: null,
    });
    // It never takes a plan row's uuid, nor lends its numbers to one.
    expect(rows.filter((r) => r.key !== 'gone').every((r) => r.fwhmPx === null)).toBe(true);
  });

  it('names a run row with no uuid and no summary frame <handle>, keyed apart from every uuid', () => {
    const rows = build(detailOf([runRow({ frameId: 41, frameUuid: null })], null));
    const orphan = byKey(rows, 'h:41');
    expect(orphan).toMatchObject({
      filename: 'frame 41', editable: false, pinnable: false, pinned: false, publisher: null,
    });
    expect(rows.map((r) => r.key)).not.toContain('41');
  });

  it('does not join a run row without a uuid to a plan row that has its handle', () => {
    const frames = [pf({ handle: 41, frameUuid: 'u1', fileName: 'a.fits' })];
    const rows = build(detailOf([runRow({ frameId: 41, frameUuid: null })], null), [], null, frames);
    expect(rows.map((r) => r.key)).toEqual(['u1', 'h:41']);
    expect(byKey(rows, 'u1')).toMatchObject({ weight: null, groupKey: null });
  });

  it('takes the group of a row with no run row from the plan map, by its handle', () => {
    const rows = build(null, [], null, PLAN_FRAMES, ctxOf({ planGroupByFrameId: new Map([[1, 'L-mono'], [4, 'Ha-mono']]) }));
    expect(rows.map((r) => r.groupKey)).toEqual(['L-mono', null, null, 'Ha-mono', null]);
  });

  it('prefers the run group over the plan map once a run row exists', () => {
    const rows = build(
      detailOf([runRow({ frameId: 1, frameUuid: 'u1', groupId: 10 })], null),
      [],
      null,
      PLAN_FRAMES,
      ctxOf({ planGroupByFrameId: new Map([[1, 'planned-elsewhere']]) }),
    );
    expect(byKey(rows, 'u1').groupKey).toBe('L-mono');
  });
});

describe('buildFrameRows for a frame set', () => {
  it('keys a row by its frame id and carries no project column', () => {
    const rows = buildFrameRows(
      [light(101, 'a.fit'), light(102, 'b.fit')],
      null,
      null,
      [102],
      ctxOf(),
    );
    expect(rows.map((r) => r.key)).toEqual(['101', '102']);
    expect(rows[1]).toMatchObject({
      frameId: 102, filename: 'b.fit', outOfNextRun: true, publisher: null, pinned: false, unavailable: null,
      editable: true,
    });
    expect(rows[0]).toMatchObject({ outOfNextRun: false });
  });

  it('carries the night and the exposure of a light', () => {
    const rows = buildFrameRows(
      [light(101, 'a.fit', { night: '2026-10-12', exptimeSec: 120 }), light(102, 'b.fit')],
      null,
      null,
      [],
      ctxOf(),
    );
    expect(rows[0]).toMatchObject({ night: '2026-10-12', exptimeSec: 120 });
    expect(rows[1]).toMatchObject({ night: null, exptimeSec: null });
  });

  it('joins a run row and its summary frame by the frame id, as before', () => {
    const detail = detailOf(
      [runRow({ frameId: 101, groupId: 10, included: false, exclusionReason: 'low weight', weight: 0.3, regRmsPx: 0.4 })],
      [summaryFrame({ frameId: 101, filename: 'from-summary.fit', weight: null, regRmsPx: null })],
    );
    const rows = buildFrameRows([light(101, 'a.fit'), light(102, 'b.fit')], detail, detail.summary, [], ctxOf());
    expect(rows[0]).toMatchObject({
      key: '101', filename: 'from-summary.fit', groupKey: 'L-mono', weight: 0.3, regRmsPx: 0.4, fwhmPx: 2.5,
      outcome: { kind: 'excluded', label: 'Excluded', tone: 'warning', reason: 'low weight' },
    });
    // No run row: the light's own name and an empty record, whatever the summary says.
    expect(rows[1]).toMatchObject({ key: '102', filename: 'b.fit', groupKey: null, weight: null, fwhmPx: null });
  });

  it('keeps the run row of a frame the set no longer lists, keyed by its id', () => {
    const detail = detailOf([runRow({ frameId: 55 })], [summaryFrame({ frameId: 55, filename: 'old.fit' })]);
    const rows = buildFrameRows([light(101, 'a.fit')], detail, detail.summary, [], ctxOf());
    expect(rows.map((r) => [r.key, r.filename])).toEqual([['101', 'a.fit'], ['55', 'old.fit']]);
    expect(rows[1]).toMatchObject({ night: null, exptimeSec: null });
  });

  it('failed_reason_drops_prefix', () => {
    const detail = detailOf(
      [runRow({
        frameId: 101, included: false, regStatus: 'failed',
        exclusionReason: 'registration failed: quad seed failed (0 quad matches)',
      })],
      null,
    );
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(row?.outcome).toEqual({
      kind: 'regFailed', label: 'Reg failed', tone: 'error', reason: 'quad seed failed (0 quad matches)',
    });
  });

  it('a_failed_row_without_a_reason_has_none', () => {
    const detail = detailOf([runRow({ frameId: 101, included: false, regStatus: 'failed', exclusionReason: null })], null);
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(row?.outcome).toMatchObject({ kind: 'regFailed', reason: null });
  });

  it('a_failure_wins_over_a_plain_exclusion', () => {
    // A failed registration is always also `included = false`: the specific outcome must not be relabelled.
    const detail = detailOf(
      [runRow({ frameId: 101, included: false, regStatus: 'failed', exclusionReason: 'registration failed: no stars' })],
      null,
    );
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [101], ctxOf());
    expect(row).toMatchObject({ outcome: { kind: 'regFailed' }, outOfNextRun: true });
  });

  it('reference_row_names_run_and_mode', () => {
    const detail = detailOf([runRow({ frameId: 101, regStatus: 'reference' })], null, { id: 8, referenceMode: 'auto' });
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(row?.outcome).toEqual({ kind: 'reference', label: 'Reference', tone: 'accent', reason: 'run #8 · auto' });
  });

  it('weight_exclusion_keeps_full_reason', () => {
    const reason = 'weight 0.006 below 0.05 of the group maximum';
    const detail = detailOf(
      [runRow({ frameId: 101, regStatus: 'skipped', included: false, exclusionReason: reason })],
      null,
    );
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(row?.outcome).toEqual({ kind: 'excluded', label: 'Excluded', tone: 'warning', reason });
  });

  it('an_included_row_has_no_reason', () => {
    const detail = detailOf([runRow({ frameId: 101 })], null);
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(row?.outcome).toEqual({ kind: 'included', label: 'Included', tone: 'success', reason: null });
  });

  it('manual_exclusion_is_a_second_signal', () => {
    const detail = detailOf([runRow({ frameId: 101, regStatus: 'reference' })], null);
    const [row] = buildFrameRows([light(101, 'a.fit')], detail, null, [101], ctxOf());
    expect(row).toMatchObject({ outcome: { kind: 'reference' }, outOfNextRun: true });
  });

  it('no_run_row_no_outcome', () => {
    const detail = detailOf([runRow({ frameId: 55 })], null);
    const rows = buildFrameRows([light(101, 'a.fit')], detail, null, [], ctxOf());
    expect(rows[0]).toMatchObject({ outcome: null, groupKey: null });
    const none = buildFrameRows([light(101, 'a.fit')], null, null, [], ctxOf());
    expect(none[0]).toMatchObject({ outcome: null, groupKey: null });
  });

  it('takes the group of a row with no run row from the plan map', () => {
    const rows = buildFrameRows(
      [light(101, 'a.fit'), light(102, 'b.fit')],
      null,
      null,
      [],
      ctxOf({ planGroupByFrameId: new Map([[101, 'L-mono']]) }),
    );
    expect(rows.map((r) => r.groupKey)).toEqual(['L-mono', null]);
  });

  it('set_pin_follows_ctx', () => {
    const detail = detailOf([runRow({ frameId: 55 })], null);
    const rows = buildFrameRows(
      [light(7, 'a.fit'), light(8, 'b.fit')],
      detail,
      null,
      [],
      ctxOf({ pinnedFrameId: 7 }),
    );
    expect(rows.map((r) => [r.frameId, r.pinned, r.pinnable])).toEqual([[7, true, true], [8, false, true], [55, false, false]]);
    expect(buildFrameRows([light(7, 'a.fit')], null, null, [], ctxOf()).some((r) => r.pinned)).toBe(false);
  });
});

describe('sortRows', () => {
  const rows = buildFrameRows(
    [
      light(1, 'c.fit', { night: '2026-10-12', exptimeSec: 300 }),
      light(2, 'a.fit', { night: null, exptimeSec: 120 }),
      light(3, 'b.fit', { night: '2026-10-11', exptimeSec: 180 }),
    ],
    detailOf(
      [runRow({ frameId: 1 }), runRow({ id: 2, frameId: 2, included: false, exclusionReason: 'low' })],
      null,
    ),
    null,
    [],
    ctxOf(),
  );

  it('sorts a text column in either direction', () => {
    expect(sortRows(rows, 'filename', 'asc').map((r) => r.filename)).toEqual(['a.fit', 'b.fit', 'c.fit']);
    expect(sortRows(rows, 'filename', 'desc').map((r) => r.filename)).toEqual(['c.fit', 'b.fit', 'a.fit']);
  });

  it('sorts a number column and keeps a missing value last in both directions', () => {
    expect(sortRows(rows, 'exptimeSec', 'asc').map((r) => r.exptimeSec)).toEqual([120, 180, 300]);
    expect(sortRows(rows, 'night', 'asc').map((r) => r.night)).toEqual(['2026-10-11', '2026-10-12', null]);
    expect(sortRows(rows, 'night', 'desc').map((r) => r.night)).toEqual(['2026-10-12', '2026-10-11', null]);
  });

  it('sorts the status by the outcome label, rows without an outcome last', () => {
    expect(sortRows(rows, 'status', 'asc').map((r) => r.outcome?.label ?? null)).toEqual(['Excluded', 'Included', null]);
  });

  it('does not touch the input', () => {
    const before = rows.map((r) => r.key);
    sortRows(rows, 'filename', 'desc');
    expect(rows.map((r) => r.key)).toEqual(before);
  });
});
