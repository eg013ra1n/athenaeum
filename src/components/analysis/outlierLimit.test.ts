import { describe, it, expect } from 'vitest';
import { autoOutlierLimit } from './outlierLimit';

describe('autoOutlierLimit', () => {
  it('autoOutlierLimit_matches_analysis', () => {
    // median = sorted[2] = 2.2; deviations 0.2 0.1 0 0.1 6.8 sort to 0 0.1 0.1 0.2 6.8,
    // so MAD = 0.1; the floor 0.1 * 2.2 = 0.22 wins: 2.2 + 3 * 0.22 = 2.86.
    const limit = autoOutlierLimit([2.0, 2.1, 2.2, 2.3, 9.0]);
    expect(limit).not.toBeNull();
    expect(Math.abs((limit as number) - 2.86)).toBeLessThan(1e-9);
    expect(autoOutlierLimit([])).toBeNull();
  });

  it('takes the upper median of an even count and does not reorder its input', () => {
    const values = [4, 1, 3, 2];
    // sorted 1 2 3 4: median = sorted[2] = 3; deviations 2 1 0 1 sort to 0 1 1 2,
    // MAD = sorted[2] = 1; floor 0.3 < 1: 3 + 3 * 1 = 6.
    expect(autoOutlierLimit(values)).toBe(6);
    expect(values).toEqual([4, 1, 3, 2]);
  });

  it('floors a tight cluster at a tenth of the median', () => {
    // MAD 0: 2 + 3 * 0.2 = 2.6
    expect(autoOutlierLimit([2, 2, 2])).toBeCloseTo(2.6, 12);
  });
});
