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
        expect(result.current.assistantRun).toEqual({
          runId: 'solve-a', done: 2, total: 10, currentFile: 'b.fits', isCancelling: false,
        }),
      );
      // Progress events move it; a late event never steps the counter back.
      await waitFor(() => expect(handlers['plate-solve-progress']).toBeDefined());
      act(() => handlers['plate-solve-progress']({ frame_id: 4, current: 5, total: 10, status: 'solving', filename: 'e.fits' }));
      act(() => handlers['plate-solve-progress']({ frame_id: 3, current: 4, total: 10, status: 'solving', filename: 'd.fits' }));
      expect(result.current.assistantRun).toMatchObject({ done: 5, total: 10, currentFile: 'd.fits' });
      // Progress is never notified.
      expect(notifyMock).not.toHaveBeenCalled();
      // Cleared on complete.
      act(() => handlers['plate-solve-complete']({ solved: 10, failed: 0, total: 10, total_time_ms: 5 }));
      expect(result.current.assistantRun).toBeNull();
    });

    it('a_run_started_after_the_page_opened_shows_from_the_started_event', async () => {
      const handlers = captureListeners();
      routeInvoke({ get_plate_solve_activity: () => activity() });
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(handlers['plate-solve-started']).toBeDefined());
      expect(result.current.assistantRun).toBeNull();
      // A run started in Athenaeum is the queue's own: not shown as the assistant's.
      act(() => handlers['plate-solve-started']({ origin: 'ui', runId: 'solve-u', total: 3 }));
      expect(result.current.assistantRun).toBeNull();
      act(() => handlers['plate-solve-started']({ origin: 'assistant', runId: 'solve-b', total: 7 }));
      expect(result.current.assistantRun).toMatchObject({ runId: 'solve-b', done: 0, total: 7, currentFile: null });
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
          dedupeKey: 'plate-solve-busy',
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
      expect(notifyMock).toHaveBeenCalledTimes(1);
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
      expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Plate solve not started', detail: BUSY_DETAIL, dedupeKey: 'plate-solve-busy' });
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
        dedupeKey: 'plate-solve-busy',
      });
      expect(batchCalls()).toHaveLength(0);
      expect(result.current.queueLength).toBe(1);
      // Not an assistant run: no assistant line.
      expect(result.current.assistantRun).toBeNull();
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
      await waitFor(() => expect(result.current.assistantRun).not.toBeNull());
      await act(async () => { await result.current.cancelAssistantRun(); });
      expect(api.invoke).toHaveBeenCalledWith('cancel_plate_solve');
      expect(result.current.assistantRun?.isCancelling).toBe(true);
      expect(notifyMock).not.toHaveBeenCalled();
    });

    it('a_failed_cancel_is_logged_and_said', async () => {
      captureListeners();
      routeInvoke({
        get_plate_solve_activity: () => activity({ running: true, origin: 'assistant', runId: 'solve-a', done: 1, total: 4 }),
        cancel_plate_solve: () => Promise.reject('no run'),
      });
      const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
      const { result } = renderHook(() => usePlateSolveQueue());
      await waitFor(() => expect(result.current.assistantRun).not.toBeNull());
      await act(async () => { await result.current.cancelAssistantRun(); });
      expect(errors).toHaveBeenCalled();
      expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Could not cancel the plate solve', hasErrors: true }));
      expect(result.current.assistantRun?.isCancelling).toBe(false);
      errors.mockRestore();
    });
  });
});
