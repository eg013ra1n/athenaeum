import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import CollabLiveStatus, { dotState, liveStatusLabel, pillLabel, SYNC_WAIT_MS } from './CollabLiveStatus';
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
    expect(liveStatusLabel({ ...base, state: 'reconnecting', retryInSecs: 3 }, 9)).toBe('Reconnecting…');
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

const NOW = '2026-09-29T10:00:00Z';

/** Sets `get_collab_live_status`'s answer; every other command answers null. */
function mockStatus(s: Status) {
  vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
    cmd === 'get_collab_live_status' ? Promise.resolve(s) : Promise.resolve(null)) as never);
}

/** The pill in the providers the component needs (notifications for a
 *  failed Sync; the router for the toast stack). */
const renderPill = (syncedAt: string | null = NOW, onSynced?: () => void) =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        <CollabLiveStatus variant="pill" syncedAt={syncedAt} onSynced={onSynced} />
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

/** `StatusDot` state → its fill class (ui/Dots.tsx). */
const DOT_CLASS = { live: 'bg-success', offline: 'bg-border', warn: 'bg-warning', error: 'bg-error' } as const;

describe('CollabLiveStatus pill variant', () => {
  it.each([
    [{ state: 'connecting' }, 'Connecting…', 'offline'],
    [{ state: 'reconnecting', retryInSecs: 12 }, 'Reconnecting in 12 s', 'offline'],
    [{ state: 'unreachable' }, 'Hub unreachable — retrying', 'error'],
    [{ state: 'signedOut' }, 'Signed out', 'offline'],
    [{ state: 'live', storage: 'unavailable', storageReason: 'path_missing' }, 'Online · storage unavailable (the Collaboration folder is missing)', 'error'],
  ] as [Partial<Status>, string, keyof typeof DOT_CLASS][])('pill variant shows %o as one pill (review focus 5)', async (patch, label, dot) => {
    mockStatus({ state: 'live', retryInSecs: null, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false, ...patch });
    renderPill();
    const pill = await screen.findByRole('button', { name: new RegExp(label.replace(/[()]/g, '\\$&')) });
    expect(pill.className).toContain('rounded-full');
    expect(pill).toHaveTextContent(label);
    // One pill: the dot is its first child, and there is no second button.
    expect(pill.firstElementChild?.className).toContain(DOT_CLASS[dot]);
    expect(screen.getAllByRole('button')).toHaveLength(1);
  });

  it('pill variant reads "Live · synced N s ago" and runs Sync on click', async () => {
    vi.useFakeTimers({ now: new Date('2026-09-29T10:00:04Z') });
    try {
      mockStatus({ state: 'live', retryInSecs: null, since: NOW, storage: 'available', storageReason: null, watcherDegraded: false, networkVolume: false });
      renderPill('2026-09-29T10:00:00Z');
      await advance(0);
      const pill = screen.getByRole('button', { name: 'Live · synced 4 s ago' });
      expect(pill.firstElementChild?.className).toContain(DOT_CLASS.live);
      fireEvent.click(pill);
      expect(api.invoke).toHaveBeenCalledWith('collab_sync_now');
    } finally {
      vi.useRealTimers();
    }
  });

  it('the synced age ticks every second and turns into minutes from 60 s', async () => {
    vi.useFakeTimers({ now: new Date('2026-09-29T10:00:04Z') });
    try {
      mockStatus({ ...base, since: NOW });
      renderPill('2026-09-29T10:00:00Z');
      await advance(0);
      expect(screen.getByRole('button', { name: 'Live · synced 4 s ago' })).toBeInTheDocument();
      await advance(1000);
      expect(screen.getByRole('button', { name: 'Live · synced 5 s ago' })).toBeInTheDocument();
      await advance(55_000);
      expect(screen.getByRole('button', { name: 'Live · synced 1 m ago' })).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('reads "Syncing…" while the sync runs, then its label again', async () => {
    let finish: (() => void) | undefined;
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status') return Promise.resolve({ ...base, state: 'connecting' });
      if (cmd === 'collab_sync_now') return new Promise<void>((r) => { finish = r; });
      return Promise.resolve(null);
    }) as never);
    renderPill();
    fireEvent.click(await screen.findByRole('button', { name: 'Connecting…' }));
    expect(await screen.findByRole('button', { name: 'Syncing…' })).toBeDisabled();
    await act(async () => finish?.());
    expect(await screen.findByRole('button', { name: 'Connecting…' })).toBeEnabled();
  });

  it('pill is disabled when collaboration is off', async () => {
    mockStatus({ state: 'off', retryInSecs: null, since: NOW, storage: 'notSet', storageReason: null, watcherDegraded: false, networkVolume: false });
    renderPill();
    expect(await screen.findByRole('button', { name: 'Collaboration is off' })).toBeDisabled();
  });

  it('a failed Sync from the pill is a warning notification', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status') return Promise.resolve(base);
      if (cmd === 'collab_sync_now') return Promise.reject('The live exchange is not running');
      return Promise.resolve(null);
    }) as never);
    renderPill();
    fireEvent.click(await screen.findByRole('button', { name: /^Live · synced/ }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Sync now failed');
  });

  it('periodic-only goes to the pill title, not beside it', async () => {
    mockStatus({ ...base, networkVolume: true });
    renderPill();
    const pill = await screen.findByRole('button', { name: /^Live · synced/ });
    expect(pill.getAttribute('title')).toContain('Changes are seen by periodic check only');
    expect(screen.queryByText('Changes are seen by periodic check only')).not.toBeInTheDocument();
  });

  it('keeps the folder-owner link after the pill, as a link button', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status')
        return Promise.resolve({ ...base, storage: 'unavailable', storageReason: 'other_device' });
      if (cmd === 'get_collab_storage_status')
        return Promise.resolve({ ...storageOk, state: 'unavailable', reason: 'other_device' });
      return Promise.resolve(null);
    }) as never);
    render(
      <MemoryRouter>
        <NotificationProvider>
          <DeviceReplaceProvider>
            <CollabLiveStatus variant="pill" syncedAt={NOW} />
          </DeviceReplaceProvider>
        </NotificationProvider>
      </MemoryRouter>,
    );
    const pill = await screen.findByRole('button', { name: /storage unavailable/ });
    const link = await screen.findByRole('button', { name: 'Resolve the folder owner…' });
    expect(link.className).toContain('text-accent');
    expect(pill.compareDocumentPosition(link) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});

