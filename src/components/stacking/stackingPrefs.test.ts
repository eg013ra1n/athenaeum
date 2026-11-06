import { afterEach, describe, expect, it, vi } from 'vitest';
import { readFramesView, readSelectedRunId, writeFramesView, writeSelectedRunId } from './stackingPrefs';

const PREFIX = 'athenaeum.stacking.selectedRun.';

describe('selected run id by subject key', () => {
  afterEach(() => localStorage.clear());

  it('stores a frame set under the prefix and the bare set id', () => {
    writeSelectedRunId('set:12', 5);
    expect(localStorage.getItem(`${PREFIX}12`)).toBe('5');
    expect(localStorage.getItem(`${PREFIX}set:12`)).toBeNull();
  });

  it('reads a value stored under the pre-subject spelling', () => {
    localStorage.setItem(`${PREFIX}12`, '9');
    expect(readSelectedRunId('set:12')).toBe(9);
  });

  it('stores a project under the prefix and project:<projectId>', () => {
    writeSelectedRunId('project:p1', 3);
    expect(localStorage.getItem(`${PREFIX}project:p1`)).toBe('3');
    expect(readSelectedRunId('project:p1')).toBe(3);
  });

  it('keeps a frame set and a project with the same digits apart', () => {
    writeSelectedRunId('set:1', 10);
    writeSelectedRunId('project:1', 20);
    expect(readSelectedRunId('set:1')).toBe(10);
    expect(readSelectedRunId('project:1')).toBe(20);
  });

  it('reads null when nothing or an empty string is stored', () => {
    expect(readSelectedRunId('set:99')).toBeNull();
    localStorage.setItem(`${PREFIX}99`, '');
    expect(readSelectedRunId('set:99')).toBeNull();
  });
});

describe('the Frames view', () => {
  afterEach(() => {
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it('frames_view_defaults_to_group_px_and_no_registration_columns', () => {
    expect(readFramesView()).toEqual({ groupMode: 'group', arcsec: false, columns: false });
  });

  it('frames_view_round_trips', () => {
    writeFramesView({ groupMode: 'status', arcsec: true, columns: true });
    expect(readFramesView()).toEqual({ groupMode: 'status', arcsec: true, columns: true });
    writeFramesView({ groupMode: 'none', arcsec: false, columns: true });
    expect(readFramesView()).toEqual({ groupMode: 'none', arcsec: false, columns: true });
  });

  it('frames_view_reads_corrupt_values_as_defaults', () => {
    localStorage.setItem('athenaeum.stacking.framesGroupMode', 'filter');
    localStorage.setItem('athenaeum.stacking.framesArcsec', 'yes');
    localStorage.setItem('athenaeum.stacking.framesColumns', '1');
    expect(readFramesView()).toEqual({ groupMode: 'group', arcsec: false, columns: false });
  });

  it('frames_view_survives_a_storage_failure', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('denied');
    });
    expect(readFramesView()).toEqual({ groupMode: 'group', arcsec: false, columns: false });
    writeFramesView({ groupMode: 'night', arcsec: true, columns: false });
    expect(warn).toHaveBeenCalledTimes(2);
  });
});
