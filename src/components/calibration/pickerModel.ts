import type {
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
  if (p.status === 'unknown') {
    // Three shapes, and each is a different thing to do about it: the set is
    // missing the value (fill it in), the frames are (the header never had
    // it), or neither side declares it and the rule can never be checked.
    if (p.setValue == null && p.frameValue == null) {
      return { label, change: 'declared by neither', limit: null };
    }
    return p.setValue == null
      ? { label, change: `${p.frameValue} → the set declares none`, limit: null }
      : { label, change: `the frames declare none → ${p.setValue}`, limit: null };
  }
  return {
    label,
    change: `${p.frameValue ?? '—'} → ${p.setValue ?? '—'}`,
    limit: p.matchingThreshold != null ? `limit ${p.matchingThreshold}` : null,
  };
}
