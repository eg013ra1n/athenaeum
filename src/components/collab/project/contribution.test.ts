import { describe, expect, it } from 'vitest';
import type { OwnFrameRow } from '../../../types/models';
import { contributionTiles } from './contribution';

const row = (o: Partial<OwnFrameRow>): OwnFrameRow => ({
  frameId: 1, frameUuid: null, fileName: 'a.fits', setId: 1, setName: 'S', night: '2026-09-28', filter: 'L',
  filterMapped: true, camera: 'C', exptimeSec: 300, byteSize: 100, fwhmArcsec: null, eccentricity: null,
  starsDetected: null, medianSnr: null, segment: 'ready', contributorState: 'notPublished', contributorReason: null,
  failures: [], contentVersion: null, pubState: null, acceptedReason: null, holdersOnline: null, holdersTotal: null,
  localState: null, publishedAt: null, lastError: null, rules: [], path: '/d/a.fits', accepted: null,
  calibratedPath: null, calibratedBytes: null, preparedAt: null, withheld: false, ...o,
});

describe('contributionTiles', () => {
  it('sums hours, nights, filters and size per segment, in segment order', () => {
    const t = contributionTiles([
      row({ segment: 'ready', filter: 'L', night: '2026-09-28' }),
      row({ segment: 'ready', filter: 'R', night: '2026-09-29', exptimeSec: 600 }),
      row({ segment: 'review', filter: 'Ha', calibratedBytes: 400 }),
      row({ segment: 'published', accepted: true, pubState: 'published', calibratedBytes: 500 }),
      row({ segment: 'published', pubState: 'pending', calibratedBytes: null, byteSize: 50 }),
    ]);
    expect(t.map((x) => x.segment)).toEqual(['ready', 'review', 'published', 'held']);
    const [ready, review, published, held] = t;
    expect([ready.count, ready.seconds, ready.nights, ready.bytes]).toEqual([2, 900, 2, 200]);
    expect(ready.filters.map((f) => f.filter)).toEqual(['L', 'R']);
    expect(review.bytes).toBe(400);
    expect(published.bytes).toBe(550);
    expect(published.footer).toBe('1 accepted · 1 pending');
    expect(held.count).toBe(0);
  });

  it('held back lists the top three reasons with counts', () => {
    const t = contributionTiles([
      row({ segment: 'held', failures: [{ kind: 'threshold', text: 'FWHM' }] }),
      row({ segment: 'held', failures: [{ kind: 'threshold', text: 'FWHM' }] }),
      row({ segment: 'held', failures: [{ kind: 'withheld', text: 'Withheld by you' }] }),
      row({ segment: 'held', failures: [{ kind: 'analyze', text: 'no analysis' }] }),
      row({ segment: 'held', failures: [{ kind: 'blackHole', text: 'In the Black Hole' }] }),
    ]);
    expect(t[3].footer).toBe('2 quality thresholds · 1 no analysis · 1 withheld by you');
  });

  it('a null exptime counts the frame but adds no time', () => {
    const t = contributionTiles([row({ exptimeSec: null })]);
    expect([t[0].count, t[0].seconds]).toEqual([1, 0]);
  });
});
