import { describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useStackingRuns } from './useStackingRuns';
import { api } from '../api';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

function progress(setId: number | null, projectId: string | null) {
  return {
    runId: 1, setId, projectId, stage: 'measure', groupKey: null, current: 1, total: 2,
    percent: 50, bytesDone: 0, bytesTotal: 0, frameId: null, message: null,
  };
}

function complete(setId: number | null, projectId: string | null) {
  return { runId: 1, setId, projectId, success: true, cancelled: false, error: null, warnings: [], masters: [] };
}

describe('useStackingRuns', () => {
  it('keys a frame-set run by its set id and ignores a project run until wave 6c', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    const { result } = renderHook(() => useStackingRuns());
    await waitFor(() => expect(handlers['stacking-complete']).toBeDefined());

    act(() => handlers['stacking-progress'](progress(null, 'p1')));
    expect(result.current.progress.size).toBe(0);
    act(() => handlers['stacking-complete'](complete(null, 'p1')));
    expect(result.current.lastOutcome.size).toBe(0);
    expect(notifyMock).not.toHaveBeenCalled();

    act(() => handlers['stacking-progress'](progress(7, null)));
    expect(result.current.progress.get(7)?.setId).toBe(7);
    act(() => handlers['stacking-complete'](complete(7, null)));
    expect(result.current.progress.has(7)).toBe(false);
    expect(result.current.lastOutcome.get(7)?.success).toBe(true);
    expect(notifyMock).toHaveBeenCalledTimes(1);
  });
});
