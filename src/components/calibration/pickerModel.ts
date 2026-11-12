import type {
  CalibrationSetDetail,
  CalibrationSetParameters,
  CalibrationSetWithScore,
  LightFrameParameters,
  ParameterVerdict,
} from '../../types/models';

/**
 * What calibration is being chosen FOR.
 *
 * The two flows the picker replaces asked the same question of different
 * subjects: light frames need flats/darks/bias, and a calibration set needs
 * its own sub-calibration. Everything downstream — the slots, the summary,
 * the candidate query, the save — follows from this one value.
 */
export type PickerSubject =
  | {
      kind: 'lights';
      frameIds: number[];
      /** Filter name for the title, e.g. "Ha" or "No Filter". */
      label: string;
      current: { flat: number | null; dark: number | null; bias: number | null };
      /** Bias is only a slot for lights when dark optimization is on. */
      useBiasForDarkOptimization: boolean;
    }
  | { kind: 'set'; setId: number; sourceType: 'flat' | 'dark' };

/** A calibration kind the subject can be given. */
export type SlotKind = 'flat' | 'darkflat' | 'dark' | 'bias';

export const SLOT_LABEL: Record<SlotKind, string> = {
  flat: 'Flats',
  darkflat: 'Dark flats',
  dark: 'Darks',
  bias: 'Bias',
};

/** Which slots this subject has, in the order the engine prefers them. */
export function slotsFor(subject: PickerSubject): SlotKind[] {
  if (subject.kind === 'lights') {
    return subject.useBiasForDarkOptimization
      ? ['flat', 'dark', 'bias']
      : ['flat', 'dark'];
  }
  // A flat is calibrated by its own dark flat, falling back to a dark and
  // then a bias — the same chain the auto-linker walks. A dark takes a bias.
  return subject.sourceType === 'flat' ? ['darkflat', 'dark', 'bias'] : ['bias'];
}

/**
 * What the subject needs, in one shape.
 *
 * The picker compares candidates against this, so both subjects have to
 * describe themselves the same way — a light group by its averages, a
 * calibration set by its own parameters.
 */
export interface Requirement {
  camera: string | null;
  filter: string | null;
  binning: string | null;
  gain: number | null;
  offset: number | null;
  exposure: number | null;
  temperature: number | null;
  dates: [string, string] | null;
  /** How many frames the subject covers — the weight behind the choice. */
  frameCount: number;
}

export function requirementFromLights(p: LightFrameParameters): Requirement {
  return {
    camera: p.instrume,
    filter: p.filter,
    binning: p.binning,
    gain: p.gain,
    offset: p.offset,
    exposure: p.avg_exptime,
    temperature: p.avg_ccd_temp,
    dates: p.date_range,
    frameCount: p.frame_count,
  };
}

export function requirementFromSet(p: CalibrationSetParameters): Requirement {
  return {
    camera: p.instrume,
    filter: p.filter,
    binning: p.binning,
    gain: p.gain,
    offset: p.offset,
    exposure: p.exptime,
    temperature: p.ccd_temp,
    dates: p.date_start && p.date_end ? [p.date_start, p.date_end] : null,
    frameCount: Number(p.frame_count),
  };
}

/** The filter row's state. Every field is "no opinion" when empty. */
export interface CandidateFilter {
  camera: string;
  exposure: string;
  from: string;
  to: string;
}

export const EMPTY_FILTER: CandidateFilter = { camera: '', exposure: '', from: '', to: '' };

export function filterIsActive(f: CandidateFilter): boolean {
  return Boolean(f.camera || f.exposure || f.from || f.to);
}

/** Distinct cameras among these candidates, sorted, blanks dropped. */
export function camerasOf(sets: CalibrationSetWithScore[]): string[] {
  return [...new Set(sets.map(s => s.set.instrume).filter((c): c is string => !!c))].sort();
}

/** Distinct exposures among these candidates, ascending. */
export function exposuresOf(sets: CalibrationSetWithScore[]): number[] {
  return [...new Set(sets.map(s => s.set.exptime).filter((e): e is number => e != null))].sort(
    (a, b) => a - b,
  );
}

/** Apply the filter row. Dates compare against the set's own start. */
export function applyFilter(
  sets: CalibrationSetWithScore[],
  f: CandidateFilter,
): CalibrationSetWithScore[] {
  if (!filterIsActive(f)) return sets;
  const from = f.from ? new Date(f.from) : null;
  const to = f.to ? new Date(`${f.to}T23:59:59`) : null;
  return sets.filter(({ set }) => {
    if (f.camera && set.instrume !== f.camera) return false;
    if (f.exposure && String(set.exptime) !== f.exposure) return false;
    if ((from || to) && set.date_start) {
      const d = new Date(set.date_start);
      if (from && d < from) return false;
      if (to && d > to) return false;
    }
    return true;
  });
}

