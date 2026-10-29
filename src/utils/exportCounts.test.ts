import { describe, expect, it } from 'vitest';
import { exportCountsText, zipCopiedText } from './exportCounts';

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
