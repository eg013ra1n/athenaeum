import { describe, expect, it } from 'vitest';
import { mergeHistory } from './historyGrouping';

interface Timed {
  at: string;
  id: string;
}

const t = (id: string, at: string): Timed => ({ id, at });

describe('mergeHistory', () => {
  it('orders by time, newest first, interleaving both inputs', () => {
    const a = [t('a1', '2026-09-30T10:00:00Z'), t('a2', '2026-09-30T08:00:00Z')];
    const b = [t('b1', '2026-09-30T09:00:00Z'), t('b2', '2026-09-30T07:00:00Z')];

    const merged = mergeHistory(a, b).map((x) => x.id);

    expect(merged).toEqual(['a1', 'b1', 'a2', 'b2']);
  });

  it('keeps equal-time order — the first list wins the tie', () => {
    const a = [t('a1', '2026-09-30T10:00:00Z')];
    const b = [t('b1', '2026-09-30T10:00:00Z')];

    expect(mergeHistory(a, b).map((x) => x.id)).toEqual(['a1', 'b1']);
  });

  it('does not reorder equal-time elements within the same input', () => {
    const a = [t('a1', '2026-09-30T10:00:00Z'), t('a2', '2026-09-30T10:00:00Z')];
    const b: Timed[] = [];

    expect(mergeHistory(a, b).map((x) => x.id)).toEqual(['a1', 'a2']);
  });

  it('drains the remainder of the longer list', () => {
    const a = [t('a1', '2026-09-30T10:00:00Z')];
    const b = [
      t('b1', '2026-09-30T09:00:00Z'),
      t('b2', '2026-09-30T08:00:00Z'),
      t('b3', '2026-09-30T07:00:00Z'),
    ];

    expect(mergeHistory(a, b).map((x) => x.id)).toEqual(['a1', 'b1', 'b2', 'b3']);
  });

  it('handles two empty lists', () => {
    expect(mergeHistory([], [])).toEqual([]);
  });
});
