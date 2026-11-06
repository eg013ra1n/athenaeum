// The Frames block (spec §6.1–§6.6): the Analysis-style toolbar over a grouped table with row selection. Rows are
// keyed by frame uuid for a project and by frame id for a frame set. The row model is tested in `frameRows.test.ts`
// and the grouping/filter logic in `frameView.test.ts`; this file renders them.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { FramesTable, type FramesTableProps } from './FramesTable';
import type { FrameRow, LightFrameRef } from './frameRows';
import type { PlanGroup, RunSummary, StackingRunDetail } from '../../types/stacking';
import { CANNOT_OPEN, PLAN_FRAMES, detailOf, runRow, summaryFrame } from './testFixtures';

const NO_WCS_TIP = 'No WCS in the manifest — plate-solved locally first';

// ── A run #8-like frame set: groups L, R and B, R lost to registration ──────────────────────────────────────────

const planGroup = (key: string, pixelScaleArcsec: number | null): PlanGroup =>
  ({ key, filter: key, exposureClass: '300s', colorMode: 'mono', binning: 1, pixelScaleArcsec }) as unknown as PlanGroup;

/** The plan's order is L, R, B; R has a scale, B has none. */
const PLAN_GROUPS = [planGroup('L', 1.2), planGroup('R', 1.2), planGroup('B', null)];

const RUN_GROUP_ID: Record<string, number> = { L: 1, R: 2, B: 3 };
const L_IDS = [7, 8, 9, 10];
const R_IDS = [21, 22, 23, 24];
const B_IDS = [31, 32];
const fileOf = (id: number) => `${id < 20 ? 'L' : id < 30 ? 'R' : 'B'}_${String(id).padStart(4, '0')}.fits`;

/** B's lights come first, so the plan's order (not the set's) is what orders the groups. */
const SET_LIGHTS: LightFrameRef[] = [...B_IDS, ...L_IDS, ...R_IDS].map((frameId) => ({
  frameId, filename: fileOf(frameId), night: null, exptimeSec: 300,
}));

const QUAD_FAIL = 'registration failed: 0 quad matches';
const FWHM: Record<number, number> = { 7: 2.0, 8: 2.0, 9: 2.1, 10: 4.0, 31: 2.0, 32: 2.2 };

function run8(geometry: 'coRegistered' | 'native' = 'coRegistered', id = 8): StackingRunDetail {
  const groupOf = (frameId: number) => (L_IDS.includes(frameId) ? 'L' : R_IDS.includes(frameId) ? 'R' : 'B');
  const ids = [...L_IDS, ...R_IDS, ...B_IDS];
  const frames = ids.map((frameId, i) => {
    const failed = R_IDS.includes(frameId);
    return runRow({
      id: i + 1, runId: id, frameId, groupId: RUN_GROUP_ID[groupOf(frameId)],
      included: !failed, exclusionReason: failed ? QUAD_FAIL : null,
      regStatus: failed ? 'failed' : frameId === 7 ? 'reference' : 'ok',
    });
  });
  const summaryOf = (frameId: number) =>
    summaryFrame({ frameId, filename: fileOf(frameId), fwhmPx: FWHM[frameId] ?? 2.0, eccentricity: 0.2 });
  const summary = {
    config: { registration: { geometry } },
    reference: { frameId: 7, filename: 'L_0007.fits', mode: 'auto', weight: 1, switchedFrom: null },
    groups: [
      { key: 'L', referenceFrameId: 7, normalizationReferenceFrameId: 9, frames: L_IDS.map(summaryOf) },
      { key: 'R', referenceFrameId: null, normalizationReferenceFrameId: null, frames: R_IDS.map(summaryOf) },
      { key: 'B', referenceFrameId: 31, normalizationReferenceFrameId: null, frames: B_IDS.map(summaryOf) },
    ],
  } as unknown as RunSummary;
  return {
    run: {
      id, referenceMode: 'auto', startedAt: '2026-11-05T22:14:03',
      configJson: JSON.stringify({ registration: { geometry } }),
    },
    groups: Object.entries(RUN_GROUP_ID).map(([groupKey, gid]) => ({ id: gid, groupKey })),
    frames,
    summary,
  } as unknown as StackingRunDetail;
}

const SET: Partial<FramesTableProps> = {
  subjectKind: 'frameSet',
  lightFrames: SET_LIGHTS,
  projectFrames: [],
  planGroups: PLAN_GROUPS,
  runDetail: run8(),
};

// The view (grouping, units, columns) is remembered in local storage; every test starts from the defaults.
beforeEach(() => localStorage.clear());

