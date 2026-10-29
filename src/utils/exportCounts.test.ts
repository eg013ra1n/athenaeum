import { describe, expect, it } from 'vitest';
import { exportCountsText, linkFallbackText, zipCopiedText } from './exportCounts';

describe('linkFallbackText', () => {
  it('names the hard links and copies Windows made instead of symbolic links', () => {
    expect(linkFallbackText({ hardLinks: 12, copies: 1 })).toBe(
      'Windows did not allow symbolic links: 12 hard links, 1 copy (turn on Developer Mode for symbolic links)',
    );
    expect(linkFallbackText({ hardLinks: 1, copies: 0 })).toBe(
      'Windows did not allow symbolic links: 1 hard link (turn on Developer Mode for symbolic links)',
    );
    expect(linkFallbackText({ hardLinks: 0, copies: 3 })).toBe(
      'Windows did not allow symbolic links: 3 copies (turn on Developer Mode for symbolic links)',
    );
  });

  it('says nothing when every link was a symbolic link', () => {
    expect(linkFallbackText({ hardLinks: 0, copies: 0 })).toBeNull();
    expect(linkFallbackText(undefined)).toBeNull();
  });
});

describe('zipCopiedText', () => {
  it('says how many frames came out of zips, and nothing when none did', () => {
    expect(zipCopiedText(12)).toBe('From zip: 12 copied');
    expect(zipCopiedText(0)).toBeNull();
    expect(zipCopiedText(undefined)).toBeNull();
  });
});

describe('exportCountsText', () => {
  it('lists only the non-zero counts', () => {
    expect(exportCountsText({ linked: 1, copied: 0, rewritten: 2, failed: 0 })).toBe('1 linked · 2 with FILTER rewritten');
  });
});
