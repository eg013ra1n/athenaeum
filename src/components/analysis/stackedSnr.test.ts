import { describe, it, expect } from 'vitest';
import { stackedFrameSnr } from './stackedSnr';

describe('stackedFrameSnr', () => {
  it('adds ratios in quadrature', () => {
    expect(stackedFrameSnr([3, 4])).toBe(5);
  });
  it('is undefined for no values', () => {
    expect(stackedFrameSnr([])).toBeUndefined();
  });
  it('skips non-finite values', () => {
    expect(stackedFrameSnr([NaN, 3])).toBe(3);
    expect(stackedFrameSnr([Infinity, NaN])).toBeUndefined();
  });
  it('returns a single value unchanged', () => {
    expect(stackedFrameSnr([2])).toBe(2);
  });
});
