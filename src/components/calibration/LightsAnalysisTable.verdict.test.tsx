import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { LightsAnalysisTable, type EnrichedLightFrame } from './LightsAnalysisTable';
import type { FrameAnalysis, FrameVerdict } from '../../types/models';

function frame(id: number): EnrichedLightFrame {
  return {
    frame_id: id,
    // Deliberately NOT equal to `frame_id` — the Project column keys
    // `projectStates` by `frame_id`; a fixture where the two happened to
    // match would still pass if the lookup used `file_id` by mistake.
    file_id: id + 1000,
    filename: `frame-${id}.fits`,
    file_path: `/data/frame-${id}.fits`,
    date_obs: '2026-09-01T00:00:00',
    exptime: 300,
    telescop: null,
    focallen: 500,
    xpixsz: null,
    binning: '1x1',
    ccd_temp: null,
    swcreate: null,
    gain: null,
    offset: null,
    rotation: null,
    objctra: null,
    objctdec: null,
    // Non-null so the WCS column renders a "Header" badge rather than its
    // own "—" — keeps that glyph unambiguous for the Project-column
    // assertions below.
    ra: 10,
    dec: 20,
    plate_solved: false,
    calibration_status: {
      frame_id: id,
      has_flats: true,
      has_darks: true,
      has_bias: true,
      has_darkflats: false,
      flats_warning: false,
      darks_warning: false,
      bias_warning: false,
      flat_set_id: null,
      dark_set_id: null,
      bias_set_id: null,
      darkflat_set_id: null,
    },
    camera: 'Camera 1',
    filter: 'L',
  };
}

function analysis(frameId: number, verdict: FrameVerdict | null, refused: number | null): FrameAnalysis {
  return {
    frame_id: frameId, stars_detected: 100, median_fwhm: 3, median_eccentricity: 0.4, median_snr: 20,
    median_hfr: 2, frame_snr: 30, snr_weight: 1, psf_signal: 1000,
    possibly_trailed: false, median_beta: null, measure_version: refused == null ? null : 5,
    stars_refused: refused, frame_verdict: verdict,
  } as unknown as FrameAnalysis;
}

function rowsText(): string[] {
  const rows = screen.getAllByRole('row').slice(1);
  return rows.map((r) => r.textContent ?? '');
}

describe('LightsAnalysisTable — Verdict and Refused columns (Plan C C8)', () => {
  const data = new Map<number, FrameAnalysis>([
    [1, analysis(1, 'trailed', 12)],
    [2, analysis(2, null, 3)],
    [3, analysis(3, 'no_stars', null)],
    [4, analysis(4, 'defocused', 7)],
  ]);

  function renderTable() {
    render(
      <MemoryRouter>
        <LightsAnalysisTable
          frames={[frame(1), frame(2), frame(3), frame(4)]}
          selectedFrameIds={new Set()}
          onSelectionChange={vi.fn()}
          analysisData={data}
        />
      </MemoryRouter>
    );
  }

  it('shows the verdict wording with a tooltip, no Trail R² column, no dB', () => {
    renderTable();
    expect(screen.queryByText('Trail R²')).not.toBeInTheDocument();
    expect(screen.queryByText('dB')).not.toBeInTheDocument();
    const trailed = screen.getByText('Trailed');
    expect(trailed).toHaveClass('text-warning');
    expect(trailed).toHaveAttribute('title', expect.stringContaining('they are trails'));
    expect(screen.getByText('Defocused')).toHaveAttribute('title', expect.stringContaining('out-of-focus rings'));
    const noStars = screen.getByText('No stars');
    expect(noStars).toHaveClass('text-content-muted');
    expect(noStars).toHaveAttribute('title', expect.stringContaining('no star to fit'));
  });

  it('shows — for a legacy row Refused', () => {
    renderTable();
    const rows = screen.getAllByRole('row').slice(1);
    const legacy = rows.find((r) => within(r).queryByText('No stars'));
    expect(legacy).toBeDefined();
    expect(within(legacy!).getAllByText('\u2014').length).toBeGreaterThan(0);
  });

  it('sorts the Verdict column by severity', () => {
    renderTable();
    const header = screen.getByRole('button', { name: 'Verdict' });
    fireEvent.click(header); // ascending: none, No stars, Defocused, Trailed
    const order = rowsText().map((t) => (t.includes('Trailed') ? 'T' : t.includes('Defocused') ? 'D' : t.includes('No stars') ? 'N' : '-'));
    expect(order).toEqual(['-', 'N', 'D', 'T']);
  });

  it('sorts the Refused column numerically', () => {
    renderTable();
    fireEvent.click(screen.getByRole('button', { name: 'Refused' }));
    const order = rowsText().map((t) => (t.includes('Trailed') ? 'T' : t.includes('Defocused') ? 'D' : t.includes('No stars') ? 'N' : '-'));
    // legacy (null) first, then 3, 7, 12
    expect(order).toEqual(['N', '-', 'D', 'T']);
  });
});