describe('pill helpers', () => {
  it('dotState maps each state to its dot', () => {
    expect(dotState(base)).toBe('live');
    expect(dotState({ ...base, storage: 'readOnly' })).toBe('warn');
    expect(dotState({ ...base, storage: 'unavailable' })).toBe('error');
    expect(dotState({ ...base, state: 'unreachable' })).toBe('error');
    for (const state of ['connecting', 'reconnecting', 'signedOut', 'outdated', 'off'] as const) {
      expect(dotState({ ...base, state })).toBe('offline');
    }
  });

  it('pillLabel shows the synced age only while live with storage available', () => {
    const t = Date.parse(NOW);
    expect(pillLabel(base, 0, NOW, t + 59_000)).toBe('Live · synced 59 s ago');
    expect(pillLabel(base, 0, NOW, t + 60_000)).toBe('Live · synced 1 m ago');
    expect(pillLabel(base, 0, NOW, t - 5_000)).toBe('Live · synced 0 s ago');
    expect(pillLabel(base, 0, null, t)).toBe('Live');
    expect(pillLabel(base, 0, 'not a date', t)).toBe('Live');
    expect(pillLabel({ ...base, storage: 'readOnly' }, 0, NOW, t)).toBe(
      'Online · read-only storage (serving, not downloading)',
    );
    expect(pillLabel({ ...base, state: 'reconnecting', retryInSecs: 12 }, 2, NOW, t)).toBe('Reconnecting in 10 s');
  });

  // Fix round 1: the age rolls to larger units — "N s" under 60 s, "N m"
  // under 60 min, "N h" under 48 h, else "N d" (whole units, rounded down).
  it('pillLabel rolls the age to minutes, hours and days at each boundary', () => {
    const t = Date.parse(NOW);
    const at = (secs: number) => pillLabel(base, 0, NOW, t + secs * 1000);
    expect(at(59)).toBe('Live · synced 59 s ago');
    expect(at(60)).toBe('Live · synced 1 m ago');
    expect(at(59 * 60)).toBe('Live · synced 59 m ago');
    expect(at(59 * 60 + 59)).toBe('Live · synced 59 m ago');
    expect(at(60 * 60)).toBe('Live · synced 1 h ago');
    expect(at(47 * 3600)).toBe('Live · synced 47 h ago');
    expect(at(48 * 3600 - 1)).toBe('Live · synced 47 h ago');
    expect(at(48 * 3600)).toBe('Live · synced 2 d ago');
    expect(at(2187 * 60)).toBe('Live · synced 36 h ago');
  });

  // Fix round 1: core stamps `fetched_at = datetime('now')` — UTC with no
  // zone designator. `Date.parse` reads a zone-less date-time as LOCAL time,
  // so the age was off by the machine's UTC offset.
  it("pillLabel reads core's zone-less datetime('now') stamp as UTC, whatever the machine's zone", () => {
    const now = Date.parse('2026-09-29T10:00:04Z');
    expect(pillLabel(base, 0, '2026-09-29 10:00:00', now)).toBe('Live · synced 4 s ago');
    expect(pillLabel(base, 0, '2026-09-29T10:00:00', now)).toBe('Live · synced 4 s ago');
    // A string that carries its zone is left alone.
    expect(pillLabel(base, 0, '2026-09-29T10:00:00Z', now)).toBe('Live · synced 4 s ago');
    expect(pillLabel(base, 0, '2026-09-29T13:00:00+03:00', now)).toBe('Live · synced 4 s ago');
    expect(pillLabel(base, 0, '2026-09-29T07:00:00-03:00', now)).toBe('Live · synced 4 s ago');
  });
});