/** Seconds as a person reads them: at most three decimals, none trailing —
 *  `6.72 s`, never the `6.71999979019165 s` a float32 header round-trips to. */
export function formatSeconds(v: number): string {
  return `${Number(v.toFixed(3))} s`;
}

/** Round every long decimal inside a value string the engine hands back
 *  (`"6.71999979019165"`, `"180 → 6.71999979019165"`) to three places. */
export function tidyNumbers(value: string): string {
  return value.replace(/-?\d+\.\d{4,}/g, m => String(Number(Number(m).toFixed(3))));
}

/** `2025-09-13` — the date part of an ISO stamp, for the date inputs. */
export function isoDay(value: string | null | undefined): string {
  return value ? value.slice(0, 10) : '';
}

// ── How a candidate differs from what the subject needs ──────────────────────

/** Parameter names as the picker spells them for a person. */
const PARAM_LABEL: Record<string, string> = {
  instrume: 'Camera',
  binning: 'Binning',
  gain: 'Gain',
  offset: 'Offset',
  filter: 'Filter',
  exptime: 'Exposure',
  ccd_temp: 'Temperature',
  focallen: 'Focal length',
  telescop: 'Telescope',
  geometry: 'Frame size',
};

/**
 * Every rule that stands between this candidate and the subject — the reason
 * a set the matcher refuses is on screen at all. `enforced` skips the
 * parameters the user's config ignores; those are not differences that matter.
 */
export function blockersOf(parameters: ParameterVerdict[]): ParameterVerdict[] {
  return parameters.filter(
    p => p.enforced && (p.status === 'mismatch' || p.status === 'unknown'),
  );
}

/**
 * One difference, split so the card can weight the parts: what was compared,
 * how it changes, and the limit that was passed.
 *
 * The change reads `needed → offered`, which is the direction a person checks
 * it in: "I need offset 30, this one has 200".
 */
export function describeDifference(p: ParameterVerdict): {
  label: string;
  change: string;
  limit: string | null;
} {
  const label = PARAM_LABEL[p.name] ?? p.name;
  // G-7: a geometry `warning` is the same size at another recorded origin.
  if (p.name === 'geometry' && p.status === 'warning') {
    return { label: 'Sub-frame origin', change: `${p.frameValue ?? '—'} → ${p.setValue ?? '—'}`, limit: null };
  }
  if (p.status === 'unknown') {
    // Three shapes, and each is a different thing to do about it: the set is
    // missing the value (fill it in), the frames are (the header never had
    // it), or neither side declares it and the rule can never be checked.
    if (p.setValue == null && p.frameValue == null) {
      return { label, change: 'declared by neither', limit: null };
    }
    return p.setValue == null
      ? { label, change: `${p.frameValue} → the set declares none`, limit: null }
      : { label, change: `the frames declare none → ${tidyNumbers(p.setValue)}`, limit: null };
  }
  return {
    label,
    change: `${tidyNumbers(p.frameValue ?? '—')} → ${tidyNumbers(p.setValue ?? '—')}`,
    limit: p.matchingThreshold != null ? `limit ${p.matchingThreshold}` : null,
  };
}

// ── Frame geometry (size + sub-frame origin) ─────────────────────────────────

/** `9576×6388`, or `6384×4258 @ 1584,1064` when the set records its
 *  sub-frame origin — the core's `Geometry::label` spelling. Null when the
 *  set records no size. */
export function geometryLabel(
  set: Pick<CalibrationSetDetail, 'naxis1' | 'naxis2' | 'xorgsubf' | 'yorgsubf'>,
): string | null {
  if (set.naxis1 == null || set.naxis2 == null) return null;
  const size = `${set.naxis1}×${set.naxis2}`;
  return set.xorgsubf != null && set.yorgsubf != null
    ? `${size} @ ${set.xorgsubf},${set.yorgsubf}`
    : size;
}

/** The frame-size rule is not the user's to relax (owner ruling 2026-10-20):
 *  a set of another size (or of mixed sizes) can never be chosen. */
export function geometryRefuses(parameters: ParameterVerdict[]): boolean {
  return parameters.some(p => p.name === 'geometry' && p.status === 'mismatch');
}

