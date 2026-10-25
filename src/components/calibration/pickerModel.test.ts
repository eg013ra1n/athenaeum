import { describe, expect, it } from 'vitest';
import type { CalibrationSetWithScore, ParameterVerdict } from '../../types/models';
import { describeDifference, geometryLabel, geometryRefuses, geometryWarns, originChoiceNote, sizeCellState } from './pickerModel';

const verdict = (status: ParameterVerdict['status']): ParameterVerdict => ({
  name: 'geometry', enforced: true, status,
  frameValue: '9576×6388', setValue: '6384×4258 @ 1584,1064',
  diff: null, warningThreshold: null, matchingThreshold: null,
});

describe('geometry in the picker', () => {
  it('spells the size like the core, origin when known', () => {
    expect(geometryLabel({ naxis1: 9576, naxis2: 6388, xorgsubf: null, yorgsubf: null })).toBe('9576×6388');
    expect(geometryLabel({ naxis1: 6384, naxis2: 4258, xorgsubf: 1584, yorgsubf: 1064 })).toBe('6384×4258 @ 1584,1064');
    expect(geometryLabel({ naxis1: null, naxis2: null, xorgsubf: null, yorgsubf: null })).toBeNull();
  });
  it('a geometry mismatch refuses; an unknown size does not', () => {
    expect(geometryRefuses([verdict('mismatch')])).toBe(true);
    expect(geometryRefuses([verdict('unknown')])).toBe(false);
    expect(geometryRefuses([{ ...verdict('mismatch'), name: 'gain' }])).toBe(false);
  });
  it('reads the difference as needed → offered', () => {
    expect(describeDifference(verdict('mismatch'))).toEqual({
      label: 'Frame size', change: '9576×6388 → 6384×4258 @ 1584,1064', limit: null,
    });
  });
  it('another sub-frame origin warns and never refuses (G-7)', () => {
    expect(geometryRefuses([verdict('warning')])).toBe(false);
    expect(geometryWarns([verdict('warning')])).toBe(true);
    expect(geometryWarns([verdict('mismatch')])).toBe(false);
    expect(describeDifference(verdict('warning')).label).toBe('Sub-frame origin');
  });
  it('the hierarchy size cell: match, size misfit, origin warning, unknown', () => {
    const row = { size: '9576×6388', sizeKey: '9576×6388', cameraSize: '9576×6388', originWarning: null };
    expect(sizeCellState(row)).toEqual({ matches: true, title: 'Frame size' });
    expect(sizeCellState({ ...row, cameraSize: '6384×4258' })).toEqual({ matches: false, title: 'Frame size' });
    expect(sizeCellState({ ...row, originWarning: 'Dark set #5 is … — another sub-frame origin' }))
      .toEqual({ matches: false, title: 'Dark set #5 is … — another sub-frame origin' });
    expect(sizeCellState({ ...row, cameraSize: null })).toEqual({ matches: null, title: 'Frame size' });
  });
  it('names a hand-chosen set at another origin, and only that', () => {
    const cand = (status: ParameterVerdict['status']) =>
      ({ set: { id: 1518 }, parameters: [verdict(status)] }) as unknown as CalibrationSetWithScore;
    expect(originChoiceNote(cand('warning'))).toBe(
      'Set #1518 is 6384×4258 @ 1584,1064, the frames are 9576×6388 — another sub-frame origin',
    );
    expect(originChoiceNote(cand('match'))).toBeNull();
    expect(originChoiceNote(cand('mismatch'))).toBeNull();
  });
});
