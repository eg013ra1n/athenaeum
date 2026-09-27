import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import DeviceReplaceDialog, { DeviceReplaceProvider, useDeviceReplace } from './DeviceReplaceDialog';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  CollabLiveStatus,
  CollabStorageStatus,
  DeviceReplaceOfferView,
  UnknownDeviceView,
} from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const offer: DeviceReplaceOfferView = {
  deviceId: 'old-id',
  deviceName: 'Old laptop',
  lastSeenAt: '2026-09-10T00:00:00Z',
  offlineDays: 15,
  prompt: true,
  proposeRetire: false,
  path: '/c',
  markerMismatch: false,
  checkedAt: null,
};

const unknown: UnknownDeviceView = {
  deviceId: 'k51qzi5uqu5dlvj2baxnqndepeb86cbk3ng7n3i46uzyxzyqj2xjonzllnv0v8',
  path: '/c',
  recordedOffline: false,
  markerMismatch: false,
  checkedAt: null,
};

const storage = (over: Partial<CollabStorageStatus>): CollabStorageStatus => ({
  state: 'unavailable',
  reason: 'other_device',
  root: '/c',
  watcherDegraded: false,
  networkVolume: false,
  replace: null,
  unknownDevice: null,
  ...over,
});

const live: CollabLiveStatus = {
  state: 'live',
  retryInSecs: null,
  since: '2026-09-25T10:00:00Z',
  storage: 'unavailable',
  storageReason: 'other_device',
  watcherDegraded: false,
  networkVolume: false,
};

let emitLive: ((p: CollabLiveStatus) => void) | undefined;

/** `get_collab_storage_status` answers `status`; the rest per `extra`. */
function mockCommands(status: CollabStorageStatus, extra: Record<string, () => Promise<unknown>> = {}) {
  vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
    if (cmd === 'get_collab_storage_status') return Promise.resolve(status);
    if (extra[cmd]) return extra[cmd]();
    return Promise.resolve(null);
  }) as never);
}

beforeEach(() => {
  emitLive = undefined;
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockImplementation(((name: string, h: (p: CollabLiveStatus) => void) => {
    if (name === 'collab-live-status') emitLive = h;
    return Promise.resolve(() => {});
  }) as never);
});

function Opener() {
  const { requestOpen } = useDeviceReplace();
  return (
    <button type="button" onClick={requestOpen}>
      open folder owner
    </button>
  );
}

const renderWithProvider = () =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        <DeviceReplaceProvider>
          <Opener />
          <DeviceReplaceDialog />
        </DeviceReplaceProvider>
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

