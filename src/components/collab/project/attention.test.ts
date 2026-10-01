import { describe, expect, it, vi } from 'vitest';
import { deriveAttention, offlineFor } from './attention';

const NOW = Date.parse('2026-09-29T10:00:00Z');
const held = (kind: string | string[], night = '2026-09-26', filter = 'Ha') => ({ segment: 'held', failures: (Array.isArray(kind) ? kind : [kind]).map((k) => ({ kind: k, text: k })), night, filter, filterMapped: true }) as never;
const pub = (p: Partial<Record<string, unknown>>) => ({ segment: 'published', failures: [], holdersTotal: 1, localState: 'own_held', byteSize: 1e9, pubState: 'published', accepted: null, ...p }) as never;

describe('deriveAttention', () => {
  it('one row per held-back cause in blocker order, with the mockup copy', () => {
    const items = deriveAttention({ own: [held('solve'), held('solve'), held('mapFilter', '2026-09-25', 'S2 6nm')], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items.map((i) => i.key)).toEqual(['solve', 'mapFilter']);
    expect(items[0]).toMatchObject({ count: 2, tone: 'warn', title: '2 frames from 2026-09-26 are not plate-solved', detail: 'They cannot be published until solved.', action: 'Review', target: { kind: 'segment', segment: 'held', state: 'solve' } });
    expect(items[1]).toMatchObject({ title: '1 frame with an unmapped filter “S2 6nm”', action: 'Map', target: { kind: 'map' } });
  });
  it('one-copy and not-on-disk rows for own published frames', () => {
    const items = deriveAttention({ own: [pub({ holdersTotal: 0, byteSize: 3.9e9 }), pub({ localState: 'own_missing', holdersTotal: 2 })], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items.find((i) => i.key === 'single')).toMatchObject({ tone: 'err', title: '1 published frame exists in one copy only · 3.9 GB', target: { kind: 'segment', segment: 'published', state: 'single' } });
    expect(items.find((i) => i.key === 'disk')).toMatchObject({ tone: 'err', target: { kind: 'segment', segment: 'published', state: 'disk' } });
  });
  it('missing-here row names the offline publishers and how long', () => {
    const library = [{ own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a-irina', publisher: 'Irina' }] as never;
    const members = [{ accountId: 'a-irina', displayName: 'Irina', online: false, lastSeenAt: '2026-09-28T10:00:00Z' }] as never;
    const items = deriveAttention({ own: [], library, members, canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'missing', title: '1 frame missing here because its holders are offline', detail: 'Irina offline 1 day.', target: { kind: 'tab', tab: 'library', state: 'missing' } });
  });
  it('live exchange off: the missing-here row keeps its count but says the exchange is off, never blaming holders', () => {
    const library = [
      { own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a-irina', publisher: 'Irina' },
      { own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a-pavel', publisher: 'Pavel' },
    ] as never;
    const members = [{ accountId: 'a-irina', displayName: 'Irina', online: false, lastSeenAt: '2026-09-28T10:00:00Z' }] as never;
    const off = deriveAttention({ own: [], library, members, canModerate: false, canReceive: true, liveRunning: false, pending: 0, now: NOW })[0];
    expect(off).toMatchObject({ key: 'missing', tone: 'err', count: 2, title: '2 frames missing here', detail: 'This device is not connected to the live exchange — they download once it is.', action: 'Show', target: { kind: 'tab', tab: 'library', state: 'missing' } });
    expect(`${off.title} ${off.detail}`).not.toMatch(/holders|offline/);
    // Running: the same frames blame their offline holders, same count.
    const on = deriveAttention({ own: [], library, members, canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW })[0];
    expect(on).toMatchObject({ count: 2, title: '2 frames missing here because their holders are offline', detail: 'Irina offline 1 day.' });
  });
  it('approval row only for a moderator, naming first publishers', () => {
    const library = [{ own: false, state: 'pending', publisher: 'Irina' }, { own: false, state: 'pending', publisher: 'Pavel' }] as never;
    expect(deriveAttention({ own: [], library, members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 2, now: NOW })).toEqual([]);
    expect(deriveAttention({ own: [], library, members: [], canModerate: true, canReceive: true, liveRunning: true, pending: 2, now: NOW })[0]).toMatchObject({ title: '2 frames wait for your approval', detail: 'First publications by Irina and Pavel.', action: 'Moderate' });
  });
  it('empty input → no rows (review focus 4)', () => {
    expect(deriveAttention({ own: [], library: [], members: [], canModerate: true, canReceive: true, liveRunning: true, pending: 0, now: NOW })).toEqual([]);
  });
  it('a frame with two blockers counts in both rows, like the Reason facet', () => {
    const items = deriveAttention({ own: [held(['linkCalibration', 'attest'])], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items.map((i) => i.key)).toEqual(['linkCalibration', 'attest']);
    expect(items.every((i) => i.count === 1)).toBe(true);
  });
  it('analyze row says Review; empty raw filter reads "no FILTER header"', () => {
    const items = deriveAttention({ own: [{ ...(held('mapFilter') as object), filter: '' } as never, held('analyze')], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items.find((i) => i.key === 'analyze')).toMatchObject({ action: 'Review' });
    expect(items.find((i) => i.key === 'mapFilter')?.title).toBe('1 frame with no FILTER header');
  });
  it('an unknown blocker kind still gets a row and one console.error', () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const items = deriveAttention({ own: [held('weird'), held('weird')], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'weird', count: 2, title: '2 frames held back: weird', detail: null, action: 'Review', target: { kind: 'segment', segment: 'held', state: 'weird' } });
    expect(err).toHaveBeenCalledTimes(1);
    err.mockRestore();
  });
  it('no missing row without canReceive', () => {
    const library = [{ own: false, state: 'published', localState: 'wanted', holdersOnline: 0, publisherAccountId: 'a', publisher: 'I' }] as never;
    expect(deriveAttention({ own: [], library, members: [], canModerate: false, canReceive: false, liveRunning: true, pending: 0, now: NOW })).toEqual([]);
  });
  it('approval detail names only other members', () => {
    const library = [{ own: true, state: 'pending', publisher: 'Me' }, { own: false, state: 'pending', publisher: 'Irina' }] as never;
    expect(deriveAttention({ own: [], library, members: [], canModerate: true, canReceive: true, liveRunning: true, pending: 2, now: NOW })[0].detail).toBe('First publications by Irina.');
  });
  it('offlineFor buckets hours and days', () => {
    expect(offlineFor('2026-09-29T05:00:00Z', NOW)).toBe('5 h');
    expect(offlineFor('2026-09-26T10:00:00Z', NOW)).toBe('3 days');
    expect(offlineFor(null, NOW)).toBe('a while');
  });

  it('withheld and blackHole held kinds get their own items, never the unknown-kind error', () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const items = deriveAttention({
      own: [held('withheld'), held('blackHole')],
      library: [], members: [], canModerate: false, canReceive: false, liveRunning: true, pending: 0, now: 0,
    });
    expect(items.map((i) => i.target)).toEqual(
      expect.arrayContaining([
        { kind: 'segment', segment: 'held', state: 'withheld' },
        { kind: 'segment', segment: 'held', state: 'blackHole' },
      ]),
    );
    expect(err).not.toHaveBeenCalled();
    err.mockRestore();
  });
  it('N prepared frames add a To review item first', () => {
    const rev = { segment: 'review', failures: [] } as never;
    const items = deriveAttention({ own: [rev, rev, held('solve')], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'review', tone: 'warn', count: 2, title: '2 calibrated frames wait for your review', target: { kind: 'segment', segment: 'review' } });
  });
  it('one prepared frame reads in the singular', () => {
    const rev = { segment: 'review', failures: [] } as never;
    const items = deriveAttention({ own: [rev], library: [], members: [], canModerate: false, canReceive: true, liveRunning: true, pending: 0, now: NOW });
    expect(items[0]).toMatchObject({ key: 'review', count: 1, title: '1 calibrated frame waits for your review' });
  });
});
