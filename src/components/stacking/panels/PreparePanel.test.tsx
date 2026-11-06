import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { PreparePanel } from './PreparePanel';
import type { PlanPrepare, StackingConfig, StackingPlan } from '../../../types/stacking';

const CONFIG = {
  calibration: { flatNorm: true, flatNormMode: 'centralThird', hotPixelCorrection: true, debayerOsc: true },
} as unknown as StackingConfig;

const COUNTS: PlanPrepare = { inPlace: 3, debayered: 1, converted: 2, unsupported: 4, atRun: 5 };

const planWith = (prepare: PlanPrepare | null): StackingPlan =>
  ({ setId: null, projectId: 'p1', readiness: { attested: false }, prepare }) as unknown as StackingPlan;

const show = (over: { config?: StackingConfig; plan?: StackingPlan | null; disabled?: boolean } = {}) => {
  const onPatchCalibration = vi.fn();
  render(
    <PreparePanel
      config={over.config ?? CONFIG}
      plan={over.plan === undefined ? planWith(COUNTS) : over.plan}
      disabled={over.disabled}
      onPatchCalibration={onPatchCalibration}
    />,
  );
  return onPatchCalibration;
};

/** The value beside a term of the counts list. */
const value = (term: string) => screen.getByText(term).closest('dt')?.nextElementSibling?.textContent;

describe('PreparePanel', () => {
  it('lists the five counts', () => {
    show();
    expect(value('In place')).toBe('3');
    expect(value('Debayered')).toBe('1');
    expect(value('Converted to float FITS')).toBe('2');
    expect(value('Unsupported — left out of the run')).toBe('4');
    expect(value('Checked when the run starts')).toBe('5');
  });

  it('shows a zero count as 0', () => {
    show({ plan: planWith({ inPlace: 0, debayered: 0, converted: 0, unsupported: 0, atRun: 0 }) });
    expect(value('In place')).toBe('0');
    expect(value('Unsupported — left out of the run')).toBe('0');
  });

  it('warns about frames left out of the run, and only then', () => {
    show();
    expect(screen.getByText('4')).toHaveClass('text-warning');
    expect(screen.getByText('3')).not.toHaveClass('text-warning');
  });

  it('turns the debayer option off through onPatchCalibration', () => {
    const onPatchCalibration = show();
    const box = screen.getByRole('checkbox', { name: 'Debayer OSC lights (VNG)' });
    expect(box).toBeChecked();
    fireEvent.click(box);
    expect(onPatchCalibration).toHaveBeenCalledTimes(1);
    expect(onPatchCalibration).toHaveBeenCalledWith({ debayerOsc: false });
  });

  it('turns the debayer option on when it is off', () => {
    const onPatchCalibration = show({
      config: { calibration: { ...CONFIG.calibration, debayerOsc: false } } as StackingConfig,
    });
    fireEvent.click(screen.getByRole('checkbox', { name: 'Debayer OSC lights (VNG)' }));
    expect(onPatchCalibration).toHaveBeenCalledWith({ debayerOsc: true });
  });

  it('locks the option while a run is active', () => {
    show({ disabled: true });
    expect(screen.getByRole('checkbox', { name: 'Debayer OSC lights (VNG)' })).toBeDisabled();
  });

  it('renders no other calibration control and says why', () => {
    show();
    expect(screen.getAllByRole('checkbox')).toHaveLength(1);
    expect(screen.queryAllByRole('radio')).toHaveLength(0);
    expect(screen.queryByText(/Normalize master flat/)).toBeNull();
    expect(screen.queryByText(/Hot-pixel correction/)).toBeNull();
    expect(
      screen.getByText('Other calibration settings do not apply: these frames are already calibrated.'),
    ).toBeInTheDocument();
  });

  it('says so when there is no plan or no counts yet, and keeps the option', () => {
    show({ plan: null });
    expect(screen.getByText('No plan loaded yet.')).toBeInTheDocument();
    expect(screen.queryByText('In place')).toBeNull();
    expect(screen.getByRole('checkbox', { name: 'Debayer OSC lights (VNG)' })).toBeInTheDocument();
  });
});
