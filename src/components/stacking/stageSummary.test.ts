import { describe, expect, it } from 'vitest';
import { stageSummary } from './stageSummary';
import type { StackingConfig } from '../../types/stacking';

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
