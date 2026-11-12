import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { CandidateCard } from './CandidateCard';
import type { CalibrationSetWithScore } from '../../types/models';

afterEach(cleanup);

const candidate = (status: 'match' | 'mismatch' | 'warning'): CalibrationSetWithScore => ({
  set: {
    id: 1843, imagetyp: 'MasterDark', exptime: 120, ccd_temp: -10, temp_min: -10, temp_max: -10,
    gain: 100, offset: 50, binning: '1x1', instrume: 'ASI6200MM', filter: null,
    date_start: '2023-07-28T17:24:40Z', date_end: '2023-07-28T18:24:40Z', date_display: '2023-07-28',
    frame_count: 1, is_master: true, naxis1: 6384, naxis2: 4258, xorgsubf: 1584, yorgsubf: 1064,
    bayerpat: null, swcreate: null, xpixsz: null, format: 'FITS', focallen: null,
    uuid: null, updated_at: null, superseded_by_set_id: null,
  },
  match_score: 0.9,
  compatible: status === 'match',
  parameters: [{
    name: 'geometry', enforced: true, status,
    frameValue: '9576×6388', setValue: '6384×4258 @ 1584,1064',
    diff: null, warningThreshold: null, matchingThreshold: null,
  }],
  match_details: {
    instrume_match: true, binning_match: true, gain_match: true, offset_match: true,
    exptime_match: true, filter_match: true, temp_diff: 0, date_diff_days: 0,
  },
});

describe('CandidateCard geometry', () => {
  it('shows the size and origin among the set facts', () => {
    render(<CandidateCard candidate={candidate('match')} selected={false} isCurrent={false} onSelect={vi.fn()} />);
    expect(screen.getByRole('button').textContent).toContain('ASI6200MM · 120 s · -10.0 °C · 1x1 · 6384×4258 @ 1584,1064');
  });
  it('a refused set that is not selected cannot be chosen', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('mismatch')} selected={false} isCurrent={false} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(onSelect).not.toHaveBeenCalled();
  });
  it('the linked-now refused set stays deselectable', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('mismatch')} selected={true} isCurrent={true} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
  it('another sub-frame origin is selectable, with a warning badge (G-7)', () => {
    const onSelect = vi.fn();
    render(<CandidateCard candidate={candidate('warning')} selected={false} isCurrent={false} onSelect={onSelect} />);
    const button = screen.getByRole('button');
    expect(button).not.toBeDisabled();
    expect(button.textContent).toContain('Another sub-frame origin');
    expect(button.textContent).toContain('⚠Sub-frame origin9576×6388 → 6384×4258 @ 1584,1064');
    fireEvent.click(button);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});

describe('CandidateCard gain and offset', () => {
  const level = (
    imagetyp: 'MasterDark' | 'MasterFlat',
    gain: [string | null, string | null],
    offset: [string | null, string | null],
    enforced = true,
  ): CalibrationSetWithScore => {
    const c = candidate('match');
    c.set.imagetyp = imagetyp;
    c.parameters = [
      { name: 'gain', enforced, status: 'match', frameValue: gain[0], setValue: gain[1], diff: null, warningThreshold: null, matchingThreshold: null },
      { name: 'offset', enforced, status: 'match', frameValue: offset[0], setValue: offset[1], diff: null, warningThreshold: null, matchingThreshold: null },
    ];
    return c;
  };
  const show = (c: CalibrationSetWithScore) =>
    render(<CandidateCard candidate={c} selected={false} isCurrent={false} onSelect={vi.fn()} />).container.textContent ?? '';

  it('badges a dark whose offset is unknown, even when the config ignores it', () => {
    expect(show(level('MasterDark', ['100', '100'], ['1', null], false))).toContain('Gain/offset unknown');
  });
  it('badges another offset, another gain, or both', () => {
    expect(show(level('MasterDark', ['100', '100'], ['30', '200']))).toContain('Another offset');
    cleanup();
    expect(show(level('MasterDark', ['100', '200'], ['30', '30']))).toContain('Another gain');
    cleanup();
    expect(show(level('MasterDark', ['100', '200'], ['30', '200']))).toContain('Another gain and offset');
  });
  it('has no badge when both agree, and none on a flat', () => {
    expect(show(level('MasterDark', ['100', '100'], ['30', '30']))).not.toMatch(/Another|unknown/);
    cleanup();
    expect(show(level('MasterFlat', ['100', '200'], ['30', '200']))).not.toMatch(/Another|unknown/);
  });
  it('uses the warning badge classes', () => {
    render(<CandidateCard candidate={level('MasterDark', ['100', '100'], ['30', '200'])} selected={false} isCurrent={false} onSelect={vi.fn()} />);
    expect(screen.getByText('Another offset').className).toContain('bg-warning-muted');
    expect(screen.getByText('Another offset').className).toContain('text-warning');
  });
});
