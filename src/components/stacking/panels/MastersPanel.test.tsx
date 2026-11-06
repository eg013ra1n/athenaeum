import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MastersPanel } from './MastersPanel';
import type { StackingPlan } from '../../../types/stacking';

const PROJECT_COPY = 'Not needed — project frames are calibrated by their publishers';
const ATTESTED_COPY = 'Not needed — the lights are calibrated externally';

const planOf = (over: { projectId?: string | null; attested?: boolean; mastersToBuild?: unknown[] }): StackingPlan =>
  ({
    setId: over.projectId ? null : 7,
    projectId: over.projectId ?? null,
    readiness: { attested: over.attested ?? false },
    mastersToBuild: over.mastersToBuild ?? [],
  }) as unknown as StackingPlan;

describe('MastersPanel', () => {
  it('says a project needs no masters, and lists no work', () => {
    render(<MastersPanel plan={planOf({ projectId: 'p1', mastersToBuild: [{ kind: 'build', setId: 1, label: 'Dark', frameCount: 9 }] })} />);
    expect(screen.getByText(PROJECT_COPY)).toBeInTheDocument();
    expect(screen.queryByText(/Nothing to build/)).toBeNull();
    expect(screen.queryByText('Dark')).toBeNull();
  });

  it('says an attested set needs no masters', () => {
    render(<MastersPanel plan={planOf({ attested: true })} />);
    expect(screen.getByText(ATTESTED_COPY)).toBeInTheDocument();
    expect(screen.queryByText(PROJECT_COPY)).toBeNull();
  });

  it('keeps the work list for a frame set', () => {
    render(<MastersPanel plan={planOf({ mastersToBuild: [{ kind: 'rebuild', setId: 2, label: 'Flat L', frameCount: 20 }] })} />);
    expect(screen.queryByText(/Not needed/)).toBeNull();
    expect(screen.getByText('Flat L')).toBeInTheDocument();
    expect(screen.getByText('rebuild · 20 frames')).toBeInTheDocument();
  });

  it('keeps the nothing-to-build line for a frame set with no work', () => {
    render(<MastersPanel plan={planOf({})} />);
    expect(screen.getByText(/Nothing to build/)).toBeInTheDocument();
  });
});
