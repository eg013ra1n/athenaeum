// Wave 6c Task 8: the Groups table's `Exp` column shows the group's exposure class (the key's own token), and its
// header carries the exposure grouping control — the mode and that mode's threshold (D1: for a frame set and for a
// project alike).
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { GroupsTable } from './GroupsTable';
import type { ExposureMode, GroupingConfig, PlanGroup } from '../../types/stacking';

const group = (over: Partial<PlanGroup> = {}): PlanGroup =>
  ({
    key: 'mono__L__bin1__180s', instrume: null, colorMode: 'mono', filter: 'L', binning: 1, cameras: ['ASI2600MM'],
    exposureS: 180, exposureClass: '180s', frameCount: 10, includedCount: 9, totalExposureS: 1620,
    calibratedCached: 0, metricsCached: 0, pixelScaleArcsec: null, scaleSource: null, scaleRatioToReference: null,
    ...over,
  }) as unknown as PlanGroup;

const grouping = (over: Partial<GroupingConfig> = {}): GroupingConfig => ({
  exposureToleranceSec: 2, exposureMode: 'seconds', stopsWidth: 1, ...over,
});

const HINT_RECALIBRATE = "The tolerance is part of the calibration hash: changing it re-calibrates this set's lights on the next run.";
const HINT_EQUIVALENT =
  "Classes by t / N², N = FOCALLEN / APTDIA from each frame's header. Frames without both are grouped by seconds; the plan names how many.";

const show = (
  over: {
    groups?: PlanGroup[];
    grouping?: GroupingConfig;
    disabled?: boolean;
    subjectKind?: 'frameSet' | 'project';
  } = {},
) => {
  const onPatchGrouping = vi.fn();
  render(
    <GroupsTable
      groups={over.groups ?? [group()]}
      grouping={over.grouping ?? grouping()}
      disabled={over.disabled}
      subjectKind={over.subjectKind ?? 'frameSet'}
      onPatchGrouping={onPatchGrouping}
    />,
  );
  return onPatchGrouping;
};

/** The `Exp` cell of the row whose key cell reads `key` (the key is split around `__` into spans). */
const expOf = (rowIndex: number) => {
  const col = screen.getAllByRole('columnheader').findIndex((h) => h.textContent === 'Exp');
  const row = screen.getAllByRole('row')[rowIndex + 1] as HTMLElement;
  return within(row).getAllByRole('cell')[col]?.textContent;
};

describe('GroupsTable Exp column', () => {
  // A frame set's class token is today's token: the cluster's first exposure, two decimals trimmed, then `s`.
  it('reads a frame set’s seconds classes as it always did', () => {
    show({
      groups: [
        group({ key: 'mono__L__bin1__180s', exposureS: 180, exposureClass: '180s' }),
        group({ key: 'mono__L__bin1__0.39s', exposureS: 0.39, exposureClass: '0.39s' }),
        group({ key: 'mono__L__bin1__unknown', exposureS: null, exposureClass: 'unknown' }),
      ],
    });
    expect([expOf(0), expOf(1), expOf(2)]).toEqual(['180s', '0.39s', 'unknown']);
  });

  it('shows the exposure class of a stops and an equivalent group, not the seconds of its first frame', () => {
    show({
      groups: [
        group({ key: 'mono__L__bin1__st120', exposureS: 120, exposureClass: 'st120' }),
        group({ key: 'mono__L__bin1__eq0.24', exposureS: 60, exposureClass: 'eq0.24' }),
        group({ key: 'mono__L__bin1__all', exposureS: 30, exposureClass: 'all' }),
      ],
      grouping: grouping({ exposureMode: 'stops' }),
    });
    expect([expOf(0), expOf(1), expOf(2)]).toEqual(['st120', 'eq0.24', 'all']);
  });
});

