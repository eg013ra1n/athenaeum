import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { ReferencePanel } from './ReferencePanel';
import type { ReferenceMode, StackingConfig, StackingPlan } from '../../../types/stacking';

const configWith = (mode: ReferenceMode) =>
  ({ reference: { mode, twoPass: true } }) as unknown as StackingConfig;

const planOf = (over: { projectId?: string | null; attested?: boolean }): StackingPlan =>
  ({
    projectId: over.projectId ?? null,
    readiness: { attested: over.attested ?? false },
    reference: { mode: 'manual', frameId: null, filename: 'ref.fits', onDisk: true },
  }) as unknown as StackingPlan;

const show = (mode: ReferenceMode, plan: StackingPlan | null) =>
  render(
    <MemoryRouter>
      <ReferencePanel config={configWith(mode)} onChange={vi.fn()} plan={plan} />
    </MemoryRouter>,
  );

const PIN_LINE = 'Pin a frame in the Frames table below.';

describe('ReferencePanel manual mode', () => {
  it('sends a project to its Frames table, not to Analysis', () => {
    show('manual', planOf({ projectId: 'p1' }));
    expect(screen.getByText(PIN_LINE)).toBeInTheDocument();
    expect(screen.queryByText(/Only frames with a WCS/)).toBeNull();
    expect(screen.queryByText('Choose in Analysis')).toBeNull();
    expect(screen.getByText('ref.fits')).toBeInTheDocument();
  });

  it('keeps Choose in Analysis for a frame set', () => {
    show('manual', planOf({}));
    expect(screen.getByRole('button', { name: 'Choose in Analysis' })).toBeInTheDocument();
    expect(screen.queryByText(PIN_LINE)).toBeNull();
  });

  it('keeps Choose in Analysis for an attested set', () => {
    show('manual', planOf({ attested: true }));
    expect(screen.getByRole('button', { name: 'Choose in Analysis' })).toBeInTheDocument();
  });

  it('says nothing about pinning in Auto mode', () => {
    show('auto', planOf({ projectId: 'p1' }));
    expect(screen.queryByText(PIN_LINE)).toBeNull();
    expect(screen.queryByText('Choose in Analysis')).toBeNull();
  });
});
