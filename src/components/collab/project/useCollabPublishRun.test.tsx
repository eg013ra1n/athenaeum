import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider, useNotifications } from '../../../contexts/NotificationContext';
import type { CollabPublishFinished, CollabPublishProgress, CollabPublishRunView } from '../../../types/models';
import { useCollabPublishRun } from './useCollabPublishRun';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const listeners: Record<string, (p: unknown) => void> = {};
const progress = (o: Partial<CollabPublishProgress> = {}): CollabPublishProgress => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', mode: null,
  stage: 'calibrating', current: 3, total: 10, currentFile: 'c_a.fits', startedAt: '2026-10-02T10:00:00Z', ...o,
});
const finished = (o: Partial<CollabPublishFinished> = {}): CollabPublishFinished => ({
  projectId: 'p1', publishRunId: 'r1', kind: 'calibrate', trigger: 'manual', outcome: 'done',
  calibrated: 10, announced: 0, updated: 0, stale: 0, heldBack: 0, error: null,
  startedAt: '2026-10-02T10:00:00Z', finishedAt: '2026-10-02T10:05:00Z', ...o,
});
const wrapper = ({ children }: { children: React.ReactNode }) => <NotificationProvider>{children}</NotificationProvider>;

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const snapshotCalls = () => vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'get_collab_publish_run');

beforeEach(() => {
  for (const k of Object.keys(listeners)) delete listeners[k];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
    listeners[name] = cb;
    return Promise.resolve(() => { delete listeners[name]; });
  }) as typeof api.listen);
});

