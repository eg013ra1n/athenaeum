// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
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

describe('MeasurePanel PSF model', () => {
  const withPsf = (psfModel: StackingConfig['measurement']['psfModel']): StackingConfig => {
    const base = cfg('matchedFilter');
    return { ...base, measurement: { ...base.measurement, psfModel } };
  };
  const betaInput = () => screen.getByLabelText('Moffat β') as HTMLInputElement;
  const patched = (onChange: ReturnType<typeof vi.fn>) =>
    onChange.mock.calls.map((c) => (c[0] as StackingConfig).measurement.psfModel);

  it('measure_panel_offers_a_moffat_with_its_own_beta', () => {
    const onChange = vi.fn();
    const { rerender } = render(<MeasurePanel config={withPsf('auto')} defaults={cfg('matchedFilter')} onChange={onChange} />);
    const select = control('PSF model') as HTMLSelectElement;
    expect(Array.from(select.options).map((o) => o.value)).toEqual(['auto', 'moffat4', 'moffat-own']);
    expect(screen.queryByLabelText('Moffat β')).toBeNull();

    fireEvent.change(select, { target: { value: 'moffat-own' } });
    expect(patched(onChange)).toEqual([{ moffat: 4 }]);

    rerender(<MeasurePanel config={withPsf({ moffat: 4 })} defaults={cfg('matchedFilter')} onChange={onChange} />);
    expect((control('PSF model') as HTMLSelectElement).value).toBe('moffat-own');
    expect(betaInput().value).toBe('4');
    expect(betaInput().step).toBe('0.1');

    fireEvent.change(betaInput(), { target: { value: '8' } });
    expect(patched(onChange)[patched(onChange).length - 1]).toEqual({ moffat: 8 });

    onChange.mockClear();
    fireEvent.change(betaInput(), { target: { value: '1.2' } });
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByText(/β from 1\.5 to 100 — not saved/)).toBeTruthy();
    fireEvent.change(betaInput(), { target: { value: '101' } });
    expect(onChange).not.toHaveBeenCalled();

    fireEvent.change(select, { target: { value: 'moffat4' } });
    expect(patched(onChange)).toEqual(['moffat4']);
  });

  it('keeps the chosen beta when the select is moved off and the own-beta key is picked again from a beta', () => {
    const onChange = vi.fn();
    render(<MeasurePanel config={withPsf({ moffat: 8 })} defaults={cfg('matchedFilter')} onChange={onChange} />);
    fireEvent.change(control('PSF model'), { target: { value: 'moffat-own' } });
    expect(patched(onChange)).toEqual([{ moffat: 8 }]);
  });

  it('names the beta in the stage summary', () => {
    expect(stageSummary('measure', withPsf({ moffat: 8 }))).toContain('Moffat β 8 PSF');
  });
});
