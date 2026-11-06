// The two-line reference strip above the Frames table (spec §6.5): line 1 names the frame the selected run registered
// onto, line 2 the reference the next run will use.
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

describe('FramesReferenceStrip', () => {
  it('strip_auto_with_weight', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { weight: 0.98765 } })) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: L_0037.fits · label(mono__L__bin1__180s) · weight 0.988 · auto',
    );
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: auto, picked at run time');
  });

  it('strip_manual_mode_is_named', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { mode: 'manual' } })) });
    expect(screen.getByTestId('reference-line-run').textContent).toMatch(/ · manual$/);
  });

  it('strip_two_pass_names_previous', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 9, filename: 'L_0012.fits', switchedFrom: 7 } })) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: L_0012.fits · label(mono__L__bin1__180s) · weight 1.000 · auto' +
        ' · two-pass moved it from L_0037.fits',
    );
  });

  it('strip_two_pass_falls_back_to_the_frame_id', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { switchedFrom: 44 } })) });
    expect(screen.getByTestId('reference-line-run').textContent).toMatch(/ · two-pass moved it from #44$/);
  });

  it('strip_group_follows_the_reference_frame', () => {
    const frames = [row({ frameId: 7, groupId: 10 }), row({ id: 2, frameId: 9, groupId: 11 })];
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 9, filename: 'L_0012.fits' } }), { frames }) });
    expect(screen.getByTestId('reference-line-run').textContent).toContain('label(mono__Ha__bin1__300s)');
  });

  it('strip_group_left_out_when_unfindable', () => {
    show({ runDetail: detailOf(summaryOf({ reference: { frameId: 99, filename: 'gone.fits' } })) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: gone.fits · weight 1.000 · auto',
    );
  });

  it('strip_native_per_group', () => {
    show({ runDetail: detailOf(summaryOf({ geometry: 'native' })) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe('Registration reference, run #8: per group');
  });

  it('strip_in_flight_uses_reference_row', () => {
    const frames = [row({ frameId: 3, groupId: 10, regStatus: 'ok' }), row({ id: 2, frameId: 7, groupId: 10, regStatus: 'reference' })];
    show({ runDetail: detailOf(null, { frames }) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: L_0037.fits · label(mono__L__bin1__180s) · auto',
    );
  });

  it('strip_in_flight_before_the_reference_is_picked', () => {
    show({ runDetail: detailOf(null, { frames: [row({ frameId: 3, regStatus: null })] }) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: not picked yet · auto',
    );
  });

  it.each(['done', 'failed', 'cancelled'])('strip_ended_%s_run_without_a_reference_reads_not_recorded', (status) => {
    show({ runDetail: detailOf(null, { status, frames: [row({ frameId: 3, regStatus: null })] }) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe(
      'Registration reference, run #8: not recorded · auto',
    );
  });

  it('strip_in_flight_native_per_group', () => {
    const configJson = JSON.stringify({ registration: { geometry: 'native' } });
    show({ runDetail: detailOf(null, { configJson, frames: [row({ frameId: 7, regStatus: 'reference' })] }) });
    expect(screen.getByTestId('reference-line-run').textContent).toBe('Registration reference, run #8: per group');
  });

  it('strip_in_flight_unreadable_config_warns_and_reads_as_co_registered', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    show({ runDetail: detailOf(null, { configJson: '{not json', frames: [row({ frameId: 7, regStatus: 'reference' })] }) });
    expect(warn).toHaveBeenCalledWith('[FramesReferenceStrip] run config unreadable:', expect.any(Error));
    expect(screen.getByTestId('reference-line-run').textContent).toContain('L_0037.fits');
  });

  it('strip_no_run_leaves_line_one_out', () => {
    show({ runDetail: null });
    expect(screen.queryByTestId('reference-line-run')).toBeNull();
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: auto, picked at run time');
  });

  it('strip_next_run_manual_on_disk', () => {
    show({ planReference: { mode: 'manual', frameId: 7, filename: 'L_0037.fits', onDisk: true } });
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: L_0037.fits · manual');
    expect(screen.queryByTitle('The reference file is not on this device')).toBeNull();
  });

  it('strip_next_run_manual_off_disk', () => {
    show({ planReference: { mode: 'manual', frameId: 7, filename: 'L_0037.fits', onDisk: false } });
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: L_0037.fits · manual');
    expect(screen.getByTitle('The reference file is not on this device')).toBeTruthy();
    expect(screen.getByRole('img', { name: 'The reference file is not on this device' })).toBeTruthy();
  });

  it('strip_next_run_manual_without_a_frame', () => {
    show({ planReference: { mode: 'manual', frameId: null, filename: null, onDisk: false } });
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: manual, no frame chosen');
    expect(screen.getByRole('img', { name: 'No reference frame is chosen' })).toBeTruthy();
    expect(screen.queryByRole('img', { name: 'The reference file is not on this device' })).toBeNull();
  });

  it('strip_next_run_without_a_plan_reads_auto', () => {
    show({ planReference: null });
    expect(screen.getByTestId('reference-line-next').textContent).toBe('Next run: auto, picked at run time');
  });
});
