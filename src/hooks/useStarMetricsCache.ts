import { useEffect, useRef, useState, useCallback } from "react";
import { api } from "../api";
import type { StarMetricsResponse } from "../types/models";

interface UseStarMetricsCacheArgs {
  frameIds: number[];
  currentIndex: number;
  enabled: boolean;
}

interface UseStarMetricsCacheResult {
  getMetrics: (index: number) => StarMetricsResponse | null;
  isLoading: boolean;
}

const MAX_CONCURRENT = 2;

export function useStarMetricsCache({
  frameIds,
  currentIndex,
  enabled,
}: UseStarMetricsCacheArgs): UseStarMetricsCacheResult {
  const cacheRef = useRef(new Map<number, StarMetricsResponse>());
  const inflightRef = useRef(new Set<number>());
  const unmountedRef = useRef(false);
  const [isLoading, setIsLoading] = useState(false);
  const [cacheVersion, setCacheVersion] = useState(0);

  const getMetrics = useCallback((index: number): StarMetricsResponse | null => {
    return cacheRef.current.get(index) ?? null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheVersion]);

  const fetchMetrics = useCallback(async (index: number) => {
    const frameId = frameIds[index];
    if (!frameId || cacheRef.current.has(index) || inflightRef.current.has(index)) return;

    inflightRef.current.add(index);
    try {
      const response = await api.invoke<StarMetricsResponse>("get_frame_star_metrics", {
        frameId,
      });
      if (!unmountedRef.current) {
        cacheRef.current.set(index, response);
        setCacheVersion((v) => v + 1);
      }
    } catch (err) {
      console.error(`Failed to load star metrics for frame ${frameId}:`, err);
    } finally {
      inflightRef.current.delete(index);
    }
  }, [frameIds]);

  useEffect(() => {
    if (!enabled || frameIds.length === 0) return;

    unmountedRef.current = false;
    setIsLoading(!cacheRef.current.has(currentIndex));

    const priorities: number[] = [currentIndex];
    const total = frameIds.length;
    for (let offset = 1; offset < total && priorities.length < MAX_CONCURRENT; offset++) {
      const idx = (currentIndex + offset) % total;
      if (!cacheRef.current.has(idx) && !inflightRef.current.has(idx)) {
        priorities.push(idx);
      }
    }

    for (const idx of priorities) {
      fetchMetrics(idx).then(() => {
        if (idx === currentIndex && !unmountedRef.current) {
          setIsLoading(false);
        }
      });
    }

    return () => {
      unmountedRef.current = true;
    };
  }, [enabled, currentIndex, frameIds, fetchMetrics]);

  return { getMetrics, isLoading };
}
