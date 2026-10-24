import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { FrameInfoPanel } from './FrameInfoPanel';
import type { FrameAnalysis } from '../../types/models';

function metrics(over: Partial<FrameAnalysis>): FrameAnalysis {
  return {
    frame_id: 1, stars_detected: 100, median_fwhm: 3, median_eccentricity: 0.3, median_hfr: 2, median_snr: 20,
    frame_snr: 30, snr_weight: 1, psf_signal: 1000, detection_threshold: 4.5, median_beta: null,
    measure_version: 5, stars_refused: 7, frame_verdict: null, possibly_trailed: false, ...over,
  } as unknown as FrameAnalysis;
}

function rowValue(label: string): string | null | undefined {
  return screen.getByText(label).nextElementSibling?.textContent;
}

describe('FrameInfoPanel metrics', () => {
  it('a legacy row shows the σ threshold as — and Trailed (legacy)', () => {
    render(<FrameInfoPanel currentFrame={undefined} metrics={metrics({ measure_version: null, stars_refused: null, possibly_trailed: true })} />);
    expect(rowValue('Detection threshold (σ)')).toBe('—');
    expect(rowValue('Verdict')).toBe('Trailed (legacy)');
    expect(rowValue('Refused')).toBe('—');
  });
  it('a current row shows the σ threshold, its verdict and Refused', () => {
    render(<FrameInfoPanel currentFrame={undefined} metrics={metrics({ frame_verdict: 'defocused' })} />);
    expect(rowValue('Detection threshold (σ)')).toBe('4.5');
    expect(rowValue('Verdict')).toBe('Defocused');
    expect(rowValue('Refused')).toBe('7');
  });
});
