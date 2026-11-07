import { useEffect, useRef, useState, useCallback } from "react";
import { api } from "../api";
import type { CollabFrameRef, StarMetricsResponse } from "../types/models";

/** Where an entry's stars come from: its catalog frame (the stored analysis, else on demand), a project frame by its
 *  ref (on demand, never stored), or nothing — an entry with neither, which is logged once and draws no star. */
export type StarSource = { frameId: number } | { projectId: string; frame: CollabFrameRef } | null;

interface UseStarMetricsCacheArgs {
  /** One source per entry, by index. */
  sources: StarSource[];
  currentIndex: number;
  enabled: boolean;
}

const describe = (source: NonNullable<StarSource>): string =>
  "frameId" in source
    ? `frame ${source.frameId}`
    : `project ${source.projectId} frame ${source.frame.frameUuid ?? source.frame.frameId}`;

interface UseStarMetricsCacheResult {
  getMetrics: (index: number) => StarMetricsResponse | null;
  isLoading: boolean;
}

const MAX_CONCURRENT = 2;

export function useStarMetricsCache({
  sources,
  currentIndex,
  enabled,
}: UseStarMetricsCacheArgs): UseStarMetricsCacheResult {
  const cacheRef = useRef(new Map<number, StarMetricsResponse>());
  const inflightRef = useRef(new Set<number>());
  // Entries already reported as having no source, so the warning is not repeated on every step.
  const unsourcedRef = useRef(new Set<number>());
  const unmountedRef = useRef(false);
  const [isLoading, setIsLoading] = useState(false);
  const [cacheVersion, setCacheVersion] = useState(0);

  const getMetrics = useCallback((index: number): StarMetricsResponse | null => {
    return cacheRef.current.get(index) ?? null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheVersion]);

  const fetchMetrics = useCallback(async (index: number) => {
    const source = sources[index];
    if (source === undefined || cacheRef.current.has(index) || inflightRef.current.has(index)) return;
    if (source === null) {
      if (!unsourcedRef.current.has(index)) {
        unsourcedRef.current.add(index);
        console.warn(`[useStarMetricsCache] entry ${index} has no catalog frame and no project ref: no stars to show`);
      }
      return;
    }

    inflightRef.current.add(index);
    try {
      const response =
        "frameId" in source
          ? await api.invoke<StarMetricsResponse>("get_frame_star_metrics", { frameId: source.frameId })
          : await api.invoke<StarMetricsResponse>("get_collab_frame_star_metrics", {
              projectId: source.projectId,
              frame: source.frame,
            });
      if (!unmountedRef.current) {
        cacheRef.current.set(index, response);
        setCacheVersion((v) => v + 1);
      }
    } catch (err) {
      console.error(`Failed to load star metrics for ${describe(source)}:`, err);
    } finally {
      inflightRef.current.delete(index);
    }
  }, [sources]);

  useEffect(() => {
    if (!enabled || sources.length === 0) return;

    unmountedRef.current = false;
    setIsLoading(!cacheRef.current.has(currentIndex));

    const priorities: number[] = [currentIndex];
    const total = sources.length;
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
  }, [enabled, currentIndex, sources, fetchMetrics]);

  return { getMetrics, isLoading };
}
