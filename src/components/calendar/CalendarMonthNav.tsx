import { ChevronLeft, ChevronRight, Calendar } from 'lucide-react';
import { format } from 'date-fns';

interface CalendarMonthNavProps {
  year: number;
  month: number; // 1-12
  onPrevMonth: () => void;
  onNextMonth: () => void;
  onToday: () => void;
  totalFrameCount: number;
  totalExposureHours: number;
}

export function CalendarMonthNav({
  year,
  month,
  onPrevMonth,
  onNextMonth,
  onToday,
  totalFrameCount,
  totalExposureHours,
}: CalendarMonthNavProps) {
  // Create a date object for formatting
  const displayDate = new Date(year, month - 1, 1);

  return (
    <div className="flex items-center justify-between mb-4">
      <div className="flex items-center gap-4">
        <div className="flex items-center gap-1">
          <button
            onClick={onPrevMonth}
            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
            title="Previous month"
          >
            <ChevronLeft size={20} />
          </button>
          <button
            onClick={onNextMonth}
            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
            title="Next month"
          >
            <ChevronRight size={20} />
          </button>
        </div>
        <h3 className="text-xl font-semibold">
          {format(displayDate, 'MMMM yyyy')}
        </h3>
        <button
          onClick={onToday}
          className="flex items-center gap-1 px-3 py-1.5 text-sm bg-gray-700 hover:bg-gray-600 rounded-lg transition-colors"
        >
          <Calendar size={14} />
          Today
        </button>
      </div>

      {/* Month stats */}
      <div className="flex items-center gap-4 text-sm text-gray-400">
        <span>
          <span className="text-gray-200 font-medium">{totalFrameCount}</span> frames
        </span>
        <span>
          <span className="text-gray-200 font-medium">{totalExposureHours.toFixed(1)}</span>h exposure
        </span>
      </div>
    </div>
  );
}
