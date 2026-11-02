// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { NormalizePanel } from './NormalizePanel';
import type { RejectionNormalization, StackingConfig } from '../../../types/stacking';

const NOTE = 'Local normalization runs for rejection only';

const cfg = (rejection: RejectionNormalization, lnEnabled: boolean): StackingConfig =>
  ({
    normalization: {
      output: 'additiveWithScaling',
      rejection,
      scaleEstimator: 'bwmv',
      local: {
        enabled: lnEnabled,
        scale: 1024,
        scaleMode: 'auto',
        gridSize: 4,
        referenceFrames: 20,
        psfModel: 'auto',
        localScale: false,
        fluxEstimator: 'psfSignal',
        aperture: 'fwhm3',
        starSelection: { minPeakSnr: 40, maxStars: 500, saturation: 0.75 },
      },
    },
  }) as unknown as StackingConfig;

/** The control that follows a label's text (label and control are siblings in the panel idiom). */
const control = (label: string) => screen.getByText(label).nextElementSibling as HTMLSelectElement;

const show = (config: StackingConfig, onChange = vi.fn()) => {
  render(<NormalizePanel config={config} defaults={cfg('local', false)} onChange={onChange} />);
  return onChange;
};

describe('NormalizePanel rejection normalization', () => {
  it('keeps local selectable with LN off, labelled recommended, and selecting it leaves LN alone', () => {
    const onChange = show(cfg('scaleZeroOffset', false));
    const select = control('Rejection normalization');
    const local = Array.from(select.options).find((o) => o.value === 'local');
    expect(local).toBeDefined();
    expect(local?.disabled).toBe(false);
    expect(local?.textContent).toBe('Local (recommended)');

    fireEvent.change(select, { target: { value: 'local' } });
    expect(onChange).toHaveBeenCalledTimes(1);
    const next = onChange.mock.calls[0]?.[0] as StackingConfig;
    expect(next.normalization.rejection).toBe('local');
    expect(next.normalization.local.enabled).toBe(false);
  });

  it('shows the rejection-only note exactly when rejection is local and LN is off', () => {
    const cases: Array<[RejectionNormalization, boolean, boolean]> = [
      ['local', false, true],
      ['local', true, false],
      ['scaleZeroOffset', false, false],
      ['scaleZeroOffset', true, false],
      ['equalizeFluxes', false, false],
      ['none', false, false],
    ];
    for (const [rejection, lnEnabled, expected] of cases) {
      const { unmount } = render(
        <NormalizePanel config={cfg(rejection, lnEnabled)} defaults={cfg('local', false)} onChange={vi.fn()} />,
      );
      expect(screen.queryByText(NOTE) !== null, `${rejection} / LN ${lnEnabled ? 'on' : 'off'}`).toBe(expected);
      unmount();
    }
  });

  it('names the benefit in plain words', () => {
    show(cfg('local', true));
    expect(screen.getByText(/Keeps the cores of your sharpest frames when seeing varies\./)).not.toBeNull();
  });

  it('turning LN off while rejection is local keeps rejection local', () => {
    const onChange = show(cfg('local', true));
    fireEvent.click(screen.getByRole('checkbox', { name: 'Enable local normalization' }));
    expect(onChange).toHaveBeenCalledTimes(1);
    const next = onChange.mock.calls[0]?.[0] as StackingConfig;
    expect(next.normalization.rejection).toBe('local');
    expect(next.normalization.local.enabled).toBe(false);
    expect(screen.queryByText(/reset to/)).toBeNull();
  });
});
