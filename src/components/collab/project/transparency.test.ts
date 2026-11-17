import { expect, it } from 'vitest';
import { formatTransparency, formatZeroPoint } from './transparency';

it('formats a loss with a real minus sign', () => {
  const s = formatTransparency(0.77);
  expect(s).toBe('−0.77 mag');
  expect(s.codePointAt(0)).toBe(0x2212);
});

it('shows a loss under half a hundredth as zero', () => {
  expect(formatTransparency(0.004)).toBe('0.00 mag');
});

it('shows a dash for no value', () => {
  expect(formatTransparency(null)).toBe('—');
  expect(formatTransparency(undefined)).toBe('—');
});

it('formats the reduced zero point with its star count and no scatter', () => {
  expect(formatZeroPoint({ zeroPoint: 21.342, zpStars: 87, zpSigma: 0.041 })).toBe('21.34 · 87 stars');
});

it('shows a dash for no zero point', () => {
  expect(formatZeroPoint({ zeroPoint: null, zpStars: null, zpSigma: null })).toBe('—');
  expect(formatZeroPoint(null)).toBe('—');
});

it('formats a single star in the singular', () => {
  expect(formatZeroPoint({ zeroPoint: 21.34, zpStars: 1, zpSigma: 0.04 })).toBe('21.34 · 1 star');
  expect(formatZeroPoint({ zeroPoint: 21.34, zpStars: 87, zpSigma: 0.04 })).toBe('21.34 · 87 stars');
});

it('drops the parts a zero point does not have', () => {
  expect(formatZeroPoint({ zeroPoint: 21.34, zpStars: null, zpSigma: 0.04 })).toBe('21.34');
  expect(formatZeroPoint({ zeroPoint: 21.34, zpStars: 87, zpSigma: null })).toBe('21.34 · 87 stars');
});

it('rounds a loss at the half hundredth away from zero', () => {
  expect(formatTransparency(0.005)).toBe('−0.01 mag');
  expect(formatTransparency(0.004999)).toBe('0.00 mag');
});

it('shows a dash for a loss that is not a number', () => {
  expect(formatTransparency(NaN)).toBe('—');
  expect(formatTransparency(Infinity)).toBe('—');
});
