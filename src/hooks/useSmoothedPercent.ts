import { useEffect, useRef, useState } from 'react';

/**
 * Smooths a step-wise percent (0-100) value that updates once per completed
 * backend unit (e.g. a solved frame). Between real updates the displayed
 * value creeps toward the next per-unit milestone so the progress bar feels
 * alive during long-running operations. Never moves backward, never
 * overshoots the next unit, and snaps up when a real update arrives.
 *
 * @param realPercent The actual percent from the backend (0-100).
 * @param total       Total number of units (used to compute step size).
 *                    Pass 0 to freeze the displayed value at 0.
 * @param estimatedMsPerUnit Roughly how long a unit takes; controls creep
 *                    speed. Defaults to 1500 ms (tuned for plate solving).
 */
export function useSmoothedPercent(
  realPercent: number,
  total: number,
  estimatedMsPerUnit = 1500,
): number {
  const [displayed, setDisplayed] = useState(realPercent);
  const displayedRef = useRef(realPercent);
  displayedRef.current = displayed;

  // Reset when total changes — a new batch/operation is starting.
  const lastTotalRef = useRef(total);
  useEffect(() => {
    if (total !== lastTotalRef.current) {
      setDisplayed(0);
      displayedRef.current = 0;
      lastTotalRef.current = total;
    }
  }, [total]);

  useEffect(() => {
    if (total <= 0) return;
    const stepPct = 100 / total;
    const nextMilestone = Math.min(100, realPercent + stepPct);
    // Leave a small sliver so the bar never claims the next unit is done
    // before the backend confirms it.
    const creepTarget = nextMilestone - stepPct * 0.05;

    let raf = 0;
    let last: number | null = null;

    const tick = (t: number) => {
      if (last == null) last = t;
      const dt = t - last;
      last = t;

      const prev = displayedRef.current;
      let next = prev;
      if (prev < realPercent) {
        next = realPercent;
      } else if (prev < creepTarget) {
        next = Math.min(prev + (stepPct / estimatedMsPerUnit) * dt, creepTarget);
      }
      if (next !== prev) {
        displayedRef.current = next;
        setDisplayed(next);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [realPercent, total, estimatedMsPerUnit]);

  return displayed;
}