/** The same size at another recorded sub-frame origin: auto-link never takes
 *  it, a person may — with a warning (owner, G-7: "auto no, manual yes"). */
export function geometryWarns(parameters: ParameterVerdict[]): boolean {
  return parameters.some(p => p.name === 'geometry' && p.status === 'warning');
}

/** The calibration hierarchy's Size cell: green when the set's size is the
 *  lights', warning on another size or (G-7) another recorded origin, muted
 *  when either size is unknown. `title` is what the cell says on hover. */
export function sizeCellState(row: {
  size: string | null;
  sizeKey: string | null;
  cameraSize: string | null;
  originWarning: string | null;
}): { matches: boolean | null; title: string } {
  if (row.originWarning != null) return { matches: false, title: row.originWarning };
  const matches = row.cameraSize != null && row.sizeKey != null ? row.sizeKey === row.cameraSize : null;
  return { matches, title: 'Frame size' };
}

/** The sentence a person sees after choosing a set at another recorded
 *  sub-frame origin by hand (G-7: allowed, with a visible warning); null for
 *  any other candidate. */
export function originChoiceNote(candidate: CalibrationSetWithScore): string | null {
  const verdict = candidate.parameters.find(p => p.name === 'geometry' && p.status === 'warning');
  if (!verdict) return null;
  return `Set #${candidate.set.id} is ${verdict.setValue ?? '—'}, the frames are ${verdict.frameValue ?? '—'} — another sub-frame origin`;
}

/** A set that is SUBTRACTED from the lights: its gain and offset set the
 *  pedestal that comes off, so they have to agree with the lights'. A flat is
 *  divided, and gain / offset barely matter to it. */
const SUBTRAHEND_NOUN: Partial<Record<string, string>> = {
  Dark: 'dark',
  MasterDark: 'dark',
  Bias: 'bias',
  MasterBias: 'bias',
  DarkFlat: 'dark flat',
  MasterDarkFlat: 'dark flat',
};

/** Two header numbers as the picker prints them: `1`, `0.5`, never `1.0000`. */
const level = (v: string): string => tidyNumbers(v);

/** Further apart than this is another gain / offset (header values are read
 *  from floats, so a hair of noise is not a difference). */
const LEVEL_TOLERANCE = 0.01;

interface LevelFacts {
  noun: string;
  /** What the lights declare (their most common value) and what the set does. */
  gain: { frame: string; set: string | null } | null;
  offset: { frame: string; set: string | null } | null;
  gainUnknown: boolean;
  offsetUnknown: boolean;
  gainDiffers: boolean;
  offsetDiffers: boolean;
}

function levelFact(
  parameters: ParameterVerdict[],
  name: 'gain' | 'offset',
): { value: { frame: string; set: string | null } | null; unknown: boolean; differs: boolean } {
  const v = parameters.find(p => p.name === name);
  // No verdict, or the lights declare nothing: there is nothing to compare
  // the set against.
  if (!v || v.frameValue == null) return { value: null, unknown: false, differs: false };
  const value = { frame: v.frameValue, set: v.setValue };
  if (v.setValue == null) return { value, unknown: true, differs: false };
  const a = Number(v.frameValue);
  const b = Number(v.setValue);
  const differs = Number.isNaN(a) || Number.isNaN(b)
    ? v.frameValue !== v.setValue
    : Math.abs(a - b) > LEVEL_TOLERANCE;
  return { value, unknown: false, differs };
}

/** The gain / offset facts of a subtrahend candidate, from the VALUES the
 *  verdicts carry — never from `status` or `enforced`, which follow the
 *  user's matching config (a config that ignores gain still gets the note).
 *  Null for any set that is not subtracted. */
function levelFacts(candidate: CalibrationSetWithScore): LevelFacts | null {
  const noun = SUBTRAHEND_NOUN[candidate.set.imagetyp];
  if (!noun) return null;
  const gain = levelFact(candidate.parameters, 'gain');
  const offset = levelFact(candidate.parameters, 'offset');
  return {
    noun,
    gain: gain.value,
    offset: offset.value,
    gainUnknown: gain.unknown,
    offsetUnknown: offset.unknown,
    gainDiffers: gain.differs,
    offsetDiffers: offset.differs,
  };
}

/** The card's badge for a dark / bias / dark-flat at another or unknown gain
 *  or offset; null when both are known and equal, and for any other set. */
