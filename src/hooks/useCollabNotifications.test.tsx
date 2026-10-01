import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../contexts/NotificationContext';
import { ToastStack } from '../components/Toast';
import { useCollabNotifications } from './useCollabNotifications';
import { api } from '../api';
import type {
  CollabDeletionChoice,
  CollabFrameChanged,
  CollabFrameLost,
  CollabPublishFinished,
  ProjectCard,
} from '../types/models';

vi.mock('../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

/** Every registered listener, by event name (the last registration wins). */
let listeners: Record<string, (p: unknown) => void> = {};
function emit<T>(event: string, payload: T) {
  const h = listeners[event];
  if (!h) throw new Error(`no listener for ${event}`);
  act(() => h(payload));
}

function projectCard(overrides: Partial<ProjectCard>): ProjectCard {
  return {
    projectId: 'proj-1',
    slug: 'm42-mosaic',
    title: 'M42 Mosaic',
    dataRole: 'send_receive',
    coordinator: false,
    canModerate: false,
    requireApproval: false,
    pendingFrames: 0,
    projectStatus: 'open',
    targetName: 'M42',
    targetRaDeg: 83.8,
    targetDecDeg: -5.4,
    targetRadiusDeg: 1.5,
    membershipVersion: 1,
    linkedSets: 1,
    candidates: 0,
    publishable: 0,
    autoReplicate: true,
    publishMode: 'manual',
    syncedAt: null,
    fetchedAt: '2026-09-24T00:00:00Z',
    publishingDevice: null,
    publishingHere: false,
    ...overrides,
  };
}

beforeEach(() => {
  listeners = {};
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'list_collab_projects') {
      return Promise.resolve([projectCard({})]);
    }
    return Promise.resolve([]);
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    listeners[event] = cb as unknown as (p: unknown) => void;
    return Promise.resolve(() => {});
  }) as never);
});

function Harness() {
  useCollabNotifications();
  return <ToastStack />;
}

function renderHarness() {
  return render(
    <MemoryRouter>
      <NotificationProvider>
        <Harness />
      </NotificationProvider>
    </MemoryRouter>,
  );
}