// ── Helpers ─────────────────────────────────────────────────────────────────────────────────────────────────────

function renderTable(over: Partial<FramesTableProps> = {}) {
  const props: FramesTableProps = {
    subjectKind: 'project',
    lightFrames: [],
    projectFrames: PLAN_FRAMES,
    runDetail: null,
    planGroups: [],
    planReference: null,
    excludedKeys: [],
    referenceFrameUuid: null,
    pinnedFrameId: null,
    planGroupByFrameId: new Map(),
    onSetExcluded: vi.fn(),
    onPin: vi.fn(),
    onBlink: vi.fn(),
    collapsed: false,
    onToggleCollapsed: vi.fn(),
    ...over,
  };
  const view = render(<FramesTable {...props} />);
  const rerender = (next: Partial<FramesTableProps>) => view.rerender(<FramesTable {...props} {...next} />);
  return { ...props, rerender };
}

const table = () => screen.getByRole('table');

const rowOf = (name: string) => {
  const row = within(table()).getByText(name).closest('tr');
  if (!(row instanceof HTMLElement)) throw new Error(`no row for ${name}`);
  return row;
};

const select = (name: string, shiftKey = false) =>
  fireEvent.click(within(rowOf(name)).getByRole('checkbox', { name: `Select ${name}` }), { shiftKey });

const selectionCount = () => screen.getByTitle('Clear selection').textContent;
const toolbar = () => screen.getByRole('toolbar', { name: 'Frames toolbar' });
const headers = () => screen.getAllByTestId('frame-group-header');
const headerOf = (title: RegExp) => {
  const header = headers().find((h) => title.test(h.textContent ?? ''));
  if (!header) throw new Error(`no header ${title}`);
  return header;
};
const fileColumn = () =>
  screen.getAllByTestId('frame-row').map((r) => r.querySelector('[data-col="file"]')?.textContent?.slice(0, 6));
const blinked = (onBlink: FramesTableProps['onBlink']) =>
  vi.mocked(onBlink).mock.calls.map((call) => call[0].map((r: FrameRow) => r.key));

// ── The frame set (run #8) ──────────────────────────────────────────────────────────────────────────────────────

