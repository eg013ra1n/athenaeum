// Blink's star overlay loader: a catalog frame by its id, a project frame by its ref, an entry with neither by nothing.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { useStarMetricsCache, type StarSource } from './useStarMetricsCache';
import { api } from '../api';
import type { StarMetricsResponse } from '../types/models';

vi.mock('../api');

const answer = (width: number) => ({ image_width: width, image_height: 10, stars: [], flip_vertical: false }) as unknown as StarMetricsResponse;
const PROJECT_REF = { projectId: 'p1', frame: { frameId: null, frameUuid: 'u1' } };

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(async (command: string) =>
    (command === 'get_frame_star_metrics' ? answer(1) : answer(2)) as never,
  );
});

afterEach(() => vi.restoreAllMocks());

const load = (sources: StarSource[], currentIndex = 0, enabled = true) =>
  renderHook((p: { i: number }) => useStarMetricsCache({ sources, currentIndex: p.i, enabled }), {
    initialProps: { i: currentIndex },
  });

describe('useStarMetricsCache', () => {
  it('loads a catalog frame by its id and a project frame by its ref', async () => {
    const { result } = load([{ frameId: 5 }, PROJECT_REF]);
    await waitFor(() => expect(result.current.getMetrics(1)).not.toBeNull());
    expect(api.invoke).toHaveBeenCalledWith('get_frame_star_metrics', { frameId: 5 });
    expect(api.invoke).toHaveBeenCalledWith('get_collab_frame_star_metrics', PROJECT_REF);
    expect(result.current.getMetrics(0)?.image_width).toBe(1);
    expect(result.current.getMetrics(1)?.image_width).toBe(2);
  });

  it('asks nothing for an entry with no source, and says so once', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const { rerender } = load([null, null]);
    rerender({ i: 1 });
    rerender({ i: 0 });
    await waitFor(() => expect(warn).toHaveBeenCalledTimes(2));
    expect(warn.mock.calls[0]![0]).toContain('entry 0 has no catalog frame and no project ref');
    expect(api.invoke).not.toHaveBeenCalled();
  });

  it('logs a failed load with the frame it was for', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue('analysis failed');
    const { result } = load([PROJECT_REF]);
    await waitFor(() => expect(error).toHaveBeenCalled());
    expect(error.mock.calls[0]![0]).toBe('Failed to load star metrics for project p1 frame u1:');
    expect(result.current.getMetrics(0)).toBeNull();
  });

  it('loads nothing while the overlay is off', () => {
    load([{ frameId: 5 }, PROJECT_REF], 0, false);
    expect(api.invoke).not.toHaveBeenCalled();
  });
});
