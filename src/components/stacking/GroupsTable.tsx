import type { PlanGroup } from '../../types/stacking';

function formatExposure(totalExposureS: number): string {
  if (totalExposureS <= 0) return '0m';
  const hours = totalExposureS / 3600;
  if (hours >= 1) return `${hours.toFixed(1)}h`;
  const minutes = totalExposureS / 60;
  return `${minutes.toFixed(0)}m`;
}

/** The group key's own exposure-cluster token (`180s`, `0.39s`, `unknown`) —
 *  hand-mirrors `crates/athenaeum-core/src/calibration_library/paths.rs`'s
 *  `fmt_num` (2 decimals, trailing zeros trimmed) rather than calling it —
 *  it is `pub(crate)`, backend-only. Display text only; the wire value
 *  (`PlanGroup.exposureS`) is the number, never this formatted string, so
 *  a drift here cannot desync anything the backend persists or matches on. */
function formatExposureToken(exposureS: number | null): string {
  if (exposureS == null) return 'unknown';
  const trimmed = exposureS.toFixed(2).replace(/0+$/, '').replace(/\.$/, '');
  return `${trimmed}s`;
}

/** Mirrors `stacking::register::SCALE_TOLERANCE` — the same tolerance the
 *  plan-time pixel-scale warning and the per-frame registration gate both
 *  read from the backend constant; there is no wire copy of the number
 *  itself, so it is pinned here. */
const SCALE_TOLERANCE = 1.25;

/** `0.78"` for a solve-measured group scale, `~0.78"` (with an explanatory
 *  title) when the group's own value rests on at least one header-implied
 *  member instead of a measured plate solve, `—` when the group has no
 *  scale at all. */
function formatScale(pixelScaleArcsec: number | null, scaleSource: PlanGroup['scaleSource']): { text: string; title?: string } {
  if (pixelScaleArcsec == null) return { text: '—' };
  const value = `${pixelScaleArcsec.toFixed(2)}"`;
  if (scaleSource === 'header') {
    return { text: `~${value}`, title: "from the header's focal length and pixel size" };
  }
  return { text: value };
}

/** Whether a group's own scale sits outside `[1 / SCALE_TOLERANCE,
 *  SCALE_TOLERANCE]` of the resolved reference — the same test the backend
 *  plan-time warning uses to decide whether to warn at all. */
function isFarFromReference(scaleRatioToReference: number | null): boolean {
  if (scaleRatioToReference == null) return false;
  return scaleRatioToReference > SCALE_TOLERANCE || scaleRatioToReference < 1 / SCALE_TOLERANCE;
}

export interface GroupsTableProps {
  groups: PlanGroup[];
}

/** Read-only summary of the plan's frame groups (spec §11.1). */
export function GroupsTable({ groups }: GroupsTableProps) {
  if (groups.length === 0) {
    return <p className="text-sm text-content-muted px-1">No groups yet.</p>;
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="text-content-muted text-left border-b border-border">
            <th className="py-1.5 px-2 font-medium">Key</th>
            <th className="py-1.5 px-2 font-medium">Camera</th>
            <th className="py-1.5 px-2 font-medium">Colour</th>
            <th className="py-1.5 px-2 font-medium">Filter</th>
            <th className="py-1.5 px-2 font-medium">Bin</th>
            <th className="py-1.5 px-2 font-medium">Scale</th>
            <th className="py-1.5 px-2 font-medium">Exp</th>
            <th className="py-1.5 px-2 font-medium">Frames</th>
            <th className="py-1.5 px-2 font-medium">Exposure</th>
            <th className="py-1.5 px-2 font-medium">Cached</th>
          </tr>
        </thead>
        <tbody>
          {groups.map((g) => {
            const scale = formatScale(g.pixelScaleArcsec, g.scaleSource);
            const farRatio = isFarFromReference(g.scaleRatioToReference) ? g.scaleRatioToReference : null;
            return (
              <tr key={g.key} className="border-b border-border/50 last:border-0">
                <td className="py-1.5 px-2 text-content font-mono">{g.key}</td>
                <td className="py-1.5 px-2 text-content-secondary">{g.cameras.length > 0 ? g.cameras.join(' + ') : '—'}</td>
                <td className="py-1.5 px-2 text-content-secondary">{g.colorMode === 'osc' ? 'OSC' : 'Mono'}</td>
                <td className="py-1.5 px-2 text-content-secondary">{g.filter ?? '—'}</td>
                <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.binning}×{g.binning}</td>
                <td className="py-1.5 px-2 text-content-secondary tabular-nums" title={scale.title}>
                  {scale.text}
                  {farRatio != null && (
                    <span className="ml-1 text-warning">×{farRatio.toFixed(1)}</span>
                  )}
                </td>
                <td className="py-1.5 px-2 text-content-secondary tabular-nums">{formatExposureToken(g.exposureS)}</td>
                <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.includedCount}/{g.frameCount}</td>
                <td className="py-1.5 px-2 text-content-secondary tabular-nums">{formatExposure(g.totalExposureS)}</td>
                <td className="py-1.5 px-2 text-content-muted tabular-nums">
                  {g.calibratedCached} calibrated · {g.metricsCached} metrics
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
