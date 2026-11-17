// The Analysis tab reads its "N to update" count and its zero point cells again when the pass after a solve batch ends.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { StrictMode } from 'react';
import { api } from '../api';
import { useZeroPointPassComplete } from './useZeroPointPassComplete';

vi.mock('../api');

const listeners = new Set<(p: unknown) => void>();
let unlistened = 0;

beforeEach(() => {
  listeners.clear();
  unlistened = 0;
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
    if (name === 'zero-point-pass-complete') listeners.add(cb);
    return Promise.resolve(() => {
      listeners.delete(cb);
      unlistened += 1;
    });
  }) as never);
});

describe('useZeroPointPassComplete', () => {
  it('calls back with the event when the pass ends', async () => {
    const onComplete = vi.fn();
    renderHook(() => useZeroPointPassComplete(onComplete));
    await waitFor(() => expect(listeners.size).toBe(1));
    [...listeners][0]({ frames: 12, measured: 11 });
    expect(onComplete).toHaveBeenCalledWith({ frames: 12, measured: 11 });
  });

  it('calls the latest callback without listening again', async () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ cb }) => useZeroPointPassComplete(cb), { initialProps: { cb: first } });
    await waitFor(() => expect(listeners.size).toBe(1));
    rerender({ cb: second });
    [...listeners][0]({ frames: 1, measured: 1 });
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    expect(vi.mocked(api.listen)).toHaveBeenCalledTimes(1);
  });

  it('leaves no listener behind after unmount, and one only under StrictMode double-mount', async () => {
    const onComplete = vi.fn();
    const { unmount } = renderHook(() => useZeroPointPassComplete(onComplete), { wrapper: StrictMode });
    await waitFor(() => expect(listeners.size).toBe(1));
    [...listeners][0]({ frames: 2, measured: 2 });
    expect(onComplete).toHaveBeenCalledTimes(1);
    unmount();
    await waitFor(() => expect(listeners.size).toBe(0));
  });
});
