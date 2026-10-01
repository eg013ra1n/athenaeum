import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider } from '../../../contexts/NotificationContext';
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

beforeEach(() => {
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

  it('follows progress and finished events for its project only', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(listeners['collab-publish-progress']).toBeDefined());
    act(() => listeners['collab-publish-progress'](progress({ projectId: 'other' })));
    expect(result.current.running).toBeNull();
    act(() => listeners['collab-publish-progress'](progress({ current: 7 })));
    expect(result.current.running?.current).toBe(7);
    act(() => listeners['collab-publish-finished'](finished()));
    expect(result.current.running).toBeNull();
    expect(result.current.last?.calibrated).toBe(10);
  });

  it('the reached step never moves back within one run (F3)', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: null, last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(listeners['collab-publish-progress']).toBeDefined());
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'announcing' })));
    act(() => listeners['collab-publish-progress'](progress({ kind: 'auto', stage: 'calibrating' })));
    expect(result.current.reached).toBe(3); // announcing
    act(() => listeners['collab-publish-progress'](progress({ publishRunId: 'r2', stage: 'queued' })));
    expect(result.current.reached).toBe(0); // a new run starts over
  });

  it('cancel invokes cancel_collab_publish and logs + notifies a failure', async () => {
    vi.mocked(api.invoke).mockResolvedValueOnce({ running: progress(), last: null });
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(result.current.running).not.toBeNull());
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('boom'));
    await act(async () => { await result.current.cancel(); });
    expect(api.invoke).toHaveBeenLastCalledWith('cancel_collab_publish', { projectId: 'p1' });
    expect(err).toHaveBeenCalled();
  });

  it('a failed snapshot read is logged and leaves an idle state', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValueOnce(new Error('db'));
    const { result } = renderHook(() => useCollabPublishRun('p1'), { wrapper });
    await waitFor(() => expect(err).toHaveBeenCalled());
    expect(result.current.running).toBeNull();
  });
});
