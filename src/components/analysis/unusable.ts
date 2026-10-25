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
    case 'no_stars': return "The star measurement could not fit this frame's stars: it found no star to fit";
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

/** What a table / panel / CSV shows for one analysis row. */
export interface RowVerdict {
  label: string;
  className: string;
  tooltip: string | undefined;
  /** Sort rank by severity (a legacy trailed row ranks with Trailed). */
  rank: number;
}

/**
 * The verdict of a whole row. A current row shows its `frame_verdict`; a legacy row (`measure_version` null, not yet
 * re-measured) flagged `possibly_trailed` shows "Trailed (legacy)" so the Reject-unusable drop is explained (PC-15);
 * any other row shows "—".
 */
export function rowVerdict(a: FrameAnalysis): RowVerdict {
  if (a.frame_verdict == null && a.measure_version == null && a.possibly_trailed) {
    return {
      label: 'Trailed (legacy)',
      className: 'text-warning',
      tooltip: 'The previous analysis flagged this frame trailed; re-analyze it for a verdict',
      rank: verdictSeverity('trailed'),
    };
  }
  const v = a.frame_verdict;
  return { label: verdictLabel(v), className: verdictClass(v), tooltip: verdictTooltip(v), rank: verdictSeverity(v) };
}
