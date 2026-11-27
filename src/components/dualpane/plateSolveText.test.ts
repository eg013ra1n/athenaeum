import { describe, expect, it } from 'vitest';
import { formatInliers, formatSolveOdds } from './plateSolveText';

describe('formatSolveOdds', () => {
  it('formats odds rounding down', () => {
    expect(formatSolveOdds(314.9)).toBe('odds 10^314 to 1');
    expect(formatSolveOdds(9.0)).toBe('odds 10^9 to 1');
  });

  it('shows a dash for a solve stored before the change', () => {
    expect(formatSolveOdds(null)).toBe('—');
    expect(formatSolveOdds(undefined)).toBe('—');
    expect(formatSolveOdds(NaN)).toBe('—');
  });
});

describe('formatInliers', () => {
  it('inliers show the in-frame reference count when known', () => {
    expect(formatInliers(120, 476)).toBe('120 / 476');
    expect(formatInliers(15, null)).toBe('15');
  });

  it('never prints a zero denominator', () => {
    expect(formatInliers(15, 0)).toBe('15');
  });
});
