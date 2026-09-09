import type { PlanGroup } from '../../types/stacking';

function formatExposure(totalExposureS: number): string {
  if (totalExposureS <= 0) return '0m';
  const hours = totalExposureS / 3600;
  if (hours >= 1) return `${hours.toFixed(1)}h`;
  const minutes = totalExposureS / 60;
  return `${minutes.toFixed(0)}m`;
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
            <th className="py-1.5 px-2 font-medium">Geometry</th>
            <th className="py-1.5 px-2 font-medium">Frames</th>
            <th className="py-1.5 px-2 font-medium">Exposure</th>
            <th className="py-1.5 px-2 font-medium">Cached</th>
          </tr>
        </thead>
        <tbody>
          {groups.map((g) => (
            <tr key={g.key} className="border-b border-border/50 last:border-0">
              <td className="py-1.5 px-2 text-content font-mono">{g.key}</td>
              <td className="py-1.5 px-2 text-content-secondary">{g.instrume ?? '—'}</td>
              <td className="py-1.5 px-2 text-content-secondary">{g.colorMode === 'osc' ? 'OSC' : 'Mono'}</td>
              <td className="py-1.5 px-2 text-content-secondary">{g.filter ?? '—'}</td>
              <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.binning}×{g.binning}</td>
              <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.width}×{g.height}</td>
              <td className="py-1.5 px-2 text-content-secondary tabular-nums">{g.includedCount}/{g.frameCount}</td>
              <td className="py-1.5 px-2 text-content-secondary tabular-nums">{formatExposure(g.totalExposureS)}</td>
              <td className="py-1.5 px-2 text-content-muted tabular-nums">
                {g.calibratedCached} calibrated · {g.metricsCached} metrics
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
