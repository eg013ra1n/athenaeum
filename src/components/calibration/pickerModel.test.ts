import { describe, expect, it } from 'vitest';
import type { CalibrationSetWithScore, ParameterVerdict } from '../../types/models';
import { describeDifference, formatSeconds, tidyNumbers, geometryLabel, geometryRefuses, geometryWarns, levelBadge, levelChoiceNote, levelNotesFor, originChoiceNote, originNotesFor, sizeCellState, writtenSubSlot, type SlotKind } from './pickerModel';

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
  it('a set subject writes one slot, and only that slot is named (G-7)', () => {
    const cand = (id: number) =>
      ({ set: { id }, parameters: [verdict('warning')] }) as unknown as CalibrationSetWithScore;
    const none = { flat: null, darkflat: null, dark: null, bias: null } as Record<SlotKind, number | null>;
    const candidates = {
      flat: [], darkflat: [cand(7)], dark: [cand(9)], bias: [],
    } as Record<SlotKind, CalibrationSetWithScore[]>;
    const slots: SlotKind[] = ['darkflat', 'dark', 'bias'];
    const picks = { darkflat: 7, dark: 9, bias: null };
    expect(writtenSubSlot(picks)).toBe('darkflat');
    expect(writtenSubSlot({ darkflat: null, dark: 9 })).toBe('dark');
    expect(writtenSubSlot({ darkflat: null, dark: null })).toBeNull();
    // dark #9 is chosen too but the save never writes it: no note for it
    const notes = originNotesFor('set', slots, picks, none, candidates);
    expect(notes).toHaveLength(1);
    expect(notes[0]).toContain('Set #7');
    // lights write every chosen slot
    expect(originNotesFor('lights', slots, picks, none, candidates)).toHaveLength(2);
    // an unchanged link is not "newly chosen"
    expect(originNotesFor('set', slots, picks, { ...none, darkflat: 7 }, candidates)).toEqual([]);
  });
});

describe('exposure and parameter numbers read like a person writes them', () => {
  it('formatSeconds drops float32 noise and trailing zeros', () => {
    expect(formatSeconds(6.71999979019165)).toBe('6.72 s');
    expect(formatSeconds(180)).toBe('180 s');
    expect(formatSeconds(0.5)).toBe('0.5 s');
  });
  it('tidyNumbers rounds long decimals inside a value string only', () => {
    expect(tidyNumbers('6.71999979019165')).toBe('6.72');
    expect(tidyNumbers('9576×6388')).toBe('9576×6388');
    expect(tidyNumbers('-9.8')).toBe('-9.8');
  });
});

describe('gain and offset of a subtracted set', () => {
  const pair = (name: 'gain' | 'offset', frame: string | null, set: string | null, enforced = true): ParameterVerdict => ({
    name, enforced, status: 'match', frameValue: frame, setValue: set,
    diff: null, warningThreshold: null, matchingThreshold: null,
  });
  const cand = (
    imagetyp: string,
    gain: [string | null, string | null],
    offset: [string | null, string | null],
    enforced = true,
    id = 1843,
  ) =>
    ({
      set: { id, imagetyp },
      parameters: [pair('gain', gain[0], gain[1], enforced), pair('offset', offset[0], offset[1], enforced)],
    }) as unknown as CalibrationSetWithScore;

  it('names a dark that declares no offset', () => {
    const c = cand('MasterDark', ['100', '100'], ['1', null]);
    expect(levelChoiceNote(c)).toBe(
      'Set #1843: Offset 1 on the lights, the dark declares none — its pedestal may not match',
    );
    expect(levelBadge(c)).toBe('Gain/offset unknown');
  });
  it('says which way another offset goes', () => {
    const higher = cand('MasterDark', ['100', '100'], ['30', '200']);
    expect(levelChoiceNote(higher)).toContain(
      "Offset 30 on the lights, 200 on the dark — the dark's offset is higher, so its pedestal will be over-subtracted",
    );
    expect(levelBadge(higher)).toBe('Another offset');
    const lower = cand('MasterBias', ['100', '100'], ['200', '30']);
    expect(levelChoiceNote(lower)).toContain("the bias's offset is lower, so its pedestal will be under-subtracted");
  });
  it('says another gain, and both together', () => {
    const gain = cand('DarkFlat', ['100', '200'], ['30', '30']);
    expect(levelChoiceNote(gain)).toBe(
      'Set #1843: Gain 100 on the lights, 200 on the dark flat — the dark flat was shot at another gain',
    );
    expect(levelBadge(gain)).toBe('Another gain');
    const both = cand('MasterDark', ['100', '200'], ['30', '200']);
    expect(levelBadge(both)).toBe('Another gain and offset');
    expect(levelChoiceNote(both)?.split('; ')).toHaveLength(2);
  });
  it('is value-based: the same note when the config ignores gain and offset', () => {
    const enforced = cand('MasterDark', ['100', '100'], ['30', '200'], true);
    const ignored = cand('MasterDark', ['100', '100'], ['30', '200'], false);
    expect(levelChoiceNote(ignored)).toBe(levelChoiceNote(enforced));
    expect(levelChoiceNote(ignored)).not.toBeNull();
  });
  it('is null when gain and offset are known and equal (within 0.01)', () => {
    expect(levelChoiceNote(cand('MasterDark', ['100', '100'], ['30', '30.004']))).toBeNull();
    expect(levelBadge(cand('MasterDark', ['100', '100'], ['30', '30']))).toBeNull();
  });
  it('applies only to a subtracted set, never to a flat or a light', () => {
    for (const t of ['Flat', 'MasterFlat', 'Light', 'MasterLight']) {
      expect(levelChoiceNote(cand(t, ['100', '200'], ['30', null]))).toBeNull();
      expect(levelBadge(cand(t, ['100', '200'], ['30', null]))).toBeNull();
    }
    for (const t of ['Dark', 'MasterDark', 'Bias', 'MasterBias', 'DarkFlat', 'MasterDarkFlat']) {
      expect(levelChoiceNote(cand(t, ['100', '100'], ['1', null]))).not.toBeNull();
    }
  });
  it('has nothing to compare when the lights declare no value or the verdict is absent', () => {
    expect(levelChoiceNote(cand('MasterDark', [null, '100'], [null, '30']))).toBeNull();
    expect(levelChoiceNote({ set: { id: 1, imagetyp: 'MasterDark' }, parameters: [] } as unknown as CalibrationSetWithScore)).toBeNull();
  });
  it('names only newly chosen sets, with the same slot rules as the origin notes', () => {
    const none = { flat: null, darkflat: null, dark: null, bias: null } as Record<SlotKind, number | null>;
    const candidates = {
      flat: [],
      darkflat: [cand('MasterDarkFlat', ['100', '100'], ['1', null], true, 7)],
      dark: [cand('MasterDark', ['100', '100'], ['30', '200'], true, 9)],
      bias: [],
    } as Record<SlotKind, CalibrationSetWithScore[]>;
    const slots: SlotKind[] = ['darkflat', 'dark', 'bias'];
    const picks = { darkflat: 7, dark: 9, bias: null };
    const notes = levelNotesFor('set', slots, picks, none, candidates);
    expect(notes).toHaveLength(1);
    expect(notes[0]).toMatch(/^Dark flats: Set #7: /);
    expect(levelNotesFor('lights', slots, picks, none, candidates)).toHaveLength(2);
    expect(levelNotesFor('set', slots, picks, { ...none, darkflat: 7 }, candidates)).toEqual([]);
  });
});
