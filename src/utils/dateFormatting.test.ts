import { describe, expect, it } from 'vitest';
import { formatTimestamp } from './dateFormatting';

describe('formatTimestamp', () => {
  const local = new Date(2026, 8, 28, 22, 54, 56).toISOString();

  it('is YYYY-MM-DD HH:MM:SS, seconds always', () => {
    expect(formatTimestamp(local)).toBe('2026-09-28 22:54:56');
  });

  it('returns unparseable input unchanged', () => {
    expect(formatTimestamp('not a date')).toBe('not a date');
  });
});
