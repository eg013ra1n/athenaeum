import { describe, expect, it, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useProjects } from './useProjects';
import { api } from '../api';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));

vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

vi.mock('../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

beforeEach(() => {
  notifyMock.mockReset();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

describe('useProjects', () => {
  it('sets updateRequired and does not notify when the hub refuses with collab_api_outdated', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'list_collab_projects':
          return Promise.resolve([]);
        case 'refresh_collab_projects':
          return Promise.reject(
            new Error(
              'collab_api_outdated: this hub needs a newer Athenaeum — update to keep collaborating',
            ),
          );
        default:
          return Promise.resolve([]);
      }
    }) as never);

    const { result } = renderHook(() => useProjects());

    await waitFor(() => expect(result.current.loading).toBe(false));
    await waitFor(() => expect(result.current.updateRequired).toBe(true));

    expect(result.current.signedOut).toBe(false);
    expect(notifyMock).not.toHaveBeenCalled();
    // The removed frames poll is never called, and the automatic refresh never
    // runs "Sync now" (it clears every back-off and reconnects — a user step).
    expect(api.invoke).not.toHaveBeenCalledWith('refresh_collab_frames');
    expect(api.invoke).not.toHaveBeenCalledWith('collab_sync_now');
  });

  it('does not flag updateRequired on a normal successful refresh, and never syncs by itself', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'list_collab_projects':
        case 'refresh_collab_projects':
          return Promise.resolve([]);
        default:
          return Promise.resolve(null);
      }
    }) as never);

    const { result } = renderHook(() => useProjects());

    await waitFor(() => expect(result.current.loading).toBe(false));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('refresh_collab_projects'));

    expect(result.current.updateRequired).toBe(false);
    expect(result.current.signedOut).toBe(false);
    expect(api.invoke).not.toHaveBeenCalledWith('refresh_collab_frames');
    expect(api.invoke).not.toHaveBeenCalledWith('collab_sync_now');
  });

  it('the manual refresh runs "Sync now" once the projects refreshed', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'list_collab_projects':
        case 'refresh_collab_projects':
          return Promise.resolve([]);
        default:
          return Promise.resolve(null);
      }
    }) as never);

    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.refresh();
    });

    expect(api.invoke).toHaveBeenCalledWith('collab_sync_now');
    expect(api.invoke).not.toHaveBeenCalledWith('refresh_collab_frames');
  });

  it('a failed "Sync now" on the manual refresh is logged, not a sign-out', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'list_collab_projects':
        case 'refresh_collab_projects':
          return Promise.resolve([]);
        case 'collab_sync_now':
          return Promise.reject('The live exchange is not running (signed out, or no Collaboration folder yet).');
        default:
          return Promise.resolve(null);
      }
    }) as never);

    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.signedOut).toBe(false);
    expect(errorSpy).toHaveBeenCalledWith('[projects] sync now failed:', expect.anything());
    errorSpy.mockRestore();
  });
});
