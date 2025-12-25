import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { CalendarMonthData } from '../types/models';

interface UseCalendarDataResult {
  data: CalendarMonthData | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch calendar data for a specific month
 * @param year - The year (e.g., 2025)
 * @param month - The month (1-12)
 */
export function useCalendarData(year: number, month: number): UseCalendarDataResult {
  const [data, setData] = useState<CalendarMonthData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<CalendarMonthData>('get_calendar_month_data', {
        year,
        month,
      });
      setData(result);
    } catch (err) {
      setError(err as string);
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [year, month]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  return { data, loading, error, refresh: loadData };
}
