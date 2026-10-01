import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../../api';
import { NotificationProvider, useNotifications } from '../../../contexts/NotificationContext';
import { gb, noSpaceRefusal, noSpaceText, usePublishing } from './usePublishing';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

/** Core's free-space refusal: decimal bytes, `needed` includes the 1 GB reserve. */
const NO_SPACE = 'collab_no_space:12500000000:3200000000';
const NO_SPACE_TEXT =
  "Not enough free space for the calibrated frames: 12.5 GB needed (incl. 1 GB reserve), 3.2 GB free on the Collaboration folder's disk.";

describe('noSpaceRefusal', () => {
  it('parses needed and free bytes', () => {
    expect(noSpaceRefusal(NO_SPACE)).toEqual({ needed: 12_500_000_000, free: 3_200_000_000 });
    expect(noSpaceRefusal('collab_no_space:1000000000:0')).toEqual({ needed: 1_000_000_000, free: 0 });
  });

  it.each([
    [''],
    ['collab_no_space:'],
    ['collab_no_space:12500000000'],
    ['collab_no_space:12500000000:'],
    ['collab_no_space:abc:3200000000'],
    ['collab_no_space:12.5:3.2'],
    ['collab_no_space:-1:3200000000'],
    ['collab_no_space:1:2:3'],
    ['disk full'],
    ['collab_publishing_device:Obs PC'],
  ])('a malformed or unrelated message is not a no-space refusal: %j', (msg) => {
    expect(noSpaceRefusal(msg)).toBeNull();
  });
});

describe('noSpaceText', () => {
  it('names both sizes in decimal GB with one decimal', () => {
    expect(noSpaceText({ needed: 12_500_000_000, free: 3_200_000_000 })).toBe(NO_SPACE_TEXT);
    expect(gb(1_049_999_999)).toBe('1.0 GB');
    expect(gb(0)).toBe('0.0 GB');
  });
});

describe('usePublishing — a free-space refusal', () => {
  const opts = { reloadDetail: vi.fn(async () => {}), reloadOwn: vi.fn(async () => {}), onCard: () => {} };
  const wrapper = ({ children }: { children: React.ReactNode }) => <NotificationProvider>{children}</NotificationProvider>;

  beforeEach(() => {
    localStorage.clear();
    vi.mocked(api.invoke).mockReset();
    vi.mocked(api.listen).mockReset();
    vi.mocked(api.listen).mockResolvedValue(() => {});
  });

  it.each([
    ['calibrate', 'calibrate_collab_frames', 'calibrateError'],
    ['publish', 'publish_collab_frames', 'publishError'],
    ['republish', 'republish_collab_frames', 'republishError'],
  ] as const)('%s shows the sizes inline, not the raw code, and toasts nothing', async (action, command, field) => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    // Tauri rejects with the bare string; the web host with the response body.
    vi.mocked(api.invoke).mockRejectedValueOnce(NO_SPACE);
    const { result } = renderHook(() => ({ p: usePublishing('proj-1', opts), n: useNotifications() }), { wrapper });
    await act(async () => { await result.current.p[action]([1, 2]); });
    expect(api.invoke).toHaveBeenCalledWith(command, { projectId: 'proj-1', frameIds: [1, 2] });
    expect(result.current.p[field]).toBe(NO_SPACE_TEXT);
    expect(err).toHaveBeenCalled();
    // F4: core ends the run `refused` with the code; collab-publish-finished
    // is the one notification.
    expect(result.current.n.toasts).toHaveLength(0);
    expect(result.current.n.notifications).toHaveLength(0);
    expect(result.current.p.refusedBy).toBeNull();
    expect(result.current.p.updateRequired).toBe(false);
    err.mockRestore();
  });
});
