import { expect, it } from 'vitest';
import { analysisCountText, NO_UPDATES } from './analysisCount';

const updates = (o: Partial<typeof NO_UPDATES>) => ({ ...NO_UPDATES, ...o });

it('a fully analysed set with nothing to update is settled', () => {
  const t = analysisCountText(30, 30, NO_UPDATES);
  expect(t.settled).toBe(true);
  expect(t.update).toBeNull();
  expect(t.catalogue).toBeNull();
  expect(t.title).toBeNull();
});

it('a fully analysed set with zero points to update shows Analyze and the count, not Re-Analyze', () => {
  const t = analysisCountText(30, 30, updates({ toUpdate: 7 }));
  expect(t.settled).toBe(false);
  expect(t.update).toBe('7 to update');
  expect(t.count).toBe('');
});

it('a partly analysed set keeps its analysed count beside the update count', () => {
  const t = analysisCountText(12, 30, updates({ toUpdate: 3 }));
  expect(t.settled).toBe(false);
  expect(t.count).toBe('12/30');
  expect(t.update).toBe('3 to update');
});

it('no rows at all: not settled, no count, nothing to update', () => {
  const t = analysisCountText(0, 30);
  expect(t.settled).toBe(false);
  expect(t.count).toBe('');
  expect(t.update).toBeNull();
});

it('an empty set is settled', () => {
  expect(analysisCountText(0, 0).settled).toBe(true);
});

it('frames waiting on the catalogue say install the catalogue, never run Analyze', () => {
  const t = analysisCountText(30, 30, updates({ noCatalogue: 12 }));
  expect(t.settled).toBe(true);
  expect(t.update).toBeNull();
  expect(t.catalogue).toBe('Install the catalogue (12 frames)');
  expect(t.catalogue).not.toMatch(/analyze/i);
  expect(t.title).not.toMatch(/analyze/i);
  expect(analysisCountText(30, 30, updates({ noCatalogue: 1 })).catalogue).toBe('Install the catalogue (1 frame)');
});

it('a failed measurement is told in the hover text and does not count as an update', () => {
  const t = analysisCountText(30, 30, updates({ measurementFailed: 2 }));
  expect(t.settled).toBe(true);
  expect(t.update).toBeNull();
  expect(t.title).toBe('the measurement failed on 2 frames — see the log');
});

it('every group has its own line in the hover text', () => {
  const t = analysisCountText(30, 30, updates({ toUpdate: 1, noCatalogue: 2, measurementFailed: 3 }));
  expect(t.title?.split('\n')).toHaveLength(3);
});
