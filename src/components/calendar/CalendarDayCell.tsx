import { format, isToday, isSameMonth } from 'date-fns';
import type { CalendarDayEvent } from '../../types/models';

interface CalendarDayCellProps {
  date: Date;
  currentMonth: Date;
  events: CalendarDayEvent | null;
  isSelected: boolean;
  onClick: (date: string, events: CalendarDayEvent | null) => void;
}

export function CalendarDayCell({
  date,
  currentMonth,
  events,
  isSelected,
  onClick,
}: CalendarDayCellProps) {
  const isCurrentMonth = isSameMonth(date, currentMonth);
  const isTodayDate = isToday(date);
  const hasFrameSets = events && events.frameSets.length > 0;
  const hasUnorganized = events && events.unorganizedGroups.length > 0;
  const hasEvents = events && (events.frameSets.length > 0 || events.unorganizedGroups.length > 0);

  const handleClick = () => {
    onClick(format(date, 'yyyy-MM-dd'), events);
  };

  return (
    <div
      onClick={hasEvents ? handleClick : undefined}
      className={`
        min-h-[80px] p-2 border border-border rounded-lg
        ${isCurrentMonth ? 'bg-surface-elevated' : 'bg-surface opacity-50'}
        ${isSelected ? 'ring-2 ring-accent border-accent' : isTodayDate ? 'ring-2 ring-accent/50' : ''}
        ${hasEvents ? 'cursor-pointer hover:border-content-muted transition-colors' : ''}
      `}
    >
      {/* Day number */}
      <div className={`
        text-sm font-medium mb-1
        ${isTodayDate ? 'text-accent' : isCurrentMonth ? 'text-content' : 'text-content-muted'}
      `}>
        {format(date, 'd')}
      </div>

      {/* Event indicators */}
      {hasEvents && (
        <div className="space-y-1">
          {/* Event dots */}
          <div className="flex items-center gap-1">
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

          {/* Frame count badge */}
          <div className="text-xs text-content-muted">
            {events!.totalFrameCount} frames
          </div>

          {/* Preview of objects */}
          <div className="text-xs text-content-muted truncate">
            {events!.frameSets.length > 0 && (
              <span title={events!.frameSets.map(fs => fs.objectName || fs.name).join(', ')}>
                {events!.frameSets[0].objectName || events!.frameSets[0].name || 'Unknown'}
                {events!.frameSets.length > 1 && ` +${events!.frameSets.length - 1}`}
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
