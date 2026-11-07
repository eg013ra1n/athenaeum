// The reference strip above the Frames table (spec §6.5): row 1 names the frame the selected run aligned onto, line by
// line; row 2 the reference the next run will use.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { FramesReferenceStrip, type FramesReferenceStripProps } from './FramesReferenceStrip';
import type {
  PlanReference,
  RunSummary,
  StackingRunDetail,
  StackingRunFrameRow,
  StackingRunGroupRow,
} from '../../types/stacking';

const FILES: Record<number, string> = { 7: 'L_0037.fits', 9: 'L_0012.fits' };
const filenameOf = (id: number): string | null => FILES[id] ?? null;
const groupLabel = (key: string): string => `label(${key})`;

const row = (over: Partial<StackingRunFrameRow>): StackingRunFrameRow =>
  ({
    id: 1, runId: 8, groupId: 10, frameId: 7, included: true, exclusionReason: null, weight: 1, regStatus: 'ok',
    ...over,
  }) as unknown as StackingRunFrameRow;

const groups = [
  { id: 10, groupKey: 'mono__L__bin1__180s' },
  { id: 11, groupKey: 'mono__Ha__bin1__300s' },
] as unknown as StackingRunGroupRow[];

type SummaryOver = {
  reference?: Partial<RunSummary['reference']>;
  geometry?: 'coRegistered' | 'native';
};

const summaryOf = (over: SummaryOver = {}): RunSummary =>
  ({
    reference: { frameId: 7, filename: 'L_0037.fits', mode: 'auto', weight: 1, switchedFrom: null, ...over.reference },
    config: { registration: { geometry: over.geometry ?? 'coRegistered' } },
  }) as unknown as RunSummary;

const detailOf = (
  summary: RunSummary | null,
  over: { frames?: StackingRunFrameRow[]; configJson?: string; referenceMode?: string; status?: string } = {},
): StackingRunDetail =>
  ({
    run: {
      id: 8,
      status: over.status ?? 'running',
      referenceMode: over.referenceMode ?? 'auto',
      configJson: over.configJson ?? JSON.stringify({ registration: { geometry: 'coRegistered' } }),
    },
    groups,
    frames: over.frames ?? [row({ frameId: 7, groupId: 10 }), row({ id: 2, frameId: 9, groupId: 10 })],
    summary,
  }) as unknown as StackingRunDetail;

const AUTO_NEXT: PlanReference = { mode: 'auto', frameId: null, filename: null, onDisk: true };

const show = (over: Partial<FramesReferenceStripProps> = {}) =>
  render(
    <FramesReferenceStrip
      runDetail={over.runDetail === undefined ? detailOf(summaryOf()) : over.runDetail}
      planReference={over.planReference === undefined ? AUTO_NEXT : over.planReference}
      groupLabel={over.groupLabel ?? groupLabel}
      filenameOf={over.filenameOf ?? filenameOf}
    />,
  );

afterEach(() => vi.restoreAllMocks());

/** A row's label, then its value line by line, as shown. */
const lines = (testId: string): (string | null)[] => {
  const row = screen.getByTestId(testId);
  return [row.querySelector('dt')?.textContent ?? null, ...[...row.querySelectorAll('dd > div')].map((d) => d.textContent)];
};
const RUN_LABEL = 'Run #8 aligned onto';
const NEXT_LABEL = 'Next run aligns onto';
const NEXT_AUTO = [NEXT_LABEL, 'a frame it picks automatically when it starts'];
const PER_GROUP = [RUN_LABEL, 'each group’s own frame, named in its header'];