describe('useCollabPublishRun', () => {
  it('a_run_in_progress_on_mount_is_shown_from_the_snapshot', async () => {
    const view: CollabPublishRunView = { running: progress(), last: null };
    vi.mocked(api.invoke).mockResolvedValueOnce(view);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(result.current.running?.current).toBe(3));
    expect(api.invoke).toHaveBeenCalledWith('get_collab_publish_run', { projectId: 'p1' });
  });

  it('reads the snapshot only after both listeners are registered', async () => {
    const pending: Array<{ name: string; d: ReturnType<typeof deferred<() => void>> }> = [];
    vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
      listeners[name] = cb;
      // The provider's own listeners resolve at once; the run's two wait.
      if (!name.startsWith('collab-publish-')) return Promise.resolve(() => {});
      const d = deferred<() => void>();
      pending.push({ name, d });
      return d.promise;
    }) as typeof api.listen);
    vi.mocked(api.invoke).mockResolvedValue({ running: null, last: null });
    renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(pending).toHaveLength(2));
    await act(async () => { pending[0].d.resolve(() => {}); });
    expect(snapshotCalls()).toHaveLength(0);
    await act(async () => { pending[1].d.resolve(() => {}); });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
  });

  it('a finished event that arrives before the snapshot reply leaves running null', async () => {
    const snap = deferred<CollabPublishRunView>();
    vi.mocked(api.invoke).mockReturnValueOnce(snap.promise as never);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    act(() => listeners['collab-publish-finished'](finished()));
    // The reply was computed before the run ended: it still says r1 runs.
    await act(async () => { snap.resolve({ running: progress(), last: null }); });
    expect(result.current.running).toBeNull();
    expect(result.current.reached).toBe(-1);
    // The heard outcome is newer than the reply's (absent) last run.
    expect(result.current.last?.publishRunId).toBe('r1');
  });

  it('a running: null reply keeps a run whose progress was heard while the read was pending', async () => {
    const snap = deferred<CollabPublishRunView>();
    vi.mocked(api.invoke).mockReturnValueOnce(snap.promise as never);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    // r1 ends, then r2 is queued — after the reply was computed.
    act(() => listeners['collab-publish-progress'](progress({ stage: 'seeding' })));
    act(() => listeners['collab-publish-finished'](finished()));
    act(() => listeners['collab-publish-progress'](progress({ publishRunId: 'r2', stage: 'announcing' })));
    await act(async () => { snap.resolve({ running: null, last: null }); });
    expect(result.current.running?.publishRunId).toBe('r2');
    expect(result.current.reached).toBe(3);
  });

  it('a running: null reply resets when no unfinished run was heard', async () => {
    const snap = deferred<CollabPublishRunView>();
    vi.mocked(api.invoke).mockReturnValueOnce(snap.promise as never);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    // Another project's progress is never recorded for this one.
    act(() => listeners['collab-publish-progress'](progress({ projectId: 'other' })));
    await act(async () => { snap.resolve({ running: null, last: null }); });
    expect(result.current.running).toBeNull();
    expect(result.current.reached).toBe(-1);
  });

  it('a running: null reply resets when the run heard has already finished', async () => {
    const snap = deferred<CollabPublishRunView>();
    vi.mocked(api.invoke).mockReturnValueOnce(snap.promise as never);
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    act(() => listeners['collab-publish-progress'](progress({ stage: 'announcing' })));
    act(() => listeners['collab-publish-finished'](finished()));
    await act(async () => { snap.resolve({ running: null, last: null }); });
    expect(result.current.running).toBeNull();
    expect(result.current.reached).toBe(-1);
  });

  it('a projectId change resets running, last and the reached step before the new snapshot', async () => {
    const p2 = deferred<CollabPublishRunView>();
    vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { projectId: string }) => {
      if (cmd !== 'get_collab_publish_run') return Promise.resolve(undefined);
      return args?.projectId === 'p1'
        ? Promise.resolve({ running: progress({ stage: 'seeding' }), last: finished({ publishRunId: 'r0' }) })
        : p2.promise;
    }) as never);
    const { result, rerender } = renderHook(({ id }) => useCollabPublishRun(id), { wrapper, initialProps: { id: 'p1' } });
    await waitFor(() => expect(result.current.running).not.toBeNull());
    expect(result.current.last?.publishRunId).toBe('r0');
    expect(result.current.reached).toBe(2);
    rerender({ id: 'p2' });
    expect(result.current.running).toBeNull();
    expect(result.current.last).toBeNull();
    expect(result.current.reached).toBe(-1);
    await act(async () => { p2.resolve({ running: null, last: finished({ projectId: 'p2', publishRunId: 'r9' }) }); });
    expect(result.current.last?.publishRunId).toBe('r9');
  });

  it('follows progress and finished events for its project only', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    act(() => listeners['collab-publish-progress'](progress({ projectId: 'other' })));
    expect(result.current.running).toBeNull();
    act(() => listeners['collab-publish-progress'](progress({ current: 7 })));
    expect(result.current.running?.current).toBe(7);
    // Another project's outcome leaves this run alone.
    act(() => listeners['collab-publish-finished'](finished({ projectId: 'other', calibrated: 99 })));
    expect(result.current.running?.current).toBe(7);
    expect(result.current.last).toBeNull();
    act(() => listeners['collab-publish-finished'](finished()));
    expect(result.current.running).toBeNull();
    expect(result.current.last?.calibrated).toBe(10);
  });

  it('the reached step never moves back within one run (F3)', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(snapshotCalls()).toHaveLength(1));
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'announcing' })));
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'calibrating' })));
    expect(result.current.reached).toBe(3); // announcing
    act(() => listeners['collab-publish-progress'](progress({ publishRunId: 'r2', stage: 'queued' })));
    expect(result.current.reached).toBe(0); // a new run starts over
  });

  it('cancelBusy is true while cancel_collab_publish is pending', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: progress(), last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(result.current.running).not.toBeNull());
    const call = deferred<void>();
    vi.mocked(api.invoke).mockReturnValueOnce(call.promise as never);
    let done: Promise<void> | undefined;
    act(() => { done = result.current.cancel(); });
    expect(result.current.cancelBusy).toBe(true);
    await act(async () => { call.resolve(); await done; });
    expect(result.current.cancelBusy).toBe(false);
  });

  it('cancel invokes cancel_collab_publish and logs + notifies a failure', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: progress(), last: null });
    const { result } = renderHook(() => ({ run: useCollabPublishRun('p1'), n: useNotifications() }), { wrapper });
    await waitFor(() => expect(result.current.run.running).not.toBeNull());
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('boom'));
    await act(async () => { await result.current.run.cancel(); });
    expect(api.invoke).toHaveBeenLastCalledWith('cancel_collab_publish', { projectId: 'p1' });
    expect(err).toHaveBeenCalled();
    expect(result.current.n.toasts.map((t) => t.message)).toEqual(['Could not cancel the run']);
    expect(result.current.run.cancelBusy).toBe(false);
    err.mockRestore();
  });

  it('a failed snapshot read is logged and leaves an idle state', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db'));
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(err).toHaveBeenCalled());
    expect(result.current.running).toBeNull();
    err.mockRestore();
  });
});
