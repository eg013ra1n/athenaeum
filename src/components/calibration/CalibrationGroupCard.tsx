import type {
  CalibrationSetWithFrameCount,
  SubCalibrationDetail,
} from '../../types/models';
import { MatchBadges, extractLightParams } from './MatchBadges';
import { typeColors, subCalTypeColors } from './CalibrationSetsTable';
import type { EnrichedLightFrame } from './LightsAnalysisTable';

export interface FlatGroupData {
  flatSet: CalibrationSetWithFrameCount;
  lights: EnrichedLightFrame[];
}

export interface DarkOnlyGroupData {
  darkSet: CalibrationSetWithFrameCount;
  lights: EnrichedLightFrame[];
}

interface CalibrationGroupCardProps {
  type: 'flat' | 'dark';
  data: FlatGroupData | DarkOnlyGroupData;
  allLightFrames: EnrichedLightFrame[];
  onClick: () => void;
}

function formatDateRange(dateStart: string | null, dateEnd: string | null): string {
  if (!dateStart) return '-';
  try {
    const start = new Date(dateStart);
    const startStr = start.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
    if (!dateEnd || dateStart === dateEnd) return startStr;
    const end = new Date(dateEnd);
    const endStr = end.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
    return `${startStr} \u2013 ${endStr}`;
  } catch {
    return '-';
  }
}

function SubCalSummary({ subCals }: { subCals: SubCalibrationDetail[] }) {
  if (subCals.length === 0) return null;

  return (
    <div className="flex items-center gap-2 flex-wrap">
      <span className="text-xs text-content-muted">Sub-cal:</span>
      {subCals.map((sub, i) => {
        const colors = subCalTypeColors[sub.calibration_type] ?? { dot: 'bg-content-muted', text: 'text-content' };
        return (
          <span key={`${sub.calibration_type}-${sub.set.id ?? i}`} className="flex items-center gap-1">
            <span className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${colors.dot}`} />
            <span className={`text-xs ${colors.text}`}>
              {sub.calibration_type} #{sub.set.id}
            </span>
            <span className="text-xs text-content-muted">({sub.set.frame_count}f)</span>
          </span>
        );
      })}
    </div>
  );
}

export function CalibrationGroupCard({
  type,
  data,
  allLightFrames,
  onClick,
}: CalibrationGroupCardProps) {
  const primarySet = type === 'flat'
    ? (data as FlatGroupData).flatSet
    : (data as DarkOnlyGroupData).darkSet;

  const lights = type === 'flat'
    ? (data as FlatGroupData).lights
    : (data as DarkOnlyGroupData).lights;

  const colors = typeColors[type];
  const lightParams = extractLightParams(allLightFrames);

  return (
    <button
      onClick={onClick}
      className={`
        w-full text-left rounded-lg border border-border bg-surface-elevated
        p-4 hover:border-accent/50 cursor-pointer transition-all
        border-l-[3px] ${colors.border}
        focus:outline-none focus-visible:ring-2 focus-visible:ring-accent
      `}
    >
      {/* Top row: Type badge, ID, master badge, light count */}
      <div className="flex items-center gap-2 mb-1.5">
        <span className={`px-1.5 py-0.5 rounded text-xs font-semibold whitespace-nowrap ${colors.badge}`}>
          {type.charAt(0).toUpperCase() + type.slice(1)}
        </span>
        {primarySet.set.id !== null && (
          <span className="text-xs text-content-muted">#{primarySet.set.id}</span>
        )}
        {primarySet.set.is_master && (
          <span className="px-1 py-0.5 text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/40 rounded whitespace-nowrap">
            Master
          </span>
        )}
        {type === 'dark' && (
          <span className="px-1 py-0.5 text-[10px] font-medium bg-warning-muted text-warning border border-warning/50 rounded whitespace-nowrap">
            No Flat
          </span>
        )}
        <span className="ml-auto text-sm text-accent font-medium">
          {lights.length} light{lights.length !== 1 ? 's' : ''}
        </span>
      </div>

      {/* Date range and set details */}
      <div className="text-xs text-content-muted mb-1.5">
        {formatDateRange(primarySet.set.date_start, primarySet.set.date_end)}
        {' \u00B7 '}
        {primarySet.set.frame_count} frame{primarySet.set.frame_count !== 1 ? 's' : ''}
        {primarySet.set.exptime !== null && ` \u00B7 ${primarySet.set.exptime}s`}
        {primarySet.set.ccd_temp !== null && ` \u00B7 ${primarySet.set.ccd_temp.toFixed(1)}\u00B0C`}
      </div>

      {/* Match badges */}
      <div className="flex items-center gap-3 mb-1.5">
        <MatchBadges set={primarySet.set} warnings={primarySet.warnings} lightParams={lightParams} />
        {primarySet.warnings.length > 0 && (
          <span className="text-xs text-warning">
            {primarySet.warnings.length} warning{primarySet.warnings.length !== 1 ? 's' : ''}
          </span>
        )}
      </div>

      {/* Sub-calibration summary */}
      {primarySet.sub_calibration && primarySet.sub_calibration.length > 0 && (
        <SubCalSummary subCals={primarySet.sub_calibration} />
      )}
    </button>
  );
}
