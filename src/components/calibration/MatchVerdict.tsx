import { AlertTriangle, HelpCircle } from 'lucide-react';
import type { ParameterVerdict } from '../../types/models';

/** Parameter names as the cards spell them. */
const LABELS: Record<string, string> = {
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

/** Every verdict that stands between this set and the user's config. */
export function blockersOf(parameters: ParameterVerdict[]): ParameterVerdict[] {
  return parameters.filter(
    p => p.enforced && (p.status === 'mismatch' || p.status === 'unknown'),
  );
}

/** One blocker as a sentence: what was compared, and what the answer was. */
export function describeBlocker(p: ParameterVerdict): string {
  const label = LABELS[p.name] ?? p.name;
  if (p.status === 'unknown') {
    // The common case is a master whose FITS header carries no GAIN/OFFSET;
    // the set cannot be compared, which is not the same as disagreeing.
    return p.setValue == null
      ? `${label}: this set does not declare one`
      : `${label}: the frames do not declare one`;
  }
  const frame = p.frameValue ?? '—';
  const set = p.setValue ?? '—';
  if (p.diff != null && p.matchingThreshold != null) {
    return `${label}: ${frame} vs ${set} — off by ${p.diff.toFixed(1)}, limit ${p.matchingThreshold.toFixed(1)}`;
  }
  return `${label}: ${frame} ≠ ${set}`;
}

/**
 * Why the user's calibration-matching config refuses this set.
 *
 * Renders nothing for a compatible set. The engine scores CLOSENESS, so an
 * incompatible candidate can read a high percentage — without this line the
 * card would look like a good match that is silently unusable, which is what
 * the flat 0 % used to hide (2026-09-05 design §5).
 */
export function MatchVerdict({
  compatible,
  parameters,
}: {
  compatible: boolean;
  parameters: ParameterVerdict[];
}) {
  if (compatible) return null;
  const blockers = blockersOf(parameters);
  if (blockers.length === 0) return null;

  return (
    <div className="mt-1.5 rounded border border-warning/30 bg-warning/10 px-2 py-1">
      <div className="flex items-center gap-1.5 text-xs font-medium text-warning">
        <AlertTriangle size={11} className="flex-shrink-0" />
        Does not match your calibration rules
      </div>
      <ul className="mt-0.5 space-y-0.5">
        {blockers.map(p => (
          <li key={p.name} className="flex items-start gap-1.5 text-xs text-content-secondary">
            {p.status === 'unknown' ? (
              <HelpCircle size={11} className="mt-0.5 flex-shrink-0 text-content-muted" />
            ) : (
              <span className="mt-0.5 flex-shrink-0 text-warning">✕</span>
            )}
            <span>{describeBlocker(p)}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
