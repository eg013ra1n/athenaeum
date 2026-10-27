import { describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { usePlateSolveQueue } from './usePlateSolveQueue';
import { api } from '../api';
import type { PlateSolveProgressEvent } from '../types/helpers';

vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: vi.fn() }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

describe('usePlateSolveQueue', () => {
  it('progress never steps back across parallel workers and labels the frame by file name', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    // The batch stays running until the test is done with it.
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      cmd === 'get_catalog_status'
        ? Promise.resolve([{ installed: true }])
        : new Promise(() => {})) as never);

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
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      Promise.resolve(cmd === 'get_catalog_status' ? [{ installed: true }] : undefined)) as never);

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
});
