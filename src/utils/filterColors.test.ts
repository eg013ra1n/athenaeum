import { describe, expect, it } from 'vitest';
import { getFilterColor } from './filterColors';

describe('filter palette (Nord, spec §4.3)', () => {
  it('maps the canonical filters and their aliases', () => {
    expect(getFilterColor('L')).toBe('#e5e9f0');
    expect(getFilterColor('Lum')).toBe('#e5e9f0');
    expect(getFilterColor('R')).toBe('#bf616a');
    expect(getFilterColor('G')).toBe('#a3be8c');
    expect(getFilterColor('B')).toBe('#5e81ac');
    expect(getFilterColor('Ha')).toBe('#d08770');
    expect(getFilterColor('H-alpha')).toBe('#d08770');
    expect(getFilterColor('OIII')).toBe('#88c0d0');
    expect(getFilterColor('O3')).toBe('#88c0d0');
    expect(getFilterColor('SII')).toBe('#b48ead');
    expect(getFilterColor('S2')).toBe('#b48ead');
    expect(getFilterColor('OSC')).toBe('#ebcb8b');
  });
  it('gives an unknown filter a stable colour across calls (review focus 2)', () => {
    const a = getFilterColor('L-eXtreme');
    expect(getFilterColor('L-eXtreme')).toBe(a);
    expect(getFilterColor('CLS')).not.toBe('');
  });
});
