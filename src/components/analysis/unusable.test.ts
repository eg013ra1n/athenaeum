import { describe, it, expect } from 'vitest';
import type { FrameAnalysis } from '../../types/models';
import { isUnusable, rowVerdict, verdictLabel, verdictSeverity } from './unusable';

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

describe('rowVerdict', () => {
  it('a legacy possibly_trailed row reads Trailed (legacy), warning, with the re-analyze tooltip and Trailed rank', () => {
    const v = rowVerdict(row({ measure_version: null, possibly_trailed: true }));
    expect(v.label).toBe('Trailed (legacy)');
    expect(v.className).toBe('text-warning');
    expect(v.tooltip).toBe('The previous analysis flagged this frame trailed; re-analyze it for a verdict');
    expect(v.rank).toBe(verdictSeverity('trailed'));
  });
  it('a legacy row not flagged trailed reads —', () => {
    const v = rowVerdict(row({ measure_version: null, possibly_trailed: false }));
    expect(v.label).toBe('—');
    expect(v.tooltip).toBeUndefined();
    expect(v.rank).toBe(0);
  });
  it('a current row shows its verdict, whatever the old flag says', () => {
    expect(rowVerdict(row({ frame_verdict: 'defocused' })).label).toBe('Defocused');
    expect(rowVerdict(row({ frame_verdict: 'defocused' })).rank).toBe(verdictSeverity('defocused'));
    expect(rowVerdict(row({ frame_verdict: 'no_stars' })).label).toBe('No stars');
    expect(rowVerdict(row({ frame_verdict: 'no_stars' })).tooltip).toBe(
      "The star measurement could not fit this frame's stars: it found no star to fit",
    );
    expect(rowVerdict(row({ possibly_trailed: true })).label).toBe('—');
  });
  it('a verdict wins over the legacy flag', () => {
    expect(rowVerdict(row({ measure_version: null, possibly_trailed: true, frame_verdict: 'trailed' })).label).toBe('Trailed');
  });
});
