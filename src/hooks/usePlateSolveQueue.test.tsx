import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { usePlateSolveQueue } from './usePlateSolveQueue';
import { api } from '../api';
import { routeInvoke } from '../test/apiMock';
import type { PlateSolveProgressEvent } from '../types/helpers';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api');

type Handlers = Record<string, (p: unknown) => void>;

/** Capture the listeners the hook registers, by event name. */
function captureListeners(): Handlers {
  const handlers: Handlers = {};
  vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
    handlers[ev] = cb;
    return Promise.resolve(() => {});
  }) as never);
  return handlers;
}

const activity = (over: Record<string, unknown> = {}) => ({
  running: false, origin: null, runId: null, done: 0, total: 0, currentFile: null, ...over,
});
const batchCalls = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'plate_solve_batch');

beforeEach(() => {
  notifyMock.mockReset();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
});
afterEach(() => vi.useRealTimers());

describe('usePlateSolveQueue', () => {
  it('progress never steps back across parallel workers and labels the frame by file name', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    // The batch stays running until the test is done with it.
    routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: activity() }, () => new Promise(() => {}));

    const { result } = renderHook(() => usePlateSolveQueue());
    await waitFor(() => expect(handlers['plate-solve-progress']).toBeDefined());
    let batchId = -1;
    act(() => {
      batchId = result.current.enqueuePlateSolve([1, 2, 3]);
    });
    await waitFor(() =>
      expect(vi.mocked(api.invoke)).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [1, 2, 3] }),
    );

    const emit = (e: Partial<PlateSolveProgressEvent>) =>
      act(() => handlers['plate-solve-progress']({ total: 3, ...e }));
    emit({ frame_id: 1, current: 0, status: 'solving', filename: 'a.fits' });
    emit({ frame_id: 2, current: 0, status: 'solving', filename: 'b.fits' });
    emit({ frame_id: 1, current: 1, status: 'solved', matched_stars: 10, rms_arcsec: 1, filename: 'a.fits' });
    emit({ frame_id: 2, current: 2, status: 'solved', matched_stars: 10, rms_arcsec: 1, filename: 'b.fits' });
    // Worker 1 read the count (1) before worker 2 finished; its event lands late.
    emit({ frame_id: 3, current: 1, status: 'solving', filename: 'c.fits' });

    const batch = result.current.activeBatches.get(batchId)!;
    expect(batch.progress).toEqual({ current: 2, total: 3 });
    expect(batch.currentFilename).toBe('c.fits');

    // A finished frame does not replace the label of the one being solved.
    emit({ frame_id: 2, current: 2, status: 'failed', error: 'x', filename: 'b.fits' });
    expect(result.current.activeBatches.get(batchId)!.currentFilename).toBe('c.fits');
  });

  it('a batch command that returns at once (web) still tracks progress until plate-solve-complete', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: activity() }, () => undefined);

    const { result } = renderHook(() => usePlateSolveQueue());
    await waitFor(() => expect(handlers['plate-solve-complete']).toBeDefined());
    let batchId = -1;
    act(() => {
      batchId = result.current.enqueuePlateSolve([1, 2]);
    });
    await waitFor(() =>
      expect(vi.mocked(api.invoke)).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [1, 2] }),
    );
    await act(async () => {});

    act(() =>
      handlers['plate-solve-progress']({
        frame_id: 1, current: 1, total: 2, status: 'solved', matched_stars: 5, rms_arcsec: 1,
      }),
    );
    expect(result.current.activeBatches.get(batchId)!.progress).toEqual({ current: 1, total: 2 });
    expect(result.current.queueLength).toBe(1);

    await act(async () => {
      handlers['plate-solve-complete']({ solved: 2, failed: 0, total: 2, total_time_ms: 5 });
    });
    await waitFor(() => expect(result.current.queueLength).toBe(0));
    expect(result.current.activeBatches.get(batchId)!.isComplete).toBe(true);
  });

  describe('the assistant\'s plate solve and the one slot', () => {
    const BUSY_DETAIL = 'A plate solve started from the assistant is running (3 of 10 frames). Wait for it or cancel it.';

    it('assistant_run_shows_progress', async () => {
      const handlers = captureListeners();
      routeInvoke({
        get_plate_solve_activity: () =>
          activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 2, total: 10, currentFile: 'b.fits' }),
      });
      const { result } = renderHook(() => usePlateSolveQueue());
      // Read on mount: a run that started before this page opened shows at once.
      await waitFor(() =>
        expect(result.current.foreignRun).toEqual({
          runId: 'solve-a', origin: 'assistant', done: 2, total: 10, currentFile: 'b.fits', isCancelling: false,
        }),
      );
      // Progress events move it; a late event never steps the counter back.
      await waitFor(() => expect(handlers['plate-solve-progress']).toBeDefined());
      act(() => handlers['plate-solve-progress']({ frame_id: 4, current: 5, total: 10, status: 'solving', filename: 'e.fits' }));
      act(() => handlers['plate-solve-progress']({ frame_id: 3, current: 4, total: 10, status: 'solving', filename: 'd.fits' }));
      expect(result.current.foreignRun).toMatchObject({ done: 5, total: 10, currentFile: 'd.fits' });
      // Progress is never notified.
      expect(notifyMock).not.toHaveBeenCalled();
      // Cleared on complete.
      act(() => handlers['plate-solve-complete']({ solved: 10, failed: 0, total: 10, total_time_ms: 5 }));
      expect(result.current.foreignRun).toBeNull();
    });

    it('a_run_started_after_the_page_opened_shows_from_the_started_event', async () => {
      const handlers = captureListeners();
      routeInvoke({ get_plate_solve_activity: () => activity() });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-started']).toBeDefined());
      expect(result.current.foreignRun).toBeNull();
      // A run begun in Athenaeum that this queue did not start (My Frames, another tab) is shown too.
      act(() => handlers['plate-solve-started']({ origin: 'ui', runId: 'solve-u', total: 3 }));
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-u', origin: 'ui', total: 3 });
      act(() => handlers['plate-solve-started']({ origin: 'assistant', runId: 'solve-b', total: 7 }));
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-b', done: 0, total: 7, currentFile: null });
    });

    it('own_batch_refused_with_notify_naming_the_assistant_run', async () => {
      const handlers = captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 3, total: 10 });
      routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => current });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-complete']).toBeDefined());
      act(() => { result.current.enqueuePlateSolve([1, 2]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      expect(notifyMock).toHaveBeenCalledWith(
        expect.objectContaining({
          title: 'Plate solve not started',
          detail: BUSY_DETAIL,
          tone: 'warning',
          dedupeKey: 'plate-solve-busy-solve-a',
        }),
      );
      // Nothing was started, and the batch is still queued (not failed).
      expect(batchCalls()).toHaveLength(0);
      expect(result.current.queueLength).toBe(1);
      expect([...result.current.activeBatches.values()][0].isComplete).toBe(false);

      // The run ends: the queued batch starts.
      current = activity();
      await act(async () => handlers['plate-solve-complete']({ solved: 10, failed: 0, total: 10, total_time_ms: 5 }));
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
      expect(batchCalls()[0][1]).toEqual({ frameIds: [1, 2] });
      // One more notification: the end of the assistant's run, whose line was shown.
      expect(notifyMock).toHaveBeenCalledTimes(2);
      expect(notifyMock.mock.calls[1][0]).toMatchObject({ dedupeKey: 'plate-solve-done-solve-a' });
    });

    it('busy_is_decided_by_the_activity_not_the_error_text', async () => {
      captureListeners();
      // The activity is idle when the batch starts and running when the backend refuses it, in other words.
      let current = activity();
      routeInvoke(
        {
          get_catalog_status: [{ installed: true }],
          get_plate_solve_activity: () => current,
          plate_solve_batch: () => {
            current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 3, total: 10 });
            return Promise.reject('slot taken: please retry later');
          },
        },
      );
      const { result } = renderHook(() => usePlateSolveQueue());
      act(() => { result.current.enqueuePlateSolve([1]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Plate solve not started', detail: BUSY_DETAIL, dedupeKey: 'plate-solve-busy-solve-a' });
      expect(result.current.queueLength).toBe(1);
      expect([...result.current.activeBatches.values()][0]).toMatchObject({ isComplete: false, errorMessage: null });
    });

    it('a_rejection_while_the_activity_is_idle_is_no_busy_refusal', async () => {
      captureListeners();
      // Wording that looks like a busy slot, but the activity says nothing runs: a plain failure.
      routeInvoke({
        get_catalog_status: [{ installed: true }],
        get_plate_solve_activity: () => activity(),
        plate_solve_batch: () => Promise.reject('A plate solve started from the assistant is running (1 of 2 frames).'),
      });
      const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
      const { result } = renderHook(() => usePlateSolveQueue());
      let id = -1;
      act(() => { id = result.current.enqueuePlateSolve([1]); });
      await waitFor(() => expect(result.current.activeBatches.get(id)?.isComplete).toBe(true));
      expect(result.current.activeBatches.get(id)!.errorMessage).toContain('1 of 2 frames');
      expect(notifyMock).not.toHaveBeenCalled();
      expect(result.current.queueLength).toBe(0);
      errors.mockRestore();
    });

    it('a_batch_waits_while_another_ui_batch_runs', async () => {
      const handlers = captureListeners();
      let current = activity({ running: true, origin: 'ui', runId: 'solve-u', done: 2, total: 5 });
      routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => current });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-complete']).toBeDefined());
      act(() => { result.current.enqueuePlateSolve([7]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      expect(notifyMock.mock.calls[0][0]).toMatchObject({
        title: 'Plate solve not started',
        detail: 'A plate solve batch is already running (2 of 5 frames). Wait for it or cancel it.',
        tone: 'warning',
        dedupeKey: 'plate-solve-busy-solve-u',
      });
      expect(batchCalls()).toHaveLength(0);
      expect(result.current.queueLength).toBe(1);
      // A run this page did not start: shown, with its origin.
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-u', origin: 'ui', done: 2, total: 5 });
      current = activity();
      await act(async () => handlers['plate-solve-complete']({ solved: 5, failed: 0, total: 5, total_time_ms: 5 }));
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
    });

    it('a_held_batch_starts_when_a_re_read_finds_the_slot_free_without_the_event', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'ui', runId: 'solve-u', done: 1, total: 5 });
      routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => current });
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      const { result } = renderHook(() => usePlateSolveQueue());
      await tick(0);
      act(() => { result.current.enqueuePlateSolve([7]); });
      await tick(0);
      expect(notifyMock).toHaveBeenCalledTimes(1);
      await tick(3100);
      // Still running: it waits, and does not say so again.
      expect(batchCalls()).toHaveLength(0);
      expect(notifyMock).toHaveBeenCalledTimes(1);
      current = activity();
      await tick(3100);
      expect(batchCalls()).toHaveLength(1);
    });

    it('cancel_from_the_ui_cancels_the_assistant_run', async () => {
      captureListeners();
      routeInvoke({
        get_plate_solve_activity: () => activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 }),
        cancel_plate_solve: () => undefined,
      });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.foreignRun).not.toBeNull());
      await act(async () => { await result.current.cancelForeignRun(); });
      expect(api.invoke).toHaveBeenCalledWith('cancel_plate_solve', { runId: 'solve-a' });
      expect(result.current.foreignRun?.isCancelling).toBe(true);
      expect(notifyMock).not.toHaveBeenCalled();
    });

    it('a_second_wait_shows_its_own_refusal', async () => {
      const handlers = captureListeners();
      let current = activity({ running: true, origin: 'ui', runId: 'solve-u1', done: 1, total: 5 });
      routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => current });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-complete']).toBeDefined());
      act(() => { result.current.enqueuePlateSolve([1]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      expect(notifyMock.mock.calls[0][0]).toMatchObject({ dedupeKey: 'plate-solve-busy-solve-u1' });
      // The first holder ends, our batch runs and ends.
      current = activity();
      await act(async () => handlers['plate-solve-complete']({ solved: 5, failed: 0, total: 5, total_time_ms: 5 }));
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
      await act(async () => handlers['plate-solve-complete']({ solved: 1, failed: 0, total: 1, total_time_ms: 5 }));
      await waitFor(() => expect(result.current.queueLength).toBe(0));
      // A second holder: its refusal is a notification of its own, not one the dedupe key swallows.
      current = activity({ running: true, origin: 'ui', runId: 'solve-u2', done: 0, total: 3 });
      act(() => { result.current.enqueuePlateSolve([2]); });
      await waitFor(() =>
        expect(notifyMock.mock.calls.filter(([n]) => String(n.dedupeKey).startsWith('plate-solve-busy'))).toHaveLength(2),
      );
      const keys = notifyMock.mock.calls.map(([n]) => n.dedupeKey).filter((k) => String(k).startsWith('plate-solve-busy'));
      expect(keys).toEqual(['plate-solve-busy-solve-u1', 'plate-solve-busy-solve-u2']);
    });

    it('a_lost_complete_event_is_caught_up_by_the_re_read', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 });
      routeInvoke({ get_plate_solve_activity: () => current });
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      const { result } = renderHook(() => usePlateSolveQueue());
      await tick(0);
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-a', done: 1 });
      // The line follows the activity without any event.
      current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 3, total: 4, currentFile: 'c.fits' });
      await tick(3100);
      expect(result.current.foreignRun).toMatchObject({ done: 3, total: 4, currentFile: 'c.fits' });
      // The run ends and its complete event is lost: the next re-read hides the line.
      current = activity();
      await tick(3100);
      expect(result.current.foreignRun).toBeNull();
      // Hidden: the timer is gone.
      const reads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_plate_solve_activity').length;
      const before = reads();
      await tick(9000);
      expect(reads()).toBe(before);
    });

    it('cancel_does_nothing_when_the_slot_holds_another_run', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 });
      routeInvoke({ get_plate_solve_activity: () => current, cancel_plate_solve: () => undefined });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.foreignRun).not.toBeNull());
      // The assistant's run ended and another run took the slot: the line is stale, it moves and nothing is cancelled.
      current = activity({ running: true, origin: 'ui', runId: 'solve-u', done: 0, total: 9 });
      await act(async () => { await result.current.cancelForeignRun(); });
      expect(api.invoke).not.toHaveBeenCalledWith('cancel_plate_solve', expect.anything());
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-u', origin: 'ui', isCancelling: false });
    });

    it('cancel_does_nothing_when_another_assistant_run_holds_the_slot', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 });
      routeInvoke({ get_plate_solve_activity: () => current, cancel_plate_solve: () => undefined });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.foreignRun?.runId).toBe('solve-a'));
      // A newer assistant run (another id) took the slot: the line moves to it and nothing is cancelled.
      current = activity({ running: true, origin: 'assistant', runId: 'solve-b', done: 0, total: 2 });
      await act(async () => { await result.current.cancelForeignRun(); });
      expect(api.invoke).not.toHaveBeenCalledWith('cancel_plate_solve', expect.anything());
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-b', isCancelling: false });
    });

    it('cancelling_a_held_batch_ends_it', async () => {
      captureListeners();
      routeInvoke({
        get_catalog_status: [{ installed: true }],
        get_plate_solve_activity: () => activity({ running: true, origin: 'ui', runId: 'solve-u', done: 1, total: 5 }),
        cancel_plate_solve: () => undefined,
      });
      const { result } = renderHook(() => usePlateSolveQueue());
      let id = -1;
      act(() => { id = result.current.enqueuePlateSolve([1, 2]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      await act(async () => { await result.current.cancelBatch(id); });
      expect(result.current.queueLength).toBe(0);
      expect(result.current.activeBatches.get(id)).toMatchObject({ isComplete: true, isCancelling: false });
      expect(result.current.hasActiveBatches).toBe(false);
      // The held batch never reached the backend, and nothing was cancelled there.
      expect(batchCalls()).toHaveLength(0);
      expect(api.invoke).not.toHaveBeenCalledWith('cancel_plate_solve');
    });

    it('an_assistant_run_that_ends_is_told_once', async () => {
      const handlers = captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 6, total: 10 });
      routeInvoke({ get_plate_solve_activity: () => current });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.foreignRun).not.toBeNull());
      current = activity();
      await act(async () => handlers['plate-solve-complete']({ solved: 8, failed: 2, total: 10, total_time_ms: 5 }));
      expect(result.current.foreignRun).toBeNull();
      expect(notifyMock).toHaveBeenCalledTimes(1);
      expect(notifyMock).toHaveBeenCalledWith(
        expect.objectContaining({
          title: 'Plate-solve finished — 8/10 solved',
          detail: '2 failed',
          kind: 'platesolve',
          hasErrors: true,
          tone: 'warning',
          dedupeKey: 'plate-solve-done-solve-a',
        }),
      );
      // A repeated event, or a later re-read, says nothing more.
      await act(async () => handlers['plate-solve-complete']({ solved: 8, failed: 2, total: 10, total_time_ms: 5 }));
      expect(notifyMock).toHaveBeenCalledTimes(1);
    });

    it('an_assistant_run_whose_event_was_lost_is_told_once_by_the_re_read', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 4, total: 4 });
      routeInvoke({ get_plate_solve_activity: () => current });
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      renderHook(() => usePlateSolveQueue());
      await tick(0);
      current = activity();
      await tick(3100);
      await tick(3100);
      expect(notifyMock).toHaveBeenCalledTimes(1);
      expect(notifyMock.mock.calls[0][0]).toMatchObject({ dedupeKey: 'plate-solve-done-solve-a', kind: 'platesolve' });
    });

    it('a_failed_cancel_is_logged_and_said', async () => {
      captureListeners();
      routeInvoke({
        get_plate_solve_activity: () => activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 }),
        cancel_plate_solve: () => Promise.reject('no run'),
      });
      const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.foreignRun).not.toBeNull());
      await act(async () => { await result.current.cancelForeignRun(); });
      expect(errors).toHaveBeenCalled();
      expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Could not cancel the plate solve', hasErrors: true }));
      expect(result.current.foreignRun?.isCancelling).toBe(false);
      errors.mockRestore();
    });
  });
  describe('a batch cancelled while it starts, and runs that are not the queue\'s', () => {
    const never = () => new Promise(() => {});

    it('a_batch_cancelled_while_starting_never_runs_and_the_next_one_does', async () => {
      captureListeners();
      let gate: Promise<unknown> | null = null;
      let release: (v: unknown) => void = () => {};
      routeInvoke(
        { get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => gate ?? activity() },
        never,
      );
      const { result } = renderHook(() => usePlateSolveQueue());
      await act(async () => {});
      // The activity read of the first batch is still pending when the user cancels it.
      gate = new Promise((r) => { release = r; });
      let a = -1;
      let b = -1;
      act(() => { a = result.current.enqueuePlateSolve([1]); });
      act(() => { b = result.current.enqueuePlateSolve([2]); });
      await act(async () => { await result.current.cancelBatch(a); });
      gate = null;
      await act(async () => { release(activity()); });
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
      expect(batchCalls()[0][1]).toEqual({ frameIds: [2] });
      expect(result.current.queueLength).toBe(1);
      expect(result.current.activeBatches.get(a)).toMatchObject({ isComplete: true, isCancelling: false });
      expect(result.current.activeBatches.get(b)).toMatchObject({ isComplete: false });
    });

    it('a_batch_cancelled_during_the_catalog_check_never_runs_and_the_next_one_does', async () => {
      captureListeners();
      let gate: Promise<unknown> | null = null;
      let release: (v: unknown) => void = () => {};
      routeInvoke(
        { get_catalog_status: () => gate ?? [{ installed: true }], get_plate_solve_activity: () => activity(), cancel_plate_solve: () => undefined },
        never,
      );
      gate = new Promise((r) => { release = r; });
      const { result } = renderHook(() => usePlateSolveQueue());
      let a = -1;
      let b = -1;
      act(() => { a = result.current.enqueuePlateSolve([1]); });
      act(() => { b = result.current.enqueuePlateSolve([2]); });
      await waitFor(() => expect(vi.mocked(api.invoke)).toHaveBeenCalledWith('get_catalog_status'));
      await act(async () => { await result.current.cancelBatch(a); });
      gate = null;
      await act(async () => { release([{ installed: true }]); });
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
      expect(batchCalls()[0][1]).toEqual({ frameIds: [2] });
      // Nothing had reached the backend, so nothing there was cancelled.
      expect(api.invoke).not.toHaveBeenCalledWith('cancel_plate_solve', expect.anything());
      expect(result.current.queueLength).toBe(1);
      expect(result.current.activeBatches.get(a)).toMatchObject({ isComplete: true });
      expect(result.current.activeBatches.get(b)).toMatchObject({ isComplete: false });
    });

    /** A batch held on a UI run (solve-a) that then ends: the batch runs and its own run is solve-own. */
    async function runningBehindAnEndedRun() {
      const handlers = captureListeners();
      let current = activity({ running: true, origin: 'ui', runId: 'solve-a', done: 1, total: 5 });
      // The web command: it returns at once, and the batch ends with its complete event.
      routeInvoke({ get_catalog_status: [{ installed: true }], get_plate_solve_activity: () => current }, () => undefined);
      const hook = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-complete']).toBeDefined());
      let id = -1;
      act(() => { id = hook.result.current.enqueuePlateSolve([1, 2]); });
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
      current = activity();
      await act(async () => handlers['plate-solve-complete']({ run_id: 'solve-a', solved: 5, failed: 0, total: 5, total_time_ms: 5 }));
      await waitFor(() => expect(batchCalls()).toHaveLength(1));
      act(() => handlers['plate-solve-started']({ origin: 'ui', runId: 'solve-own', total: 2 }));
      return { ...hook, handlers, id };
    }

    it('a_late_complete_for_another_run_is_ignored', async () => {
      const { result, handlers, id } = await runningBehindAnEndedRun();
      // The ended run's complete arrives a second time (or late): the batch it is not is not ended by it.
      await act(async () => handlers['plate-solve-complete']({ run_id: 'solve-a', solved: 5, failed: 0, total: 5, total_time_ms: 5 }));
      expect(result.current.activeBatches.get(id)).toMatchObject({ isComplete: false, summary: null });
      expect(result.current.queueLength).toBe(1);
      // The batch's own run ends it.
      await act(async () => handlers['plate-solve-complete']({ run_id: 'solve-own', solved: 2, failed: 0, total: 2, total_time_ms: 5 }));
      await waitFor(() => expect(result.current.queueLength).toBe(0));
      expect(result.current.activeBatches.get(id)).toMatchObject({ isComplete: true, summary: { solved: 2, total: 2 } });
    });

    it('foreign_progress_never_lands_on_the_queues_batch', async () => {
      const { result, handlers, id } = await runningBehindAnEndedRun();
      act(() => handlers['plate-solve-progress']({ run_id: 'solve-a', frame_id: 9, current: 4, total: 5, status: 'solved', matched_stars: 3, rms_arcsec: 1 }));
      let batch = result.current.activeBatches.get(id)!;
      expect(batch.frameStatuses.has(9)).toBe(false);
      expect(batch.progress).toBeNull();
      act(() => handlers['plate-solve-progress']({ run_id: 'solve-own', frame_id: 1, current: 1, total: 2, status: 'solved', matched_stars: 3, rms_arcsec: 1 }));
      batch = result.current.activeBatches.get(id)!;
      expect(batch.progress).toEqual({ current: 1, total: 2 });
      expect(batch.frameStatuses.get(1)).toMatchObject({ kind: 'solved' });
    });

    it('the_queues_own_run_is_not_shown_as_a_run_it_did_not_start', async () => {
      const { result, handlers } = await runningBehindAnEndedRun();
      expect(result.current.foreignRun).toBeNull();
      act(() => handlers['plate-solve-progress']({ run_id: 'solve-own', frame_id: 1, current: 1, total: 2, status: 'solving', filename: 'a.fits' }));
      expect(result.current.foreignRun).toBeNull();
    });

    it('a_ui_run_this_page_did_not_start_is_shown_with_cancel', async () => {
      const handlers = captureListeners();
      routeInvoke({
        get_plate_solve_activity: () => activity({ running: true, origin: 'ui', runId: 'solve-u', done: 2, total: 5, currentFile: 'x.fits' }),
        cancel_plate_solve: () => undefined,
      });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() =>
        expect(result.current.foreignRun).toEqual({
          runId: 'solve-u', origin: 'ui', done: 2, total: 5, currentFile: 'x.fits', isCancelling: false,
        }),
      );
      // Its events move the line; another run's events do not.
      await waitFor(() => expect(handlers['plate-solve-progress']).toBeDefined());
      act(() => handlers['plate-solve-progress']({ run_id: 'solve-u', frame_id: 3, current: 3, total: 5, status: 'solving', filename: 'y.fits' }));
      expect(result.current.foreignRun).toMatchObject({ done: 3, currentFile: 'y.fits' });
      act(() => handlers['plate-solve-progress']({ run_id: 'solve-other', frame_id: 3, current: 5, total: 5, status: 'solving', filename: 'z.fits' }));
      expect(result.current.foreignRun).toMatchObject({ done: 3, currentFile: 'y.fits' });
      // Cancel names the run.
      await act(async () => { await result.current.cancelForeignRun(); });
      expect(api.invoke).toHaveBeenCalledWith('cancel_plate_solve', { runId: 'solve-u' });
      expect(result.current.foreignRun?.isCancelling).toBe(true);
      // A started event of a run begun elsewhere shows too.
      act(() => handlers['plate-solve-started']({ origin: 'ui', runId: 'solve-v', total: 4 }));
      expect(result.current.foreignRun).toMatchObject({ runId: 'solve-v', origin: 'ui', total: 4, isCancelling: false });
    });

    it('a_run_whose_end_was_only_seen_by_a_re_read_says_its_result_was_not_received', async () => {
      captureListeners();
      let current = activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 4, total: 4 });
      routeInvoke({ get_plate_solve_activity: () => current });
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'] });
      const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
      renderHook(() => usePlateSolveQueue());
      await tick(0);
      current = activity();
      await tick(3100);
      expect(notifyMock).toHaveBeenCalledTimes(1);
      const n = notifyMock.mock.calls[0][0];
      expect(n.title).toBe('Plate solve from the assistant ended');
      expect(n.detail).toBe('Its result was not received here — ask the assistant for solve_status.');
      expect(JSON.stringify(n)).not.toMatch(/were done/);
    });
  });
});
