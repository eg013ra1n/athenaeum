import type { FrameAnalysis, FrameVerdict } from '../../types/models';

/**
 * A frame the star measurement marks unusable (PC-15): any verdict, or for a legacy row (`measure_version` null, not
 * yet re-measured) its old `possibly_trailed` flag.
 */
export function isUnusable(a: FrameAnalysis): boolean {
  if (a.frame_verdict != null) return true;
  if (a.measure_version == null) return a.possibly_trailed;
  return false;
}

export function verdictLabel(v: FrameVerdict | null): string {
  switch (v) {
    case 'trailed': return 'Trailed';
    case 'defocused': return 'Defocused';
    case 'no_stars': return 'No stars';
    default: return '—';
  }
}

/** Sort rank by severity: none, No stars, Defocused, Trailed. */
export function verdictSeverity(v: FrameVerdict | null): number {
  switch (v) {
    case 'no_stars': return 1;
    case 'defocused': return 2;
    case 'trailed': return 3;
    default: return 0;
  }
}

/** Tooltip words of a verdict; `undefined` for none. */
export function verdictTooltip(v: FrameVerdict | null): string | undefined {
  switch (v) {
    case 'trailed': return "The star measurement could not fit this frame's stars: they are trails";
    case 'defocused': return "The star measurement could not fit this frame's stars: they are out-of-focus rings";
    case 'no_stars': return 'The star measurement could not fit this frame: it found no star to fit';
    default: return undefined;
  }
}

/** Text colour class of a verdict. */
export function verdictClass(v: FrameVerdict | null): string {
  switch (v) {
    case 'trailed':
    case 'defocused': return 'text-warning';
    case 'no_stars': return 'text-content-muted';
    default: return 'text-content-muted';
  }
}
