import { StrictMode, createElement, type ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useHipsPack } from './useHipsPack';
import { api } from '../api';
import type { HipsPackEstimate, PackProgress } from '../types/models';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const SURVEY = 'CDS/P/DSS2/color';

function status(over: Partial<PackProgress> = {}): PackProgress {
  return {
    state: 'idle', selection: null, total: 0, present: 0, fetched: 0, absent: 0, failed: 0,
    bytesFetched: 0, waitingS: null, error: null, ...over,
  };
}

const ESTIMATE: HipsPackEstimate = {
  total: 100, present: 10, missing: 90, bytesPerTile: 1000, estimatedBytes: 90000, freeBytes: 1e12, fits: true,
};

let handlers: Record<string, (p: unknown) => void>;
let active: number;
let invokeMock: ReturnType<typeof vi.fn>;

function wire(statusReply: PackProgress) {
  handlers = {};
  active = 0;
  invokeMock = vi.mocked(api.invoke) as unknown as ReturnType<typeof vi.fn>;
  invokeMock.mockReset();
  invokeMock.mockImplementation(((cmd: string) => {
    if (cmd === 'get_hips_pack_status') return Promise.resolve(statusReply);
    if (cmd === 'estimate_hips_pack') return Promise.resolve(ESTIMATE);
    return Promise.resolve(undefined);
  }) as never);
  vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
    handlers[ev] = cb;
    active += 1;
    return Promise.resolve(() => { active -= 1; });
  }) as never);
}

const estimateCalls = () => invokeMock.mock.calls.filter((c) => c[0] === 'estimate_hips_pack');

describe('useHipsPack', () => {
  beforeEach(() => notifyMock.mockClear());

  it('restores selection from an interrupted status', async () => {
    const selection = { surveyId: SURVEY, skyOrder: 3, aroundLocations: false, aroundMessier: true, aroundNgcIc: true };
    wire(status({ state: 'interrupted', selection, total: 100, present: 40 }));
    const { result } = renderHook(() => useHipsPack(SURVEY));
    await waitFor(() => expect(result.current.estimate).not.toBeNull());
    expect(result.current.selection).toEqual(selection);
    expect(result.current.progress?.state).toBe('interrupted');
    expect(estimateCalls()[0][1]).toEqual({ selection });
  });

  it('re-estimates once per debounced change', async () => {
    wire(status());
    const { result } = renderHook(() => useHipsPack(SURVEY));
    await waitFor(() => expect(estimateCalls().length).toBe(1));
    act(() => result.current.setSelection({ ...result.current.selection, skyOrder: 2 }));
    act(() => result.current.setSelection({ ...result.current.selection, skyOrder: 3 }));
    act(() => result.current.setSelection({ ...result.current.selection, skyOrder: 5 }));
    await waitFor(() => expect(estimateCalls().length).toBe(2));
    await new Promise((r) => setTimeout(r, 400));
    expect(estimateCalls().length).toBe(2);
    expect(estimateCalls()[1][1].selection.skyOrder).toBe(5);
  });

  it('ignores progress for another survey', async () => {
    wire(status());
    const { result } = renderHook(() => useHipsPack(SURVEY));
    await waitFor(() => expect(handlers['hips-download-progress']).toBeDefined());
    await waitFor(() => expect(result.current.progress).not.toBeNull());
    const other = { ...result.current.selection, surveyId: 'other/survey' };
    act(() => handlers['hips-download-progress'](status({ state: 'running', selection: other, total: 5 })));
    expect(result.current.progress?.state).toBe('idle');
    act(() => handlers['hips-download-progress'](
      status({ state: 'running', selection: result.current.selection, total: 5 })));
    expect(result.current.progress?.state).toBe('running');
  });

  it('a failed terminal state notifies, calls onFinished and re-estimates; success stays silent', async () => {
    wire(status());
    const onFinished = vi.fn();
    const { result } = renderHook(() => useHipsPack(SURVEY, onFinished));
    await waitFor(() => expect(estimateCalls().length).toBe(1));
    const sel = result.current.selection;
    act(() => handlers['hips-download-progress'](status({ state: 'done', selection: sel })));
    expect(notifyMock).not.toHaveBeenCalled();
    act(() => handlers['hips-download-progress'](status({ state: 'failed', selection: sel, error: 'disk full' })));
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Sky pack stopped', detail: 'disk full' });
    expect(onFinished).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(estimateCalls().length).toBe(2));
  });

  it('cancel shows cancelling locally and survives a NotFound from the backend', async () => {
    wire(status({ state: 'running', selection: { surveyId: SURVEY, skyOrder: 4, aroundLocations: true, aroundMessier: true, aroundNgcIc: false }, total: 9 }));
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === 'cancel_hips_pack') return Promise.reject('not found');
      if (cmd === 'get_hips_pack_status') return Promise.resolve(status({ state: 'running', selection: { surveyId: SURVEY, skyOrder: 4, aroundLocations: true, aroundMessier: true, aroundNgcIc: false }, total: 9 }));
      return Promise.resolve(ESTIMATE);
    }) as never);
    const { result } = renderHook(() => useHipsPack(SURVEY));
    await waitFor(() => expect(result.current.progress?.state).toBe('running'));
    await act(async () => { await result.current.cancel(); });
    expect(result.current.progress?.state).toBe('cancelling');
    act(() => handlers['hips-download-progress'](status({ state: 'running', selection: result.current.selection, total: 9 })));
    expect(result.current.progress?.state).toBe('cancelling');
    act(() => handlers['hips-download-progress'](status({ state: 'cancelled', selection: result.current.selection, total: 9 })));
    expect(result.current.progress?.state).toBe('cancelled');
  });

  it('listener is registered once under StrictMode double mount', async () => {
    wire(status());
    const wrapper = ({ children }: { children: ReactNode }) => createElement(StrictMode, null, children);
    const { unmount } = renderHook(() => useHipsPack(SURVEY), { wrapper });
    await waitFor(() => expect(active).toBe(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(active).toBe(1);
    unmount();
    expect(active).toBe(0);
  });
});
