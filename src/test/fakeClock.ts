// Deterministic time for component tests that assert on a debounced write
// (`useAutosaveDocument`'s debounce, `useSettingField.setValue`'s). A
// real-timer `waitFor` races such a debounce against its own 1000 ms budget
// and fails when the worker is starved (seen under a parallel Rust build);
// with `vi.useFakeTimers()` in the test file, `advance()` moves the clock
// explicitly instead, so the assertion that follows sees the settled state
// whatever the machine load.
import { vi } from 'vitest';
import { act } from '@testing-library/react';

/** Far past any settings debounce — advancing this much means a write that is
 *  going to happen has happened, and a second (late) write would show too. */
export const PAST_THE_DEBOUNCE_MS = 5_000;

const SLICE_MS = 100;

/** Advances the fake clock by `ms` in `act`-wrapped slices. The async advance
 *  yields to the real event loop, so mocked `api.invoke` promises settle; each
 *  `act` exit flushes the renders and effects they cause before the next
 *  slice, so a timer an effect schedules mid-advance (the debounce behind a
 *  late second patch, say) still runs within the same advance — as real time
 *  would. `advance(0)` is one flush of pending promises and renders. */
export async function advance(ms: number): Promise<void> {
  let left = ms;
  do {
    const step = Math.min(SLICE_MS, left);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(step);
    });
    left -= step;
  } while (left > 0);
}