describe('FramesTable for a frame set', () => {
  it('groups_by_stacking_group_by_default', () => {
    renderTable(SET);
    expect(headers().map((h) => h.textContent?.slice(0, 1))).toEqual(['L', 'R', 'B']);
    expect(headerOf(/^L/)).toHaveTextContent('L · 300s · mono');
    expect(headerOf(/^L/)).toHaveTextContent('4 / 4 included · 20m · median FWHM 2.10 px');
    const lost = within(headerOf(/^R/)).getByText('0 / 4 included — 4 registration failed (0 quad matches)');
    expect(lost).toHaveClass('text-error');
  });

  it('pre_run_group_header_is_muted', () => {
    // Before any run nothing was lost: the header counts what is not yet included, in muted text, never the error form.
    renderTable({
      ...SET,
      lightFrames: SET_LIGHTS.filter((f) => L_IDS.slice(0, 2).includes(f.frameId)),
      runDetail: null,
      planGroupByFrameId: new Map([[7, 'L'], [8, 'L']]),
    });
    expect(headers()).toHaveLength(1);
    const summary = within(headerOf(/^L/)).getByText('0 / 2 included');
    expect(summary).not.toHaveClass('text-error');
    expect(summary).toHaveClass('text-content-muted');
  });

  it('groups_before_first_run', () => {
    // No run yet: the plan names each group's members, so the rows sit under the plan's groups, in the plan's order,
    // and nothing falls under `Not grouped yet`.
    const lights = SET_LIGHTS.filter((f) => [...L_IDS, ...R_IDS].includes(f.frameId));
    const groupOf = (id: number): [number, string] => [id, L_IDS.includes(id) ? 'L' : 'R'];
    renderTable({
      ...SET,
      lightFrames: lights,
      runDetail: null,
      planGroups: [planGroup('L', 1.2), planGroup('R', 1.2)],
      planGroupByFrameId: new Map(lights.map((f) => groupOf(f.frameId))),
    });
    expect(headers().map((h) => h.textContent?.slice(0, 1))).toEqual(['L', 'R']);
    expect(screen.queryByText(/Not grouped yet/)).toBeNull();
    expect(headerOf(/^L/)).toHaveTextContent('0 / 4 included');
    expect(headerOf(/^R/)).toHaveTextContent('0 / 4 included');
  });

  it('names the selected run in the header line, or says there is none', () => {
    const { rerender } = renderTable(SET);
    expect(screen.getByText('metrics from run #8 · 2026-11-05 22:14:03')).toBeInTheDocument();
    rerender({ runDetail: null });
    expect(screen.getByText('no run yet')).toBeInTheDocument();
  });

  it('exclude_counts_eligible_subset', () => {
    const { onSetExcluded } = renderTable({ ...SET, excludedKeys: ['8', '9'] });
    for (const name of ['L_0007.fits', 'L_0008.fits', 'L_0009.fits', 'L_0010.fits', 'R_0021.fits']) select(name);
    const exclude = screen.getByRole('button', { name: 'Exclude (3 of 5)' });
    expect(screen.getByRole('button', { name: 'Include (2 of 5)' })).toBeEnabled();
    fireEvent.click(exclude);
    expect(onSetExcluded).toHaveBeenCalledWith(['7', '10', '21'], true);
  });

  it('includes the listed part of the selection by frame id', () => {
    const { onSetExcluded } = renderTable({ ...SET, excludedKeys: ['8'] });
    select('L_0008.fits');
    expect(screen.getByRole('button', { name: 'Exclude' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Include' }));
    expect(onSetExcluded).toHaveBeenCalledWith(['8'], false);
  });

  it('selection_survives_run_switch', () => {
    const { rerender } = renderTable(SET);
    for (const name of ['L_0007.fits', 'R_0022.fits', 'B_0031.fits']) select(name);
    expect(selectionCount()).toBe('3');
    rerender({ runDetail: run8('coRegistered', 9) });
    expect(screen.getByText(/^metrics from run #9/)).toBeInTheDocument();
    expect(selectionCount()).toBe('3');
    expect(within(rowOf('R_0022.fits')).getByRole('checkbox')).toBeChecked();
  });

  it('units_fall_back_to_px_without_scale', () => {
    renderTable(SET);
    expect(within(rowOf('L_0008.fits')).getByText('2.00 px')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'px' }));
    expect(within(rowOf('L_0008.fits')).getByText('2.40"')).toBeInTheDocument();
    expect(within(rowOf('B_0031.fits')).getByText('2.00 px')).toBeInTheDocument();
  });

  it('hides the units toggle when no group has a pixel scale', () => {
    renderTable({ ...SET, planGroups: [planGroup('L', null), planGroup('R', null), planGroup('B', null)] });
    expect(screen.queryByRole('button', { name: 'px' })).toBeNull();
  });

  it('reference_star_pins_set_row', () => {
    const { onPin } = renderTable(SET);
    fireEvent.click(within(rowOf('L_0008.fits')).getByRole('button', { name: 'Use L_0008.fits as the reference frame' }));
    expect(onPin).toHaveBeenCalledWith('8');
    // Choosing the reference never selects the row.
    expect(selectionCount()).toBe('0');
  });

  it('fills the star of the pinned frame', () => {
    renderTable({ ...SET, pinnedFrameId: 9 });
    const pinned = within(rowOf('L_0009.fits')).getByRole('button', { name: 'Clear the reference frame L_0009.fits' });
    expect(pinned).toHaveAttribute('aria-pressed', 'true');
    expect(pinned.querySelector('svg')).toHaveClass('fill-current');
  });

  it('group_header_names_ln_and_native_references', () => {
    const { rerender } = renderTable({ ...SET, runDetail: run8('native') });
    expect(headerOf(/^L/)).toHaveTextContent('Reference: L_0007.fits');
    expect(headerOf(/^L/)).toHaveTextContent('LN ref: L_0009.fits');
    rerender({ runDetail: run8('coRegistered') });
    expect(headerOf(/^L/)).not.toHaveTextContent('Reference:');
    expect(headerOf(/^L/)).toHaveTextContent('LN ref: L_0009.fits');
  });

  it('out_of_next_run_chip_counts_listed', () => {
    const excludedKeys = ['8', '9', '22'];
    renderTable({ ...SET, excludedKeys });
    expect(screen.getByRole('button', { name: `Out of next run ${excludedKeys.length}` })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Reg failed 4' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Included 6' })).toBeInTheDocument();
    // A frame set has no "Not stackable here" chip.
    expect(screen.queryByRole('button', { name: /^Not stackable here/ })).toBeNull();
  });

  it('a status chip filters the rows and tints them by outcome and exclusion', () => {
    renderTable({ ...SET, excludedKeys: ['8'] });
    expect(rowOf('R_0021.fits')).toHaveClass('bg-error/10');
    expect(rowOf('L_0008.fits')).toHaveClass('bg-warning/10');
    expect(within(rowOf('R_0021.fits')).getByText('0 quad matches')).toHaveAttribute('title', '0 quad matches');
    fireEvent.click(screen.getByRole('button', { name: 'Reg failed 4' }));
    expect(fileColumn()).toEqual(['R_0021', 'R_0022', 'R_0023', 'R_0024']);
    expect(headers()).toHaveLength(1);
  });

  it('tints an outlier cell and the Outliers toggle keeps only outlier rows', () => {
    renderTable(SET);
    expect(within(rowOf('L_0010.fits')).getByText('4.00 px')).toHaveClass('bg-warning-muted', 'text-warning');
    expect(within(rowOf('L_0009.fits')).getByText('2.10 px')).not.toHaveClass('bg-warning-muted');
    fireEvent.click(screen.getByRole('button', { name: 'Show outlier frames only' }));
    expect(fileColumn()).toEqual(['L_0010']);
  });

  it('the Columns toggle adds the registration columns', () => {
    renderTable(SET);
    expect(screen.queryByRole('columnheader', { name: 'Reg RMS' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Show the registration columns' }));
    for (const name of ['Reg RMS', 'Inliers', 'Model', 'Scale']) {
      expect(screen.getByRole('columnheader', { name })).toBeInTheDocument();
    }
  });

  it('collapsing a group hides its rows and drops them from the selection', () => {
    renderTable(SET);
    select('L_0007.fits');
    select('B_0031.fits');
    fireEvent.click(within(headerOf(/^L/)).getByRole('button'));
    expect(within(table()).queryByText('L_0007.fits')).toBeNull();
    expect(selectionCount()).toBe('1');
    fireEvent.click(within(headerOf(/^L/)).getByRole('button'));
    expect(within(rowOf('L_0007.fits')).getByRole('checkbox')).not.toBeChecked();
  });

  it('Night grouping shows each night with its counts', () => {
    const lights = SET_LIGHTS.map((f) => ({ ...f, night: L_IDS.includes(f.frameId) ? 'October 18, 2025' : 'October 19, 2025' }));
    renderTable({ ...SET, lightFrames: lights });
    fireEvent.click(within(toolbar()).getByRole('button', { name: 'Night' }));
    expect(headers().map((h) => h.textContent)).toEqual([
      expect.stringContaining('October 19, 2025'),
      expect.stringContaining('October 18, 2025'),
    ]);
    expect(headerOf(/^October 19/)).toHaveTextContent('6 frames · 2 included');
    expect(screen.queryByRole('button', { name: 'Publisher' })).toBeNull();
  });

  it('says so when the set has no light frames', () => {
    renderTable({ ...SET, lightFrames: [], runDetail: null });
    expect(screen.getByText('No light frames in this set.')).toBeInTheDocument();
  });
});

// ── The project ─────────────────────────────────────────────────────────────────────────────────────────────────

describe('FramesTable for a project', () => {
  it('adds the Publisher column and lists every plan frame', () => {
    renderTable();
    expect(screen.getByRole('columnheader', { name: 'Publisher' })).toBeInTheDocument();
    expect(screen.getByRole('columnheader', { name: 'Reference' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^Frames \(5\)/ })).toBeInTheDocument();
    expect(within(rowOf('a.fits')).getByText('Alice')).toBeInTheDocument();
    expect(within(rowOf('d.fits')).getByText('Cleo')).toBeInTheDocument();
    // Group by offers Publisher, never Night.
    expect(within(toolbar()).getByRole('button', { name: 'Publisher' })).toBeInTheDocument();
    expect(within(toolbar()).queryByRole('button', { name: 'Night' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Not stackable here 3' })).toBeInTheDocument();
  });

  it('sorts by the publisher', () => {
    renderTable();
    fireEvent.click(within(screen.getByRole('columnheader', { name: 'Publisher' })).getByRole('button'));
    expect(fileColumn()).toEqual(['a.fits', 'c.fits', 'b.fits', 'e.fits', 'd.fits']);
  });

  it('shift_click_selects_range', () => {
    renderTable();
    select('a.fits');
    select('d.fits', true);
    expect(selectionCount()).toBe('4');
    for (const name of ['a.fits', 'b.fits', 'c.fits', 'd.fits']) {
      expect(within(rowOf(name)).getByRole('checkbox')).toBeChecked();
    }
    expect(within(rowOf('e.fits')).getByRole('checkbox')).not.toBeChecked();
    // The clicked row decides the direction: Shift+click on a selected row clears the range from the anchor.
    select('b.fits', true);
    expect(selectionCount()).toBe('2');
    expect(within(rowOf('a.fits')).getByRole('checkbox')).not.toBeChecked();
    expect(within(rowOf('c.fits')).getByRole('checkbox')).toBeChecked();
  });

  it('selection_prunes_hidden_rows', () => {
    const { onBlink } = renderTable();
    for (const name of ['a.fits', 'b.fits', 'd.fits']) select(name);
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search' }), { target: { value: 'Alice' } });
    expect(selectionCount()).toBe('1');
    fireEvent.click(screen.getByRole('button', { name: /^Blink/ }));
    expect(blinked(onBlink)).toEqual([['u1']]);
    // The hidden rows left the selection: clearing the search does not bring them back.
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search' }), { target: { value: '' } });
    expect(selectionCount()).toBe('1');
  });

  it('Blink with nothing selected takes every visible row in table order', () => {
    const { onBlink } = renderTable();
    fireEvent.click(screen.getByRole('button', { name: 'Blink all 5 frames' }));
    expect(blinked(onBlink)).toEqual([['u1', 'u2', 'u3', 'u4', 'u5']]);
  });

  it('run_active_disables_writes_not_blink', () => {
    renderTable({ disabled: true, excludedKeys: ['u4'] });
    select('a.fits');
    select('d.fits');
    expect(screen.getByRole('button', { name: 'Exclude (1 of 2)' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Include (1 of 2)' })).toBeDisabled();
    expect(within(rowOf('a.fits')).getByRole('button', { name: /reference frame/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: /^Blink/ })).toBeEnabled();
  });

  it('non_editable_rows_not_eligible', () => {
    const runDetail = detailOf(
      [runRow({ frameId: 1, frameUuid: 'u1' }), runRow({ id: 2, frameId: 41, frameUuid: 'gone' })],
      [summaryFrame({ frameId: 41, filename: 'old.fits' })],
    );
    const { onSetExcluded } = renderTable({ runDetail, excludedKeys: ['gone'] });
    select('a.fits');
    select('old.fits');
    expect(within(rowOf('old.fits')).queryByRole('button')).toBeNull();
    // The orphan is listed, but no write reaches a frame the project no longer has.
    expect(screen.getByRole('button', { name: 'Include' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Exclude (1 of 2)' }));
    expect(onSetExcluded).toHaveBeenCalledWith(['u1'], true);
  });

  it('no_match_message', () => {
    renderTable();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search' }), { target: { value: 'zzz' } });
    expect(screen.getByText('No frames match these filters')).toBeInTheDocument();
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

  it('gives a frame with a WCS a plain star tooltip', () => {
    renderTable();
    expect(within(rowOf('a.fits')).getByRole('button', { name: /reference frame/ })).toHaveAttribute(
      'title',
      'Use as the reference frame',
    );
  });

  it('disables the star of an unusable frame and puts its reason in the tooltip', () => {
    const { onPin } = renderTable();
    for (const [file, reason] of [
      ['b.fits', 'not on this device'],
      ['c.fits', 'unsupported file: 8-bit JPEG'],
      ['e.fits', CANNOT_OPEN],
    ] as const) {
      const star = within(rowOf(file)).getByRole('button', { name: `Use ${file} as the reference frame` });
      expect(star).toBeDisabled();
      expect(star).toHaveAttribute('title', reason);
      fireEvent.click(star);
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

  it('renders a run row that left the plan from the run alone, with no star', () => {
    const runDetail = detailOf(
      [runRow({ frameId: 41, frameUuid: 'gone' }), runRow({ id: 2, frameId: 42, frameUuid: null })],
      [summaryFrame({ frameId: 41, filename: 'old.fits' })],
    );
    renderTable({ runDetail });
    expect(within(rowOf('old.fits')).queryByRole('button')).toBeNull();
    expect(within(rowOf('frame 42')).queryByRole('button')).toBeNull();
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
    );
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

  it('Publisher grouping orders the members by name', () => {
    renderTable();
    fireEvent.click(within(toolbar()).getByRole('button', { name: 'Publisher' }));
    expect(headers().map((h) => h.textContent?.split(' ')[0])).toEqual(['Alice', 'Bob', 'Cleo']);
    expect(headerOf(/^Alice/)).toHaveTextContent('2 frames · 0 included');
  });

  it('says so when a project has no frames', () => {
    renderTable({ projectFrames: [] });
    expect(screen.getByText('No frames in this project.')).toBeInTheDocument();
  });
});
