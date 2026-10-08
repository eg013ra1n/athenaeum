// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MeasurePanel } from './MeasurePanel';
import { stageSummary } from '../stageSummary';
import type { SeedSource, StackingConfig } from '../../../types/stacking';

const cfg = (seedSource: SeedSource): StackingConfig =>
  ({
    measurement: {
      weightMode: 'psfSignalWeight',
      keyword: '',
      formula: { fwhm: 1, eccentricity: 1, snr: 1, stars: 1, pedestal: 1 },
      psfModel: 'auto',
      maxStars: 500,
      detectionSigma: 20,
      seedPrefilter: 'none',
      seedDetector: seedSource === 'structure' ? 'structure' : 'peak',
      seedSource,
      seedThreshold: 6,
    },
    selection: {
      minWeightFraction: 0,
      maxFwhmPx: null,
      maxEccentricity: null,
      minStars: null,
      excludeOnRegistrationFailure: true,
    },
  }) as unknown as StackingConfig;

/** The control that follows a label's text (label and control are siblings in the panel idiom). */
const control = (label: string) => screen.getByText(label).nextElementSibling as HTMLInputElement | HTMLSelectElement;

const show = (source: SeedSource) =>
  render(<MeasurePanel config={cfg(source)} defaults={cfg('matchedFilter')} onChange={vi.fn()} />);

describe('MeasurePanel seed source', () => {
  it('offers the three sources and no Seed detector select', () => {
    show('matchedFilter');
    const options = Array.from((control('Seed source') as HTMLSelectElement).options).map((o) => o.value);
    expect(options).toEqual(['matchedFilter', 'peak', 'structure']);
    expect(screen.queryByText('Seed detector')).toBeNull();
  });

  it('matchedFilter enables only the seed threshold', () => {
    show('matchedFilter');
    expect(control('Seed threshold (σ_f)').disabled).toBe(false);
    expect(control('Detection threshold (σ)').disabled).toBe(true);
    expect(control('Seed pre-filter').disabled).toBe(true);
  });

  it('peak enables the two peak dials only', () => {
    show('peak');
    expect(control('Seed threshold (σ_f)').disabled).toBe(true);
    expect(control('Detection threshold (σ)').disabled).toBe(false);
    expect(control('Seed pre-filter').disabled).toBe(false);
  });

  it('structure disables all three dials', () => {
    show('structure');
    expect(control('Seed threshold (σ_f)').disabled).toBe(true);
    expect(control('Detection threshold (σ)').disabled).toBe(true);
    expect(control('Seed pre-filter').disabled).toBe(true);
  });
});

describe('stageSummary measure seeds', () => {
  it('names the matched filter and its threshold', () => {
    expect(stageSummary('measure', cfg('matchedFilter'))).toMatch(/ · seeds: matched filter \(σ_f 6\.0\)$/);
  });
  it('keeps the peak and structure wording', () => {
    expect(stageSummary('measure', cfg('peak'))).toMatch(/ · σ 20$/);
    expect(stageSummary('measure', cfg('structure'))).toMatch(/ · structure-map seeds$/);
  });
});