describe('GroupsTable exposure grouping control', () => {
  const control = () => screen.getByRole('group', { name: 'Exposure grouping' });
  const mode = (name: string) => within(control()).getByRole('button', { name });

  it('offers the four modes and marks the current one', () => {
    show({ grouping: grouping({ exposureMode: 'equivalent' }) });
    const names = within(control()).getAllByRole('button').map((b) => b.textContent);
    expect(names).toEqual(['Seconds', 'Stops', 'Equivalent t/N²', 'None']);
    expect(mode('Equivalent t/N²')).toHaveAttribute('aria-pressed', 'true');
    expect(mode('Seconds')).toHaveAttribute('aria-pressed', 'false');
  });

  it.each<[string, ExposureMode]>([
    ['Seconds', 'seconds'], ['Stops', 'stops'], ['Equivalent t/N²', 'equivalent'], ['None', 'none'],
  ])('patches the mode %s', (label, wire) => {
    const current: ExposureMode = wire === 'seconds' ? 'stops' : 'seconds';
    const onPatch = show({ grouping: grouping({ exposureMode: current }) });
    fireEvent.click(mode(label));
    expect(onPatch).toHaveBeenCalledTimes(1);
    expect(onPatch).toHaveBeenCalledWith({ exposureMode: wire });
  });

  it('writes nothing when the current mode is chosen again', () => {
    const onPatch = show({ grouping: grouping({ exposureMode: 'stops' }) });
    fireEvent.click(mode('Stops'));
    expect(onPatch).not.toHaveBeenCalled();
  });

  it('shows Tolerance for seconds, Width for stops and equivalent, and no field for none', () => {
    const fields = (m: ExposureMode) => {
      const { unmount } = render(
        <GroupsTable
          groups={[group()]}
          grouping={grouping({ exposureMode: m })}
          subjectKind="project"
          onPatchGrouping={vi.fn()}
        />,
      );
      const found = [screen.queryByLabelText('Tolerance (s)') !== null, screen.queryByLabelText('Width (stops)') !== null];
      unmount();
      return found;
    };
    expect(fields('seconds')).toEqual([true, false]);
    expect(fields('stops')).toEqual([false, true]);
    expect(fields('equivalent')).toEqual([false, true]);
    expect(fields('none')).toEqual([false, false]);
  });

  it('shows the stored thresholds', () => {
    show({ grouping: grouping({ exposureMode: 'seconds', exposureToleranceSec: 3.5, stopsWidth: 0.7 }) });
    expect((screen.getByLabelText('Tolerance (s)') as HTMLInputElement).value).toBe('3.5');
  });

  it('patches the tolerance', () => {
    const onPatch = show();
    fireEvent.change(screen.getByLabelText('Tolerance (s)'), { target: { value: '5' } });
    expect(onPatch).toHaveBeenCalledWith({ exposureToleranceSec: 5 });
  });

  it.each<ExposureMode>(['stops', 'equivalent'])('patches the width in %s mode', (m) => {
    const onPatch = show({ grouping: grouping({ exposureMode: m }) });
    fireEvent.change(screen.getByLabelText('Width (stops)'), { target: { value: '0.5' } });
    expect(onPatch).toHaveBeenCalledWith({ stopsWidth: 0.5 });
  });

  // `NumericField` holds a value to its `min`: a tolerance of 0 or a width of 0.05 reaches the config as the floor.
  it('never lets a tolerance of 0 through: the floor is 0.01', () => {
    const onPatch = show();
    expect(screen.getByLabelText('Tolerance (s)')).toHaveAttribute('min', '0.01');
    expect(screen.getByLabelText('Tolerance (s)')).toHaveAttribute('step', '0.5');
    fireEvent.change(screen.getByLabelText('Tolerance (s)'), { target: { value: '0' } });
    expect(onPatch).toHaveBeenCalledWith({ exposureToleranceSec: 0.01 });
    expect(onPatch).not.toHaveBeenCalledWith({ exposureToleranceSec: 0 });
  });

  it('never lets a width of 0.05 through: the floor is 0.1', () => {
    const onPatch = show({ grouping: grouping({ exposureMode: 'stops' }) });
    expect(screen.getByLabelText('Width (stops)')).toHaveAttribute('min', '0.1');
    expect(screen.getByLabelText('Width (stops)')).toHaveAttribute('step', '0.1');
    fireEvent.change(screen.getByLabelText('Width (stops)'), { target: { value: '0.05' } });
    expect(onPatch).toHaveBeenCalledWith({ stopsWidth: 0.1 });
    expect(onPatch).not.toHaveBeenCalledWith({ stopsWidth: 0.05 });
  });

  it('disables the modes and the field while a run is active', () => {
    show({ disabled: true });
    for (const b of within(control()).getAllByRole('button')) expect(b).toBeDisabled();
    expect(screen.getByLabelText('Tolerance (s)')).toBeDisabled();
  });

  it('is there with no groups yet, next to the empty line', () => {
    show({ groups: [] });
    expect(screen.getByText('No groups yet.')).toBeInTheDocument();
    expect(control()).toBeInTheDocument();
  });

  describe('hints', () => {
    it('says a frame set’s tolerance is part of the calibration hash, in seconds mode', () => {
      show({ subjectKind: 'frameSet' });
      expect(screen.getByText(HINT_RECALIBRATE)).toBeInTheDocument();
    });

    it('never says it for a project, whatever the mode', () => {
      for (const m of ['seconds', 'stops', 'equivalent', 'none'] as const) {
        const { unmount } = render(
          <GroupsTable
            groups={[group()]}
            grouping={grouping({ exposureMode: m })}
            subjectKind="project"
            onPatchGrouping={vi.fn()}
          />,
        );
        expect(screen.queryByText(HINT_RECALIBRATE), m).toBeNull();
        unmount();
      }
    });

    it('does not say it for a frame set outside seconds mode, where the tolerance is not in use', () => {
      for (const m of ['stops', 'equivalent', 'none'] as const) {
        const { unmount } = render(
          <GroupsTable
            groups={[group()]}
            grouping={grouping({ exposureMode: m })}
            subjectKind="frameSet"
            onPatchGrouping={vi.fn()}
          />,
        );
        expect(screen.queryByText(HINT_RECALIBRATE), m).toBeNull();
        unmount();
      }
    });

    it('explains t / N² in equivalent mode only', () => {
      for (const m of ['seconds', 'stops', 'none'] as const) {
        const { unmount } = render(
          <GroupsTable
            groups={[group()]}
            grouping={grouping({ exposureMode: m })}
            subjectKind="project"
            onPatchGrouping={vi.fn()}
          />,
        );
        expect(screen.queryByText(HINT_EQUIVALENT), m).toBeNull();
        unmount();
      }
      show({ grouping: grouping({ exposureMode: 'equivalent' }), subjectKind: 'project' });
      expect(screen.getByText(HINT_EQUIVALENT)).toBeInTheDocument();
    });
  });
});
