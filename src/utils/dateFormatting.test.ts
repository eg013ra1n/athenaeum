import { describe, expect, it } from 'vitest';
import { formatTimestamp } from './dateFormatting';

describe('formatTimestamp', () => {
  const local = new Date(2026, 8, 28, 22, 54, 56).toISOString();

  it('default form is YYYY-MM-DD HH:MM', () => {
    expect(formatTimestamp(local)).toBe('2026-09-28 22:54');
  });

  it('seconds option appends :SS', () => {
    expect(formatTimestamp(local, { seconds: true })).toBe('2026-09-28 22:54:56');
  });

  it('returns unparseable input unchanged in both forms', () => {
    expect(formatTimestamp('not a date')).toBe('not a date');
    expect(formatTimestamp('not a date', { seconds: true })).toBe('not a date');
  });
});
