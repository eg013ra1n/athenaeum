import { describe, it, expect } from 'vitest';
import type { FrameAnalysis } from '../../types/models';
import { EMPTY_THRESHOLDS } from '../calibration/RejectionThresholdBar';
import { computeRejectedFrameIds } from './rejection';

function a(id: number, over: Partial<FrameAnalysis>): FrameAnalysis {
  return {
    frame_id: id, median_fwhm: 3, median_eccentricity: 0.3, frame_snr: 30, snr_weight: 1,
    measure_version: 5, frame_verdict: null, possibly_trailed: false, ...over,
  } as FrameAnalysis;
}

describe('computeRejectedFrameIds', () => {
  const data = new Map<number, FrameAnalysis>([
    [1, a(1, {})],
    [2, a(2, { frame_verdict: 'defocused' })],
    [3, a(3, { measure_version: null, possibly_trailed: true })],
  ]);
  it('Reject unusable frames drops a verdict row and a legacy trailed row, keeps a usable one', () => {
    const out = computeRejectedFrameIds([1, 2, 3, 4], data, { ...EMPTY_THRESHOLDS, trail: 'true' }, null);
    expect([...out].sort()).toEqual([2, 3]);
  });
  it('rejects nothing for unusable rows when the switch is off', () => {
    expect(computeRejectedFrameIds([1, 2, 3], data, EMPTY_THRESHOLDS, null).size).toBe(0);
  });
});
