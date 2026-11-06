// Wave 6c Task 6: the Frames table of a project is keyed by frame uuid — its row source, the Publisher column, the
// exclusion checkbox and the reference pin — and the frame-set table keeps the rows and columns it always had.
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import {
  buildFrameRows,
  buildProjectFrameRows,
  FramesTable,
  type FramesTableProps,
} from './FramesTable';
import type {
  PlanProjectFrame,
  RunSummary,
  StackingRunDetail,
  StackingRunFrameRow,
  SummaryFrame,
} from '../../types/stacking';

const NO_WCS_TIP = 'No WCS in the manifest — plate-solved locally first';
const CANNOT_OPEN = 'cannot be opened (offline, moved or renamed)';

const pf = (over: Partial<PlanProjectFrame> = {}): PlanProjectFrame => ({
  handle: 1, frameUuid: 'u1', fileName: 'a.fits', publisher: 'Alice', filterCanonical: 'L', exptimeSec: 300,
  colorMode: 'mono', hasWcs: true, onDisk: true, reason: null, ...over,
});

/** Five plan rows: usable with a WCS (u1), not on this device (u2), unsupported (u3), usable without a WCS (u4), and
 *  landed but not openable (u5: `onDisk` true AND a reason). */
const PLAN_FRAMES: PlanProjectFrame[] = [
  pf({ handle: 1, frameUuid: 'u1', fileName: 'a.fits', publisher: 'Alice' }),
  pf({ handle: null, frameUuid: 'u2', fileName: 'b.fits', publisher: 'Bob', onDisk: false, reason: 'not on this device' }),
  pf({ handle: 3, frameUuid: 'u3', fileName: 'c.fits', publisher: 'Alice', reason: 'unsupported file: 8-bit JPEG' }),
  pf({ handle: 4, frameUuid: 'u4', fileName: 'd.fits', publisher: 'Cleo', hasWcs: false }),
  pf({ handle: 5, frameUuid: 'u5', fileName: 'e.fits', publisher: 'Bob', reason: CANNOT_OPEN }),
];

const runRow = (over: Partial<StackingRunFrameRow>): StackingRunFrameRow => ({
  id: 1, runId: 5, groupId: 10, frameId: 7, included: true, exclusionReason: null, weight: 0.8,
  weightChannelsJson: null, metricsJson: null, regStatus: 'ok', regModel: 'homography', regRmsPx: 0.2, regInliers: 90,
  regInlierRatio: 0.9, regFlipped: false, rejectedFraction: null, frameUuid: null, ...over,
});

const summaryFrame = (over: Partial<SummaryFrame>): SummaryFrame =>
  ({
    frameId: 7, filename: 'from-summary.fits', included: true, exclusionReason: null, weight: 0.75, weightChannels: [],
    fwhmPx: 2.5, eccentricity: 0.2, stars: 120, regStatus: 'ok', regModel: 'homography', regRmsPx: 0.3, regInliers: 88,
    lnScale: null, ...over,
  }) as unknown as SummaryFrame;

const detailOf = (frames: StackingRunFrameRow[], summaryFrames: SummaryFrame[] | null) => ({
  groups: [{ id: 10, groupKey: 'L-mono' }] as unknown as StackingRunDetail['groups'],
  frames,
  summary: summaryFrames && ({ groups: [{ frames: summaryFrames }] } as unknown as RunSummary),
});

const build = (
  detail: ReturnType<typeof detailOf> | null = null,
  excluded: string[] = [],
  referenceFrameUuid: string | null = null,
  frames: PlanProjectFrame[] = PLAN_FRAMES,
) =>
  buildProjectFrameRows(
    frames,
    detail && { groups: detail.groups, frames: detail.frames },
    detail?.summary ?? null,
    new Set(excluded),
    referenceFrameUuid,
  );

const byKey = (rows: ReturnType<typeof build>, key: string) => {
  const row = rows.find((r) => r.key === key);
  if (!row) throw new Error(`no row ${key}`);
  return row;
};

