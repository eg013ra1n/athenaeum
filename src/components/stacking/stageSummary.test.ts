import { describe, expect, it } from 'vitest';
import { boardMode, rowState, stageLabel, stageSummary } from './stageSummary';
import type { PlanPrepare, Stage, StackingConfig, StackingPlan } from '../../types/stacking';
import type { RunOutcome, RunProgress } from '../../hooks/useStackingRuns';

const withLocal = (local: Partial<StackingConfig['normalization']['local']>) =>
  ({ normalization: { output: 'additiveWithScaling', rejection: 'scaleZeroOffset', scaleEstimator: 'mad',
    local: { enabled: true, scale: 1024, scaleMode: 'auto', gridSize: 4, referenceFrames: 20, psfModel: 'auto', localScale: false, fluxEstimator: 'psfSignal', aperture: 'fwhm3', starSelection: { minPeakSnr: 40, maxStars: 500, saturation: 0.75 }, ...local } } } as unknown as StackingConfig);

describe('stageSummary normalize', () => {
  it('auto', () => expect(stageSummary('normalize', withLocal({}))).toBe('Local · scale auto (÷4) · ref 20'));
  it('fixed', () => expect(stageSummary('normalize', withLocal({ scaleMode: 'fixed', scale: 2048 }))).toBe('Local · scale 2048 · ref 20'));
  it('local scale', () => expect(stageSummary('normalize', withLocal({ localScale: true }))).toBe('Local · scale auto (÷4) · ref 20 · local scale'));
  it('common aperture', () =>
    expect(stageSummary('normalize', withLocal({ fluxEstimator: 'commonAperture', aperture: 'fwhm3' }))).toBe(
      'Local · scale auto (÷4) · ref 20 · common aperture 3×FWHM',
    ));
  it('growth curve', () =>
    expect(stageSummary('normalize', withLocal({ fluxEstimator: 'commonAperture', aperture: 'growthCurve' }))).toBe(
      'Local · scale auto (÷4) · ref 20 · common aperture 2×FWHM + growth',
    ));
});

describe('stageSummary integrate LN rejection', () => {
  const integrate = (rejection: StackingConfig['normalization']['rejection'], lnEnabled: boolean) =>
    ({
      normalization: { ...withLocal({ enabled: lnEnabled }).normalization, rejection },
      integration: {
        combination: 'average',
        rejection: { method: 'sigmaClip', sigmaLow: 3, sigmaHigh: 3 },
        minWeight: 0.005,
        largeScale: { enabled: false, protectedLayers: 3, growth: 1 },
      },
    }) as unknown as StackingConfig;

  it('says LN rejection for local rejection with LN output on', () =>
    expect(stageSummary('integrate', integrate('local', true))).toBe(
      'Average · sigma clip 3/3 · min weight 0.005 · LN rejection',
    ));
  it('says LN rejection for local rejection with LN output off', () =>
    expect(stageSummary('integrate', integrate('local', false))).toBe(
      'Average · sigma clip 3/3 · min weight 0.005 · LN rejection',
    ));
  it('says nothing for a non-local rejection normalization', () =>
    expect(stageSummary('integrate', integrate('scaleZeroOffset', true))).toBe(
      'Average · sigma clip 3/3 · min weight 0.005',
    ));
});

// Wave 6c Task 5: a project and an attested set show Prepare and need no masters.
const PREPARE_COUNTS: PlanPrepare = { inPlace: 3, debayered: 0, converted: 2, unsupported: 1, atRun: 0 };

const planOf = (
  over: { projectId?: string | null; attested?: boolean; prepare?: PlanPrepare | null; blockers?: { code: string; message: string }[]; mastersToBuild?: unknown[] } = {},
): StackingPlan =>
  ({
    setId: over.projectId ? null : 7,
    projectId: over.projectId ?? null,
    readiness: { attested: over.attested ?? false },
    mastersToBuild: over.mastersToBuild ?? [],
    blockers: over.blockers ?? [],
    staleStages: [],
    groups: [],
    prepare: over.prepare === undefined ? null : over.prepare,
  }) as unknown as StackingPlan;

const frameSetPlan = planOf();
const attestedPlan = planOf({ attested: true, prepare: PREPARE_COUNTS });
const projectPlan = planOf({ projectId: 'p1', prepare: PREPARE_COUNTS });