describe('CollabLiveStatus pill — fix round 1', () => {
  it("the pill reads a datetime('now')-shaped fetchedAt as UTC", async () => {
    vi.useFakeTimers({ now: new Date('2026-09-29T10:00:04Z') });
    try {
      mockStatus({ ...base, since: NOW });
      renderPill('2026-09-29 10:00:00');
      await advance(0);
      expect(screen.getByRole('button', { name: 'Live · synced 4 s ago' })).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('onSynced runs once after a successful Sync', async () => {
    const onSynced = vi.fn();
    renderPill(NOW, onSynced);
    fireEvent.click(await screen.findByRole('button', { name: /^Live · synced/ }));
    await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1));
    expect(api.invoke).toHaveBeenCalledWith('collab_sync_now');
  });

  it('onSynced does not run after a failed Sync', async () => {
    const onSynced = vi.fn();
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'get_collab_live_status') return Promise.resolve(base);
      if (cmd === 'collab_sync_now') return Promise.reject('The live exchange is not running');
      return Promise.resolve(null);
    }) as never);
    renderPill(NOW, onSynced);
    fireEvent.click(await screen.findByRole('button', { name: /^Live · synced/ }));
    expect(await screen.findByText('Sync now failed')).toBeInTheDocument();
    expect(onSynced).not.toHaveBeenCalled();
  });
});

