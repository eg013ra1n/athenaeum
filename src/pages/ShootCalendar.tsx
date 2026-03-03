import { useState, useCallback, useMemo } from 'react';
import { useNavigate } from 'react-router-dom';
import { addMonths, subMonths, addYears, subYears } from 'date-fns';
import { Loader2, AlertCircle } from 'lucide-react';
import { useCalendarData } from '../hooks/useCalendarData';
import { useCalendarYearData } from '../hooks/useCalendarYearData';
import { CalendarMonthNav, type CalendarViewMode } from '../components/calendar/CalendarMonthNav';
import { CalendarGrid } from '../components/calendar/CalendarGrid';
import { CalendarYearList } from '../components/calendar/CalendarYearList';
import { CalendarEventPanel } from '../components/calendar/CalendarEventPanel';
import type { CalendarDayEvent } from '../types/models';

export default function ShootCalendar() {
  const navigate = useNavigate();

  // View mode state
  const [viewMode, setViewMode] = useState<CalendarViewMode>('month');

  // Current date state
  const [currentDate, setCurrentDate] = useState(() => new Date());
  const year = currentDate.getFullYear();
  const month = currentDate.getMonth() + 1; // 1-12

  // Fetch calendar data based on view mode
  const monthData = useCalendarData(year, month);
  const yearData = useCalendarYearData(year, viewMode === 'year');

  // Use the appropriate data based on view mode
  const loading = viewMode === 'month' ? monthData.loading : yearData.loading;
  const error = viewMode === 'month' ? monthData.error : yearData.error;

  // Selected day state
  const [selectedDate, setSelectedDate] = useState<string | null>(null);

  // Get events for selected date
  const selectedEvents = useMemo(() => {
    if (!selectedDate) return null;

    if (viewMode === 'month') {
      if (!monthData.data?.days) return null;
      return monthData.data.days.find(d => d.date === selectedDate) || null;
    } else {
      // Year view - search through all months
      if (!yearData.data?.months) return null;
      for (const m of yearData.data.months) {
        const found = m.days.find(d => d.date === selectedDate);
        if (found) return found;
      }
      return null;
    }
  }, [selectedDate, viewMode, monthData.data?.days, yearData.data?.months]);

  // Navigation handlers
  const handlePrev = useCallback(() => {
    if (viewMode === 'month') {
      setCurrentDate((prev) => subMonths(prev, 1));
    } else {
      setCurrentDate((prev) => subYears(prev, 1));
    }
    setSelectedDate(null);
  }, [viewMode]);

  const handleNext = useCallback(() => {
    if (viewMode === 'month') {
      setCurrentDate((prev) => addMonths(prev, 1));
    } else {
      setCurrentDate((prev) => addYears(prev, 1));
    }
    setSelectedDate(null);
  }, [viewMode]);

  // View mode change handler
  const handleViewModeChange = useCallback((mode: CalendarViewMode) => {
    setViewMode(mode);
    setSelectedDate(null);
  }, []);

  // Day click handler
  const handleDayClick = useCallback(
    (date: string, events: CalendarDayEvent | null) => {
      if (events) {
        setSelectedDate(date);
      }
    },
    []
  );

  // Navigation handlers for panel
  const handleNavigateToSkyChart = useCallback(
    (ra: number, dec: number) => {
      navigate(`/skychart?ra=${ra}&dec=${dec}&zoom=800`);
    },
    [navigate]
  );

  const handleNavigateToFrameSet = useCallback(
    (frameSetId: number) => {
      navigate(`/objects/${frameSetId}`);
    },
    [navigate]
  );

  // Calculate totals for display based on view mode
  const totalFrameCount = viewMode === 'month'
    ? monthData.data?.totalFrameCount ?? 0
    : yearData.data?.totalFrameCount ?? 0;
  const totalExposureHours = viewMode === 'month'
    ? (monthData.data?.totalExposureSeconds ?? 0) / 3600
    : (yearData.data?.totalExposureSeconds ?? 0) / 3600;

  return (
    <div className="p-4 pt-3 h-full flex flex-col">
      {/* Header */}
      <div className="flex-shrink-0 mb-2">
        <h2 className="text-2xl font-bold">
          Shoot Calendar
          <span className="text-sm font-normal text-content-muted ml-3">Browse captures by date with equipment and target information</span>
        </h2>
      </div>

      {/* Navigation - always visible */}
      <div className="flex-shrink-0">
        <CalendarMonthNav
          year={year}
          month={month}
          viewMode={viewMode}
          onViewModeChange={handleViewModeChange}
          onPrev={handlePrev}
          onNext={handleNext}
          totalFrameCount={totalFrameCount}
          totalExposureHours={totalExposureHours}
        />
      </div>

      {/* Loading state */}
      {loading && (
        <div className="flex items-center justify-center py-20">
          <Loader2 className="h-8 w-8 animate-spin text-accent" />
          <span className="ml-3 text-content-muted">Loading calendar...</span>
        </div>
      )}

      {/* Error state */}
      {error && (
        <div className="bg-error-muted border border-error/50 rounded-lg p-4 flex items-center gap-3">
          <AlertCircle className="text-error flex-shrink-0" />
          <div>
            <p className="text-error font-medium">Failed to load calendar</p>
            <p className="text-error/70 text-sm">{error}</p>
          </div>
        </div>
      )}

      {/* Calendar content */}
      {!loading && !error && (
        <div className="flex gap-4 flex-1 min-h-0 mt-4">
          {/* Left: Calendar Grid or Year List + Legend */}
          <div className="flex-[2] flex flex-col min-w-0">
            {viewMode === 'month' ? (
              <>
                <div className="flex-1 overflow-auto">
                  <CalendarGrid
                    year={year}
                    month={month}
                    events={monthData.data?.days ?? []}
                    selectedDate={selectedDate}
                    onDayClick={handleDayClick}
                  />
                </div>

                {/* Legend */}
                <div className="flex-shrink-0 mt-4 flex items-center gap-6 text-sm text-content-muted">
                  <div className="flex items-center gap-2">
                    <span className="w-3 h-3 rounded-full bg-accent" />
                    <span>Frame sets</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="w-3 h-3 rounded-full bg-warning" />
                    <span>Unorganized frames</span>
                  </div>
                </div>
              </>
            ) : (
              <>
                {yearData.data && (
                  <CalendarYearList
                    data={yearData.data}
                    selectedDate={selectedDate}
                    onDayClick={handleDayClick}
                  />
                )}

                {/* Legend for year view */}
                <div className="flex-shrink-0 mt-4 flex items-center gap-6 text-sm text-content-muted">
                  <div className="flex items-center gap-2">
                    <span className="w-3 h-3 rounded-full bg-accent" />
                    <span>Frame sets</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="w-3 h-3 rounded-full bg-warning" />
                    <span>Unorganized frames</span>
                  </div>
                </div>
              </>
            )}
          </div>

          {/* Right: Details Panel */}
          <div className="w-80 flex-shrink-0">
            <CalendarEventPanel
              selectedDate={selectedDate}
              events={selectedEvents}
              onNavigateToSkyChart={handleNavigateToSkyChart}
              onNavigateToFrameSet={handleNavigateToFrameSet}
            />
          </div>
        </div>
      )}
    </div>
  );
}
