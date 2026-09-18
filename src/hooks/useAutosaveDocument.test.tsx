import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { useAutosaveDocument } from './useAutosaveDocument';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));

vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

interface Doc {
  a: { b: number };
  c: string;
}

const INITIAL: Doc = { a: { b: 1 }, c: 'x' };

/** A handful of microtask hops — enough for a chain of `await`s inside the
 *  hook's effects to settle, whether or not fake timers are active (fake
 *  timers only replace `setTimeout`/friends, never Promise scheduling). */
async function flush() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  notifyMock.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('useAutosaveDocument — load', () => {
  it('a load never triggers save', async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults: null }));
    await act(async () => {
      await flush();
    });

    expect(result.current.doc).toEqual(INITIAL);
    expect(save).not.toHaveBeenCalled();
  });

  it('a failed load exposes error and leaves doc null', async () => {
    const load = vi.fn().mockRejectedValue(new Error('disk error'));
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults: null }));
    await act(async () => {
      await flush();
    });

    expect(result.current.doc).toBeNull();
    expect(result.current.error).toBe('disk error');
  });
});

describe('useAutosaveDocument — patch and debounce', () => {
  it('two patches inside the debounce window save once with the last document', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, defaults: null, debounceMs: 500 }),
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.patch({ c: 'y' });
    });
    act(() => {
      result.current.patch({ c: 'z' });
    });

    expect(save).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith({ a: { b: 1 }, c: 'z' });
  });

  it('a patch function form sees the latest document', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, defaults: null, debounceMs: 500 }),
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.patch((d) => ({ ...d, a: { b: d.a.b + 1 } }));
    });
    act(() => {
      result.current.patch((d) => ({ ...d, a: { b: d.a.b + 1 } }));
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith({ a: { b: 3 }, c: 'x' });
  });

  it('unmount flushes a pending write', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result, unmount } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, defaults: null, debounceMs: 500 }),
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.patch({ c: 'y' });
    });

    unmount();

    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith({ a: { b: 1 }, c: 'y' });
  });

  it('a clean unmount (nothing dirty) never calls save', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { unmount } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults: null }));
    await act(async () => {
      await flush();
    });

    unmount();

    expect(save).not.toHaveBeenCalled();
  });

  it('a save rejection exposes error, keeps doc, and notifies once', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockRejectedValue(new Error('offline'));

    const { result } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, defaults: null, debounceMs: 500, label: 'Test config' }),
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.patch({ c: 'y' });
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(result.current.error).toBe('offline');
    expect(result.current.doc).toEqual({ a: { b: 1 }, c: 'y' });
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Test config not saved', detail: 'offline' });
  });

  it('an unmount flush failure does not notify', async () => {
    vi.useFakeTimers();
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockRejectedValue(new Error('offline'));

    const { result, unmount } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, defaults: null, debounceMs: 500 }),
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.patch({ c: 'y' });
    });

    unmount();
    await act(async () => {
      await flush();
    });

    expect(save).toHaveBeenCalledTimes(1);
    expect(notifyMock).not.toHaveBeenCalled();
  });
});

describe('useAutosaveDocument — defaults, isDefault and resetField', () => {
  const defaults: Doc = { a: { b: 42 }, c: 'default' };

  it('isDefault compares the document at a dotted path against defaults', async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults }));
    await act(async () => {
      await flush();
    });

    expect(result.current.isDefault('a.b')).toBe(false);
    expect(result.current.isDefault('c')).toBe(false);
  });

  it("resetField('a.b') patches the default's value at that path", async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults }));
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.resetField('a.b');
    });

    expect(result.current.doc).toEqual({ a: { b: 42 }, c: 'x' });
    expect(result.current.isDefault('a.b')).toBe(true);
    expect(result.current.isDefault('c')).toBe(false);
  });

  it('resetField with no defaults loaded is a no-op', async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults: null }));
    await act(async () => {
      await flush();
    });

    act(() => {
      result.current.resetField('a.b');
    });

    expect(result.current.doc).toEqual(INITIAL);
  });
});

describe('useAutosaveDocument — resetAll', () => {
  it('resetAll calls opts.resetAll and reloads', async () => {
    const reloaded: Doc = { a: { b: 0 }, c: 'reset' };
    const load = vi.fn().mockResolvedValueOnce(INITIAL).mockResolvedValueOnce(reloaded);
    const save = vi.fn().mockResolvedValue(undefined);
    const resetAllFn = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, resetAll: resetAllFn, defaults: null }),
    );
    await act(async () => {
      await flush();
    });

    await act(async () => {
      await result.current.resetAll();
    });

    expect(resetAllFn).toHaveBeenCalledTimes(1);
    expect(load).toHaveBeenCalledTimes(2);
    expect(result.current.doc).toEqual(reloaded);
  });

  it('resetAll with no opts.resetAll provided is a no-op', async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(() => useAutosaveDocument<Doc>({ load, save, defaults: null }));
    await act(async () => {
      await flush();
    });

    await act(async () => {
      await result.current.resetAll();
    });

    expect(result.current.doc).toEqual(INITIAL);
  });

  it('a resetAll failure sets error and notifies', async () => {
    const load = vi.fn().mockResolvedValue(INITIAL);
    const save = vi.fn().mockResolvedValue(undefined);
    const resetAllFn = vi.fn().mockRejectedValue(new Error('reset failed'));

    const { result } = renderHook(() =>
      useAutosaveDocument<Doc>({ load, save, resetAll: resetAllFn, defaults: null }),
    );
    await act(async () => {
      await flush();
    });

    await act(async () => {
      await result.current.resetAll();
    });

    expect(result.current.error).toBe('reset failed');
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });
});