describe('buildProjectFrameRows', () => {
  it('has one row per plan frame, keyed by its uuid, with the publisher and the file name', () => {
    const rows = build();
    expect(rows.map((r) => r.key)).toEqual(['u1', 'u2', 'u3', 'u4', 'u5']);
    expect(rows.map((r) => r.filename)).toEqual(['a.fits', 'b.fits', 'c.fits', 'd.fits', 'e.fits']);
    expect(rows.map((r) => r.publisher)).toEqual(['Alice', 'Bob', 'Alice', 'Cleo', 'Bob']);
    expect(rows.every((r) => r.editable)).toBe(true);
    expect(rows.every((r) => r.group === '—' && r.weight === null)).toBe(true);
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

  it('reads the exclusion state from the uuid set', () => {
    const rows = build(null, ['u1', 'u4']);
    expect(byKey(rows, 'u1')).toMatchObject({ statusLabel: 'Excluded (manual)', statusTone: 'warning' });
    expect(byKey(rows, 'u4')).toMatchObject({ statusLabel: 'Excluded (manual)' });
    expect(byKey(rows, 'u5').statusLabel).not.toBe('Excluded (manual)');
    expect(build().find((r) => r.key === 'u1')).toMatchObject({ statusLabel: 'Included', statusTone: 'success' });
  });

  it('says why a frame no run can stack is not stacked, apart from an excluded one', () => {
    const rows = build();
    expect(byKey(rows, 'u2')).toMatchObject({ statusLabel: 'Not stackable here', statusTone: 'warning' });
    expect(byKey(rows, 'u5').statusLabel).toBe('Not stackable here');
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
      filename: 'a.fits', group: 'L-mono', weight: 0.75, fwhmPx: 2.5, stars: 120, regModel: 'homography',
      statusLabel: 'Included',
    });
    expect(byKey(rows, 'u9')).toMatchObject({ filename: 'z.fits', group: '—', weight: null, fwhmPx: null });
  });

  it('shows the run outcome of a joined row and still says the frame is unavailable now', () => {
    const rows = build(
      detailOf(
        [runRow({ frameId: 5, frameUuid: 'u5', included: false, exclusionReason: 'low weight' })],
        [summaryFrame({ frameId: 5, included: false, weight: null })],
      ),
    );
    expect(byKey(rows, 'u5')).toMatchObject({
      statusLabel: 'low weight', unavailable: CANNOT_OPEN, pinnable: false, group: 'L-mono',
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
      filename: 'from-summary.fits', publisher: null, group: 'L-mono', editable: false, pinnable: false,
      pinned: false, unavailable: null, fwhmPx: 2.5,
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
    expect(byKey(rows, 'u1')).toMatchObject({ weight: null, group: '—' });
  });
});

describe('buildFrameRows for a frame set', () => {
  it('keys a row by its frame id and carries no project column', () => {
    const rows = buildFrameRows(
      [{ frameId: 101, filename: 'a.fit' }, { frameId: 102, filename: 'b.fit' }],
      null,
      null,
      [102],
    );
    expect(rows.map((r) => r.key)).toEqual(['101', '102']);
    expect(rows[1]).toMatchObject({
      frameId: 102, filename: 'b.fit', statusLabel: 'Excluded (manual)', publisher: null, pinnable: false,
      pinned: false, unavailable: null, editable: true,
    });
    expect(rows[0]).toMatchObject({ statusLabel: 'Included', statusTone: 'success' });
  });

  it('joins a run row and its summary frame by the frame id, as before', () => {
    const detail = detailOf(
      [runRow({ frameId: 101, groupId: 10, included: false, exclusionReason: 'low weight', weight: 0.3, regRmsPx: 0.4 })],
      [summaryFrame({ frameId: 101, filename: 'from-summary.fit', weight: null, regRmsPx: null })],
    );
    const rows = buildFrameRows(
      [{ frameId: 101, filename: 'a.fit' }, { frameId: 102, filename: 'b.fit' }],
      detail,
      detail.summary,
      [],
    );
    expect(rows[0]).toMatchObject({
      key: '101', filename: 'from-summary.fit', group: 'L-mono', weight: 0.3, regRmsPx: 0.4, fwhmPx: 2.5,
      statusLabel: 'low weight', statusTone: 'warning',
    });
    // No run row: the light's own name and an empty record, whatever the summary says.
    expect(rows[1]).toMatchObject({ key: '102', filename: 'b.fit', group: '—', weight: null, fwhmPx: null });
  });

  it('keeps the run row of a frame the set no longer lists, keyed by its id', () => {
    const detail = detailOf([runRow({ frameId: 55 })], [summaryFrame({ frameId: 55, filename: 'old.fit' })]);
    const rows = buildFrameRows([{ frameId: 101, filename: 'a.fit' }], detail, detail.summary, []);
    expect(rows.map((r) => [r.key, r.filename])).toEqual([['101', 'a.fit'], ['55', 'old.fit']]);
  });
});

function renderTable(over: Partial<FramesTableProps> = {}) {
  const props: FramesTableProps = {
    subjectKind: 'project',
    lightFrames: [],
    projectFrames: PLAN_FRAMES,
    runDetail: null,
    excludedKeys: [],
    referenceFrameUuid: null,
    onToggleExclude: vi.fn(),
    onPin: vi.fn(),
    collapsed: false,
    onToggleCollapsed: vi.fn(),
    ...over,
  };
  render(<FramesTable {...props} />);
  return props;
}

const rowOf = (name: string) => {
  const row = screen.getByText(name).closest('tr');
  if (!(row instanceof HTMLElement)) throw new Error(`no row for ${name}`);
  return row;
};

describe('FramesTable for a project', () => {
  it('adds the Publisher and Reference columns and lists every plan frame', () => {
    renderTable();
    expect(screen.getByRole('button', { name: 'Publisher' })).toBeInTheDocument();
    expect(screen.getByRole('columnheader', { name: 'Reference' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^Frames \(5\)/ })).toBeInTheDocument();
    expect(within(rowOf('a.fits')).getByText('Alice')).toBeInTheDocument();
    expect(within(rowOf('d.fits')).getByText('Cleo')).toBeInTheDocument();
  });

  it('sorts by the publisher', () => {
    renderTable();
    fireEvent.click(screen.getByRole('button', { name: 'Publisher' }));
    const names = screen.getAllByRole('row').slice(1).map((r) => within(r).getAllByRole('cell')[0]?.textContent);
    expect(names.map((n) => n?.slice(0, 6))).toEqual(['a.fits', 'c.fits', 'b.fits', 'e.fits', 'd.fits']);
  });

  it('pins a usable frame by its uuid, with or without a WCS', () => {
    const { onPin } = renderTable();
    fireEvent.click(within(rowOf('a.fits')).getByRole('button', { name: 'Use a.fits as the reference frame' }));
    expect(onPin).toHaveBeenLastCalledWith('u1');
    const noWcs = within(rowOf('d.fits')).getByRole('button', { name: 'Use d.fits as the reference frame' });
    expect(noWcs).toBeEnabled();
    expect(noWcs).toHaveAttribute('title', NO_WCS_TIP);
    fireEvent.click(noWcs);
    expect(onPin).toHaveBeenLastCalledWith('u4');
    expect(onPin).toHaveBeenCalledTimes(2);
  });

  it('gives a frame with a WCS a plain pin tooltip', () => {
    renderTable();
    expect(within(rowOf('a.fits')).getByRole('button', { name: /reference frame/ })).toHaveAttribute(
      'title',
      'Use as the reference frame',
    );
  });

  it('disables the pin of an unusable frame and puts its reason in the tooltip', () => {
    const { onPin } = renderTable();
    for (const [file, reason] of [
      ['b.fits', 'not on this device'],
      ['c.fits', 'unsupported file: 8-bit JPEG'],
      ['e.fits', CANNOT_OPEN],
    ] as const) {
      const pin = within(rowOf(file)).getByRole('button', { name: `Use ${file} as the reference frame` });
      expect(pin).toBeDisabled();
      expect(pin).toHaveAttribute('title', reason);
      fireEvent.click(pin);
    }
    expect(onPin).not.toHaveBeenCalled();
    // The reason also reads in the row, not only in a tooltip.
    expect(within(rowOf('b.fits')).getAllByText('not on this device').length).toBeGreaterThan(0);
  });

  it('shows the pinned frame as pressed and clears it by the same click', () => {
    const { onPin } = renderTable({ referenceFrameUuid: 'u4' });
    const pinned = within(rowOf('d.fits')).getByRole('button', { name: 'Clear the reference frame d.fits' });
    expect(pinned).toHaveAttribute('aria-pressed', 'true');
    expect(pinned).toHaveAttribute('title', 'The reference frame — click to clear');
    expect(within(rowOf('a.fits')).getByRole('button', { name: /reference frame/ })).toHaveAttribute(
      'aria-pressed',
      'false',
    );
    fireEvent.click(pinned);
    expect(onPin).toHaveBeenCalledWith('u4');
  });

  it('lets the pin of a frame that became unusable be cleared', () => {
    const { onPin } = renderTable({ referenceFrameUuid: 'u2' });
    const pinned = within(rowOf('b.fits')).getByRole('button', { name: 'Clear the reference frame b.fits' });
    expect(pinned).toBeEnabled();
    fireEvent.click(pinned);
    expect(onPin).toHaveBeenCalledWith('u2');
  });

  it('toggles exclusion by uuid and shows an excluded frame unchecked', () => {
    const { onToggleExclude } = renderTable({ excludedKeys: ['u1'] });
    const included = within(rowOf('a.fits')).getByRole('checkbox', { name: 'Include a.fits in the next run' });
    expect(included).not.toBeChecked();
    expect(within(rowOf('d.fits')).getByRole('checkbox')).toBeChecked();
    fireEvent.click(included);
    expect(onToggleExclude).toHaveBeenCalledWith('u1');
  });

  it('disables the checkboxes and the pins while a run is active', () => {
    renderTable({ disabled: true });
    expect(within(rowOf('a.fits')).getByRole('checkbox')).toBeDisabled();
    expect(within(rowOf('a.fits')).getByRole('button', { name: /reference frame/ })).toBeDisabled();
  });

  it('renders a run row that left the plan from the run alone, with no checkbox and no pin', () => {
    const detail = detailOf(
      [runRow({ frameId: 41, frameUuid: 'gone' }), runRow({ id: 2, frameId: 42, frameUuid: null })],
      [summaryFrame({ frameId: 41, filename: 'old.fits' })],
    );
    const runDetail = { run: { id: 5 }, ...detail } as unknown as StackingRunDetail;
    renderTable({ runDetail });
    const gone = rowOf('old.fits');
    expect(within(gone).queryByRole('checkbox')).toBeNull();
    expect(within(gone).queryByRole('button')).toBeNull();
    const nameless = rowOf('frame 42');
    expect(within(nameless).queryByRole('checkbox')).toBeNull();
    expect(within(nameless).queryByRole('button')).toBeNull();
    expect(screen.getByRole('button', { name: /^Frames \(7\)/ })).toBeInTheDocument();
  });

  it('says so when a project has no frames', () => {
    renderTable({ projectFrames: [] });
    expect(screen.getByText('No frames in this project.')).toBeInTheDocument();
  });
});

describe('FramesTable for a frame set', () => {
  const setProps: Partial<FramesTableProps> = {
    subjectKind: 'frameSet',
    lightFrames: [{ frameId: 101, filename: 'a.fit' }, { frameId: 102, filename: 'b.fit' }],
    projectFrames: [],
    excludedKeys: ['102'],
  };

  it('keeps its columns: no Publisher, no Reference', () => {
    renderTable(setProps);
    expect(screen.queryByRole('button', { name: 'Publisher' })).toBeNull();
    expect(screen.queryByRole('columnheader', { name: 'Reference' })).toBeNull();
    expect(screen.queryByRole('button', { name: /reference frame/ })).toBeNull();
    expect(screen.getByRole('button', { name: 'Filename' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Status' })).toBeInTheDocument();
  });

  it('toggles by the frame id spelled as a string', () => {
    const { onToggleExclude } = renderTable(setProps);
    const second = screen.getByRole('checkbox', { name: 'Include b.fit in the next run' });
    expect(second).not.toBeChecked();
    fireEvent.click(second);
    expect(onToggleExclude).toHaveBeenCalledWith('102');
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include a.fit in the next run' }));
    expect(onToggleExclude).toHaveBeenLastCalledWith('101');
  });

  it('says so when the set has no light frames', () => {
    renderTable({ ...setProps, lightFrames: [], excludedKeys: [] });
    expect(screen.getByText('No light frames in this set.')).toBeInTheDocument();
  });
});
