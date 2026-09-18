import { describe, expect, it } from 'vitest';
import { boolCodec, enumCodec, floatCodec, intCodec, stringCodec } from './codecs';

describe('boolCodec', () => {
  it('parses a valid value', () => {
    expect(boolCodec.parse('true')).toBe(true);
    expect(boolCodec.parse('false')).toBe(false);
    expect(boolCodec.format(true)).toBe('true');
  });
  it('rejects an invalid value', () => {
    const result = boolCodec.parse('yes');
    expect(result).toBeInstanceOf(Error);
  });
});

describe('intCodec', () => {
  const codec = intCodec(1, 32);
  it('parses a valid value', () => {
    expect(codec.parse('12')).toBe(12);
    expect(codec.format(12)).toBe('12');
  });
  it('rejects an invalid value', () => {
    expect(codec.parse('99')).toBeInstanceOf(Error);
    expect(codec.parse('1.5')).toBeInstanceOf(Error);
    expect(codec.parse('')).toBeInstanceOf(Error);
  });
});

describe('floatCodec', () => {
  const codec = floatCodec(0, 180);
  it('parses a valid value', () => {
    expect(codec.parse('3.5')).toBe(3.5);
    expect(codec.format(3.5)).toBe('3.5');
  });
  it('rejects an invalid value', () => {
    expect(codec.parse('200')).toBeInstanceOf(Error);
    expect(codec.parse('not a number')).toBeInstanceOf(Error);
  });
});

describe('stringCodec', () => {
  const codec = stringCodec(5);
  it('parses a valid value', () => {
    expect(codec.parse('abc')).toBe('abc');
    expect(codec.format('abc')).toBe('abc');
  });
  it('rejects an invalid value', () => {
    expect(codec.parse('too long')).toBeInstanceOf(Error);
  });
});

describe('enumCodec', () => {
  const codec = enumCodec(['deg', 'arcmin', 'arcsec'] as const);
  it('parses a valid value', () => {
    expect(codec.parse('deg')).toBe('deg');
    expect(codec.format('arcmin')).toBe('arcmin');
  });
  it('rejects an invalid value', () => {
    expect(codec.parse('lightyears')).toBeInstanceOf(Error);
  });
});
