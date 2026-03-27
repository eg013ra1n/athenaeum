import { useEffect, useRef, useState, useCallback } from "react";
import type { FileWithFrame } from "../types/models";

interface UseBlinkCacheArgs {
  frames: FileWithFrame[];
  currentIndex: number;
  showAnnotations: boolean;
  cacheModeReady: boolean;
  maxConcurrent: number;
  loadedImages: Map<number, string>;
  annotatedImages: Map<number, string>;
  loadImage: (index: number) => Promise<void>;
  loadAnnotatedImage: (index: number) => Promise<void>;
}

interface UseBlinkCacheResult {
  isCaching: boolean;
  cacheProgress: { current: number; total: number };
  cacheStats: { elapsedMs: number; frameCount: number } | null;
}

const DEFAULT_MAX_CONCURRENT = 8;

type JobType = "plain" | "annotated";

/**
 * Unified priority-queue caching controller for BlinkViewer.
 *
 * Manages a single pool of MAX_CONCURRENT in-flight slots across both
 * plain and annotated image loading. Priority lanes:
 *   0 (CURRENT)          – current frame for the active display mode
 *   1 (ACTIVE_PREFETCH)  – same mode, other frames, by proximity
 *   2 (BACKGROUND)       – inactive mode frames, by proximity
 */
export function useBlinkCache({
  frames,
  currentIndex,
  showAnnotations,
  cacheModeReady,
  maxConcurrent,
  loadedImages,
  annotatedImages,
  loadImage,
  loadAnnotatedImage,
}: UseBlinkCacheArgs): UseBlinkCacheResult {
  const [isCaching, setIsCaching] = useState(false);
  const [cacheProgress, setCacheProgress] = useState({ current: 0, total: 0 });
  const [cacheStats, setCacheStats] = useState<{ elapsedMs: number; frameCount: number } | null>(null);

  // Refs for fresh reads inside dispatch callbacks
  const framesRef = useRef(frames);
  const currentIndexRef = useRef(currentIndex);
  const showAnnotationsRef = useRef(showAnnotations);
  const loadedImagesRef = useRef(loadedImages);
  const annotatedImagesRef = useRef(annotatedImages);
  const loadImageRef = useRef(loadImage);
  const loadAnnotatedImageRef = useRef(loadAnnotatedImage);

  framesRef.current = frames;
  currentIndexRef.current = currentIndex;
  showAnnotationsRef.current = showAnnotations;
  loadedImagesRef.current = loadedImages;
  annotatedImagesRef.current = annotatedImages;
  loadImageRef.current = loadImage;
  loadAnnotatedImageRef.current = loadAnnotatedImage;

  // Track every job ever dispatched (never removed — prevents re-dispatch
  // when React state refs are stale between .finally() and next render)
  const dispatchedRef = useRef(new Set<string>());
  // Separate counter for concurrency limiting
  const inflightCountRef = useRef(0);
  const completedRef = useRef(0);
  const cacheStartTimeRef = useRef(0);
  // Guard against running after unmount
  const unmountedRef = useRef(false);

  const jobKey = (type: JobType, index: number) => `${type}:${index}`;

  const isCached = useCallback((type: JobType, index: number): boolean => {
    if (type === "plain") return loadedImagesRef.current.has(index);
    return annotatedImagesRef.current.has(index);
  }, []);

  /**
   * Pick the single best job to dispatch next.
   * Returns null when everything is cached or in-flight.
   */
  const pickNextJob = useCallback((): { type: JobType; index: number } | null => {
    const total = framesRef.current.length;
    if (total === 0) return null;

    const ci = currentIndexRef.current;
    const annOn = showAnnotationsRef.current;
    const activeType: JobType = annOn ? "annotated" : "plain";
    const bgType: JobType = annOn ? "plain" : "annotated";

    const isAvailable = (type: JobType, idx: number) =>
      !isCached(type, idx) && !dispatchedRef.current.has(jobKey(type, idx));

    // Check if current frame in active mode needs loading (priority 0)
    if (isAvailable(activeType, ci)) {
      return { type: activeType, index: ci };
    }

    // Priority 1: active mode, other frames, by proximity
    for (let offset = 1; offset < total; offset++) {
      const idx = (ci + offset) % total;
      if (isAvailable(activeType, idx)) {
        return { type: activeType, index: idx };
      }
    }

    // Priority 2: background mode, current frame first, then by proximity
    if (isAvailable(bgType, ci)) {
      return { type: bgType, index: ci };
    }
    for (let offset = 1; offset < total; offset++) {
      const idx = (ci + offset) % total;
      if (isAvailable(bgType, idx)) {
        return { type: bgType, index: idx };
      }
    }

    return null;
  }, [isCached]);

  const updateProgress = useCallback(() => {
    if (unmountedRef.current) return;
    const total = framesRef.current.length;
    // Count cached frames in each mode separately
    let plainCached = 0;
    let annotatedCached = 0;
    for (let i = 0; i < total; i++) {
      if (loadedImagesRef.current.has(i)) plainCached++;
      if (annotatedImagesRef.current.has(i)) annotatedCached++;
    }
    // Show progress for the active mode (what the user sees)
    const annOn = showAnnotationsRef.current;
    const activeCached = annOn ? annotatedCached : plainCached;
    setCacheProgress({ current: activeCached, total });

    // Hide spinner once the active mode is fully cached.
    // Background mode continues caching silently.
    if (activeCached >= total) {
      setIsCaching(false);
      if (cacheStartTimeRef.current > 0) {
        setCacheStats({
          elapsedMs: Date.now() - cacheStartTimeRef.current,
          frameCount: total,
        });
      }
    }
  }, []);

  /**
   * Fill idle concurrency slots with the best available jobs.
   * Self-sustaining: each completed job calls tryDispatch() via .finally().
   */
  const tryDispatch = useCallback(() => {
    if (unmountedRef.current) return;

    const limit = maxConcurrent || DEFAULT_MAX_CONCURRENT;
    while (inflightCountRef.current < limit) {
      const job = pickNextJob();
      if (!job) break;

      const key = jobKey(job.type, job.index);
      dispatchedRef.current.add(key);
      inflightCountRef.current++;

      const loader = job.type === "plain"
        ? loadImageRef.current(job.index)
        : loadAnnotatedImageRef.current(job.index);

      loader.finally(() => {
        inflightCountRef.current--;
        completedRef.current++;
        updateProgress();
        // Fill the freed slot
        tryDispatch();
      });
    }

    updateProgress();
  }, [pickNextJob, updateProgress]);

  // Main effect: start the caching loop once cacheModeReady and frames are available
  useEffect(() => {
    if (!cacheModeReady || frames.length === 0) return;

    unmountedRef.current = false;
    dispatchedRef.current = new Set();
    inflightCountRef.current = 0;
    completedRef.current = 0;
    cacheStartTimeRef.current = Date.now();
    setIsCaching(true);
    setCacheStats(null);
    setCacheProgress({ current: 0, total: frames.length });

    tryDispatch();

    return () => {
      unmountedRef.current = true;
    };
    // Only re-run when caching becomes ready or frame count changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheModeReady, frames.length]);

  // Kick effect: re-evaluate priorities when annotations toggle or user navigates
  useEffect(() => {
    tryDispatch();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAnnotations, currentIndex]);

  return { isCaching, cacheProgress, cacheStats };
}
