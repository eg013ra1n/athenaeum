import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { DebayerPanel } from './DebayerPanel';
import type { StackingConfig, StackingPlan } from '../../../types/stacking';

const configWith = (debayerOsc: boolean) => ({ calibration: { debayerOsc } }) as unknown as StackingConfig;

const planOf = (over: { projectId?: string | null; attested?: boolean }): StackingPlan =>
  ({ projectId: over.projectId ?? null, readiness: { attested: over.attested ?? false } }) as unknown as StackingPlan;

describe('DebayerPanel', () => {
  it('points at the Prepare panel for a project', () => {
    render(<DebayerPanel config={configWith(true)} plan={planOf({ projectId: 'p1' })} />);
    expect(screen.getByText('Debayering is on — set on the Prepare panel.')).toBeInTheDocument();
    expect(screen.queryByText(/Calibrate panel/)).toBeNull();
  });

  it('points at the Prepare panel for an attested set, debayering off', () => {
    render(<DebayerPanel config={configWith(false)} plan={planOf({ attested: true })} />);
    expect(
      screen.getByText(
        'Debayering is off — OSC lights stay Bayer-mosaiced. Turn it on from the Prepare panel.',
      ),
    ).toBeInTheDocument();
  });

  it('keeps the Calibrate panel wording for a frame set', () => {
    render(<DebayerPanel config={configWith(true)} plan={planOf({})} />);
    expect(screen.getByText('Debayering is on — set on the Calibrate panel.')).toBeInTheDocument();
    expect(screen.getByText(/inside calibration/)).toBeInTheDocument();
  });

  it('keeps the Calibrate panel wording with no plan', () => {
    render(<DebayerPanel config={configWith(false)} plan={null} />);
    expect(screen.getByText(/Turn it on from the Calibrate panel\./)).toBeInTheDocument();
  });
});
