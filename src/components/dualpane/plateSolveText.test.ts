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
  it('inliers show the in-frame reference count of a recipe-3 row', () => {
    expect(formatInliers(120, 476, 314.4)).toBe('120 / 476');
  });

  it('a row an older build wrote after the step shows the matched count alone', () => {
    expect(formatInliers(15, 1, null)).toBe('15');
    expect(formatInliers(15, 1, undefined)).toBe('15');
    expect(formatInliers(15, 1, NaN)).toBe('15');
  });

  it('never prints a missing or zero denominator', () => {
    expect(formatInliers(15, null, 12.5)).toBe('15');
    expect(formatInliers(15, 0, 12.5)).toBe('15');
  });
});