const CONFIG = { calibration: { flatNorm: true, flatNormMode: 'centralThird', hotPixelCorrection: true, debayerOsc: true } } as unknown as StackingConfig;

describe('boardMode', () => {
  it('is calibrate for a frame set, with or without a plan', () => {
    expect(boardMode(frameSetPlan)).toBe('calibrate');
    expect(boardMode(null)).toBe('calibrate');
  });
  it('is prepare for an attested frame set', () => expect(boardMode(attestedPlan)).toBe('prepare'));
  it('is prepare for a project', () => expect(boardMode(projectPlan)).toBe('prepare'));
});

describe('stageLabel', () => {
  it('names row 1 Prepare in prepare mode and Calibrate otherwise', () => {
    expect(stageLabel('calibrate', 'prepare')).toBe('Prepare');
    expect(stageLabel('calibrate', 'calibrate')).toBe('Calibrate');
  });
  it('leaves every other row alone in both modes', () => {
    expect(stageLabel('masters', 'prepare')).toBe('Masters');
    expect(stageLabel('measure', 'prepare')).toBe('Measure & select');
    expect(stageLabel('normalize', 'calibrate')).toBe('Local normalization');
  });
});

describe('stageSummary prepare', () => {
  it('lists what the run does with the frames and leaves zero terms out', () =>
    expect(stageSummary('calibrate', CONFIG, projectPlan)).toBe('3 in place · 2 converted'));
  it('says the same for an attested set', () =>
    expect(stageSummary('calibrate', CONFIG, attestedPlan)).toBe('3 in place · 2 converted'));
  it('adds the frames checked at run time', () =>
    expect(
      stageSummary('calibrate', CONFIG, planOf({ projectId: 'p1', prepare: { ...PREPARE_COUNTS, debayered: 4, atRun: 5 } })),
    ).toBe('3 in place · 4 debayered · 2 converted + 5 checked at run'));
  it('names only the run-time check when nothing else is known', () =>
    expect(
      stageSummary('calibrate', CONFIG, planOf({ projectId: 'p1', prepare: { inPlace: 0, debayered: 0, converted: 0, unsupported: 0, atRun: 6 } })),
    ).toBe('6 checked at run'));
  it('says there is nothing to prepare when every count is zero', () =>
    expect(
      stageSummary('calibrate', CONFIG, planOf({ projectId: 'p1', prepare: { inPlace: 0, debayered: 0, converted: 0, unsupported: 2, atRun: 0 } })),
    ).toBe('Nothing to prepare'));
  it('keeps the calibration summary for a frame set', () =>
    expect(stageSummary('calibrate', CONFIG, frameSetPlan)).toBe('flat-norm central third · hot-pixel correction'));
});

describe('stageSummary masters when none are needed', () => {
  it('says project frames are calibrated by their publishers', () =>
    expect(stageSummary('masters', CONFIG, projectPlan)).toBe('Not needed \u2014 project frames are calibrated by their publishers'));
  it('says an attested set is calibrated externally', () =>
    expect(stageSummary('masters', CONFIG, attestedPlan)).toBe('Not needed \u2014 the lights are calibrated externally'));
  it('keeps the work list for a frame set', () =>
    expect(stageSummary('masters', CONFIG, frameSetPlan)).toBe('Nothing to build'));
});

