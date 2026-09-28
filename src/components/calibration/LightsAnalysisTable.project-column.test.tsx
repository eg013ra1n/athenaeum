import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { LightsAnalysisTable, type EnrichedLightFrame } from './LightsAnalysisTable';

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

describe('LightsAnalysisTable — Project column (Task 9, §8.3)', () => {
  it('renders the Project column and per-row chips when projectStates is non-empty', () => {
    const projectStates = new Map([
      [1, { state: 'updatePending', reason: null }],
      [2, { state: 'failsGate', reason: 'no analysis' }],
    ]);
    render(
      <MemoryRouter>
        <LightsAnalysisTable
          frames={[frame(1), frame(2)]}
          selectedFrameIds={new Set()}
          onSelectionChange={vi.fn()}
          projectStates={projectStates}
        />
      </MemoryRouter>
    );
    expect(screen.getByText('Project')).toBeInTheDocument();
    expect(screen.getByText('update')).toBeInTheDocument();
    const failsGateChip = screen.getByText('fails gate');
    expect(failsGateChip).toHaveAttribute('title', 'no analysis');
  });

  it('shows — for a frame with no entry in projectStates', () => {
    const projectStates = new Map([[1, { state: 'updatePending', reason: null }]]);
    render(
      <MemoryRouter>
        <LightsAnalysisTable
          frames={[frame(1), frame(2)]}
          selectedFrameIds={new Set()}
          onSelectionChange={vi.fn()}
          projectStates={projectStates}
        />
      </MemoryRouter>
    );
    expect(screen.getByText('—')).toBeInTheDocument();
  });

  it('omits the Project column header when projectStates is empty or absent', () => {
    render(
      <MemoryRouter>
        <LightsAnalysisTable
          frames={[frame(1)]}
          selectedFrameIds={new Set()}
          onSelectionChange={vi.fn()}
        />
      </MemoryRouter>
    );
    expect(screen.queryByText('Project')).not.toBeInTheDocument();
  });
});
