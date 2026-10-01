import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import PublishRunPanel from './PublishRunPanel';
import type { PublishRunState } from './useCollabPublishRun';
import type { CollabPublishProgress } from '../../../types/models';

afterEach(cleanup);

const prog = (o: Partial<CollabPublishProgress>): CollabPublishProgress => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', mode: null, stage: 'calibrating',
  current: 12, total: 48, currentFile: 'M42_Ha_0012.fits', startedAt: new Date(Date.now() - 134_000).toISOString(), ...o,
});
const state = (o: Partial<PublishRunState>): PublishRunState => ({
  running: null, last: null, reached: -1, cancel: vi.fn(async () => {}), cancelBusy: false, ...o,
});

describe('PublishRunPanel', () => {
  it('a running calibrate shows its title, why, steps, file, elapsed and Cancel', () => {
    const run = state({ running: prog({}), reached: 1 });
    render(<PublishRunPanel run={run} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('manual')).toBeInTheDocument();
    expect(screen.getByText('Calibrating 12 of 48')).toBeInTheDocument();
    expect(screen.getByText('you clicked Calibrate')).toBeInTheDocument();
    expect(screen.getByText('✓ Queued')).toBeInTheDocument();
    expect(screen.getByText('Calibrate 12 / 48')).toHaveAttribute('aria-current', 'step');
    expect(screen.getByText('M42_Ha_0012.fits')).toBeInTheDocument();
    expect(screen.getByText(/2m 1[34]s elapsed/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(run.cancel).toHaveBeenCalled();
  });

  it('an automatic run reads the mode and "started automatically"', () => {
    render(<PublishRunPanel run={state({ running: prog({ kind: 'auto', trigger: 'auto', mode: 'automatic', stage: 'seeding', current: 31 }), reached: 2 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('Fully automatic · seeding 31 of 48')).toBeInTheDocument();
    expect(screen.getByText('started automatically')).toBeInTheDocument();
    expect(screen.getByText('✓ Calibrate')).toBeInTheDocument();
    expect(screen.getByText('Seed 31 / 48')).toHaveAttribute('aria-current', 'step');
  });

  it('F3: a stage that re-enters an earlier step never moves a finished step back', () => {
    render(<PublishRunPanel run={state({ running: prog({ kind: 'auto', trigger: 'auto', mode: 'automatic', stage: 'calibrating', current: 1, total: 2 }), reached: 3 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('✓ Seed')).toBeInTheDocument();
    expect(screen.getByText('Announce 1 / 2')).toHaveAttribute('aria-current', 'step');
  });

  it('F9: a publish run regenerating an update lights no step', () => {
    const { container } = render(<PublishRunPanel run={state({ running: prog({ kind: 'publish', stage: 'calibrating', current: 1, total: 3 }), reached: 1 })} onOpenSegment={vi.fn()} />);
    expect(container.querySelector('[aria-current="step"]')).toBeNull();
    expect(screen.getByText('Calibrating 1 of 3')).toBeInTheDocument();
  });

  it('a queued run says it waits for the compute slot', () => {
    render(<PublishRunPanel run={state({ running: prog({ stage: 'queued', current: 0 }), reached: 0 })} onOpenSegment={vi.fn()} />);
    expect(screen.getByText('Waiting for a compute slot')).toBeInTheDocument();
    expect(screen.getByText('one compute slot · other runs wait')).toBeInTheDocument();
  });

  it('a finished run shows its line and links the segment it points at', () => {
    const onOpenSegment = vi.fn();
    render(<PublishRunPanel run={state({ last: { projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual',
      outcome: 'done', calibrated: 46, announced: 0, updated: 0, stale: 0, heldBack: 2, error: null,
      startedAt: '2026-10-01T14:00:00Z', finishedAt: '2026-10-01T14:03:22Z' } })} onOpenSegment={onOpenSegment} />);
    expect(screen.getByText('done')).toBeInTheDocument();
    expect(screen.getByText(/Calibrated 46 · 2 held back · .* · manual/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Review →' }));
    expect(onOpenSegment).toHaveBeenCalledWith('review');
  });

  it('renders nothing with no run and no last run', () => {
    const { container } = render(<PublishRunPanel run={state({})} onOpenSegment={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('Cancel is disabled while the cancel is in flight', () => {
    render(<PublishRunPanel run={state({ running: prog({}), reached: 1, cancelBusy: true })} onOpenSegment={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
  });
});