describe('rowState masters for a project and an attested set', () => {
  const progress = (stage: Stage): RunProgress =>
    ({ runId: 1, subjectKey: 'project:p1', stage, groupKey: null, current: 1, total: 2, percent: 50, bytesDone: 0, bytesTotal: 0, frameId: null, message: null, startedAt: 0 });
  const outcome = (over: Partial<RunOutcome>): RunOutcome =>
    ({ runId: 1, subjectKey: 'project:p1', success: true, cancelled: false, error: null, warnings: [], masters: [], finishedAt: 0, ...over });
  const blocked = [{ code: 'masters', message: 'a master is missing' }, { code: 'masterFiles', message: 'a master file is gone' }];
  const states: [string, RunProgress | undefined, RunOutcome | undefined, readonly Stage[] | null][] = [
    ['idle', undefined, undefined, null],
    ['running in Calibrate', progress('calibrate'), undefined, null],
    ['running in Integrate', progress('integrate'), undefined, null],
    ['done', undefined, outcome({}), ['calibrate', 'measure', 'integrate', 'output']],
    ['failed', undefined, outcome({ success: false, error: 'boom' }), []],
    ['cancelled', undefined, outcome({ success: false, cancelled: true }), ['calibrate']],
    ['done, run detail not loaded', undefined, outcome({}), null],
    ['failed, run detail not loaded', undefined, outcome({ success: false, error: 'boom' }), null],
  ];

  for (const [name, p, o, finished] of states) {
    it(`is off for a project, ${name}`, () => {
      expect(rowState('masters', projectPlan, p, o, CONFIG, finished)).toBe('off');
      expect(rowState('masters', planOf({ projectId: 'p1', blockers: blocked }), p, o, CONFIG, finished)).toBe('off');
    });
    it(`is off for an attested set, ${name}`, () => {
      expect(rowState('masters', attestedPlan, p, o, CONFIG, finished)).toBe('off');
      expect(rowState('masters', planOf({ attested: true, blockers: blocked, mastersToBuild: [{}] }), p, o, CONFIG, finished)).toBe('off');
    });
  }

  it('is still blocked for a frame set with a masters blocker', () =>
    expect(rowState('masters', planOf({ blockers: blocked }), undefined, undefined, CONFIG)).toBe('blocked'));
});

describe('rowState register for a project whose grid is not confirmed', () => {
  const gridBlocker = { code: 'grid', message: 'Confirm the project\'s grid in the Stacking tab' };

  it('is blocked while the plan carries the grid blocker', () => {
    expect(rowState('register', planOf({ projectId: 'p1', blockers: [gridBlocker] }), undefined, undefined, CONFIG)).toBe('blocked');
  });

  it('is ready once the blocker is gone', () => {
    expect(rowState('register', planOf({ projectId: 'p1' }), undefined, undefined, CONFIG)).toBe('ready');
  });

  it('puts the grid blocker on no other row', () => {
    const plan = planOf({ projectId: 'p1', blockers: [gridBlocker] });
    for (const stage of ['calibrate', 'measure', 'reference', 'normalize', 'integrate', 'output'] as const) {
      expect(rowState(stage, plan, undefined, undefined, CONFIG), stage).toBe('ready');
    }
  });

  it('lets a live run outrank the blocker', () => {
    const progress = { runId: 1, subjectKey: 'project:p1', stage: 'register', groupKey: null, current: 1, total: 2, percent: 50, bytesDone: 0, bytesTotal: 0, frameId: null, message: null, startedAt: 0 } as unknown as RunProgress;
    expect(rowState('register', planOf({ projectId: 'p1', blockers: [gridBlocker] }), progress, undefined, CONFIG)).toBe('running');
  });
});

// Wave 6c Task 8: the Output row names the coverage map and the crop when they are on.
describe('stageSummary output coverage', () => {
  const out = (o: Partial<StackingConfig['output']> = {}) =>
    ({ output: { format: 'fits', cleanup: 'keepAll', coverageMap: false, cropToCoverage: 0, ...o } }) as unknown as StackingConfig;

  it('says only the format and the cleanup with both off', () =>
    expect(stageSummary('output', out())).toBe('FITS · keep all'));
  it('mentions the coverage map when it is on', () =>
    expect(stageSummary('output', out({ coverageMap: true }))).toBe('FITS · keep all · coverage map'));
  it('mentions the crop when it is above 0', () =>
    expect(stageSummary('output', out({ cropToCoverage: 3 }))).toBe('FITS · keep all · crop ≥ 3'));
  it('names the map first, then the crop', () =>
    expect(stageSummary('output', out({ format: 'xisf', cleanup: 'deleteIntermediates', coverageMap: true, cropToCoverage: 5 }))).toBe(
      'XISF · delete intermediates · coverage map · crop ≥ 5',
    ));
  it('keeps the line of a config stored before the two fields existed', () =>
    expect(stageSummary('output', { output: { format: 'fits', cleanup: 'keepAll' } } as unknown as StackingConfig)).toBe(
      'FITS · keep all',
    ));
});
