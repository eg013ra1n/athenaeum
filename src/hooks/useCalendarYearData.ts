import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { CalendarMonthData, CalendarYearData } from '../types/models';

interface UseCalendarYearDataResult {
  data: CalendarYearData | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch calendar data for an entire year
 * Fetches all 12 months in parallel and combines results
 * @param year - The year (e.g., 2025)
 * @param enabled - Whether to fetch data (default true)
 */
export function useCalendarYearData(year: number, enabled: boolean = true): UseCalendarYearDataResult {
  const [data, setData] = useState<CalendarYearData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    if (!enabled) return;

    try {
      setLoading(true);
      setError(null);

      // Fetch all 12 months in parallel
      const monthPromises = Array.from({ length: 12 }, (_, i) =>
        invoke<CalendarMonthData>('get_calendar_month_data', {
          year,
          month: i + 1,
        })
      );

      const monthResults = await Promise.all(monthPromises);

      // Filter to only months with data and calculate totals
      const monthsWithData = monthResults.filter(
        (m) => m.days.length > 0
      );

      const totalFrameCount = monthsWithData.reduce(
        (sum, m) => sum + m.totalFrameCount,
        0
      );
      const totalExposureSeconds = monthsWithData.reduce(
        (sum, m) => sum + m.totalExposureSeconds,
        0
      );

      setData({
        year,
        months: monthsWithData,
        totalFrameCount,
        totalExposureSeconds,
      });
    } catch (err) {
      setError(err as string);
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [year, enabled]);

  useEffect(() => {
    if (enabled) {
      loadData();
    }
  }, [loadData, enabled]);

  return { data, loading, error, refresh: loadData };
}
