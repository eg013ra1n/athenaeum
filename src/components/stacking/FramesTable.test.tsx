// Wave 6c Task 6: the Frames table of a project is keyed by frame uuid — the Publisher column, the exclusion checkbox
// and the reference pin — and the frame-set table keeps the columns it always had. The row model itself is tested in
// `frameRows.test.ts`; this file renders it.
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { FramesTable, type FramesTableProps } from './FramesTable';
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
  run: { id: 5, referenceMode: 'auto' },
  groups: [{ id: 10, groupKey: 'L-mono' }] as unknown as StackingRunDetail['groups'],
  frames,
  summary: summaryFrames && ({ groups: [{ frames: summaryFrames }] } as unknown as RunSummary),
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
    // The tooltip keeps the clear hint and adds why the frame can no longer be used.
    expect(pinned).toHaveAttribute('title', 'The reference frame — click to clear (not on this device)');
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
    const runDetail = detail as unknown as StackingRunDetail;
    renderTable({ runDetail });
    const gone = rowOf('old.fits');
    expect(within(gone).queryByRole('checkbox')).toBeNull();
    expect(within(gone).queryByRole('button')).toBeNull();
    const nameless = rowOf('frame 42');
    expect(within(nameless).queryByRole('checkbox')).toBeNull();
    expect(within(nameless).queryByRole('button')).toBeNull();
    expect(screen.getByRole('button', { name: /^Frames \(7\)/ })).toBeInTheDocument();
  });

  it('shows the outcome chip and, apart from it, the next-run exclusion chip', () => {
    const runDetail = detailOf(
      [
        runRow({ frameId: 1, frameUuid: 'u1', regStatus: 'reference' }),
        runRow({ id: 2, frameId: 4, frameUuid: 'u4', included: false, exclusionReason: 'low weight' }),
        runRow({
          id: 3, frameId: 5, frameUuid: 'u5', included: false, regStatus: 'failed',
          exclusionReason: 'registration failed: quad seed failed (0 quad matches)',
        }),
      ],
      null,
    ) as unknown as StackingRunDetail;
    renderTable({ runDetail, excludedKeys: ['u1', 'u4'] });
    const reference = rowOf('a.fits');
    expect(within(reference).getByText('Reference')).toHaveClass('bg-accent/20');
    expect(within(reference).getByText('run #5 · auto')).toBeInTheDocument();
    // The reference row is also out of the next run: both signals, never one chip.
    expect(within(reference).getByText('Out of next run')).toBeInTheDocument();
    const excluded = rowOf('d.fits');
    expect(within(excluded).getByText('Excluded')).toBeInTheDocument();
    expect(within(excluded).getByText('low weight')).toBeInTheDocument();
    expect(within(excluded).getByText('Out of next run')).toBeInTheDocument();
    const failed = rowOf('e.fits');
    expect(within(failed).getByText('Reg failed')).toBeInTheDocument();
    expect(within(failed).getByText('quad seed failed (0 quad matches)')).toBeInTheDocument();
    expect(within(failed).queryByText('Out of next run')).toBeNull();
  });

  it('shows no outcome chip for a usable frame with no run row, and Not stackable here for an unusable one', () => {
    renderTable({ excludedKeys: ['u4'] });
    const usable = rowOf('a.fits');
    expect(within(usable).queryByText('Included')).toBeNull();
    expect(within(usable).queryByText('Out of next run')).toBeNull();
    expect(within(rowOf('d.fits')).getByText('Out of next run')).toBeInTheDocument();
    expect(within(rowOf('b.fits')).getByText('Not stackable here')).toBeInTheDocument();
  });

  it('says so when a project has no frames', () => {
    renderTable({ projectFrames: [] });
    expect(screen.getByText('No frames in this project.')).toBeInTheDocument();
  });
});

describe('FramesTable for a frame set', () => {
  const setProps: Partial<FramesTableProps> = {
    subjectKind: 'frameSet',
    lightFrames: [
      { frameId: 101, filename: 'a.fit', night: null, exptimeSec: null },
      { frameId: 102, filename: 'b.fit', night: null, exptimeSec: null },
    ],
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
