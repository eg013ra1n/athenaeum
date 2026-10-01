import { describe, expect, it } from 'vitest';
import { formatDuration, formatDurationPadded, formatRate, formatRelative, formatSize, pluralize } from './format';

describe('pluralize', () => {
  it('picks the singular at exactly 1, the plural (default +s) otherwise', () => {
    expect(pluralize(1, 'member')).toBe('member');
    expect(pluralize(0, 'member')).toBe('members');
    expect(pluralize(2, 'member')).toBe('members');
  });
  it('accepts an explicit irregular plural', () => {
    expect(pluralize(1, 'copy', 'copies')).toBe('copy');
    expect(pluralize(3, 'copy', 'copies')).toBe('copies');
  });
});

describe('formatDuration', () => {
  it('0 seconds is 0m (not 0s)', () => {
    expect(formatDuration(0)).toBe('0m');
  });
  it('under a minute is seconds', () => {
    expect(formatDuration(45)).toBe('45s');
  });
  it('hours and minutes drop a zero part', () => {
    expect(formatDuration(5400)).toBe('1h 30m');
    expect(formatDuration(360000)).toBe('100h');
    expect(formatDuration(45 * 60)).toBe('45m');
    expect(formatDuration(7 * 3600)).toBe('7h');
  });
});

describe('formatRate', () => {
  it('formats bytes/s across scales', () => {
    expect(formatRate(0)).toBe('0 B/s');
    expect(formatRate(972800)).toBe('950 KB/s');
    expect(formatRate(32505856)).toBe('31.0 MB/s');
  });
  it('formats GB/s at the top scale', () => {
    expect(formatRate(2 * 1073741824)).toBe('2.00 GB/s');
  });
  it('non-finite or negative renders an em dash', () => {
    expect(formatRate(-1)).toBe('—');
    expect(formatRate(Infinity)).toBe('—');
    expect(formatRate(NaN)).toBe('—');
  });
});

describe('formatRelative', () => {
  const now = Date.parse('2026-09-30T12:00:00Z');
  it('buckets by elapsed time', () => {
    expect(formatRelative(new Date(now - 10_000).toISOString(), now)).toBe('just now');
    expect(formatRelative(new Date(now - 5 * 60_000).toISOString(), now)).toBe('5 min ago');
    expect(formatRelative(new Date(now - 3 * 3_600_000).toISOString(), now)).toBe('3 h ago');
    expect(formatRelative(new Date(now - 3 * 86_400_000).toISOString(), now)).toBe('3 days ago');
    expect(formatRelative(new Date(now - 86_400_000).toISOString(), now)).toBe('1 day ago');
  });
  it('an unparseable iso renders an em dash', () => {
    expect(formatRelative('not-a-date', now)).toBe('—');
  });
});

describe('mockup formatters', () => {
  it('formatSize promotes to the next unit when rounding reaches 1000', () => {
    expect(formatSize(999_499)).toBe('999 KB');
    expect(formatSize(999_500)).toBe('1 MB');
    expect(formatSize(999_499_999)).toBe('999 MB');
    expect(formatSize(999_500_000)).toBe('1.0 GB');
    expect(formatSize(999_940_000_000)).toBe('999.9 GB');
    expect(formatSize(999_950_000_000)).toBe('1.00 TB');
    expect(formatSize(1_000_000)).toBe('1 MB');
    expect(formatSize(1_000_000_000)).toBe('1.0 GB');
  });

  it('formatSize uses decimal units like the mockup', () => {
    expect(formatSize(121_920_698)).toBe('122 MB');
    expect(formatSize(4_900_000_000)).toBe('4.9 GB');
    expect(formatSize(1_250_000_000_000)).toBe('1.25 TB');
    expect(formatSize(950_000)).toBe('950 KB');
  });
  it('formatDurationPadded pads minutes after hours', () => {
    expect(formatDurationPadded(3 * 3600 + 2 * 60)).toBe('3h 02m');
    expect(formatDurationPadded(18 * 60)).toBe('18m');
    expect(formatDurationPadded(45 * 3600 + 57 * 60)).toBe('45h 57m');
    expect(formatDurationPadded(0)).toBe('0m');
  });
  it('rounds to total minutes before splitting (never prints 60m)', () => {
    expect(formatDurationPadded(7170)).toBe('2h 00m');
    expect(formatDurationPadded(3599)).toBe('1h 00m');
    expect(formatDurationPadded(3 * 3600 + 3590)).toBe('4h 00m');
    expect(formatDurationPadded(29)).toBe('0m');
    expect(formatDurationPadded(30)).toBe('1m');
  });
});
