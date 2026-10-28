import { describe, expect, it } from 'vitest';
import { extractionLine, isForeignZipFile, isInZip, revealRequest, zipChipLabel, zipLocationOf } from './zipChips';

describe('zipChipLabel', () => {
  it('names all, some and none', () => {
    expect(zipChipLabel(5, 5)).toBe('zip');
    expect(zipChipLabel(2, 5)).toBe('2 of 5 in zip');
    expect(zipChipLabel(0, 5)).toBeNull();
  });
});

describe('extractionLine', () => {
  it('reads files and size, and stays quiet when nothing is extracted', () => {
    expect(extractionLine({ files: 120, bytes: 6.2e9 })).toBe('From zip: 120 files, 6.2 GB');
    expect(extractionLine({ files: 0, bytes: 0 })).toBeNull();
    expect(extractionLine(null)).toBeNull();
  });
  it('uses the singular and megabytes for small batches', () => {
    expect(extractionLine({ files: 1, bytes: 25_000_000 })).toBe('From zip: 1 file, 25 MB');
    expect(extractionLine({ files: 1500, bytes: 1.5e10 })).toBe('From zip: 1,500 files, 15.0 GB');
  });
});

describe('zip markers', () => {
  const foreign = { archive_zip_path: '/z/night.zip', archive_path_in_zip: 'Lights/Ha/L_001.fits', archived_in_operation: null };
  const ours = { ...foreign, archived_in_operation: 7 };
  it('tells foreign zip rows from ours and from disk rows', () => {
    expect(isInZip(foreign)).toBe(true);
    expect(isForeignZipFile(foreign)).toBe(true);
    expect(isForeignZipFile(ours)).toBe(false);
    expect(isInZip({ archive_zip_path: null, archive_path_in_zip: null })).toBe(false);
    expect(isForeignZipFile({})).toBe(false);
  });
  it('gives the zip and the folder of the entry', () => {
    expect(zipLocationOf(foreign)).toEqual({ zipPath: '/z/night.zip', innerDir: 'Lights/Ha' });
    expect(zipLocationOf({ archive_zip_path: '/z/a.zip', archive_path_in_zip: 'L.fits' })).toEqual({ zipPath: '/z/a.zip', innerDir: '' });
    expect(zipLocationOf({ archive_zip_path: null, archive_path_in_zip: null })).toBeNull();
  });
});

describe('revealRequest', () => {
  it('carries the zip of a zipped row and nothing for a disk row', () => {
    const ours = { archive_zip_path: '/arch/op7.zip', archive_path_in_zip: 'Lights/Ha/L1.fits', archived_in_operation: 7 };
    const r = revealRequest('/astro/M31/L1.fits', ours);
    expect(r.path).toBe('/astro/M31/L1.fits');
    expect(r.zip).toEqual({ zipPath: '/arch/op7.zip', innerDir: 'Lights/Ha' });
    expect(revealRequest('/astro/x.fits', {}).zip).toBeUndefined();
  });
});
