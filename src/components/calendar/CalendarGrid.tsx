import {
  startOfMonth,
  endOfMonth,
  startOfWeek,
  endOfWeek,
  eachDayOfInterval,
  format,
} from 'date-fns';
import { CalendarDayCell } from './CalendarDayCell';
import type { CalendarDayEvent } from '../../types/models';

interface CalendarGridProps {
  year: number;
  month: number; // 1-12
  events: CalendarDayEvent[];
  onDayClick: (date: string, events: CalendarDayEvent | null, element: HTMLElement) => void;
}

const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

export function CalendarGrid({
  year,
  month,
  events,
  onDayClick,
}: CalendarGridProps) {
  // Create the month date
  const monthDate = new Date(year, month - 1, 1);

  // Get the calendar grid range (includes days from adjacent months)
  const monthStart = startOfMonth(monthDate);
  const monthEnd = endOfMonth(monthDate);
  const calendarStart = startOfWeek(monthStart);
  const calendarEnd = endOfWeek(monthEnd);

  // Get all days in the grid
  const days = eachDayOfInterval({ start: calendarStart, end: calendarEnd });

  // Create a map of date strings to events for quick lookup
  const eventsByDate = new Map<string, CalendarDayEvent>();
  events.forEach((event) => {
    eventsByDate.set(event.date, event);
  });

  return (
    <div className="bg-gray-800 rounded-lg p-4">
      {/* Weekday headers */}
      <div className="grid grid-cols-7 gap-2 mb-2">
        {WEEKDAYS.map((day) => (
          <div
            key={day}
            className="text-center text-sm font-medium text-gray-400 py-2"
          >
            {day}
          </div>
        ))}
      </div>

      {/* Calendar days grid */}
      <div className="grid grid-cols-7 gap-2">
        {days.map((day) => {
          const dateKey = format(day, 'yyyy-MM-dd');
          const dayEvents = eventsByDate.get(dateKey) || null;

          return (
            <CalendarDayCell
              key={dateKey}
              date={day}
              currentMonth={monthDate}
              events={dayEvents}
              onClick={onDayClick}
            />
          );
        })}
      </div>
    </div>
  );
}
