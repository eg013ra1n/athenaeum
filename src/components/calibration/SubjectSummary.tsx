import { isoDay, type CandidateFilter, type Requirement } from './pickerModel';

function fmtExposure(v: number | null): string | null {
  if (v == null) return null;
  return Number.isInteger(v) ? `${v} s` : `${v.toFixed(3)} s`;
}

/**
 * One line of the summary. A row whose value can narrow the list is a button
 * that does exactly that — the fastest way to find "the flats from that same
 * camera" is to point at the camera you already need.
 */
function Row({
  label,
  value,
  onUse,
  useHint,
}: {
  label: string;
  value: string | null;
  onUse?: () => void;
  useHint?: string;
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 py-1">
      <span className="text-xs uppercase tracking-wide text-content-muted">{label}</span>
      {value == null ? (
        <span className="text-sm text-content-muted">not declared</span>
      ) : onUse ? (
        <button
          type="button"
          onClick={onUse}
          title={useHint}
          className="rounded px-1 -mr-1 text-sm text-content underline decoration-dotted decoration-content-muted underline-offset-4 transition-colors hover:bg-accent/10 hover:text-accent focus:outline-none focus-visible:ring-1 focus-visible:ring-accent"
        >
          {value}
        </button>
      ) : (
        <span className="text-sm text-content">{value}</span>
      )}
    </div>
  );
}

/**
 * What the calibration has to fit: the subject's own parameters, and the
 * shortcut from each of them into the filter above the list.
 */
export function SubjectSummary({
  requirement,
  onFilterChange,
}: {
  requirement: Requirement;
  onFilterChange: (patch: Partial<CandidateFilter>) => void;
}) {
  const r = requirement;
  // Same reasoning as the candidate cards: on a single night the date repeats
  // and only the clock distinguishes anything.
  const dates = (() => {
    if (!r.dates) return null;
    const [a, b] = r.dates.map(v => new Date(v));
    if (Number.isNaN(a.getTime()) || Number.isNaN(b.getTime())) {
      return `${isoDay(r.dates[0])} → ${isoDay(r.dates[1])}`;
    }
    const pad = (n: number) => String(n).padStart(2, '0');
    const day = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
    const clock = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    return day(a) === day(b)
      ? `${day(a)} ${clock(a)}–${clock(b)}`
      : `${day(a)} ${clock(a)} → ${day(b)} ${clock(b)}`;
  })();

  return (
    <div>
      <h3 className="mb-1 text-sm font-medium text-content">What the calibration has to fit</h3>
      <p className="mb-3 text-xs text-content-muted">
        {r.frameCount} frame{r.frameCount === 1 ? '' : 's'}. Click a value to narrow the list.
      </p>

      <div className="divide-y divide-border/60">
        <Row
          label="Camera"
          value={r.camera}
          onUse={r.camera ? () => onFilterChange({ camera: r.camera as string }) : undefined}
          useHint="Show only sets from this camera"
        />
        <Row label="Filter" value={r.filter || 'No filter'} />
        <Row label="Binning" value={r.binning} />
        <Row label="Gain" value={r.gain != null ? String(r.gain) : null} />
        <Row label="Offset" value={r.offset != null ? String(r.offset) : null} />
        <Row
          label="Exposure"
          value={fmtExposure(r.exposure)}
          onUse={
            r.exposure != null ? () => onFilterChange({ exposure: String(r.exposure) }) : undefined
          }
          useHint="Show only sets at this exposure"
        />
        <Row
          label="Temperature"
          value={r.temperature != null ? `${r.temperature.toFixed(1)} °C` : null}
        />
        <Row
          label="Nights"
          value={dates}
          onUse={
            r.dates
              ? () => onFilterChange({ from: isoDay(r.dates![0]), to: isoDay(r.dates![1]) })
              : undefined
          }
          useHint="Show only sets shot in this window"
        />
      </div>
    </div>
  );
}
