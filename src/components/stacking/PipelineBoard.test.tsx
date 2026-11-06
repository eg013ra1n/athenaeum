import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { PipelineBoard } from './PipelineBoard';
import type { PlanPrepare, StackingConfig, StackingPlan } from '../../types/stacking';

// A complete config: the board summarises every row, and each row reads its own part of it.
const CONFIG = {
  calibration: { flatNorm: true, flatNormMode: 'centralThird', hotPixelCorrection: true, debayerOsc: true },
  measurement: {
    seedSource: 'matchedFilter', seedThreshold: 5, detectionSigma: 5, seedPrefilter: 'none', weightMode: 'psfSignalWeight',
    psfModel: 'auto', maxStars: 500,
  },
  reference: { mode: 'auto', twoPass: false },
  registration: {
    geometry: 'coRegistered', model: 'auto', distortion: 'off', tpsSmoothing: 0, localDistortion: false,
    interpolation: 'lanczos3', clampingThreshold: 0.3, maxStars: 500,
  },
  normalization: {
    output: 'additiveWithScaling', rejection: 'scaleZeroOffset', scaleEstimator: 'mad',
    local: { enabled: false },
  },
  integration: {
    combination: 'average', rejection: { method: 'auto' }, minWeight: 0.005,
    largeScale: { enabled: false, protectedLayers: 3, growth: 1 },
  },
  drizzle: { enabled: false },
  output: { format: 'fits', cleanup: 'keepAll' },
} as unknown as StackingConfig;

const PREPARE: PlanPrepare = { inPlace: 3, debayered: 0, converted: 2, unsupported: 1, atRun: 0 };

const planOf = (
  over: { projectId?: string | null; attested?: boolean; prepare?: PlanPrepare | null; blockers?: { code: string; message: string }[] },
): StackingPlan =>
  ({
    setId: over.projectId ? null : 7,
    projectId: over.projectId ?? null,
    readiness: { attested: over.attested ?? false },
    mastersToBuild: [],
    blockers: over.blockers ?? [],
    staleStages: [],
    groups: [],
    prepare: over.prepare ?? null,
  }) as unknown as StackingPlan;

const show = (plan: StackingPlan) =>
  render(
    <PipelineBoard
      plan={plan}
      config={CONFIG}
      progress={undefined}
      outcome={undefined}
      selectedStage="calibrate"
      onSelectStage={vi.fn()}
      onToggleDrizzle={vi.fn()}
    />,
  );

/** The board row of a stage: its name, its summary and its state. */
function row(stage: string) {
  const el = document.querySelector(`[data-stage="${stage}"]`) as HTMLElement;
  return { text: el.textContent ?? '', el };
}

describe('PipelineBoard', () => {
  it('shows Prepare for a project, with what the run does with the frames', () => {
    show(planOf({ projectId: 'p1', prepare: PREPARE }));
    const r = row('calibrate').text;
    expect(r).toContain('Prepare');
    expect(r).toContain('3 in place · 2 converted');
    expect(r).not.toContain('Calibrate');
    expect(r).not.toContain('flat-norm');
  });

  it('shows Prepare for an attested set', () => {
    show(planOf({ attested: true, prepare: PREPARE }));
    expect(row('calibrate').text).toContain('Prepare');
  });

  it('keeps Calibrate and the calibration summary for a frame set', () => {
    show(planOf({}));
    const r = row('calibrate').text;
    expect(r).toContain('Calibrate');
    expect(r).toContain('flat-norm central third · hot-pixel correction');
  });

  it('shows the Masters row off and not needed for a project, even with a masters blocker', () => {
    show(planOf({ projectId: 'p1', prepare: PREPARE, blockers: [{ code: 'masters', message: 'a master is missing' }] }));
    const r = row('masters').text;
    expect(r).toContain('Not needed — project frames are calibrated by their publishers');
    expect(r).toContain('Off');
    expect(r).not.toContain('Blocked');
  });

  it('shows the Masters row off and not needed for an attested set', () => {
    show(planOf({ attested: true, prepare: PREPARE }));
    const r = row('masters').text;
    expect(r).toContain('Not needed — the lights are calibrated externally');
    expect(r).toContain('Off');
  });

  it('shows the Masters row blocked for a frame set with the same blocker', () => {
    show(planOf({ blockers: [{ code: 'masters', message: 'a master is missing' }] }));
    expect(row('masters').text).toContain('Blocked');
  });

  it('gives the whole summary as the row title, for a line the board truncates', () => {
    show(planOf({ projectId: 'p1', prepare: PREPARE }));
    expect(screen.getByTitle('Not needed — project frames are calibrated by their publishers')).toBeInTheDocument();
  });
});
