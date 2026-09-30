import { describe, expect, it } from 'vitest';
import { deriveAttention, offlineFor } from './attention';

const NOW = Date.parse('2026-09-29T10:00:00Z');
const held = (kind: string, night = '2026-09-26', filter = 'Ha') => ({ segment: 'held', failures: [{ kind, text: kind }], night, filter, filterMapped: kind !== 'mapFilter' }) as never;
const pub = (p: Partial<Record<string, unknown>>) => ({ segment: 'published', failures: [], holdersTotal: 1, localState: 'own_held', byteSize: 1e9, ...p }) as never;

describe('deriveAttention', () => {
  it('one row per held-back cause in blocker order, with the mockup copy', () => {
    const items = deriveAttention({ own: [held('solve'), held('solve'), held('mapFilter', '2026-09-25', 'S2 6nm')], library: [], members: [], canModerate: false, pending: 0, now: NOW });
    expect(items.map((i) => i.key)).toEqual(['solve', 'mapFilter']);
    expect(items[0]).toMatchObject({ count: 2, tone: 'warn', title: '2 frames from 2026-09-26 are not plate-solved', detail: 'They cannot be published until solved.', action: 'Review', target: { kind: 'segment', segment: 'held', state: 'solve' } });
    expect(items[1]).toMatchObject({ title: '1 frame with an unmapped filter “S2 6nm”', action: 'Map' });
  });
  it('one-copy and not-on-disk rows for own published frames', () => {
    const items = deriveAttention({ own: [pub({ holdersTotal: 0, byteSize: 3.9e9 }), pub({ localState: 'own_missing' })], library: [], members: [], canModerate: false, pending: 0, now: NOW });
    expect(items.find((i) => i.key === 'single')).toMatchObject({ tone: 'err', title: '1 published frame exists in one copy only · 3.9 GB', target: { kind: 'segment', segment: 'published', state: 'single' } });
    expect(items.find((i) => i.key === 'disk')).toMatchObject({ tone: 'err', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  });
  it('missing-here row names the offline publishers and how long', () => {
    const library = [{ own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a-irina', publisher: 'Irina' }] as never;
    const members = [{ accountId: 'a-irina', displayName: 'Irina', online: false, lastSeenAt: '2026-09-28T10:00:00Z' }] as never;
    const items = deriveAttention({ own: [], library, members, canModerate: false, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'missing', title: '1 frame missing here because its holders are offline', detail: 'Irina offline 1 day.', target: { kind: 'tab', tab: 'library', state: 'missing' } });
  });
  it('approval row only for a moderator, naming first publishers', () => {
    const library = [{ own: false, state: 'pending', publisher: 'Irina' }, { own: false, state: 'pending', publisher: 'Pavel' }] as never;
    expect(deriveAttention({ own: [], library, members: [], canModerate: false, pending: 2, now: NOW })).toEqual([]);
    expect(deriveAttention({ own: [], library, members: [], canModerate: true, pending: 2, now: NOW })[0]).toMatchObject({ title: '2 frames wait for your approval', detail: 'First publications by Irina and Pavel.', action: 'Moderate' });
  });
  it('empty input → no rows (review focus 4)', () => {
    expect(deriveAttention({ own: [], library: [], members: [], canModerate: true, pending: 0, now: NOW })).toEqual([]);
  });
  it('offlineFor buckets hours and days', () => {
    expect(offlineFor('2026-09-29T05:00:00Z', NOW)).toBe('5 h');
    expect(offlineFor('2026-09-26T10:00:00Z', NOW)).toBe('3 days');
    expect(offlineFor(null, NOW)).toBe('a while');
  });
});
