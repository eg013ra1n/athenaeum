import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import CollabAttention from './CollabAttention';
import { api } from '../../api';
import type { CollabAttention as Attention, CollabAttentionChanged, LastCopyView } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const attention: Attention = {
  changed: [
    { frameUuid: 'u1', fileName: 'c_a.fits', path: '/c/m31/o/c_a.fits', detectedAt: '2026-09-25T10:00:00Z', newVersionWaiting: true },
  ],
  awaitingChoice: [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }],
  notKept: [{ frameUuid: 'u3', fileName: 'c_c.fits', contentVersion: 1 }],
  otherFiles: [{ path: '/c/m31/stray.fits', seenAt: '2026-09-25T09:00:00Z' }],
};

const empty: Attention = { changed: [], awaitingChoice: [], notKept: [], otherFiles: [] };

let preview: LastCopyView[];
let changedFile: (args: { confirmedDelete: boolean }) => Promise<unknown>;
let emitAttention: ((p: CollabAttentionChanged) => void) | undefined;

beforeEach(() => {
  localStorage.clear();
  emitAttention = undefined;
  preview = [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }];
  changedFile = ({ confirmedDelete }) =>
    confirmedDelete
      ? Promise.resolve({ trashed: false })
      : Promise.reject(new Error('trash_unavailable: the system trash is not available — confirm to delete the changed file'));
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { confirmedDelete: boolean }) => {
    switch (cmd) {
      case 'list_collab_attention':
        return Promise.resolve(attention);
      case 'preview_collab_stop_keeping':
        return Promise.resolve(preview);
      case 'resolve_collab_changed_file':
        return changedFile(args ?? { confirmedDelete: false });
      default:
        return Promise.resolve(1);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, h: (p: CollabAttentionChanged) => void) => {
    if (name === 'collab-attention-changed') emitAttention = h;
    return Promise.resolve(() => {});
  }) as never);
});

const renderIt = () =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        <CollabAttention projectId="p1" />
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

const attentionReads = () =>
  vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'list_collab_attention').length;

