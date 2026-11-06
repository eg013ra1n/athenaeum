import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useStackingRuns } from './useStackingRuns';
import { api } from '../api';
import type { StackingConfig } from '../types/stacking';
import type { StackSubject } from '../components/stacking/subject';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

function progress(setId: number | null, projectId: string | null, runId = 1) {
  return {
    runId, setId, projectId, stage: 'measure', groupKey: null, current: 1, total: 2,
    percent: 50, bytesDone: 0, bytesTotal: 0, frameId: null, message: null,
  };
}

function complete(setId: number | null, projectId: string | null, runId = 1) {
  return { runId, setId, projectId, success: true, cancelled: false, error: null, warnings: [], masters: [] };
}

async function mount() {
  const handlers: Record<string, (p: unknown) => void> = {};
  vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
    handlers[ev] = cb;
    return Promise.resolve(() => {});
  }) as never);
  const hook = renderHook(() => useStackingRuns());
  await waitFor(() => expect(handlers['stacking-complete']).toBeDefined());
  return { ...hook, handlers };
}

const frameSet = (framesSetId: number): StackSubject => ({ kind: 'frameSet', framesSetId, lightFrames: [] });
const project = (projectId: string): StackSubject => ({ kind: 'project', projectId, title: 'T' });

describe('useStackingRuns', () => {
  beforeEach(() => {
    notifyMock.mockClear();
    vi.mocked(api.invoke).mockReset();
  });

  afterEach(() => vi.restoreAllMocks());

  it('keys a frame-set run by its set subject key', async () => {
    const { result, handlers } = await mount();

    act(() => handlers['stacking-progress'](progress(7, null)));
    expect(result.current.progress.get('set:7')?.subjectKey).toBe('set:7');
    expect(result.current.isRunning('set:7')).toBe(true);
    act(() => handlers['stacking-complete'](complete(7, null)));
    expect(result.current.progress.has('set:7')).toBe(false);
    expect(result.current.isRunning('set:7')).toBe(false);
    expect(result.current.lastOutcome.get('set:7')?.success).toBe(true);
    expect(result.current.lastOutcome.get('set:7')?.subjectKey).toBe('set:7');
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0].link).toBe('/objects/7?tab=stacking');
  });

  it('tracks a project run under project:<projectId> and under no set key', async () => {
    const { result, handlers } = await mount();

    act(() => handlers['stacking-progress'](progress(null, 'p1')));
    expect([...result.current.progress.keys()]).toEqual(['project:p1']);
    expect(result.current.progress.get('project:p1')?.subjectKey).toBe('project:p1');
    expect(result.current.isRunning('project:p1')).toBe(true);

    act(() => handlers['stacking-complete'](complete(null, 'p1')));
    expect([...result.current.lastOutcome.keys()]).toEqual(['project:p1']);
    expect(result.current.lastOutcome.get('project:p1')?.success).toBe(true);
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });

  it('a completion for one project clears only that subject and links to its tab', async () => {
    const { result, handlers } = await mount();

    act(() => handlers['stacking-progress'](progress(7, null, 1)));
    act(() => handlers['stacking-progress'](progress(null, 'p1', 2)));
    act(() => handlers['stacking-progress'](progress(null, 'p2', 3)));
    expect(result.current.progress.size).toBe(3);

    act(() => handlers['stacking-complete'](complete(null, 'p1', 2)));
    expect([...result.current.progress.keys()].sort()).toEqual(['project:p2', 'set:7']);
    expect([...result.current.lastOutcome.keys()]).toEqual(['project:p1']);
    expect(result.current.lastOutcome.get('set:7')).toBeUndefined();
    expect(result.current.lastOutcome.get('project:p2')).toBeUndefined();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    const note = notifyMock.mock.calls[0][0];
    expect(note.link).toBe('/projects/p1?tab=stacking');
    expect(note.title).toBe('Stacking finished — 0 master(s)');
    expect(note.dedupeKey).toBe('stack-2');
  });

  it('keeps the progress of a newer run when a superseded run of the same subject completes', async () => {
    const { result, handlers } = await mount();

    act(() => handlers['stacking-progress'](progress(null, 'p1', 5)));
    act(() => handlers['stacking-complete'](complete(null, 'p1', 4)));
    expect(result.current.progress.get('project:p1')?.runId).toBe(5);
  });

  it('names the master files in the detail of a project completion', async () => {
    const { handlers } = await mount();

    act(() => handlers['stacking-complete']({
      ...complete(null, 'p1'),
      masters: [
        { path: '/work/p1/L_master.xisf', drizzlePath: null },
        { path: 'C:\\work\\p1\\R_master.xisf', drizzlePath: null },
      ],
    }));
    const note = notifyMock.mock.calls[0][0];
    expect(note.title).toBe('Stacking finished — 2 master(s)');
    expect(note.detail).toBe('L_master.xisf, R_master.xisf');
  });

  it('ignores and logs an event that carries neither a set nor a project, or both', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { result, handlers } = await mount();

    act(() => handlers['stacking-progress'](progress(null, null)));
    act(() => handlers['stacking-progress'](progress(7, 'p1')));
    act(() => handlers['stacking-complete'](complete(null, null)));
    act(() => handlers['stacking-complete'](complete(7, 'p1')));
    expect(result.current.progress.size).toBe(0);
    expect(result.current.lastOutcome.size).toBe(0);
    expect(notifyMock).not.toHaveBeenCalled();
    expect(err).toHaveBeenCalledTimes(4);
  });

  it('starts a project run through start_project_stacking with its project id', async () => {
    vi.mocked(api.invoke).mockResolvedValue({ runId: 42 });
    const { result } = await mount();
    const cfg = { version: 1 } as unknown as StackingConfig;

    let runId = 0;
    await act(async () => { runId = await result.current.startRun(project('p1'), cfg, 'register'); });
    expect(runId).toBe(42);
    expect(api.invoke).toHaveBeenCalledWith('start_project_stacking', {
      projectId: 'p1', config: cfg, rerunFrom: 'register',
    });
  });

  it('starts a frame-set run through start_stacking with its set id', async () => {
    vi.mocked(api.invoke).mockResolvedValue({ runId: 43 });
    const { result } = await mount();
    const cfg = { version: 1 } as unknown as StackingConfig;

    let runId = 0;
    await act(async () => { runId = await result.current.startRun(frameSet(7), cfg, 'register'); });
    expect(runId).toBe(43);
    expect(api.invoke).toHaveBeenCalledWith('start_stacking', {
      setId: 7, config: cfg, rerunFrom: 'register',
    });
  });

  it('logs, notifies and rethrows a failed start for either subject kind', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue('boom');
    const { result } = await mount();

    await act(async () => {
      await expect(result.current.startRun(project('p1'))).rejects.toBe('boom');
    });
    expect(err).toHaveBeenCalledWith('[useStackingRuns] start_project_stacking failed:', 'boom');
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0].title).toBe('Failed to start stacking');

    notifyMock.mockClear();
    await act(async () => {
      await expect(result.current.startRun(frameSet(7))).rejects.toBe('boom');
    });
    expect(err).toHaveBeenCalledWith('[useStackingRuns] start_stacking failed:', 'boom');
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0].title).toBe('Failed to start stacking');
  });
});