/** Let the mount-time `list_collab_projects` fetch and every `api.listen`
 * registration settle before firing an event. */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe('useCollabNotifications', () => {
  it('no longer listens for the removed replication-paused event', async () => {
    renderHarness();
    await settle();
    expect(listeners['collab-replication-paused']).toBeUndefined();
  });

  it('collab-deletion-choice is one warning linking the Library tab, deduplicated per occurrence', async () => {
    renderHarness();
    await settle();
    const choice: CollabDeletionChoice = {
      count: 15,
      projectIds: ['proj-1'],
      dedupeKey: 'collab-deletion-choice:proj-1:1001',
    };
    emit('collab-deletion-choice', choice);
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('15 replicas were deleted — choose what to do');
    expect(screen.getByRole('button', { name: '15 replicas were deleted — choose what to do' })).toBeInTheDocument();
    // The detail names the Library tab (the retired page called it Receive).
    const history = localStorage.getItem('athenaeum.notifications.v1') ?? '';
    expect(history).toContain('re-fetch or stop keeping on the Library tab');
    expect(history).not.toContain('Receive tab');

    // A replay of the same batch (same key) is not shown again…
    emit('collab-deletion-choice', choice);
    expect(screen.getAllByRole('status')).toHaveLength(1);

    // …a new batch for the same project always is.
    emit('collab-deletion-choice', { ...choice, dedupeKey: 'collab-deletion-choice:proj-1:1002' });
    expect(screen.getAllByRole('status')).toHaveLength(2);
  });

  it('a deletion choice of one replica reads in the singular', async () => {
    renderHarness();
    await settle();
    emit('collab-deletion-choice', { count: 1, projectIds: ['proj-1'], dedupeKey: 'k:1' });
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('1 replica was deleted — choose what to do');
  });

  it('collab-frame-lost points to the Trash when the file is gone', async () => {
    renderHarness();
    await settle();
    const lost: CollabFrameLost = {
      projectId: 'proj-1',
      frameUuid: 'u2',
      fileName: 'c_b.fits',
      inPreviousFolder: false,
      previousPath: null,
    };
    emit('collab-frame-lost', lost);
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('c_b.fits is lost everywhere — restore it from the Trash');
  });

  it('collab-frame-lost points to the previous Collaboration folder, never the Trash, when the file is still there', async () => {
    renderHarness();
    await settle();
    emit<CollabFrameLost>('collab-frame-lost', {
      projectId: 'proj-1',
      frameUuid: 'u2',
      fileName: 'c_b.fits',
      inPreviousFolder: true,
      previousPath: '/old/collab/m42/c_b.fits',
    });
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('c_b.fits: no member holds it — it is still in the previous Collaboration folder');
    expect(toasts[0]).not.toHaveTextContent(/lost everywhere/);
    expect(toasts[0]).not.toHaveTextContent(/Trash/);
    // The history entry carries the path and the re-fetch promise.
    const stored = JSON.parse(localStorage.getItem('athenaeum.notifications.v1') ?? '{}') as {
      notifications: { title: string; detail: string }[];
    };
    const entry = stored.notifications.find((n) => n.title.startsWith('c_b.fits: no member holds it'));
    expect(entry?.detail).toContain('/old/collab/m42/c_b.fits');
    expect(entry?.detail).toContain('fetched again if another member serves it');
    expect(entry?.detail).not.toMatch(/Trash/);
  });

  it('collab-frame-changed is a warning that the file was set aside', async () => {
    renderHarness();
    await settle();
    emit<CollabFrameChanged>('collab-frame-changed', { projectId: 'proj-1', frameUuid: 'u1', fileName: 'c_a.fits' });
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('c_a.fits changed on disk and was set aside');
  });

  describe('collab-publish-finished (spec 5.4)', () => {
    const run = (patch: Partial<CollabPublishFinished>): CollabPublishFinished => ({
      projectId: 'proj-1', publishRunId: 'r1', kind: 'publish', trigger: 'manual', outcome: 'done',
      calibrated: 0, announced: 0, updated: 0, stale: 0, heldBack: 0, error: null,
      startedAt: '2026-10-01T10:00:00Z', finishedAt: '2026-10-01T10:00:09Z', ...patch,
    });
    type Stored = { title: string; detail: string; tone: string; link?: string; hasErrors?: boolean };
    const history = (): Stored[] =>
      (JSON.parse(localStorage.getItem('athenaeum.notifications.v1') ?? '{"notifications":[]}') as { notifications: Stored[] })
        .notifications;

    async function fire(patch: Partial<CollabPublishFinished>) {
      renderHarness();
      await settle();
      emit('collab-publish-finished', run(patch));
    }

    it('a calibrate run links To review', async () => {
      await fire({ kind: 'calibrate', calibrated: 46 });
      const [n] = history();
      expect(n.title).toBe('Calibrated 46 frames in M42 Mosaic — review them');
      expect(n.link).toBe('/projects/proj-1?tab=mine&segment=review');
      expect(screen.getAllByRole('status')).toHaveLength(1);
    });

    it('an auto run that calibrated and sent nothing says review them (F8)', async () => {
      await fire({ kind: 'auto', trigger: 'auto', calibrated: 5 });
      expect(history()[0].title).toBe('Calibrated 5 frames in M42 Mosaic — review them');
    });

    it('a refusal by the publishing device names the device, never the raw code (F8)', async () => {
      await fire({ outcome: 'refused', error: 'collab_publishing_device:Obs PC' });
      expect(history()[0].title).toBe('Obs PC publishes M42 Mosaic');
      expect(history()[0].detail).not.toContain('collab_publishing_device');
    });

    it('a publish links Published', async () => {
      await fire({ announced: 3, updated: 1 });
      const [n] = history();
      expect(n.title).toBe('Published 4 frames in M42 Mosaic');
      expect(n.link).toBe('/projects/proj-1?tab=mine&segment=published');
    });

    it('nothing sent with frames held back warns and links Held back', async () => {
      await fire({ heldBack: 2 });
      const [n] = history();
      expect(n.title).toBe('Nothing new to publish in M42 Mosaic');
      expect(n.link).toBe('/projects/proj-1?tab=mine&segment=held');
    });

    it('a cancelled run is a history entry, no toast', async () => {
      await fire({ outcome: 'cancelled' });
      expect(history()[0].title).toBe('Stopped in M42 Mosaic');
      expect(screen.queryAllByRole('status')).toHaveLength(0);
    });

    it('a refused auto run is silent, a refused manual run toasts', async () => {
      await fire({ outcome: 'refused', trigger: 'auto', error: 'busy' });
      expect(history()).toHaveLength(1);
      expect(screen.queryAllByRole('status')).toHaveLength(0);
      emit('collab-publish-finished', run({ outcome: 'refused', trigger: 'manual', error: 'busy' }));
      expect(screen.getAllByRole('status')).toHaveLength(1);
    });

    it('a failed run is an error entry that carries the cause', async () => {
      const err = vi.spyOn(console, 'error').mockImplementation(() => {});
      await fire({ outcome: 'failed', error: 'disk full' });
      const [n] = history();
      expect(n.hasErrors).toBe(true);
      expect(n.detail).toContain('disk full');
      expect(err).toHaveBeenCalled();
      err.mockRestore();
    });

    it('a done run with every count 0 notifies nothing', async () => {
      await fire({});
      expect(history()).toHaveLength(0);
      expect(screen.queryAllByRole('status')).toHaveLength(0);
    });
  });
});
