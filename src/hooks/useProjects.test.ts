import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
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
    // The outdated refusal must never fall through to the frames poll (the
    // projects refresh did not succeed, so there is nothing fresh to poll).
    expect(api.invoke).not.toHaveBeenCalledWith('refresh_collab_frames');
  });

  it('does not flag updateRequired on a normal successful refresh', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'list_collab_projects':
        case 'refresh_collab_projects':
          return Promise.resolve([]);
        case 'refresh_collab_frames':
          return Promise.resolve([]);
        default:
          return Promise.resolve([]);
      }
    }) as never);

    const { result } = renderHook(() => useProjects());

    await waitFor(() => expect(result.current.loading).toBe(false));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('refresh_collab_frames'));

    expect(result.current.updateRequired).toBe(false);
    expect(result.current.signedOut).toBe(false);
  });
});