describe('DeviceReplaceDialog — replace (another device of this account)', () => {
  it('offers the replace for a device offline more than 7 days and confirms it', async () => {
    mockCommands(storage({ replace: offer }), {
      collab_replace_device: () => Promise.resolve({ scanned: 12, adopted: 12 }),
    });
    render(
      <MemoryRouter>
        <NotificationProvider>
          <DeviceReplaceDialog />
          <ToastStack />
        </NotificationProvider>
      </MemoryRouter>,
    );
    expect(await screen.findByText('This device replaces Old laptop')).toBeInTheDocument();
    expect(
      screen.getByText(
        'Old laptop has been offline for 15 days. Replacing it retires that device and adopts the files already in this folder — nothing is downloaded again.',
      ),
    ).toBeInTheDocument();
    // The offer's age is as of the last check: say so, keep "Check again" in reach.
    expect(screen.getByText(/as of the last check/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeInTheDocument();
    expect(screen.queryByText(/can be retired/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Replace Old laptop' }));
    // The replace names the folder the dialog shows — never left to core's pick.
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('collab_replace_device', { deviceId: 'old-id', root: '/c' }),
    );
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('Adopted 12 of 12 files');
    await waitFor(() => expect(screen.queryByText('This device replaces Old laptop')).not.toBeInTheDocument());
  });

  it('stays closed when there is nothing to replace', async () => {
    vi.mocked(api.invoke).mockImplementation((() =>
      Promise.resolve(storage({ state: 'available', reason: null }))) as never);
    const { container } = render(
      <MemoryRouter>
        <NotificationProvider>
          <DeviceReplaceDialog />
        </NotificationProvider>
      </MemoryRouter>,
    );
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_storage_status'));
    expect(container).toBeEmptyDOMElement();
  });

  it('Escape closes the prompt that opened on its own', async () => {
    mockCommands(storage({ replace: offer }));
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    // The listener is attached in the commit that shows the dialog, so the
    // Escape is handled at once — no waiting that could hide a late listener.
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.queryByText('This device replaces Old laptop')).not.toBeInTheDocument();
  });

  it('proposes retiring a device offline for more than 30 days', async () => {
    mockCommands(storage({ replace: { ...offer, offlineDays: 40, proposeRetire: true } }));
    renderWithProvider();
    expect(await screen.findByText('Old laptop can be retired (offline for more than 30 days).')).toBeInTheDocument();
  });

  it('"Not now" closes it for this app start; the link still opens it', async () => {
    mockCommands(storage({ replace: offer }));
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    fireEvent.click(screen.getByRole('button', { name: 'Not now' }));
    expect(screen.queryByText('This device replaces Old laptop')).not.toBeInTheDocument();

    // A later storage event re-reads the status, but does not reopen it.
    act(() => emitLive?.(live));
    await waitFor(() =>
      expect(vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_collab_storage_status')).toHaveLength(2),
    );
    expect(screen.queryByText('This device replaces Old laptop')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'open folder owner' }));
    expect(await screen.findByText('This device replaces Old laptop')).toBeInTheDocument();
  });

  it('explains a swapped disk instead of offering a replace that would be refused', async () => {
    mockCommands(storage({ replace: { ...offer, markerMismatch: true } }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    expect(await screen.findByText(/another disk is mounted at \/c/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Replace Old laptop' })).not.toBeInTheDocument();
  });
});

describe('DeviceReplaceDialog — take-over (a device this account does not list)', () => {
  it('confirms with a clear warning before taking the folder over', async () => {
    mockCommands(storage({ unknownDevice: unknown }), {
      take_over_collab_folder: () => Promise.resolve({ scanned: 4, adopted: 3 }),
    });
    renderWithProvider();
    // Never opened on its own for an unknown device…
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_storage_status'));
    expect(screen.queryByText('This folder belongs to another device')).not.toBeInTheDocument();
    // …the user opens it.
    fireEvent.click(screen.getByRole('button', { name: 'open folder owner' }));
    expect(await screen.findByText('This folder belongs to another device')).toBeInTheDocument();
    expect(screen.queryByText(/Recorded while offline/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Take over this folder…' }));
    expect(
      await screen.findByText(/This folder was written by a device this account does not list/),
    ).toBeInTheDocument();
    expect(api.invoke).not.toHaveBeenCalledWith('take_over_collab_folder', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: 'Take over' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('take_over_collab_folder', { root: '/c', confirmed: true }),
    );
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('Adopted 3 of 4 files');
  });

  it('labels a classification recorded while offline, in the dialog and in the take-over confirm', async () => {
    mockCommands(storage({ unknownDevice: { ...unknown, recordedOffline: true } }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    expect(
      await screen.findByText('Recorded while offline — the device may belong to this account. Check again once online.'),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Take over this folder…' }));
    expect(
      await screen.findByText(
        /\(recorded while offline — it may be one of this account's devices; Check again while online first\)/,
      ),
    ).toBeInTheDocument();
  });

  it('a confirm for a folder classified online carries no offline caveat', async () => {
    mockCommands(storage({ unknownDevice: unknown }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Take over this folder…' }));
    const message = await screen.findByText(/This folder was written by a device this account does not list/);
    expect(message).not.toHaveTextContent(/recorded while offline/);
  });

  it('explains a swapped disk instead of offering a take-over that would be refused', async () => {
    mockCommands(storage({ unknownDevice: { ...unknown, markerMismatch: true } }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    expect(await screen.findByText(/another disk is mounted at \/c/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Take over this folder…' })).not.toBeInTheDocument();
    expect(api.invoke).not.toHaveBeenCalledWith('take_over_collab_folder', expect.anything());
  });
});

describe('DeviceReplaceDialog — Check again', () => {
  it('an unclassified folder offers "Check again", which asks the hub and shows the answer', async () => {
    mockCommands(storage({}), {
      check_collab_folder_owner: () => Promise.resolve(storage({ replace: { ...offer, prompt: false } })),
    });
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    expect(await screen.findByText(/marker names another device/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('check_collab_folder_owner', { root: '/c' }));
    expect(await screen.findByText('This device replaces Old laptop')).toBeInTheDocument();
  });

  it('a check the hub could not answer is a warning and keeps what was shown', async () => {
    mockCommands(storage({ replace: offer }), {
      check_collab_folder_owner: () => Promise.reject('the hub could not be asked'),
    });
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('Could not check the folder owner');
    expect(screen.getByText('This device replaces Old laptop')).toBeInTheDocument();
  });

  it('says that checking the designated folder replaces a pending offer for another folder, and Replace acts on the folder shown', async () => {
    mockCommands(storage({ root: '/a', replace: { ...offer, path: '/b', prompt: false } }), {
      collab_replace_device: () => Promise.resolve({ scanned: 2, adopted: 2 }),
    });
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    expect(
      await screen.findByText(/Checking \/a replaces the pending offer for \/b/),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Replace Old laptop' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('collab_replace_device', { deviceId: 'old-id', root: '/b' }),
    );
  });

  it('re-reads the status only when the (storage, reason) pair changes', async () => {
    mockCommands(storage({ state: 'available', reason: null }));
    renderWithProvider();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_storage_status'));
    const reads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_collab_storage_status').length;
    expect(reads()).toBe(1);

    act(() => emitLive?.(live));
    await waitFor(() => expect(reads()).toBe(2));
    // The same pair again (a reconnect, a countdown change): no read.
    act(() => emitLive?.({ ...live, state: 'reconnecting', retryInSecs: 5 }));
    act(() => emitLive?.(live));
    await act(async () => {
      await Promise.resolve();
    });
    expect(reads()).toBe(2);
    // Another reason: read again.
    act(() => emitLive?.({ ...live, storageReason: 'marker_mismatch' }));
    await waitFor(() => expect(reads()).toBe(3));
  });

  it('reads the status on mount and on storage events only — never on a timer, never asks the hub by itself', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      mockCommands(storage({ state: 'available', reason: null }));
      renderWithProvider();
      await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_storage_status'));
      const reads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_collab_storage_status').length;
      expect(reads()).toBe(1);

      await act(async () => {
        vi.advanceTimersByTime(30 * 60 * 1000);
      });
      expect(reads()).toBe(1);

      act(() => emitLive?.(live));
      await waitFor(() => expect(reads()).toBe(2));
      expect(api.invoke).not.toHaveBeenCalledWith('check_collab_folder_owner', expect.anything());
      expect(api.invoke).not.toHaveBeenCalledWith('check_collab_folder_owner');
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('DeviceReplaceDialog — when the answer was last checked', () => {
  const checkedAt = '2026-09-26T08:30:00Z';

  it('prints "last checked" next to the recorded last-seen, and keeps "Check again"', async () => {
    mockCommands(storage({ replace: { ...offer, checkedAt } }));
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    const line = screen.getByText(/^Last seen /);
    expect(line).toHaveTextContent(`Last seen ${formatTimestamp(offer.lastSeenAt!)}`);
    expect(line).toHaveTextContent(`last checked ${formatTimestamp(checkedAt)}`);
    expect(screen.getByRole('button', { name: 'Check again' })).toBeInTheDocument();
  });

  it('a device never seen online still says when it was last checked', async () => {
    mockCommands(storage({ replace: { ...offer, lastSeenAt: null, offlineDays: null, checkedAt } }));
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    expect(screen.getByText(/^Never seen online/)).toHaveTextContent(`last checked ${formatTimestamp(checkedAt)}`);
  });

  it('a record without a check time prints no timestamp', async () => {
    mockCommands(storage({ replace: offer }));
    renderWithProvider();
    await screen.findByText('This device replaces Old laptop');
    expect(screen.queryByText(/last checked/)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeInTheDocument();
  });

  it('a folder of an unlisted device says when that was last checked', async () => {
    mockCommands(storage({ unknownDevice: { ...unknown, checkedAt } }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    await screen.findByText('This folder belongs to another device');
    expect(screen.getByText(`Last checked ${formatTimestamp(checkedAt)}.`)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeInTheDocument();
  });

  it('a folder of an unlisted device without a check time prints no timestamp', async () => {
    mockCommands(storage({ unknownDevice: unknown }));
    renderWithProvider();
    fireEvent.click(await screen.findByRole('button', { name: 'open folder owner' }));
    await screen.findByText('This folder belongs to another device');
    expect(screen.queryByText(/last checked/i)).not.toBeInTheDocument();
  });
});