describe('CollabLiveStatus pill — waits for the hub (spec §6.4)', () => {
  const handlers = new Map<string, (p: unknown) => void>();
  const emitEv = (ev: string, p: unknown) => act(() => handlers.get(ev)?.(p));
  const synced = (o: Record<string, unknown> = {}) => ({
    projectId: 'p1',
    syncedAt: new Date(Date.now() + 5).toISOString(),
    ok: true,
    error: null,
    changed: false,
    ...o,
  });

  beforeEach(() => {
    handlers.clear();
    vi.mocked(api.listen).mockImplementation((async (ev: string, cb: (p: unknown) => void) => {
      handlers.set(ev, cb);
      return () => {
        handlers.delete(ev);
      };
    }) as never);
  });

  const renderWait = (props: Record<string, unknown>) =>
    render(
      <MemoryRouter>
        <NotificationProvider>
          <CollabLiveStatus variant="pill" {...props} />
          <ToastStack />
        </NotificationProvider>
      </MemoryRouter>,
    );

  it("the click waits for this project's synced report, then calls onSynced", async () => {
    const onSynced = vi.fn();
    renderWait({ projectId: 'p1', syncedAt: new Date(Date.now() - 50_000).toISOString(), onSynced });
    fireEvent.click(await screen.findByRole('button', { name: /synced/ }));
    expect(await screen.findByText('Syncing…')).toBeInTheDocument();
    await waitFor(() => expect(handlers.has('collab-project-synced')).toBe(true));
    emitEv('collab-project-synced', synced({ projectId: 'p2', changed: true }));
    expect(screen.getByText('Syncing…')).toBeInTheDocument();
    emitEv('collab-project-synced', synced());
    await waitFor(() => expect(screen.queryByText('Syncing…')).toBeNull());
    expect(screen.getByRole('button', { name: /synced [0-2] s ago/ })).toBeInTheDocument();
    expect(onSynced).toHaveBeenCalledTimes(1);
  });

  it('a_not_ok_report_stops_the_pill_and_notifies', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const onSynced = vi.fn();
    renderWait({ projectId: 'p1', syncedAt: null, onSynced });
    fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
    await waitFor(() => expect(handlers.has('collab-project-synced')).toBe(true));
    emitEv('collab-project-synced', synced({ syncedAt: null, ok: false, error: 'the hub refused the project (403)' }));
    await waitFor(() => expect(screen.queryByText('Syncing…')).toBeNull());
    expect(
      await screen.findByText('Sync did not complete — the hub refused the project (403)'),
    ).toBeInTheDocument();
    expect(err).toHaveBeenCalled();
    expect(onSynced).not.toHaveBeenCalled();
    err.mockRestore();
  });

  it('an ok report stamped before the click does not end the wait', async () => {
    renderWait({ projectId: 'p1', syncedAt: null });
    fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
    await waitFor(() => expect(handlers.has('collab-project-synced')).toBe(true));
    emitEv('collab-project-synced', synced({ syncedAt: new Date(Date.now() - 5_000).toISOString(), changed: true }));
    expect(screen.getByText('Syncing…')).toBeInTheDocument();
  });

  it('thirty seconds without a report stop the pill and say there was no answer', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      renderWait({ projectId: 'p1', syncedAt: null });
      fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
      await act(async () => {
        vi.advanceTimersByTime(SYNC_WAIT_MS);
      });
      expect(screen.queryByText('Syncing…')).toBeNull();
      expect(screen.getByText('Sync did not complete — no answer from the hub')).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
      err.mockRestore();
    }
  });

  it('a status that leaves live shows at once while the wait continues', async () => {
    const onSynced = vi.fn();
    renderWait({ projectId: 'p1', syncedAt: null, onSynced });
    fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
    await waitFor(() => expect(handlers.has('collab-live-status')).toBe(true));
    expect(await screen.findByText('Syncing…')).toBeInTheDocument();
    emitEv('collab-live-status', { ...base, state: 'unreachable' });
    expect(screen.getByRole('button', { name: 'Hub unreachable — retrying' })).toBeInTheDocument();
    expect(screen.queryByText('Syncing…')).toBeNull();
    emitEv('collab-live-status', base);
    emitEv('collab-project-synced', synced());
    await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1));
  });

  it('F5: a synced report restarts the age without a card re-read', async () => {
    renderWait({ projectId: 'p1', syncedAt: new Date(Date.now() - 50_000).toISOString() });
    expect(await screen.findByRole('button', { name: /synced 5\d s ago/ })).toBeInTheDocument();
    await waitFor(() => expect(handlers.has('collab-project-synced')).toBe(true));
    emitEv('collab-project-synced', synced({ syncedAt: new Date().toISOString() }));
    expect(await screen.findByRole('button', { name: /synced [0-2] s ago/ })).toBeInTheDocument();
  });

  it('without projectId the pill keeps calling onSynced right after collab_sync_now', async () => {
    const onSynced = vi.fn();
    renderWait({ syncedAt: null, onSynced });
    fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
    await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1));
  });

  it('a failed collab_sync_now ends the wait and notifies Sync now failed', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockImplementation((async (cmd: string) => {
      if (cmd === 'collab_sync_now') throw new Error('not signed in');
      return base;
    }) as never);
    renderWait({ projectId: 'p1', syncedAt: null });
    fireEvent.click(await screen.findByRole('button', { name: /Live/ }));
    expect(await screen.findByText('Sync now failed')).toBeInTheDocument();
    expect(screen.queryByText('Syncing…')).toBeNull();
    err.mockRestore();
  });
});