describe('CollabAttention', () => {
  it('lists the four kinds', async () => {
    renderIt();
    expect(await screen.findByText('Changed files')).toBeInTheDocument();
    expect(screen.getByText('A new version is waiting')).toBeInTheDocument();
    expect(screen.getByText('Waiting for your choice')).toBeInTheDocument();
    expect(screen.getByText(/Nothing else is paused/)).toBeInTheDocument();
    expect(screen.getByText('Not kept')).toBeInTheDocument();
    expect(screen.getByText('Other files')).toBeInTheDocument();
    expect(screen.getByText(/The app never deletes them/)).toBeInTheDocument();
    expect(screen.getByText('/c/m31/stray.fits')).toBeInTheDocument();
    expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'p1' });
  });

  it('renders nothing when nothing needs attention', async () => {
    vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(empty)) as never);
    const { container } = renderIt();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'p1' }));
    expect(container.querySelector('section')).toBeNull();
  });

  it('reloads on its own project\'s attention event only', async () => {
    renderIt();
    await screen.findByText('Changed files');
    expect(attentionReads()).toBe(1);
    act(() => emitAttention?.({ projectId: 'other' }));
    act(() => emitAttention?.({ projectId: 'p1' }));
    await waitFor(() => expect(attentionReads()).toBe(2));
  });

  it('every button has its own accessible name — per-row names carry the file name', async () => {
    const two: Attention = {
      changed: [
        ...attention.changed,
        { frameUuid: 'u4', fileName: 'c_d.fits', path: '/c/m31/o/c_d.fits', detectedAt: '2026-09-25T10:05:00Z', newVersionWaiting: false },
      ],
      awaitingChoice: [
        ...attention.awaitingChoice,
        { frameUuid: 'u5', fileName: 'c_e.fits', holdersOnline: 2, holdersTotal: 3, atRisk: false },
      ],
      notKept: [...attention.notKept, { frameUuid: 'u6', fileName: 'c_f.fits', contentVersion: 2 }],
      otherFiles: attention.otherFiles,
    };
    vi.mocked(api.invoke).mockImplementation(((cmd: string) =>
      Promise.resolve(cmd === 'list_collab_attention' ? two : 1)) as never);
    renderIt();
    await screen.findByText('Changed files');
    const names = screen.getAllByRole('button').map((b) => b.getAttribute('aria-label') ?? b.textContent ?? '');
    expect(new Set(names).size).toBe(names.length);
    for (const name of [
      'Re-fetch all',
      'Stop keeping all',
      'Keep all again',
      'Re-fetch c_b.fits',
      'Re-fetch c_e.fits',
      'Stop keeping c_b.fits',
      'Stop keeping c_e.fits',
      'Keep again c_c.fits',
      'Keep again c_f.fits',
      'Re-fetch original c_a.fits',
      'Re-fetch original c_d.fits',
      'Delete c_a.fits',
      'Delete c_d.fits',
    ]) {
      expect(screen.getAllByRole('button', { name })).toHaveLength(1);
    }
  });

  it('stop keeping warns about the last copy before it acts', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping all' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('preview_collab_stop_keeping', { projectId: 'p1', frameUuids: ['u2'] }),
    );
    expect(await screen.findByText(/fewer than 2 other copies/)).toBeInTheDocument();
    expect(screen.getByText(/0 online, 1 in total/)).toBeInTheDocument();
    expect(api.invoke).not.toHaveBeenCalledWith('resolve_collab_deletions', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: 'Stop keeping' }));
    // The frames the user was warned about are exactly the ones dropped.
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', {
        projectId: 'p1',
        frameUuids: ['u2'],
        action: 'stopKeeping',
      }),
    );
  });

  it('the last-copy warning names 10 frames and counts the rest; the confirm drops the whole previewed list', async () => {
    const many = Array.from({ length: 100 }, (_, i) => {
      const n = String(i).padStart(3, '0');
      return { frameUuid: `m${n}`, fileName: `c_${n}.fits`, holdersOnline: 0, holdersTotal: 1, atRisk: true };
    });
    preview = many;
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'list_collab_attention') return Promise.resolve({ ...empty, awaitingChoice: many });
      if (cmd === 'preview_collab_stop_keeping') return Promise.resolve(preview);
      return Promise.resolve(1);
    }) as never);
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping all' }));
    const message = await screen.findByText(/fewer than 2 other copies/);
    for (let i = 0; i < 10; i++) {
      expect(message).toHaveTextContent(`c_${String(i).padStart(3, '0')}.fits — fewer than 2 other copies`);
    }
    expect(message).not.toHaveTextContent('c_010.fits');
    expect(message).toHaveTextContent('…and 90 more (100 at risk in total)');
    fireEvent.click(screen.getByRole('button', { name: 'Stop keeping' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', {
        projectId: 'p1',
        frameUuids: many.map((r) => r.frameUuid),
        action: 'stopKeeping',
      }),
    );
  });

  it('stop keeping acts directly when every frame has enough other copies', async () => {
    preview = [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 2, holdersTotal: 3, atRisk: false }];
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping c_b.fits' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', {
        projectId: 'p1',
        frameUuids: ['u2'],
        action: 'stopKeeping',
      }),
    );
    expect(screen.queryByText(/fewer than 2 other copies/)).not.toBeInTheDocument();
  });

  it('re-fetch works per frame and for all', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch c_b.fits' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', { projectId: 'p1', frameUuids: ['u2'], action: 'refetch' }),
    );
    fireEvent.click(screen.getByRole('button', { name: 'Re-fetch all' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', { projectId: 'p1', frameUuids: null, action: 'refetch' }),
    );
  });

  it('re-fetch original falls back to a confirmed delete when there is no trash', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch original c_a.fits' }));
    expect(await screen.findByText(/system trash is not available/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Delete and re-fetch' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
        projectId: 'p1',
        frameUuid: 'u1',
        action: 'refetchOriginal',
        confirmedDelete: true,
      }),
    );
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('c_a.fits was deleted; the original is being re-fetched');
  });

  it('a re-fetch that trashed the changed file says so', async () => {
    changedFile = () => Promise.resolve({ trashed: true });
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch original c_a.fits' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('c_a.fits moved to the Trash; the original is being re-fetched');
    expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
      projectId: 'p1',
      frameUuid: 'u1',
      action: 'refetchOriginal',
      confirmedDelete: false,
    });
  });

  it('delete asks first, then deletes without a re-fetch', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Delete c_a.fits' }));
    await screen.findByText('Delete the changed file? It will not be re-fetched.');
    expect(api.invoke).not.toHaveBeenCalledWith('resolve_collab_changed_file', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
        projectId: 'p1',
        frameUuid: 'u1',
        action: 'delete',
        confirmedDelete: true,
      }),
    );
  });

  it('keep again works per frame and for all', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Keep again c_c.fits' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: ['u3'] }));
    fireEvent.click(screen.getByRole('button', { name: 'Keep all again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: null }));
  });

  it('a failed action is one warning notification', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'list_collab_attention') return Promise.resolve(attention);
      if (cmd === 'keep_collab_frames_again') return Promise.reject('collaboration project refused');
      return Promise.resolve(1);
    }) as never);
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Keep all again' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Keep again failed');
  });
});
