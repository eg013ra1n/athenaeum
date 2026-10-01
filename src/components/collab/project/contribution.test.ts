import { describe, expect, it } from 'vitest';
import type { OwnFrameRow, RuleVerdict } from '../../../types/models';
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
      row({ segment: 'published', pubState: 'pending', accepted: true, calibratedBytes: null, byteSize: 50 }),
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

  const rule = (metricKey: string, label: string, pass: boolean | null): RuleVerdict =>
    ({ metricKey, label, value: null, needs: '', pass });
  const threshold = (rules: RuleVerdict[]) =>
    row({ segment: 'held', failures: [{ kind: 'threshold', text: 'fails' }], rules });

  it('a threshold frame is named by its first failing rule (spec §7.2 example)', () => {
    const t = contributionTiles([
      ...Array.from({ length: 3 }, () => threshold([rule('fwhm_arcsec', 'FWHM', false), rule('not_trailed', 'trailed', false)])),
      ...Array.from({ length: 2 }, () => threshold([rule('fwhm_arcsec', 'FWHM', true), rule('not_trailed', 'trailed', false)])),
      row({ segment: 'held', failures: [{ kind: 'withheld', text: 'Withheld by you' }] }),
    ]);
    expect(t[3].footer).toBe('3 FWHM · 2 trailed · 1 withheld by you');
  });

  it('a threshold frame with no failing rule entry falls back to the kind label', () => {
    const t = contributionTiles([threshold([rule('fwhm_arcsec', 'FWHM', true), rule('stars_detected', 'stars', null)])]);
    expect(t[3].footer).toBe('1 quality thresholds');
  });

  it('count ties break by the held-kind order, then by label', () => {
    const t = contributionTiles([
      row({ segment: 'held', failures: [{ kind: 'withheld', text: 'Withheld by you' }] }),
      threshold([rule('fwhm_arcsec', 'FWHM', false)]),
      threshold([rule('eccentricity', 'eccentricity', false)]),
    ]);
    expect(t[3].footer).toBe('1 eccentricity · 1 FWHM · 1 withheld by you');
  });

  it('an unknown held kind sorts after the known ones', () => {
    const t = contributionTiles([
      row({ segment: 'held', failures: [{ kind: 'mystery', text: '?' }] }),
      row({ segment: 'held', failures: [{ kind: 'blackHole', text: 'In the Black Hole' }] }),
    ]);
    expect(t[3].footer).toBe('1 in the Black Hole · 1 mystery');
  });

  it('a Black Hole reason keeps its proper name in the held back footer', () => {
    const t = contributionTiles([
      row({ segment: 'held', failures: [{ kind: 'blackHole', text: 'In the Black Hole' }] }),
      row({ segment: 'held', failures: [{ kind: 'solve', text: 'no coordinates' }] }),
    ]);
    expect(t[3].footer).toBe('1 no coordinates or pixel scale · 1 in the Black Hole');
  });

  it('an empty held back tile says nothing is held back', () => {
    expect(contributionTiles([row({ segment: 'ready' })])[3].footer).toBe('Nothing held back');
  });

  it('a null exptime counts the frame but adds no time', () => {
    const t = contributionTiles([row({ exptimeSec: null })]);
    expect([t[0].count, t[0].seconds]).toEqual([1, 0]);
  });
});
