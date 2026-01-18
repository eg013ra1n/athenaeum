import { ChevronLeft, ChevronRight } from 'lucide-react';
import { format } from 'date-fns';

export type CalendarViewMode = 'month' | 'year';

interface CalendarMonthNavProps {
  year: number;
  month: number; // 1-12
  viewMode: CalendarViewMode;
  onViewModeChange: (mode: CalendarViewMode) => void;
  onPrev: () => void;
  onNext: () => void;
  totalFrameCount: number;
  totalExposureHours: number;
}

export function CalendarMonthNav({
  year,
  month,
  viewMode,
  onViewModeChange,
  onPrev,
  onNext,
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
            onClick={onPrev}
            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
            title={viewMode === 'month' ? 'Previous month' : 'Previous year'}
          >
            <ChevronLeft size={20} />
          </button>
          <button
            onClick={onNext}
            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
            title={viewMode === 'month' ? 'Next month' : 'Next year'}
          >
            <ChevronRight size={20} />
          </button>
        </div>
        <h3 className="text-xl font-semibold min-w-[180px]">
          {viewMode === 'month' ? format(displayDate, 'MMMM yyyy') : year}
        </h3>

        {/* View Mode Toggle */}
        <div className="flex items-center bg-gray-700 rounded-lg p-0.5">
          <button
            onClick={() => onViewModeChange('month')}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              viewMode === 'month'
                ? 'bg-gray-600 text-white'
                : 'text-gray-400 hover:text-gray-200'
            }`}
          >
            Month
          </button>
          <button
            onClick={() => onViewModeChange('year')}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              viewMode === 'year'
                ? 'bg-gray-600 text-white'
                : 'text-gray-400 hover:text-gray-200'
            }`}
          >
            Year
          </button>
        </div>
      </div>

      {/* Stats */}
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