export function levelBadge(candidate: CalibrationSetWithScore): string | null {
  const f = levelFacts(candidate);
  if (!f) return null;
  if (f.gainUnknown || f.offsetUnknown) return 'Gain/offset unknown';
  if (f.gainDiffers && f.offsetDiffers) return 'Another gain and offset';
  if (f.offsetDiffers) return 'Another offset';
  if (f.gainDiffers) return 'Another gain';
  return null;
}

/** The sentence a person sees after choosing a dark, bias or dark flat whose
 *  gain or offset is unknown or differs from the lights'. The link is never
 *  blocked: the pedestal that comes off is the set's own, and this says when
 *  it may not be the lights'. Null for any other candidate. */
export function levelChoiceNote(candidate: CalibrationSetWithScore): string | null {
  const f = levelFacts(candidate);
  if (!f) return null;
  const parts: string[] = [];
  if (f.offset && f.offsetUnknown) {
    parts.push(
      `Offset ${level(f.offset.frame)} on the lights, the ${f.noun} declares none — its pedestal may not match`,
    );
  } else if (f.offset && f.offsetDiffers && f.offset.set != null) {
    const frame = Number(f.offset.frame);
    const set = Number(f.offset.set);
    const how = Number.isNaN(frame) || Number.isNaN(set)
      ? 'another offset, so its pedestal will not match'
      : set > frame
        ? `the ${f.noun}'s offset is higher, so its pedestal will be over-subtracted`
        : `the ${f.noun}'s offset is lower, so its pedestal will be under-subtracted`;
    parts.push(
      `Offset ${level(f.offset.frame)} on the lights, ${level(f.offset.set)} on the ${f.noun} — ${how}`,
    );
  }
  if (f.gain && f.gainUnknown) {
    parts.push(
      `Gain ${level(f.gain.frame)} on the lights, the ${f.noun} declares none — its signal level may not match`,
    );
  } else if (f.gain && f.gainDiffers && f.gain.set != null) {
    parts.push(
      `Gain ${level(f.gain.frame)} on the lights, ${level(f.gain.set)} on the ${f.noun} — the ${f.noun} was shot at another gain`,
    );
  }
  return parts.length > 0 ? `Set #${candidate.set.id}: ${parts.join('; ')}` : null;
}

/** The one slot a sub-calibration save writes: a flat takes ONE of dark flat /
 *  dark / bias — the fallback chain, in order — so the first chosen slot wins
 *  and the rest are left cleared. */
export function writtenSubSlot(picks: Partial<Record<SlotKind, number | null>>): SlotKind | null {
  for (const slot of ['darkflat', 'dark', 'bias'] as SlotKind[]) {
    if (picks[slot]) return slot;
  }
  return null;
}

/** One sentence per newly chosen set at another recorded sub-frame origin
 *  (G-7), naming only links the save really writes: every slot for lights,
 *  the single slot `writtenSubSlot` assigns for a calibration set. */
export function originNotesFor(
  subjectKind: PickerSubject['kind'],
  slots: SlotKind[],
  selected: Partial<Record<SlotKind, number | null>>,
  current: Record<SlotKind, number | null>,
  candidates: Record<SlotKind, CalibrationSetWithScore[]>,
): string[] {
  const written = subjectKind === 'set' ? writtenSubSlot(selected) : null;
  return slots.flatMap(k => {
    if (subjectKind === 'set' && k !== written) return [];
    const chosen = selected[k] ?? null;
    if (chosen === null || chosen === current[k]) return [];
    const c = candidates[k].find(x => x.set.id === chosen);
    const note = c ? originChoiceNote(c) : null;
    return note ? [`${SLOT_LABEL[k]}: ${note}`] : [];
  });
}

/** One sentence per newly chosen dark, bias or dark flat at another or unknown
 *  gain or offset, naming only links the save really writes — the same slot
 *  rules as `originNotesFor`. */
export function levelNotesFor(
  subjectKind: PickerSubject['kind'],
  slots: SlotKind[],
  selected: Partial<Record<SlotKind, number | null>>,
  current: Record<SlotKind, number | null>,
  candidates: Record<SlotKind, CalibrationSetWithScore[]>,
): string[] {
  const written = subjectKind === 'set' ? writtenSubSlot(selected) : null;
  return slots.flatMap(k => {
    if (subjectKind === 'set' && k !== written) return [];
    const chosen = selected[k] ?? null;
    if (chosen === null || chosen === current[k]) return [];
    const c = candidates[k].find(x => x.set.id === chosen);
    const note = c ? levelChoiceNote(c) : null;
    return note ? [`${SLOT_LABEL[k]}: ${note}`] : [];
  });
}
