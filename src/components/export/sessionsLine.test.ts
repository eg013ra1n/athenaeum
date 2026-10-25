import { describe, expect, it } from 'vitest';
import { formatSessions } from './sessionsLine';

describe('formatSessions', () => {
  it('shows the catalog night count, not the days in the span', () => {
    // A year-long span with five imaging nights: "(5 nights)", never "(380 nights)".
    const line = formatSessions(['2024-09-01T12:00:00Z', '2025-09-14T12:00:00Z'], 5);
    expect(line).toMatch(/\(5 nights\)$/);
    expect(line).not.toMatch(/380/);
  });

  it('says "1 night" for a single night that crosses midnight', () => {
    expect(formatSessions(['2025-06-28T21:00:00Z', '2025-06-29T03:00:00Z'], 1)).toMatch(/\(1 night\)$/);
  });

  it('leaves the count off when there is none', () => {
    expect(formatSessions(['2025-06-28T21:00:00Z', '2025-06-28T22:00:00Z'], 0)).not.toMatch(/night/);
  });
});
