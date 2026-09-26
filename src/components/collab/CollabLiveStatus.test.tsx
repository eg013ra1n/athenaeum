import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import CollabLiveStatus, { liveStatusLabel } from './CollabLiveStatus';
import DeviceReplaceDialog, { DeviceReplaceProvider } from './DeviceReplaceDialog';
import { api } from '../../api';
import type { CollabLiveStatus as Status, CollabStorageStatus } from '../../types/models';
import { advance } from '../../test/fakeClock';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const base: Status = {
  state: 'live',
  retryInSecs: null,
  since: '2026-09-25T10:00:00Z',
  storage: 'available',
  storageReason: null,
  watcherDegraded: false,
  networkVolume: false,
};

const storageOk: CollabStorageStatus = {
  state: 'available',
  reason: null,
  root: '/c',
  watcherDegraded: false,
  networkVolume: false,
  replace: null,
  unknownDevice: null,
};

let emit: ((p: Status) => void) | undefined;

beforeEach(() => {
  emit = undefined;
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
    cmd === 'get_collab_live_status' ? Promise.resolve(base) : Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, h: (p: Status) => void) => {
    if (name === 'collab-live-status') emit = h;
    return Promise.resolve(() => {});
  }) as never);
});

const renderIt = () =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        <CollabLiveStatus />
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

describe('liveStatusLabel', () => {
  it('labels every state', () => {
    expect(liveStatusLabel(base, 0)).toBe('Live');
    expect(liveStatusLabel({ ...base, state: 'connecting' }, 0)).toBe('Connecting…');
    expect(liveStatusLabel({ ...base, state: 'reconnecting', retryInSecs: 12 }, 0)).toBe('Reconnecting in 12 s');
    expect(liveStatusLabel({ ...base, state: 'reconnecting', retryInSecs: 12 }, 5)).toBe('Reconnecting in 7 s');
    expect(liveStatusLabel({ ...base, state: 'reconnecting', retryInSecs: 3 }, 9)).toBe('Reconnecting in 0 s');
    expect(liveStatusLabel({ ...base, state: 'unreachable' }, 0)).toBe('Hub unreachable — retrying');
    expect(liveStatusLabel({ ...base, state: 'signedOut' }, 0)).toBe('Signed out');
    expect(liveStatusLabel({ ...base, state: 'outdated' }, 0)).toBe('Update required');
    expect(liveStatusLabel({ ...base, state: 'off' }, 0)).toBe('Collaboration is off');
  });

  it('names the storage state while live', () => {
    expect(liveStatusLabel({ ...base, storage: 'unavailable', storageReason: 'other_device' }, 0)).toBe(
      'Online · storage unavailable (this folder belongs to another device)',
    );
    expect(liveStatusLabel({ ...base, storage: 'unavailable', storageReason: 'marker_mismatch' }, 0)).toBe(
      'Online · storage unavailable (another disk is mounted there)',
    );
    for (const reason of ['path_missing', 'not_a_directory', 'marker_missing']) {
      expect(liveStatusLabel({ ...base, storage: 'unavailable', storageReason: reason }, 0)).toBe(
        'Online · storage unavailable (the Collaboration folder is missing)',
      );
    }
    expect(liveStatusLabel({ ...base, storage: 'unavailable', storageReason: null }, 0)).toBe(
      'Online · storage unavailable',
    );
    expect(liveStatusLabel({ ...base, storage: 'readOnly' }, 0)).toBe(
      'Online · read-only storage (serving, not downloading)',
    );
  });
});

describe('CollabLiveStatus', () => {
  it('follows the live event and Sync now calls the global command', async () => {
    renderIt();
    expect(await screen.findByText('Live')).toBeInTheDocument();
    act(() => emit?.({ ...base, state: 'reconnecting', retryInSecs: 5 }));
    expect(await screen.findByText(/Reconnecting in \d+ s/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Sync now' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('collab_sync_now'));
  });

  it('counts the reconnect down once a second', async () => {
    // A fully fake clock: with `shouldAdvanceTime` real elapsed time also
    // moved it, so a stall could add a tick the test never asked for.
    vi.useFakeTimers();
    try {
      renderIt();
      await advance(0);
      expect(screen.getByText('Live')).toBeInTheDocument();
      act(() => emit?.({ ...base, state: 'reconnecting', retryInSecs: 5 }));
      expect(screen.getByText('Reconnecting in 5 s')).toBeInTheDocument();
      await advance(2000);
      expect(screen.getByText('Reconnecting in 3 s')).toBeInTheDocument();
      // A fresh event restarts the count from its own retryInSecs.
      act(() => emit?.({ ...base, state: 'reconnecting', retryInSecs: 10, since: '2026-09-25T10:01:00Z' }));
      expect(screen.getByText('Reconnecting in 10 s')).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('says when changes are seen by periodic check only', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      cmd === 'get_collab_live_status'
        ? Promise.resolve({ ...base, watcherDegraded: true })
        : Promise.resolve(null)) as never);
    renderIt();
    expect(await screen.findByText('Changes are seen by periodic check only')).toBeInTheDocument();
  });

  it('a failed Sync now is a warning notification', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status') return Promise.resolve(base);
      if (cmd === 'collab_sync_now') return Promise.reject('The live exchange is not running');
      return Promise.resolve(null);
    }) as never);
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Sync now' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Sync now failed');
  });

  it('never reads or classifies the storage itself', async () => {
    renderIt();
    await screen.findByText('Live');
    act(() => emit?.({ ...base, storage: 'unavailable', storageReason: 'other_device' }));
    await screen.findByText(/storage unavailable/);
    expect(api.invoke).not.toHaveBeenCalledWith('get_collab_storage_status');
    expect(api.invoke).not.toHaveBeenCalledWith('check_collab_folder_owner', expect.anything());
  });

  it('offers "Replace a device…" when the folder belongs to another device, and it opens the dialog', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status')
        return Promise.resolve({ ...base, storage: 'unavailable', storageReason: 'other_device' });
      if (cmd === 'get_collab_storage_status')
        return Promise.resolve({
          ...storageOk,
          state: 'unavailable',
          reason: 'other_device',
          replace: {
            deviceId: 'old-id',
            deviceName: 'Old laptop',
            lastSeenAt: '2026-09-22T00:00:00Z',
            offlineDays: 3,
            prompt: false,
            proposeRetire: false,
            path: '/c',
            markerMismatch: false,
          },
        });
      return Promise.resolve(null);
    }) as never);
    render(
      <MemoryRouter>
        <NotificationProvider>
          <DeviceReplaceProvider>
            <CollabLiveStatus />
            <DeviceReplaceDialog />
          </DeviceReplaceProvider>
        </NotificationProvider>
      </MemoryRouter>,
    );
    // `prompt` is false (offline for 3 days): no modal on its own…
    const link = await screen.findByRole('button', { name: 'Replace a device…' });
    expect(screen.queryByText('This device replaces Old laptop')).not.toBeInTheDocument();
    // …but the link opens the same dialog.
    fireEvent.click(link);
    expect(await screen.findByText('This device replaces Old laptop')).toBeInTheDocument();
  });
});
