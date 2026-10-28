import { describe, expect, it } from 'vitest';
import { findZipInPath, innerParent, zipCandidates, zipCrumbs, zipLocationLabel, zipUpLabel } from './zipView';

describe('zipView helpers', () => {
  it('splits a virtual path into the zip and the entry folder', () => {
    expect(findZipInPath('/a/night.zip/Lights/Ha/L_1.fits')).toEqual({ zipPath: '/a/night.zip', innerDir: 'Lights/Ha' });
    expect(findZipInPath('/a/night.zip/top.fits')).toEqual({ zipPath: '/a/night.zip', innerDir: '' });
    expect(findZipInPath('C:\\a\\Night.ZIP\\Lights\\L_1.fits')).toEqual({ zipPath: 'C:\\a\\Night.ZIP', innerDir: 'Lights' });
    expect(findZipInPath('/a/night.zip')).toBeNull();
    expect(findZipInPath('/a/b/L_1.fits')).toBeNull();
  });

  it('walks up one inner folder at a time and leaves the zip at its top', () => {
    expect(innerParent('Lights/Ha')).toBe('Lights');
    expect(innerParent('Lights')).toBe('');
    expect(innerParent('')).toBeNull();
    const z = { path: '/a/night.zip', innerDir: 'Lights/Ha' };
    expect(zipLocationLabel(z)).toBe('night.zip/Lights/Ha');
    expect(zipCrumbs(z).map((c) => c.innerDir)).toEqual(['', 'Lights', 'Lights/Ha']);
    expect(zipUpLabel(z, '/a')).toBe('night.zip/Lights');
    expect(zipUpLabel({ path: z.path, innerDir: 'Lights' }, '/a')).toBe('night.zip');
    expect(zipUpLabel({ path: z.path, innerDir: '' }, '/a')).toBe('/a');
  });
});

describe('zipCandidates', () => {
  it('offers every .zip component, leftmost first, so a folder named x.zip does not hide the real zip', () => {
    expect(zipCandidates('/a/old.zip/night.zip/L/x.fits')).toEqual([
      { zipPath: '/a/old.zip', innerDir: 'night.zip/L' },
      { zipPath: '/a/old.zip/night.zip', innerDir: 'L' },
    ]);
    expect(zipCandidates('/a/b/L.fits')).toEqual([]);
    expect(zipCandidates('/a/night.zip')).toEqual([]);
  });
});
