import type {
  CalibrationSetDetail,
  CalibrationWarning,
  LightFrameWithCalibration,
} from '../../types/models';

// ── Match Badge Types & Helpers ────────────────────────────────────────

export type MatchLevel = 'match' | 'warning' | 'mismatch' | 'unknown';

/** Exact-match comparison for locked parameters (gain, binning, offset) */
export function exactMatchLevel(
  calValue: number | string | null | undefined,
  lightValue: number | string | null | undefined,
): MatchLevel {
  if (calValue == null && lightValue == null) return 'unknown';
  if (calValue == null || lightValue == null) return 'unknown';
  if (typeof calValue === 'string' || typeof lightValue === 'string') {
    return String(calValue) === String(lightValue) ? 'match' : 'mismatch';
  }
  return Math.abs(calValue - lightValue) < 0.01 ? 'match' : 'mismatch';
}

/** Temperature match level derived from the backend's existing warnings */
export function tempMatchLevel(
  calTemp: number | null,
  lightTemp: number | null,
  warnings: CalibrationWarning[],
): MatchLevel {
  if (calTemp == null && lightTemp == null) return 'unknown';
  if (calTemp == null || lightTemp == null) return 'unknown';
  const hasTempWarning = warnings.some(w => w.warning_type === 'temperature');
  return hasTempWarning ? 'warning' : 'match';
}

export const matchStyles: Record<MatchLevel, string> = {
  match: 'bg-success-muted text-success',
  warning: 'bg-warning-muted text-warning',
  mismatch: 'bg-error-muted text-error',
  unknown: 'bg-surface-hover text-content-muted',
};

export function fmtVal(v: number | string | null | undefined, unit?: string): string {
  if (v == null) return 'N/A';
  if (typeof v === 'number') return unit ? `${v}${unit}` : String(v);
  return String(v);
}

export function exactTooltip(label: string, calVal: number | string | null | undefined, lightVal: number | string | null | undefined, level: MatchLevel): string {
  if (level === 'unknown') return `${label}: N/A`;
  return `${label}: ${fmtVal(calVal)} ${level === 'match' ? '=' : '\u2260'} ${fmtVal(lightVal)} (${level})`;
}

export function tempTooltip(calTemp: number | null, lightTemp: number | null, level: MatchLevel): string {
  if (level === 'unknown') return 'Temp: N/A';
  const delta = Math.abs((calTemp ?? 0) - (lightTemp ?? 0)).toFixed(1);
  if (level === 'match') return `Temp: ${fmtVal(calTemp, '\u00B0C')} = ${fmtVal(lightTemp, '\u00B0C')} (match)`;
  return `Temp: ${fmtVal(calTemp, '\u00B0C')} vs ${fmtVal(lightTemp, '\u00B0C')} (${delta}\u00B0 delta, warning)`;
}

/** Derive reference parameters from the first light frame */
export function extractLightParams(frames: LightFrameWithCalibration[]) {
  if (frames.length === 0) return null;
  const f = frames[0];
  return {
    gain: f.gain,
    offset: f.offset,
    binning: f.binning,
    ccd_temp: f.ccd_temp,
    focallen: f.focallen,
  };
}

export type LightParams = ReturnType<typeof extractLightParams>;

// ── Components ─────────────────────────────────────────────────────────

export function MatchBadge({ letter, level, tooltip }: { letter: string; level: MatchLevel; tooltip: string }) {
  return (
    <span
      className={`inline-flex items-center justify-center w-5 h-5 rounded text-[10px] font-bold ${matchStyles[level]}`}
      title={tooltip}
    >
      {letter}
    </span>
  );
}

export function MatchBadges({
  set,
  warnings,
  lightParams,
}: {
  set: CalibrationSetDetail;
  warnings: CalibrationWarning[];
  lightParams: LightParams;
}) {
  if (!lightParams) return <span className="text-content-muted text-xs">-</span>;

  const gLevel = exactMatchLevel(set.gain, lightParams.gain);
  const bLevel = exactMatchLevel(set.binning, lightParams.binning);
  const oLevel = exactMatchLevel(set.offset, lightParams.offset);
  const tLevel = tempMatchLevel(set.ccd_temp, lightParams.ccd_temp, warnings);

  return (
    <div className="flex items-center gap-0.5">
      <MatchBadge letter="G" level={gLevel} tooltip={exactTooltip('Gain', set.gain, lightParams.gain, gLevel)} />
      <MatchBadge letter="B" level={bLevel} tooltip={exactTooltip('Binning', set.binning, lightParams.binning, bLevel)} />
      <MatchBadge letter="O" level={oLevel} tooltip={exactTooltip('Offset', set.offset, lightParams.offset, oLevel)} />
      <MatchBadge letter="T" level={tLevel} tooltip={tempTooltip(set.ccd_temp, lightParams.ccd_temp, tLevel)} />
    </div>
  );
}
