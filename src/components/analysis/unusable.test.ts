import { describe, it, expect } from 'vitest';
import type { FrameAnalysis } from '../../types/models';
import { isUnusable, verdictLabel, verdictSeverity } from './unusable';

function row(over: Partial<FrameAnalysis>): FrameAnalysis {
  return { measure_version: 5, frame_verdict: null, possibly_trailed: false, ...over } as FrameAnalysis;
}

describe('isUnusable', () => {
  it('any verdict is unusable', () => {
    expect(isUnusable(row({ frame_verdict: 'no_stars' }))).toBe(true);
    expect(isUnusable(row({ frame_verdict: 'defocused' }))).toBe(true);
  });
  it('a current row without a verdict is usable, whatever the old flag says', () => {
    expect(isUnusable(row({}))).toBe(false);
    expect(isUnusable(row({ possibly_trailed: true }))).toBe(false);
  });
  it('a legacy row follows its possibly_trailed', () => {
    expect(isUnusable(row({ measure_version: null, possibly_trailed: true }))).toBe(true);
    expect(isUnusable(row({ measure_version: null, possibly_trailed: false }))).toBe(false);
  });
});

describe('verdictLabel', () => {
  it('words each verdict', () => {
    expect(verdictLabel('trailed')).toBe('Trailed');
    expect(verdictLabel('defocused')).toBe('Defocused');
    expect(verdictLabel('no_stars')).toBe('No stars');
    expect(verdictLabel(null)).toBe('—');
  });
});

describe('verdictSeverity', () => {
  it('orders none < No stars < Defocused < Trailed', () => {
    expect(verdictSeverity(null)).toBeLessThan(verdictSeverity('no_stars'));
    expect(verdictSeverity('no_stars')).toBeLessThan(verdictSeverity('defocused'));
    expect(verdictSeverity('defocused')).toBeLessThan(verdictSeverity('trailed'));
  });
});
