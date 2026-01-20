import { format, parseISO } from 'date-fns';
import { Camera, Clock } from 'lucide-react';
import type { CalendarYearData, CalendarDayEvent } from '../../types/models';

interface CalendarYearListProps {
  data: CalendarYearData;
  selectedDate: string | null;
  onDayClick: (date: string, events: CalendarDayEvent | null) => void;
}

function formatExposure(seconds: number): string {
  if (seconds < 60) {
    return `${seconds.toFixed(0)}s`;
  }
  const minutes = seconds / 60;
  if (minutes < 60) {
    return `${minutes.toFixed(1)}m`;
  }
  const hours = minutes / 60;
  return `${hours.toFixed(1)}h`;
}

function DateRow({
  day,
  isSelected,
  onClick,
}: {
  day: CalendarDayEvent;
  isSelected: boolean;
  onClick: () => void;
}) {
  const date = parseISO(day.date);
  const hasFrameSets = day.frameSets.length > 0;
  const hasUnorganized = day.unorganizedGroups.length > 0;

  // Get primary object name to display
  const primaryName =
    day.frameSets[0]?.objectName ||
    day.frameSets[0]?.name ||
    day.unorganizedGroups[0]?.objectName ||
    'Unnamed';

  const additionalCount =
    day.frameSets.length + day.unorganizedGroups.length - 1;

  return (
    <button
      onClick={onClick}
      className={`
        w-full flex items-center gap-3 px-3 py-2 rounded-lg text-left transition-colors
        ${isSelected
          ? 'bg-accent/30 border border-accent'
          : 'hover:bg-surface-hover/50 border border-transparent'
        }
      `}
    >
      {/* Date */}
      <div className="w-16 flex-shrink-0">
        <span className="text-sm font-medium text-content">
          {format(date, 'd MMM')}
        </span>
      </div>

      {/* Indicators */}
      <div className="flex items-center gap-1 flex-shrink-0">
        {hasFrameSets && (
          <span
            className="w-2 h-2 rounded-full bg-accent"
            title="Organized frame sets"
          />
        )}
        {hasUnorganized && (
          <span
            className="w-2 h-2 rounded-full bg-warning"
            title="Unorganized frames"
          />
        )}
      </div>

      {/* Object name */}
      <div className="flex-1 min-w-0">
        <span className="text-sm text-content-secondary truncate block">
          {primaryName}
          {additionalCount > 0 && (
            <span className="text-content-muted"> +{additionalCount}</span>
          )}
        </span>
      </div>

      {/* Stats */}
      <div className="flex items-center gap-3 text-xs text-content-muted flex-shrink-0">
        <span className="flex items-center gap-1">
          <Camera size={12} />
          {day.totalFrameCount}
        </span>
        <span className="flex items-center gap-1">
          <Clock size={12} />
          {formatExposure(day.totalExposureSeconds)}
        </span>
      </div>
    </button>
  );
}

const MONTH_NAMES = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
];

export function CalendarYearList({
  data,
  selectedDate,
  onDayClick,
}: CalendarYearListProps) {
  if (data.months.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-content-muted">
        <p>No imaging data for {data.year}</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto bg-surface-elevated rounded-lg p-4 space-y-6">
      {data.months.map((month) => (
        <div key={month.month}>
          {/* Month header */}
          <div className="flex items-center justify-between mb-2">
            <h4 className="text-sm font-semibold text-content-secondary uppercase tracking-wide">
              {MONTH_NAMES[month.month - 1]}
            </h4>
            <span className="text-xs text-content-muted">
              {month.totalFrameCount} frames | {formatExposure(month.totalExposureSeconds)}
            </span>
          </div>

          {/* Days list */}
          <div className="space-y-1">
            {month.days.map((day) => (
              <DateRow
                key={day.date}
                day={day}
                isSelected={selectedDate === day.date}
                onClick={() => onDayClick(day.date, day)}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
