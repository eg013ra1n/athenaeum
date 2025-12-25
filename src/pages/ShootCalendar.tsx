import { useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { addMonths, subMonths } from 'date-fns';
import { Loader2, AlertCircle } from 'lucide-react';
import { useCalendarData } from '../hooks/useCalendarData';
import { CalendarMonthNav } from '../components/calendar/CalendarMonthNav';
import { CalendarGrid } from '../components/calendar/CalendarGrid';
import { CalendarEventPopup } from '../components/calendar/CalendarEventPopup';
import type { CalendarDayEvent } from '../types/models';

interface PopupState {
  date: string;
  events: CalendarDayEvent;
  element: HTMLElement;
}

export default function ShootCalendar() {
  const navigate = useNavigate();

  // Current month state
  const [currentDate, setCurrentDate] = useState(() => new Date());
  const year = currentDate.getFullYear();
  const month = currentDate.getMonth() + 1; // 1-12

  // Fetch calendar data
  const { data, loading, error } = useCalendarData(year, month);

  // Popup state
  const [popup, setPopup] = useState<PopupState | null>(null);

  // Month navigation handlers
  const handlePrevMonth = useCallback(() => {
    setCurrentDate((prev) => subMonths(prev, 1));
    setPopup(null);
  }, []);

  const handleNextMonth = useCallback(() => {
    setCurrentDate((prev) => addMonths(prev, 1));
    setPopup(null);
  }, []);

  const handleToday = useCallback(() => {
    setCurrentDate(new Date());
    setPopup(null);
  }, []);

  // Day click handler
  const handleDayClick = useCallback(
    (date: string, events: CalendarDayEvent | null, element: HTMLElement) => {
      if (events) {
        setPopup({ date, events, element });
      }
    },
    []
  );

  // Navigation handlers
  const handleNavigateToSkyAtlas = useCallback(
    (ra: number, dec: number) => {
      // Navigate to SkyAtlas with coordinates
      navigate(`/skyatlas?ra=${ra}&dec=${dec}&zoom=800`);
    },
    [navigate]
  );

  const handleNavigateToFrameSet = useCallback(
    (frameSetId: number) => {
      navigate(`/objects/${frameSetId}`);
    },
    [navigate]
  );

  const handleClosePopup = useCallback(() => {
    setPopup(null);
  }, []);

  // Calculate totals for display
  const totalFrameCount = data?.totalFrameCount ?? 0;
  const totalExposureHours = (data?.totalExposureSeconds ?? 0) / 3600;

  return (
    <div className="p-6">
      {/* Header */}
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">Shoot Calendar</h2>
        <p className="text-gray-400">
          Browse captures by date with equipment and target information
        </p>
      </div>

      {/* Loading state */}
      {loading && (
        <div className="flex items-center justify-center py-20">
          <Loader2 className="h-8 w-8 animate-spin text-blue-500" />
          <span className="ml-3 text-gray-400">Loading calendar...</span>
        </div>
      )}

      {/* Error state */}
      {error && (
        <div className="bg-red-900/20 border border-red-500/50 rounded-lg p-4 flex items-center gap-3">
          <AlertCircle className="text-red-500 flex-shrink-0" />
          <div>
            <p className="text-red-400 font-medium">Failed to load calendar</p>
            <p className="text-red-400/70 text-sm">{error}</p>
          </div>
        </div>
      )}

      {/* Calendar content */}
      {!loading && !error && (
        <>
          {/* Month navigation */}
          <CalendarMonthNav
            year={year}
            month={month}
            onPrevMonth={handlePrevMonth}
            onNextMonth={handleNextMonth}
            onToday={handleToday}
            totalFrameCount={totalFrameCount}
            totalExposureHours={totalExposureHours}
          />

          {/* Calendar grid */}
          <CalendarGrid
            year={year}
            month={month}
            events={data?.days ?? []}
            onDayClick={handleDayClick}
          />

          {/* Legend */}
          <div className="mt-4 flex items-center gap-6 text-sm text-gray-400">
            <div className="flex items-center gap-2">
              <span className="w-3 h-3 rounded-full bg-blue-500" />
              <span>Frame sets</span>
            </div>
            <div className="flex items-center gap-2">
              <span className="w-3 h-3 rounded-full bg-yellow-500" />
              <span>Unorganized frames</span>
            </div>
          </div>
        </>
      )}

      {/* Event popup */}
      {popup && (
        <CalendarEventPopup
          date={popup.date}
          events={popup.events}
          anchorElement={popup.element}
          onClose={handleClosePopup}
          onNavigateToSkyAtlas={handleNavigateToSkyAtlas}
          onNavigateToFrameSet={handleNavigateToFrameSet}
        />
      )}
    </div>
  );
}
