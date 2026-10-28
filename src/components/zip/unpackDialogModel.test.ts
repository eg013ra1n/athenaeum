import { describe, expect, it } from 'vitest';
import { defaultUnpackDir, mainZip, unpackResultLine, unpackSummary, zipParent, zipStem } from './unpackDialogModel';

describe('default unpack target', () => {
  it('is a folder named like the zip, next to it', () => {
    expect(defaultUnpackDir('/data/astro/M31 night 1.zip')).toBe('/data/astro/M31 night 1');
    expect(defaultUnpackDir('C:\\astro\\Night.ZIP')).toBe('C:\\astro\\Night');
    expect(zipStem('/a/b.c.zip')).toBe('b.c');
    expect(zipParent('/a/b.zip')).toBe('/a');
    expect(defaultUnpackDir('night.zip')).toBe('night');
  });
  it('follows the zip holding most of the frames', () => {
    expect(mainZip(['/a.zip', '/b.zip', '/b.zip'])).toBe('/b.zip');
    expect(mainZip([])).toBeNull();
  });
});

describe('unpack wording', () => {
  it('summarises a clean run, a run with conflicts and a failure', () => {
    expect(unpackSummary({ unpacked: 12, adopted: 0, conflicts: 0 }, 'completed')).toEqual({ title: 'Unpacked 12 frames', detail: undefined });
    expect(unpackSummary({ unpacked: 10, adopted: 2, conflicts: 0 }, 'completed')).toEqual({ title: 'Unpacked 12 frames', detail: '10 unpacked, 2 already in place' });
    expect(unpackSummary({ unpacked: 9, adopted: 0, conflicts: 3 }, 'completed_with_conflicts').title).toBe('Unpack finished with 3 frames left in the zip');
    expect(unpackSummary({ unpacked: 0, adopted: 0, conflicts: 0 }, 'failed').title).toBe('Unpack failed');
  });
  it('lists counts for the dialog', () => {
    expect(unpackResultLine({ unpacked: 5, adopted: 1, conflicts: ['x: y'] })).toBe('5 unpacked, 1 already in place, 1 left in the zip');
  });
});