describe('FramesReferenceStrip', () => {
  it('strip_auto_with_weight', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { weight: 0.98765 } })) });
    expect(lines('reference-line-run')).toEqual([
      RUN_LABEL, 'L_0037.fits', 'This frame is in group label(mono__L__bin1__180s), weight 0.988, chosen automatically',
    ]);
    expect(lines('reference-line-next')).toEqual(NEXT_AUTO);
  });

  it('puts the label and the value in the two columns of one definition list', () => {
    show();
    const row = screen.getByTestId('reference-line-run');
    expect(row.parentElement?.tagName).toBe('DL');
    expect(row.className).toContain('contents');
    expect(row.querySelector('dt')?.getAttribute('title')).toContain('every frame of run #8 was aligned onto it');
  });

  it('strip_manual_mode_is_named', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { mode: 'manual' } })) });
    expect(lines('reference-line-run')[2]).toMatch(/, pinned by hand$/);
  });

  it('strip_two_pass_names_previous', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 9, filename: 'L_0012.fits', switchedFrom: 7 } })) });
    expect(lines('reference-line-run')).toEqual([
      RUN_LABEL,
      'L_0012.fits',
      'This frame is in group label(mono__L__bin1__180s), weight 1.000, chosen automatically',
      'The second pass moved it here from L_0037.fits, which sat further from the median framing',
    ]);
  });

  it('strip_two_pass_falls_back_to_the_frame_id', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { switchedFrom: 44 } })) });
    expect(lines('reference-line-run')[3]).toContain('moved it here from #44,');
  });

  it('strip_group_follows_the_reference_frame', () => {
    const frames = [row({ frameId: 7, groupId: 10 }), row({ id: 2, frameId: 9, groupId: 11 })];
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 9, filename: 'L_0012.fits' } }), { frames }) });
    expect(lines('reference-line-run')[2]).toContain('label(mono__Ha__bin1__300s)');
  });

  it('strip_group_left_out_when_unfindable', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 99, filename: 'gone.fits' } })) });
    expect(lines('reference-line-run')).toEqual([RUN_LABEL, 'gone.fits', 'This frame has weight 1.000, chosen automatically']);
  });

  it('names how a frame with no group and no weight was chosen', () => {
    const frames = [row({ id: 2, frameId: 7, groupId: 99, regStatus: 'reference' })];
    show({ runDetail: detailOf(null, { frames, referenceMode: 'manual' }) });
    expect(lines('reference-line-run')).toEqual([RUN_LABEL, 'L_0037.fits', 'This frame was pinned by hand']);
  });

  it('strip_native_per_group', () => {
    show({ runDetail: detailOf(summaryOf({ geometry: 'native' })) });
    expect(lines('reference-line-run')).toEqual(PER_GROUP);
  });

  it('strip_in_flight_uses_reference_row', () => {
    const frames = [row({ frameId: 3, groupId: 10, regStatus: 'ok' }), row({ id: 2, frameId: 7, groupId: 10, regStatus: 'reference' })];
    show({ runDetail: detailOf(null, { frames }) });
    expect(lines('reference-line-run')).toEqual([RUN_LABEL, 'L_0037.fits', 'This frame is in group label(mono__L__bin1__180s), chosen automatically']);
  });

  it('strip_in_flight_before_the_reference_is_picked', () => {
    show({ runDetail: detailOf(null, { frames: [row({ frameId: 3, regStatus: null })] }) });
    expect(lines('reference-line-run')).toEqual([RUN_LABEL, 'not picked yet', 'chosen automatically']);
  });

  it.each(['done', 'failed', 'cancelled'])('strip_ended_%s_run_without_a_reference_reads_not_recorded', (status) => {
    show({ runDetail: detailOf(null, { status, frames: [row({ frameId: 3, regStatus: null })] }) });
    expect(lines('reference-line-run')).toEqual([RUN_LABEL, 'not recorded', 'chosen automatically']);
  });

  it('strip_in_flight_native_per_group', () => {
    const configJson = JSON.stringify({ registration: { geometry: 'native' } });
    show({ runDetail: detailOf(null, { configJson, frames: [row({ frameId: 7, regStatus: 'reference' })] }) });
    expect(lines('reference-line-run')).toEqual(PER_GROUP);
  });

  it('strip_in_flight_unreadable_config_warns_and_reads_as_co_registered', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    show({ runDetail: detailOf(null, { configJson: '{not json', frames: [row({ frameId: 7, regStatus: 'reference' })] }) });
    expect(warn).toHaveBeenCalledWith('[FramesReferenceStrip] run config unreadable:', expect.any(Error));
    expect(lines('reference-line-run')[1]).toBe('L_0037.fits');
  });

  it('strip_no_run_leaves_line_one_out', () => {
    show({ runDetail: null });
    expect(screen.queryByTestId('reference-line-run')).toBeNull();
    expect(lines('reference-line-next')).toEqual(NEXT_AUTO);
  });

  it('strip_next_run_manual_on_disk', () => {
    show({ planReference: { mode: 'manual', frameId: 7, filename: 'L_0037.fits', onDisk: true } });
    expect(lines('reference-line-next')).toEqual([NEXT_LABEL, 'L_0037.fits · pinned by hand']);
    expect(screen.queryByTitle('The reference file is not on this device')).toBeNull();
  });

  it('strip_next_run_manual_off_disk', () => {
    show({ planReference: { mode: 'manual', frameId: 7, filename: 'L_0037.fits', onDisk: false } });
    expect(lines('reference-line-next')).toEqual([NEXT_LABEL, 'L_0037.fits · pinned by hand']);
    expect(screen.getByTitle('The reference file is not on this device')).toBeTruthy();
    expect(screen.getByRole('img', { name: 'The reference file is not on this device' })).toBeTruthy();
  });

  it('strip_next_run_manual_without_a_frame', () => {
    show({ planReference: { mode: 'manual', frameId: null, filename: null, onDisk: false } });
    expect(lines('reference-line-next')).toEqual([NEXT_LABEL, 'a frame pinned by hand, but none is chosen']);
    expect(screen.getByRole('img', { name: 'No reference frame is chosen' })).toBeTruthy();
    expect(screen.queryByRole('img', { name: 'The reference file is not on this device' })).toBeNull();
  });

  it('strip_next_run_without_a_plan_reads_auto', () => {
    show({ planReference: null });
    expect(lines('reference-line-next')).toEqual(NEXT_AUTO);
  });
});
