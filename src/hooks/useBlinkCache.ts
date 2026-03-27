import { useEffect, useRef, useState, useCallback } from "react";
import type { FileWithFrame } from "../types/models";

interface UseBlinkCacheArgs {
  frames: FileWithFrame[];
  currentIndex: number;
  cacheModeReady: boolean;
  maxConcurrent: number;
  loadedImages: Map<number, string>;
  loadImage: (index: number) => Promise<void>;
}

interface UseBlinkCacheResult {
  isCaching: boolean;
  cacheProgress: { current: number; total: number };
  cacheStats: { elapsedMs: number; frameCount: number } | null;
}

const DEFAULT_MAX_CONCURRENT = 8;

/**
 * Priority-queue caching controller for BlinkViewer plain images.
 *
 * Manages a single pool of MAX_CONCURRENT in-flight slots.
 * Priority: current frame first, then by proximity.
 */
export function useBlinkCache({
  frames,
  currentIndex,
  cacheModeReady,
  maxConcurrent,
  loadedImages,
  loadImage,
}: UseBlinkCacheArgs): UseBlinkCacheResult {
  const [isCaching, setIsCaching] = useState(false);
  const [cacheProgress, setCacheProgress] = useState({ current: 0, total: 0 });
  const [cacheStats, setCacheStats] = useState<{ elapsedMs: number; frameCount: number } | null>(null);

  const framesRef = useRef(frames);
  const currentIndexRef = useRef(currentIndex);
  const loadedImagesRef = useRef(loadedImages);
  const loadImageRef = useRef(loadImage);

  framesRef.current = frames;
  currentIndexRef.current = currentIndex;
  loadedImagesRef.current = loadedImages;
  loadImageRef.current = loadImage;

  const dispatchedRef = useRef(new Set<number>());
  const inflightCountRef = useRef(0);
  const cacheStartTimeRef = useRef(0);
  const unmountedRef = useRef(false);

  const pickNextJob = useCallback((): number | null => {
    const total = framesRef.current.length;
    if (total === 0) return null;

    const ci = currentIndexRef.current;

    const isAvailable = (idx: number) =>
      !loadedImagesRef.current.has(idx) && !dispatchedRef.current.has(idx);

    if (isAvailable(ci)) return ci;

    for (let offset = 1; offset < total; offset++) {
      const idx = (ci + offset) % total;
      if (isAvailable(idx)) return idx;
    }

    return null;
  }, []);

  const tryDispatch = useCallback(() => {
    if (unmountedRef.current) return;

    const limit = maxConcurrent || DEFAULT_MAX_CONCURRENT;
    while (inflightCountRef.current < limit) {
      const idx = pickNextJob();
      if (idx === null) break;

      dispatchedRef.current.add(idx);
      inflightCountRef.current++;

      loadImageRef.current(idx).finally(() => {
        inflightCountRef.current--;
        tryDispatch();
      });
    }
  }, [pickNextJob, maxConcurrent]);

  useEffect(() => {
    if (!isCaching) return;
    const total = frames.length;
    if (total === 0) return;
    setCacheProgress({ current: loadedImages.size, total });
    if (loadedImages.size >= total) {
      setIsCaching(false);
      if (cacheStartTimeRef.current > 0) {
        setCacheStats({
          elapsedMs: Date.now() - cacheStartTimeRef.current,
          frameCount: total,
        });
      }
    }
  }, [loadedImages, frames.length, isCaching]);

  useEffect(() => {
    if (!cacheModeReady || frames.length === 0) return;

    unmountedRef.current = false;
    dispatchedRef.current = new Set();
    inflightCountRef.current = 0;
    cacheStartTimeRef.current = Date.now();
    setIsCaching(true);
    setCacheStats(null);
    setCacheProgress({ current: 0, total: frames.length });

    tryDispatch();

    return () => {
      unmountedRef.current = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheModeReady, frames.length]);

  useEffect(() => {
    tryDispatch();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentIndex]);

  return { isCaching, cacheProgress, cacheStats };
}
