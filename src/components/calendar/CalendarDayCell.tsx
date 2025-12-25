import { format, isToday, isSameMonth } from 'date-fns';
import type { CalendarDayEvent } from '../../types/models';

interface CalendarDayCellProps {
  date: Date;
  currentMonth: Date;
  events: CalendarDayEvent | null;
  onClick: (date: string, events: CalendarDayEvent | null, element: HTMLElement) => void;
}

export function CalendarDayCell({
  date,
  currentMonth,
  events,
  onClick,
}: CalendarDayCellProps) {
  const isCurrentMonth = isSameMonth(date, currentMonth);
  const isTodayDate = isToday(date);
  const hasFrameSets = events && events.frameSets.length > 0;
  const hasUnorganized = events && events.unorganizedGroups.length > 0;
  const hasEvents = events && (events.frameSets.length > 0 || events.unorganizedGroups.length > 0);

  const handleClick = (e: React.MouseEvent<HTMLDivElement>) => {
    onClick(format(date, 'yyyy-MM-dd'), events, e.currentTarget);
  };

  return (
    <div
      onClick={hasEvents ? handleClick : undefined}
      className={`
        min-h-[80px] p-2 border border-gray-700 rounded-lg
        ${isCurrentMonth ? 'bg-gray-800' : 'bg-gray-850 opacity-50'}
        ${isTodayDate ? 'ring-2 ring-blue-500' : ''}
        ${hasEvents ? 'cursor-pointer hover:border-gray-500 transition-colors' : ''}
      `}
    >
      {/* Day number */}
      <div className={`
        text-sm font-medium mb-1
        ${isTodayDate ? 'text-blue-400' : isCurrentMonth ? 'text-gray-200' : 'text-gray-500'}
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
                className="w-2 h-2 rounded-full bg-blue-500"
                title="Organized frame sets"
              />
            )}
            {hasUnorganized && (
              <span
                className="w-2 h-2 rounded-full bg-yellow-500"
                title="Unorganized frames"
              />
            )}
          </div>

          {/* Frame count badge */}
          <div className="text-xs text-gray-400">
            {events!.totalFrameCount} frames
          </div>

          {/* Preview of objects */}
          <div className="text-xs text-gray-500 truncate">
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
