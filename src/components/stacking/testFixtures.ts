// Shared test fixtures of the Frames block: a project's plan frames and a run's frame rows and summary frames, used
// by the row-model tests (`frameRows.test.ts`) and the table's render tests (`FramesTable.test.tsx`).
import type {
  PlanProjectFrame,
  RunSummary,
  StackingRunDetail,
  StackingRunFrameRow,
  SummaryFrame,
} from '../../types/stacking';
import type { RunFrameData } from './frameRows';

export const CANNOT_OPEN = 'cannot be opened (offline, moved or renamed)';

export const pf = (over: Partial<PlanProjectFrame> = {}): PlanProjectFrame => ({
  handle: 1, frameUuid: 'u1', fileName: 'a.fits', publisher: 'Alice', filterCanonical: 'L', exptimeSec: 300,
  colorMode: 'mono', hasWcs: true, onDisk: true, reason: null, ...over,
});

/** Five plan rows: usable with a WCS (u1), not on this device (u2), unsupported (u3), usable without a WCS (u4), and
 *  landed but not openable (u5: `onDisk` true AND a reason). */
export const PLAN_FRAMES: PlanProjectFrame[] = [
  pf({ handle: 1, frameUuid: 'u1', fileName: 'a.fits', publisher: 'Alice' }),
  pf({ handle: null, frameUuid: 'u2', fileName: 'b.fits', publisher: 'Bob', onDisk: false, reason: 'not on this device' }),
  pf({ handle: 3, frameUuid: 'u3', fileName: 'c.fits', publisher: 'Alice', reason: 'unsupported file: 8-bit JPEG' }),
  pf({ handle: 4, frameUuid: 'u4', fileName: 'd.fits', publisher: 'Cleo', hasWcs: false }),
  pf({ handle: 5, frameUuid: 'u5', fileName: 'e.fits', publisher: 'Bob', reason: CANNOT_OPEN }),
];

export const runRow = (over: Partial<StackingRunFrameRow>): StackingRunFrameRow => ({
  id: 1, runId: 5, groupId: 10, frameId: 7, included: true, exclusionReason: null, weight: 0.8,
  weightChannelsJson: null, metricsJson: null, regStatus: 'ok', regModel: 'homography', regRmsPx: 0.2, regInliers: 90,
  regInlierRatio: 0.9, regFlipped: false, rejectedFraction: null, frameUuid: null, ...over,
});

export const summaryFrame = (over: Partial<SummaryFrame>): SummaryFrame =>
  ({
    frameId: 7, filename: 'from-summary.fits', included: true, exclusionReason: null, weight: 0.75, weightChannels: [],
    fwhmPx: 2.5, eccentricity: 0.2, stars: 120, regStatus: 'ok', regModel: 'homography', regRmsPx: 0.3, regInliers: 88,
    lnScale: null, ...over,
  }) as unknown as SummaryFrame;

const CO_REGISTERED = { registration: { geometry: 'coRegistered' } };

/** Run #5 of one group `L-mono` (group id 10), co-registered: its frame rows, and a summary of one group when
 *  `summaryFrames` is given (`null`: the run's summary is not written yet). */
export const detailOf = (
  frames: StackingRunFrameRow[],
  summaryFrames: SummaryFrame[] | null,
  run: RunFrameData['run'] = { id: 5, referenceMode: 'auto' },
): StackingRunDetail =>
  ({
    run: { startedAt: '2026-11-01T10:00:00', configJson: JSON.stringify(CO_REGISTERED), ...run },
    groups: [{ id: 10, groupKey: 'L-mono' }],
    frames,
    summary:
      summaryFrames &&
      ({
        config: CO_REGISTERED,
        reference: { frameId: null, filename: null, mode: run.referenceMode, weight: null, switchedFrom: null },
        groups: [{ key: 'L-mono', referenceFrameId: null, normalizationReferenceFrameId: null, frames: summaryFrames }],
      } as unknown as RunSummary),
  }) as unknown as StackingRunDetail;
