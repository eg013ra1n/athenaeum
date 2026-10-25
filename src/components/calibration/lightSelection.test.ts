import { describe, expect, it } from 'vitest';
import { nextLightSelection } from './lightSelection';

const keys = ['a', 'b', 'c', 'd', 'e'];
const plain = { shift: false, toggle: false };
const shift = { shift: true, toggle: false };
const toggle = { shift: false, toggle: true };
const sorted = (s: Set<string>) => [...s].sort();

describe('light-group selection', () => {
  it('a plain click picks one group; clicking the only selected one clears it', () => {
    expect(sorted(nextLightSelection(new Set(['a', 'c']), keys, 'b', null, plain))).toEqual(['b']);
    expect(sorted(nextLightSelection(new Set(['b']), keys, 'b', 'b', plain))).toEqual([]);
  });

  it('Cmd/Ctrl-click toggles one group and keeps the rest', () => {
    expect(sorted(nextLightSelection(new Set(['a']), keys, 'c', 'a', toggle))).toEqual(['a', 'c']);
    expect(sorted(nextLightSelection(new Set(['a', 'c']), keys, 'c', 'a', toggle))).toEqual(['a']);
  });

  it('Shift-click adds the range from the anchor, in display order, either direction', () => {
    expect(sorted(nextLightSelection(new Set(['b']), keys, 'd', 'b', shift))).toEqual(['b', 'c', 'd']);
    expect(sorted(nextLightSelection(new Set(['d']), keys, 'b', 'd', shift))).toEqual(['b', 'c', 'd']);
  });

  it('Shift-click on a selected group removes the range', () => {
    expect(sorted(nextLightSelection(new Set(['a', 'b', 'c', 'd']), keys, 'c', 'a', shift))).toEqual(['d']);
  });

  it('Shift-click with no anchor (or a vanished one) toggles the clicked group', () => {
    expect(sorted(nextLightSelection(new Set(['a']), keys, 'c', null, shift))).toEqual(['a', 'c']);
    expect(sorted(nextLightSelection(new Set(['a']), keys, 'c', 'gone', shift))).toEqual(['a', 'c']);
  });
});
