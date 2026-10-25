import { describe, expect, it } from 'vitest';
import type { CalibrationSetDetail } from '../types/models';
import { emptyFilters, matchesLibraryFilters, sizeKey } from './DarkLibraryFilters';

const set = (patch: Partial<CalibrationSetDetail>): CalibrationSetDetail => ({
  id: 1, imagetyp: 'Dark', exptime: 120, ccd_temp: -10, temp_min: -10.2, temp_max: -9.8,
  gain: 100, offset: 50, binning: '1x1', instrume: 'ZWO ASI6200MM Pro', filter: null,
  date_start: '2023-07-27T21:00:00', date_end: '2023-07-27T23:00:00', date_display: '2023-07',
  frame_count: 30, is_master: false, naxis1: 9576, naxis2: 6388, xorgsubf: null, yorgsubf: null,
  bayerpat: null, swcreate: null, xpixsz: 3.76, format: 'FITS', focallen: null, uuid: null,
  updated_at: null, superseded_by_set_id: null,
  ...patch,
});

describe('calibration-library filters', () => {
  it('spells the size key without the origin', () => {
    expect(sizeKey(set({ xorgsubf: 1584, yorgsubf: 1064 }))).toBe('9576×6388');
    expect(sizeKey(set({ naxis1: null }))).toBeNull();
  });

  it('empty filters pass every set', () => {
    expect(matchesLibraryFilters(set({ gain: null, binning: null, naxis1: null }), emptyFilters)).toBe(true);
  });

  it('narrows by frame size', () => {
    const f = { ...emptyFilters, size: '9576×6388' };
    expect(matchesLibraryFilters(set({}), f)).toBe(true);
    expect(matchesLibraryFilters(set({ naxis1: 6384, naxis2: 4258 }), f)).toBe(false);
    expect(matchesLibraryFilters(set({ naxis1: null, naxis2: null }), f)).toBe(false);
  });

  it('narrows by gain and by binning', () => {
    expect(matchesLibraryFilters(set({}), { ...emptyFilters, gain: 100 })).toBe(true);
    expect(matchesLibraryFilters(set({ gain: 0 }), { ...emptyFilters, gain: 100 })).toBe(false);
    expect(matchesLibraryFilters(set({ gain: null }), { ...emptyFilters, gain: 100 })).toBe(false);
    expect(matchesLibraryFilters(set({ binning: '2x2' }), { ...emptyFilters, binning: '1x1' })).toBe(false);
  });

  it('keeps the existing temperature band and exposure range rules', () => {
    expect(matchesLibraryFilters(set({}), { ...emptyFilters, tempBand: '-10 to -5°C' })).toBe(true);
    expect(matchesLibraryFilters(set({ ccd_temp: -5 }), { ...emptyFilters, tempBand: '-10 to -5°C' })).toBe(false);
    expect(matchesLibraryFilters(set({ exptime: 0.5 }), { ...emptyFilters, expFrom: 1 })).toBe(false);
  });
});
