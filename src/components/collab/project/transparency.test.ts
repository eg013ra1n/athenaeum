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

it('formats a zero point with its star count and scatter', () => {
  expect(formatZeroPoint({ zeroPoint: 21.342, zpStars: 87, zpSigma: 0.041 })).toBe('21.34 · 87 stars · ±0.04');
});

it('shows a dash for no zero point', () => {
  expect(formatZeroPoint({ zeroPoint: null, zpStars: null, zpSigma: null })).toBe('—');
  expect(formatZeroPoint(null)).toBe('—');
});
